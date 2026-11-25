use std::{path::Path, time::Duration};
use zeroize::Zeroizing;
use crate::{
  egress::resolve_for_app,
  muse_native::{allocate_isolated_session, fetch_session_cookies, read_fingerprint_history},
  probe_cancel::CancelToken,
  rooms::{self, Dispatch, Milestone, RoomBook, RoomGate},
  send_ledger::SendLedger,
  storage::ProxySetting,
};

pub async fn run(
  app: &tauri::AppHandle, gate: &RoomGate, ledger: &SendLedger, path: &Path, dispatch: &Dispatch,
  proxy: Option<&ProxySetting>, mut token: CancelToken, recover: bool,
) -> Result<RoomBook, String> {
  let was_sent = if recover { Milestone::Uncertain } else { Milestone::FailedBeforeSending };
  let setup = async {
    let egress = resolve_for_app(app, proxy)?;
    let cookies = Zeroizing::new(fetch_session_cookies(app, &dispatch.attempt.account_uuid).await?);
    if cookies.is_empty() { return Err("请先登录该 Muse 账号".to_string()); }
    Ok((egress, cookies))
  };
  let ready = tokio::select! {
    biased;
    _ = token.cancelled() => None,
    result = tokio::time::timeout(Duration::from_secs(15), setup) => result.ok().and_then(Result::ok),
  };
  let Some((egress, cookies)) = ready else { return finish(gate, path, dispatch, was_sent).await; };
  if recover {
    let session = dispatch.attempt.session_id.as_deref().ok_or("没有隔离会话记录")?;
    let user_id = dispatch.attempt.user_message_id.as_deref().ok_or("没有可核对的发送回执")?;
    let result = tokio::select! {
      biased;
      _ = token.cancelled() => None,
      result = tokio::time::timeout(Duration::from_secs(24), Box::pin(read_fingerprint_history(&egress, &cookies, session, user_id))) =>
        result.ok().and_then(Result::ok).flatten(),
    };
    return finish(gate, path, dispatch, result.map_or(Milestone::Uncertain, |text| Milestone::Complete {
      text, source: "history",
    })).await;
  }

  let mission = match ledger.mission(&dispatch.attempt.account_uuid) {
    Ok(mission) => mission,
    Err(_) => return finish(gate, path, dispatch, Milestone::FailedBeforeSending).await,
  };
  let setup = Box::pin(async {
    let mut gateway = Box::pin(egress.connect(&cookies)).await.map_err(|_| ())?;
    let session = allocate_isolated_session(&mut gateway).await.map_err(|_| ())?;
    if uuid::Uuid::parse_str(&session).is_err() { return Err(()); }
    let route = muse_native_client::chat_send::IsolatedProbeRoute::new(session);
    let mut subscription = gateway.subscribe_isolated_probe(&route).await.map_err(|_| ())?;
    subscription.wait_ready(Duration::from_secs(25)).await.map_err(|_| ())?;
    Ok::<_, ()>((route, subscription))
  });
  let ready = tokio::select! {
    biased;
    _ = token.cancelled() => None,
    result = tokio::time::timeout(Duration::from_secs(65), setup) => result.ok().and_then(Result::ok),
  };
  let Some((route, mut subscription)) = ready else {
    return finish(gate, path, dispatch, Milestone::FailedBeforeSending).await;
  };
  let session = route.session_id();
  // The room goal stays in its room file. The shared journal receives a digest.
  let send_id = match mission.begin_room_send(session, &rooms::sha256(&dispatch.prompt)) {
    Ok(id) => id,
    Err(_) => return finish(gate, path, dispatch, Milestone::FailedBeforeSending).await,
  };
  let ready = {
    let _guard = gate.0.lock().await;
    rooms::record(path, dispatch, Milestone::Ready { session: session.into(), send_id: send_id.clone() })
  };
  if ready.is_err() || token.is_cancelled() || ledger.mark_in_flight_in(&send_id, Some(session)).is_err() {
    ledger.fail(&send_id, "尚未发送".into());
    return finish(gate, path, dispatch, Milestone::FailedBeforeSending).await;
  }
  let tracked = subscription.send_on_subscription(&dispatch.prompt, &route, Duration::from_secs(30), token.receiver()).await;
  let ack = match tracked.outcome {
    Ok(ack) if ack.session_id.as_deref() == Some(session) => {
      ledger.confirm(&send_id);
      ack
    }
    _ if tracked.on_wire => {
      ledger.mark_unknown(&send_id, "未能确认该隔离会话的发送回执".into());
      return finish(gate, path, dispatch, Milestone::Uncertain).await;
    }
    _ => {
      ledger.fail(&send_id, "尚未发送".into());
      return finish(gate, path, dispatch, Milestone::FailedBeforeSending).await;
    }
  };
  let ack_saved = {
    let _guard = gate.0.lock().await;
    !ledger.is_poisoned() && rooms::record(path, dispatch, Milestone::Acknowledged { user_message_id: ack.message_id.clone() }).is_ok()
  };
  if !ack_saved { return finish(gate, path, dispatch, Milestone::Uncertain).await; }
  let live = tokio::select! {
    biased;
    _ = token.cancelled() => None,
    result = subscription.collect(&ack, session, Duration::from_secs(95), Duration::from_secs(10), || {}) => Some(result),
  };
  if let Some(live) = live.filter(|reply| reply.done && !reply.text.trim().is_empty()) {
    return finish(gate, path, dispatch, Milestone::Complete { text: live.text, source: "stream" }).await;
  }
  let history = tokio::select! {
    biased;
    _ = token.cancelled() => None,
    result = tokio::time::timeout(Duration::from_secs(22), Box::pin(read_fingerprint_history(&egress, &cookies, session, &ack.message_id))) =>
      result.ok().and_then(Result::ok).flatten(),
  };
  finish(gate, path, dispatch, history.map_or(Milestone::Uncertain, |text| Milestone::Complete {
    text, source: "history",
  })).await
}

async fn finish(gate: &RoomGate, path: &Path, dispatch: &Dispatch, event: Milestone) -> Result<RoomBook, String> {
  let _guard = gate.0.lock().await;
  match rooms::record(path, dispatch, event) {
    Ok(book) => Ok(book),
    Err(error) => {
      // Resolve a late completion or storage-side failure conservatively. A
      // valid ACK remains available for an explicit read-only recovery.
      if rooms::record(path, dispatch, Milestone::Uncertain).is_ok() {
        rooms::apply(path, rooms::Command::List, &[], false)
      } else {
        let current = rooms::apply(path, rooms::Command::List, &[], false)?;
        if current.is_in_flight(dispatch.room_id, dispatch.attempt.id) {
          Err(error)
        } else {
          Ok(current)
        }
      }
    }
  }
}
