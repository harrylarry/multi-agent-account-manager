mod account_cookies;
mod account_backup;
mod muse_native;
mod muse_sessions;
mod platform;
mod hark_native;
#[cfg(any(target_os = "ios", test))]
mod hark_send;
mod probe_cancel;
mod proxy_pool;
mod rooms;
#[cfg(target_os = "ios")]
mod room_dispatch;
mod webview_proxy;
mod providers;
mod quota;
mod secrets;
mod send_ledger;
mod session_manager;
mod storage;
mod updater;

#[cfg(not(target_os = "android"))]
mod egress;

use session_manager::{ManagerSnapshot, SessionManager, APP_TITLE};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use updater::{InstallUpdateOutcome, InstallUpdateRequest, UpdateCheckResult};

/// #11/#16：取账号的代理设置，交给原生探针做统一出站决策。
async fn account_proxy_setting(
  mgr: &SessionManager,
  provider_id: &str,
  uuid: &str,
) -> Option<storage::ProxySetting> {
  let pid = providers::ProviderId::parse(provider_id)?;
  mgr.proxy_setting(pid, uuid).await
}

#[tauri::command]
async fn get_manager_state(mgr: State<'_, Arc<SessionManager>>) -> Result<ManagerSnapshot, String> {
  Ok(mgr.get_snapshot().await)
}

#[tauri::command]
async fn select_provider(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
) -> Result<ManagerSnapshot, String> {
  mgr.select_provider(&app, &provider_id).await
}

#[tauri::command]
async fn add_account(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
  alias: String,
  cookie_input: Option<String>,
) -> Result<ManagerSnapshot, String> {
  let cookies = cookie_input.map(|raw| parse_account_cookies(&provider_id, raw)).transpose()?;
  mgr.add_account(&app, &provider_id, &alias, cookies.as_deref()).await
}

#[tauri::command]
async fn manage_rooms(
  app: AppHandle, mgr: State<'_, Arc<SessionManager>>, gate: State<'_, rooms::RoomGate>, command: rooms::Command,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<rooms::RoomBook, String> {
  let evidence = match &command {
    rooms::Command::BindSession { account_uuid, thread_id, .. } => {
      let sessions = list_muse_sessions(app.clone(), mgr.clone(), cancel.clone(), account_uuid.clone()).await?;
      Some(sessions.into_iter().find(|session| session.id == *thread_id).ok_or("该会话不在所选账号的原生会话列表中")?.title)
    }
    rooms::Command::SaveArtifact { input: rooms::ArtifactInput::CodeChange { url }, .. } => {
      let (owner, repo, sha) = rooms::commit_coordinates(url.trim())?;
      let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(12)).build().map_err(|_| "无法初始化提交核验")?;
      let endpoint = format!("https://api.github.com/repos/{owner}/{repo}/commits/{sha}");
      let mut response = client.get(endpoint).header("User-Agent", "Diehao-iOS")
        .header("Accept", "application/vnd.github.sha").send().await.map_err(|_| "无法连接 GitHub 核验提交")?;
      if !response.status().is_success() || response.content_length().is_some_and(|len| len > 1024) {
        return Err("GitHub 未提供可核验的公开提交 SHA".into());
      }
      let mut body = Vec::new();
      while let Some(chunk) = response.chunk().await.map_err(|_| "读取提交凭证失败")? {
        if body.len() + chunk.len() > 1024 { return Err("GitHub 未提供可核验的提交 SHA".into()); }
        body.extend_from_slice(&chunk);
      }
      if std::str::from_utf8(&body).ok().map(str::trim) != Some(sha.as_str()) { return Err("GitHub 返回的提交 SHA 不匹配".into()); }
      Some(sha)
    }
    _ => None,
  };
  let interrupt = match &command { rooms::Command::Stop { id, .. } | rooms::Command::Interrupt { id } => Some(*id), _ => None };
  let mut initialized = gate.0.lock().await;
  let dir = app.path().app_data_dir().map_err(|_| "无法读取应用目录")?;
  let snapshot = mgr.get_snapshot().await;
  let accounts: Vec<_> = snapshot.accounts.into_iter().filter(|account|
    !snapshot.conflicts.iter().any(|conflict| conflict.uuid == account.uuid)
      && !snapshot.pending_cleanup.iter().any(|key| key.uuid == account.uuid)).collect();
  let result = rooms::apply_verified(&dir.join("rooms-v1.json"), command, &accounts, !*initialized, evidence);
  if let Ok(book) = &result {
    *initialized = true;
    if let Some((account, task)) = interrupt.and_then(|id| book.cancellation_target(id)) {
      cancel.cancel_task(&account, task);
    }
  }
  result
}

#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn share_room_artifact(
  app: AppHandle, gate: State<'_, rooms::RoomGate>, room: uuid::Uuid, artifact: uuid::Uuid,
) -> Result<bool, String> {
  #[cfg(target_os = "ios")]
  {
    let (name, contents) = {
      let _guard = gate.0.lock().await;
      let path = app.path().app_data_dir().map_err(|_| "无法读取应用目录")?.join("rooms-v1.json");
      rooms::apply(&path, rooms::Command::List, &[], false)?.artifact_for_export(room, artifact)?
    };
    tauri_plugin_account_session::share_room_artifact(&app, &name, &contents).await
  }
  #[cfg(not(target_os = "ios"))]
  Err("附件分享需要 iOS".into())
}

#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn run_room(
  app: AppHandle, mgr: State<'_, Arc<SessionManager>>, gate: State<'_, rooms::RoomGate>,
  ledger: State<'_, Arc<send_ledger::SendLedger>>, cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
  fingerprints: State<'_, FingerprintGate>,
  id: uuid::Uuid, revision: u64, recover: bool,
) -> Result<rooms::RoomBook, String> {
  #[cfg(target_os = "ios")]
  {
    use tauri::Emitter;
    let _exclusive = fingerprints.0.try_lock().map_err(|_| "Muse 指纹任务正在运行，请结束后再派发房间")?;
    let dir = app.path().app_data_dir().map_err(|_| "无法读取应用目录")?;
    let path = dir.join("rooms-v1.json");
    let mut next_revision = revision;
    loop {
      let proxy_guard = mgr.proxy_write_lock.lock().await;
      let mut initialized = gate.0.lock().await;
      let snapshot = mgr.get_snapshot().await;
      let current = rooms::apply(&path, rooms::Command::List, &snapshot.accounts, !*initialized)?;
      *initialized = true;
      if next_revision != revision && current.next_relay_revision(id) != Some(next_revision) {
        return Ok(current);
      }
      let allowed: Vec<_> = snapshot.accounts.into_iter().filter(|account| {
        !snapshot.conflicts.iter().any(|conflict| conflict.uuid == account.uuid)
          && !snapshot.pending_cleanup.iter().any(|key| key.uuid == account.uuid)
      }).collect();
      let Some(dispatch) = rooms::begin_dispatch(&path, id, next_revision, &allowed, recover)? else {
        return rooms::apply(&path, rooms::Command::List, &allowed, false);
      };
      let account = allowed.iter().find(|account| account.uuid == dispatch.attempt.account_uuid
        && account.provider_id == providers::ProviderId::Muse).ok_or("账号不存在")?;
      let worker_id = dispatch.attempt.worker_id.unwrap_or(dispatch.attempt.id);
      let (token, _cancel_guard) = cancel.register_task(&account.uuid, "room", worker_id);
      drop(initialized);
      drop(proxy_guard);
      let _ = app.emit("room-progress", id);
      let result = Box::pin(room_dispatch::run(
        &app, &gate, &ledger, &path, &dispatch, account.proxy.as_ref(), token, recover,
      )).await?;
      let _ = app.emit("room-progress", id);
      // Explicit recovery reads just this attempt, without authorizing another send.
      let Some(revision) = result.after_relay_attempt(id, &dispatch.attempt).filter(|_| !recover) else {
        return Ok(result);
      };
      next_revision = revision;
    }
  }
  #[cfg(not(target_os = "ios"))]
  { Err("房间真实派发需要 iOS".into()) }
}

#[tauri::command]
async fn rename_account(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
  uuid: String,
  alias: String,
  group: Option<String>,
  timezone: Option<String>,
) -> Result<ManagerSnapshot, String> {
  mgr.rename_account(&app, &provider_id, &uuid, &alias, group.as_deref(), timezone.as_deref()).await
}

#[tauri::command]
async fn remove_account(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
  uuid: String,
) -> Result<ManagerSnapshot, String> {
  mgr.remove_account(&app, &provider_id, &uuid).await
}

#[tauri::command]
async fn switch_account(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
  uuid: String,
) -> Result<ManagerSnapshot, String> {
  mgr.switch_account(&app, &provider_id, &uuid).await
}

#[tauri::command]
async fn set_modal_active(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  active: bool,
) -> Result<ManagerSnapshot, String> {
  mgr.set_modal_active(&app, active).await
}

#[tauri::command]
async fn open_official_site(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
) -> Result<ManagerSnapshot, String> {
  mgr.open_official_site(&app, &provider_id).await
}

#[tauri::command]
async fn resolve_uuid_conflict(
  mgr: State<'_, Arc<SessionManager>>,
  keep_provider_id: String,
  uuid: String,
) -> Result<ManagerSnapshot, String> {
  mgr.resolve_uuid_conflict(&keep_provider_id, &uuid).await
}

#[tauri::command]
async fn dismiss_notice(mgr: State<'_, Arc<SessionManager>>) -> Result<ManagerSnapshot, String> {
  Ok(mgr.dismiss_notice().await)
}

#[tauri::command]
async fn spike_ios_quota_sync_probe(
  app: tauri::AppHandle,
) -> Result<Vec<session_manager::QuotaSyncDiagnostic>, String> {
  if !muse_native::is_spike_build(app.clone()) {
    return Err("此功能仅用于 Spike 构建".into());
  }
  #[cfg(target_os = "ios")]
  {
    app.state::<Arc<SessionManager>>().sync_ios_quotas(&app, true).await
  }
  #[cfg(not(target_os = "ios"))]
  {
    Err("此功能仅支持 iOS".into())
  }
}

#[tauri::command]
async fn trigger_quota_sync(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  force: Option<bool>,
) -> Result<ManagerSnapshot, String> {
  // Batch-1: auto-triggers pass force=false (the 30s throttle applies);
  // the desktop button omits it (force=true, as before).
  mgr.trigger_silent_sync(&app, force.unwrap_or(true)).await;
  Ok(mgr.get_snapshot().await)
}

/// Batch-1: reorder one provider's accounts (long-press + drag in the list).
#[tauri::command]
async fn reorder_accounts(
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
  uuids: Vec<String>,
) -> Result<ManagerSnapshot, String> {
  mgr.reorder_accounts(&provider_id, uuids).await
}

#[tauri::command]
async fn check_for_updates(app: AppHandle) -> Result<UpdateCheckResult, String> {
  updater::check_for_updates(&app).await
}

#[tauri::command]
async fn download_and_install_update(
  app: AppHandle,
  request: InstallUpdateRequest,
) -> Result<InstallUpdateOutcome, String> {
  updater::download_and_install_update(&app, request).await
}

#[tauri::command]
async fn open_external_url(app: AppHandle, url: String) -> Result<(), String> {
  updater::open_external_url(&app, &url).await
}

/// Spike 0.1.22：原生握手诊断（取凭证 → WSS → Noise 握手 → ping）。
/// 只做诊断，不发聊天；结果不含任何密钥。由 Spike 版启动自检或手动调用。
#[tauri::command]
async fn muse_native_diagnose(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  mgr: State<'_, Arc<SessionManager>>,
) -> Result<muse_native::SpikeDiagnose, String> {
  let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
  muse_native::diagnose(&app, &provider_id, &uuid, setting).await
}

/// B1：`chat.subscribe` 探针（Issue #13 #5）。订阅 N 秒，只返回事件计数与去重结果。
#[tauri::command]
async fn muse_native_watch_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  task_id: String,
  seconds: u64,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<muse_native::WatchProbe, String> {
  let task = uuid::Uuid::parse_str(&task_id).map_err(|_| "订阅任务标识无效".to_string())?;
  let (mut token, _guard) = cancel.register_task(&uuid, "watch", task);
  tokio::select! {
    biased;
    _ = token.cancelled() => Err("订阅探针已停止".into()),
    result = tokio::time::timeout(std::time::Duration::from_secs(120), async {
      let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
      muse_native::watch_probe(&app, &provider_id, &uuid, seconds, setting.as_ref()).await
    }) => result.map_err(|_| "订阅探针达到总时限".to_string())?,
  }
}

#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn observe_account_tasks(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
  provider_id: String,
  uuid: String,
  timezone: String,
  automatic: Option<bool>,
) -> Result<serde_json::Value, String> {
  #[cfg(target_os = "ios")]
  {
    let provider = providers::ProviderId::parse(&provider_id).ok_or("服务标识无效")?;
    let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
    let proxy_lock = mgr.proxy_write_lock.lock().await;
    let (mut token, _guard) = cancel.register(&uuid, "product_task_watch");
    let snapshot = mgr.get_snapshot().await;
    drop(proxy_lock);
    let account = snapshot.accounts.iter().find(|account| account.provider_id == provider && account.uuid == uuid
      && account.auth_state == storage::AuthState::Authenticated).ok_or("请先登录该账号")?;
    if snapshot.conflicts.iter().any(|entry| entry.uuid == uuid)
      || snapshot.pending_cleanup.iter().any(|key| key.uuid == uuid) {
      return Err("请先处理账号归属或清理状态".into());
    }
    let timezone = account.timezone.as_deref().unwrap_or(&timezone);
    if timezone.is_empty() || timezone.len() > 96
      || !timezone.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_+-".contains(&b)) {
      return Err("系统时区格式无效".into());
    }
    if automatic.unwrap_or(false)
      && (!account.task_notifications || !platform::ios::automatic_task_polling_available(&app).await?)
    {
      return Err("自动任务检测暂缓".into());
    }
    // Construct the native network futures on a Tokio worker. The WKWebView
    // IPC callback has a small main-thread stack and must not construct them.
    let app = app.clone();
    let mgr = Arc::clone(&mgr);
    let timezone = timezone.to_owned();
    let task_notifications = account.task_notifications;
    let proxy = account.proxy.clone();
    let observation = tokio::spawn(async move {
      let native = Box::pin(async {
        match provider {
          providers::ProviderId::Muse => {
            let probe = Box::pin(muse_native::watch_probe(&app, "muse", &uuid, 5, proxy.as_ref())).await
              .ok().filter(|probe| probe.status.is_some_and(|status| (200..300).contains(&status)));
            probe.map_or((None, Vec::new()), |probe| {
              (probe.ok.then_some(probe.summary.activity).flatten(), probe.completed_ids)
            })
          }
          providers::ProviderId::Hark => {
            let probe = Box::pin(hark_native::diagnose(&app, &uuid, proxy.as_ref(), &timezone)).await;
            (probe.task_status(), probe.completed_ids)
          }
        }
      });
      let approval_poll = Box::pin(async {
        if task_notifications && provider == providers::ProviderId::Muse {
          Box::pin(muse_native::approvals_probe(&app, "muse", &uuid, proxy.as_ref())).await
            .ok().filter(|result| result.ok).map(|result| {
              let now = storage::now_ms() as i64;
              result.items.into_iter().filter(|item| {
                item.status == "pending" && (item.expires_at_ms == 0 || item.expires_at_ms > now)
              }).collect::<Vec<_>>()
            })
        } else {
          None
        }
      });
      if token.is_cancelled() {
        return Err("账号出站配置已变化，已停止检测".into());
      }
      let ((native, completed), approvals) = tokio::select! {
        biased;
        _ = token.cancelled() => return Err("账号出站配置已变化，已停止检测".into()),
        status = tokio::time::timeout(std::time::Duration::from_secs(32), Box::pin(async {
          tokio::join!(native, approval_poll)
        })) => status.unwrap_or_default(),
      };
      mgr.record_task_observations(&app, provider, &uuid, &completed, approvals.as_deref()).await?;
      if let Some(activity) = native {
        return Ok(serde_json::json!({ "activity": activity, "source": "native", "observedAtMs": storage::now_ms() }));
      }
      if provider == providers::ProviderId::Muse && !token.is_cancelled() {
        let fallback = tokio::time::timeout(std::time::Duration::from_secs(3), platform::ios::read_parked_task_state(&app, &uuid))
          .await.ok().and_then(Result::ok).flatten();
        if let Some(activity) = fallback {
          return Ok(serde_json::json!({ "activity": activity, "source": "page", "observedAtMs": storage::now_ms() }));
        }
      }
      Ok(serde_json::json!({ "activity": "unknown", "source": null, "observedAtMs": null }))
    });
    observation.await.map_err(|_| "任务检测未能完成".to_string())?
  }
  #[cfg(not(target_os = "ios"))]
  {
    Err("此功能需要 iOS".into())
  }
}

#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn set_account_task_notifications(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
  uuid: String,
  enabled: bool,
) -> Result<ManagerSnapshot, String> {
  #[cfg(target_os = "ios")]
  {
    let provider = providers::ProviderId::parse(&provider_id).ok_or("服务标识无效")?;
    let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
    let snapshot = mgr.get_snapshot().await;
    if !snapshot.accounts.iter().any(|account| account.provider_id == provider && account.uuid == uuid) {
      return Err("账号不存在".into());
    }
    if enabled {
      let _ = platform::ios::task_notification_permission(&app).await;
    }
    mgr.set_task_notifications(&app, provider, &uuid, enabled).await
  }
  #[cfg(not(target_os = "ios"))]
  {
    Err("此功能需要 iOS".into())
  }
}

/// B2：只读拉取 `egress.approvals`。
#[tauri::command]
async fn muse_native_approvals_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<muse_native::ApprovalsProbe, String> {
  let uuid = storage::normalize_uuid(&uuid).ok_or_else(|| "账号标识无效".to_string())?;
  if provider_id != "muse" || !mgr.get_snapshot().await.accounts.iter().any(|account| {
    account.provider_id == providers::ProviderId::Muse && account.uuid == uuid
      && account.auth_state == storage::AuthState::Authenticated
  }) {
    return Err("审批查询需要已登录的 Muse 账号".to_string());
  }
  let (mut token, _guard) = cancel.register(&uuid, "approvals");
  tokio::select! {
    biased;
    _ = token.cancelled() => Err("审批查询已停止".into()),
    response = tokio::time::timeout(std::time::Duration::from_secs(60), async {
      let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
      muse_native::approvals_probe(&app, &provider_id, &uuid, setting.as_ref()).await
    }) => response.map_err(|_| "审批查询超时".to_string())?,
  }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalDecisionRequest {
  approval_id: String,
  session_scope_id: String,
  expires_at_ms: i64,
  decision: String,
  scope: Option<String>,
  permanent_confirmed: bool,
}

#[tauri::command]
#[cfg_attr(target_os = "android", allow(unused_variables))]
async fn decide_muse_approval(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
  uuid: String,
  request: ApprovalDecisionRequest,
) -> Result<(), String> {
  #[cfg(not(target_os = "android"))]
  {
    use muse_native_client::{approvals, transport::{HttpRequest, Service}};
    let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
    if request.approval_id.is_empty() || request.approval_id.len() > 128
      || !request.approval_id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
      || request.session_scope_id.is_empty() || request.session_scope_id.len() > 128
      || !matches!(request.decision.as_str(), "allow_once" | "deny" | "allow_always")
      || (request.decision == "allow_always" && (!request.permanent_confirmed
        || !matches!(request.scope.as_deref(), Some("destination_domain" | "entity"))))
      || (request.decision != "allow_always" && request.scope.is_some())
    {
      return Err("审批选择或标识无效".into());
    }
    let proxy_lock = mgr.proxy_write_lock.lock().await;
    let (mut token, _guard) = cancel.register(&uuid, "approval_decision");
    let snapshot = mgr.get_snapshot().await;
    drop(proxy_lock);
    let account = snapshot.accounts.iter().find(|account| {
      account.provider_id == providers::ProviderId::Muse && account.uuid == uuid
        && account.auth_state == storage::AuthState::Authenticated
    }).ok_or("请先登录该 Muse 账号")?;
    if snapshot.conflicts.iter().any(|entry| entry.uuid == uuid)
      || snapshot.pending_cleanup.iter().any(|key| key.uuid == uuid) {
      return Err("请先处理账号归属或清理状态".into());
    }
    let check = async {
      let cookies = zeroize::Zeroizing::new(muse_native::cookie_header_for(&app, &uuid).await?);
      if cookies.is_empty() { return Err("该账号没有登录态".into()); }
      let cookies = muse_native_client::cookie::SessionCookies::from_header(&cookies);
      let mut gateway = egress::resolve_for_app(&app, account.proxy.as_ref())?
        .connect(&cookies).await.map_err(|_| "无法连接审批服务")?;
      let path = format!("/approvals/{}", request.approval_id);
      let (status, body) = gateway.request(HttpRequest {
        service: Service::Sentinel, verb: "GET".into(), path: path.clone(),
        headers: vec![("x-app-id".into(), "hatch-web".into())], body: Vec::new(),
      }).await.map_err(|_| "无法复查审批")?;
      let detail: serde_json::Value = serde_json::from_slice(&body).map_err(|_| "审批复查响应无法识别")?;
      if !(200..300).contains(&status) || !approvals::validate_pending_decision(
        &detail, &request.approval_id, &request.session_scope_id, request.expires_at_ms,
        storage::now_ms() as i64, &request.decision, request.scope.as_deref(),
      ) {
        return Err("审批状态、会话、期限或授权范围已变化，请刷新核对".into());
      }
      Ok::<_, String>((gateway, path))
    };
    let (mut gateway, path) = tokio::select! {
      biased;
      _ = token.cancelled() => return Err("审批已停止，尚未提交".into()),
      result = tokio::time::timeout(std::time::Duration::from_secs(40), check) =>
        result.map_err(|_| "审批复查超时，尚未提交".to_string())??,
    };

    let key = ring::digest::digest(&ring::digest::SHA256, format!("{}|{}", uuid, request.approval_id).as_bytes());
    let key: String = key.as_ref().iter().map(|byte| format!("{byte:02x}")).collect();
    if storage::now_ms() as i64 >= request.expires_at_ms || token.is_cancelled() {
      return Err("审批已停止或过期，尚未提交".into());
    }
    mgr.record_approval_attempt(&key, false).await?;
    let mut body = serde_json::json!({
      "approval_id": request.approval_id, "decision": request.decision,
      "notification_channel":"ui", "reason": null, "expires_at_ms": null, "expected_message_revision": null,
    });
    if let Some(scope) = request.scope {
      body["always_scope"] = scope.clone().into();
      body["allow_always_scope"] = scope.into();
    }
    let sent = tokio::select! {
      biased;
      _ = token.cancelled() => return Err("审批结果未知，请打开会话核对".into()),
      result = tokio::time::timeout(std::time::Duration::from_secs(20), gateway.request(HttpRequest {
        service: Service::Sentinel, verb: "POST".into(), path: format!("{path}/decide"),
        headers: vec![("Content-Type".into(), "application/json".into()), ("x-app-id".into(), "hatch-web".into())],
        body: serde_json::to_vec(&body).map_err(|_| "无法编码审批选择")?,
      })) => result,
    };
    let Ok(Ok((status, response))) = sent else {
      return Err("审批结果未知，请打开会话核对".into());
    };
    let acknowledged = serde_json::from_slice(&response).ok().is_some_and(|response| approvals::confirms_decision(
      &response, &request.approval_id, &request.decision,
    ));
    if !(200..300).contains(&status) || !acknowledged {
      return Err("审批结果未知，请打开会话核对".into());
    }
    mgr.record_approval_attempt(&key, true).await
  }
  #[cfg(target_os = "android")]
  {
    Err("此操作在 Android 上不可用".into())
  }
}

#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn process_approval_notification_actions(
  app: AppHandle, mgr: State<'_, Arc<SessionManager>>, cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<Vec<String>, String> {
  #[cfg(target_os = "ios")]
  {
    let mut notes = Vec::new();
    for item in tauri_plugin_account_session::take_approval_notification_actions(&app).await? {
      let (Some(uuid), Some(approval_id), Some(session_scope_id), Some(expires_at_ms), Some(decision)) = (
        item["uuid"].as_str(), item["approvalId"].as_str(), item["sessionScopeId"].as_str(),
        item["expiresAtMs"].as_i64(), item["decision"].as_str(),
      ) else { continue; };
      if !matches!(decision, "allow_once" | "deny") { continue; }
      let request = ApprovalDecisionRequest { approval_id:approval_id.into(), session_scope_id:session_scope_id.into(),
        expires_at_ms, decision:decision.into(), scope:None, permanent_confirmed:false };
      match Box::pin(decide_muse_approval(app.clone(), mgr.clone(), cancel.clone(), uuid.to_string(), request)).await {
        Ok(()) => notes.push(if decision == "allow_once" { "通知审批已允许一次" } else { "通知审批已拒绝" }.into()),
        Err(error) => notes.push(error),
      }
    }
    Ok(notes)
  }
  #[cfg(not(target_os = "ios"))]
  Ok(Vec::new())
}

#[tauri::command]
fn preview_account_cookies(provider_id: String, raw: String) -> Result<serde_json::Value, String> {
  let cookies = parse_account_cookies(&provider_id, raw)?;
  let domains = cookies.iter().map(|cookie| cookie.domain.trim_start_matches('.'))
    .collect::<std::collections::BTreeSet<_>>();
  let next_expiry = cookies.iter().filter_map(|cookie| cookie.expiration_date)
    .min_by(f64::total_cmp);
  Ok(serde_json::json!({ "count": cookies.len(), "domains": domains, "nextExpiry": next_expiry }))
}

fn parse_account_cookies(provider_id: &str, raw: String) -> Result<Vec<account_cookies::AccountCookie>, String> {
  let raw = zeroize::Zeroizing::new(raw);
  let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
    .map_err(|_| "系统时间异常".to_string())?.as_secs_f64();
  account_cookies::parse(&raw, provider_id, now)
}

#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn transfer_account_cookies(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
  provider_id: String,
  uuid: String,
  raw: Option<String>,
) -> Result<serde_json::Value, String> {
  #[cfg(target_os = "ios")]
  {
    let provider = providers::ProviderId::parse(&provider_id).ok_or("服务标识无效")?;
    let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
    let cookies = raw.map(|raw| parse_account_cookies(&provider_id, raw)).transpose()?;
    if cookies.is_some() {
      cancel.cancel_account(&uuid);
    }
    let (result, skipped) = mgr.with_cookie_profile(&app, provider, &uuid, cookies.as_deref(), false).await?;
    Ok(serde_json::json!({ "count": result.count, "skipped": skipped }))
  }
  #[cfg(not(target_os = "ios"))]
  {
    Err("此操作仅支持 iOS".to_string())
  }
}

#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn list_muse_sessions(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
  uuid: String,
) -> Result<Vec<muse_sessions::MuseSession>, String> {
  #[cfg(target_os = "ios")]
  {
    let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
    let proxy_lock = mgr.proxy_write_lock.lock().await;
    let (mut token, _guard) = cancel.register(&uuid, "sessions");
    let snapshot = mgr.get_snapshot().await;
    drop(proxy_lock);
    let account = snapshot.accounts.iter().find(|account| {
      account.provider_id == providers::ProviderId::Muse && account.uuid == uuid
        && account.auth_state == storage::AuthState::Authenticated
    }).ok_or("请先登录 Muse 账号")?;
    if snapshot.conflicts.iter().any(|entry| entry.uuid == uuid)
      || snapshot.pending_cleanup.iter().any(|key| key.uuid == uuid) {
      return Err("请先解决账号归属或清理状态".to_string());
    }
    tokio::select! {
      biased;
      _ = token.cancelled() => Err("会话查询已停止".into()),
      result = tokio::time::timeout(std::time::Duration::from_secs(35), async {
        let header = zeroize::Zeroizing::new(muse_native::cookie_header_for(&app, &uuid).await?);
        if header.is_empty() { return Err("没有可用的 Muse 登录态".into()); }
        let cookies = muse_native_client::cookie::SessionCookies::from_header(&header);
        let egress = egress::resolve_for_app(&app, account.proxy.as_ref())?;
        Box::pin(muse_sessions::fetch(&egress, &cookies)).await
      }) => result.map_err(|_| "会话查询超时".to_string())?,
    }
  }
  #[cfg(not(target_os = "ios"))]
  {
    Err("此操作仅支持 iOS".to_string())
  }
}

#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn open_muse_thread(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  uuid: String,
  thread_id: String,
) -> Result<ManagerSnapshot, String> {
  #[cfg(target_os = "ios")]
  {
    let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
    let thread = uuid::Uuid::parse_str(&thread_id).map_err(|_| "会话标识无效")?;
    mgr.open_muse_thread(&app, uuid, thread).await
  }
  #[cfg(not(target_os = "ios"))]
  {
    Err("此操作仅支持 iOS".to_string())
  }
}

#[tauri::command]
async fn export_account_backup(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  gate: State<'_, proxy_pool::ProxyPoolGate>,
  password: String,
  include_groups: bool,
  include_proxy_endpoints: bool,
  expiry_days: Option<u8>,
  include_rooms: Option<bool>,
  room_gate: State<'_, rooms::RoomGate>,
) -> Result<bool, String> {
  #[cfg(target_os = "ios")]
  {
    let snapshot = mgr.get_snapshot().await;
    let mut pool = Vec::new();
    if include_proxy_endpoints {
      let _guard = gate.0.lock().await;
      let dir = app.path().app_data_dir().map_err(|_| "无法读取应用目录")?;
      pool = proxy_pool::ProxyPool::read(&secrets::Secrets::new(&dir))?.backup_metadata()?;
      for restored in mgr.restored_proxy_metadata().await {
        if !pool.contains(&restored) { pool.push(restored); }
      }
    }
    let expiration = match expiry_days {
      None | Some(0) => None,
      Some(days @ (1 | 7 | 30)) => Some(storage::now_ms() + u64::from(days) * 86_400_000),
      _ => return Err("备份有效期无效".into()),
    };
    let mut backup = account_backup::from_accounts(&snapshot.accounts, include_groups, include_proxy_endpoints, pool, expiration);
    if include_rooms.unwrap_or(false) {
      let _guard = room_gate.0.lock().await;
      let path = app.path().app_data_dir().map_err(|_| "无法读取应用目录")?.join("rooms-v1.json");
      let book = rooms::apply(&path, rooms::Command::List, &[], false)?;
      book.validate_archive()?;
      backup.rooms = Some(book);
    }
    let password = zeroize::Zeroizing::new(password);
    let encrypted = tokio::task::spawn_blocking(move || account_backup::encrypt(&backup, &password))
      .await.map_err(|_| "备份加密操作失败".to_string())??;
    platform::ios::share_account_backup(&app, &encrypted).await
  }
  #[cfg(not(target_os = "ios"))]
  {
    let _ = (app, mgr, gate, password, include_groups, include_proxy_endpoints, expiry_days, include_rooms, room_gate);
    Err("此操作仅支持 iOS".into())
  }
}

#[tauri::command]
async fn preview_account_backup(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  room_gate: State<'_, rooms::RoomGate>,
  encrypted: String,
  password: String,
) -> Result<account_backup::RestorePreview, String> {
  let password = zeroize::Zeroizing::new(password);
  let backup = tokio::task::spawn_blocking(move || account_backup::decrypt(&encrypted, &password))
    .await.map_err(|_| "备份解密操作失败".to_string())??;
  let mut result = account_backup::preview(&backup, &mgr.get_snapshot().await.accounts);
  if backup.rooms.is_some() {
    let _guard = room_gate.0.lock().await;
    let path = app.path().app_data_dir().map_err(|_| "无法读取应用目录")?.join("rooms-v1.json");
    result.room_checksum = Some(rooms::apply(&path, rooms::Command::List, &[], false)?.archive_checksum()?);
  }
  Ok(result)
}

#[tauri::command]
async fn restore_room_backup(
  app: AppHandle, room_gate: State<'_, rooms::RoomGate>, encrypted: String, password: String,
  overwrite: bool, room_checksum: String,
) -> Result<(), String> {
  let password = zeroize::Zeroizing::new(password);
  let backup = tokio::task::spawn_blocking(move || account_backup::decrypt(&encrypted, &password))
    .await.map_err(|_| "备份解密操作失败")??;
  let archive = backup.rooms.ok_or("备份不含房间资料")?;
  let _guard = room_gate.0.lock().await;
  let path = app.path().app_data_dir().map_err(|_| "无法读取应用目录")?.join("rooms-v1.json");
  rooms::restore_archive(&path, archive, overwrite, &room_checksum)
}

#[tauri::command]
async fn restore_account_backup(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  encrypted: String,
  password: String,
  overwrite: bool,
  preview_checksum: String,
) -> Result<ManagerSnapshot, String> {
  let password = zeroize::Zeroizing::new(password);
  let backup = tokio::task::spawn_blocking(move || account_backup::decrypt(&encrypted, &password))
    .await.map_err(|_| "备份解密操作失败".to_string())??;
  mgr.restore_account_backup(&app, backup, overwrite, &preview_checksum).await
}

#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn inspect_account_cookies(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
  uuid: String,
) -> Result<serde_json::Value, String> {
  #[cfg(target_os = "ios")]
  {
    let provider = providers::ProviderId::parse(&provider_id).ok_or("服务标识无效")?;
    let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
    let (info, _) = mgr.with_cookie_profile(&app, provider, &uuid, None, true).await?;
    Ok(serde_json::json!({ "count": info.count, "nextExpiry": info.next_expiry }))
  }
  #[cfg(not(target_os = "ios"))]
  {
    Err("此操作仅支持 iOS".into())
  }
}

/// B3：只读拉一次额度（Server Action 冷发现 + 重试），失败时带每步诊断。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaProbe {
  pub ok: bool,
  pub quota: Option<quota::QuotaSnapshot>,
  pub diag: Vec<String>,
  pub error: Option<String>,
}

#[tauri::command]
async fn muse_native_quota_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  cache: tauri::State<'_, std::sync::Arc<quota::ActionCache>>,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<QuotaProbe, String> {
  if provider_id != "muse" {
    return Err("额度探针只支持 Muse 账号".to_string());
  }
  // 额度请求也属于账号出站流量：启用代理的账号必须经代理，解析失败即失败。
  let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
  let proxy_lock = mgr.proxy_write_lock.lock().await;
  let (mut token, _guard) = cancel.register(&uuid, "quota");
  let snapshot = mgr.get_snapshot().await;
  drop(proxy_lock);
  let account = snapshot.accounts.iter().find(|account| {
    account.provider_id == providers::ProviderId::Muse && account.uuid == uuid
      && account.auth_state == storage::AuthState::Authenticated
  }).ok_or("请先登录该 Muse 账号")?;
  if snapshot.conflicts.iter().any(|entry| entry.uuid == uuid)
    || snapshot.pending_cleanup.iter().any(|key| key.uuid == uuid) {
    return Err("请先处理账号归属或清理状态".into());
  }
  let setting = account.proxy.as_ref();
  let egress = match egress::resolve_for_app(&app, setting) {
    Ok(e) => e,
    Err(e) => {
      mgr.record_native_quota(&app, &uuid, None).await?;
      return Ok(QuotaProbe {
        ok: false,
        quota: None,
        diag: vec![format!("出站策略不可用（未回退直连）: {e}")],
        error: Some(e),
      })
    }
  };
  let client = match egress.http_client(std::time::Duration::from_secs(15), false) {
    Ok(c) => c,
    Err(e) => {
      mgr.record_native_quota(&app, &uuid, None).await?;
      return Ok(QuotaProbe {
        ok: false,
        quota: None,
        diag: vec![format!("HTTP 客户端不可用: {e}")],
        error: Some(e.to_string()),
      })
    }
  };
  let cookie = match muse_native::cookie_header_for(&app, &uuid).await {
    Ok(c) => zeroize::Zeroizing::new(c),
    Err(e) => {
      mgr.record_native_quota(&app, &uuid, None).await?;
      return Ok(QuotaProbe {
        ok: false,
        quota: None,
        diag: vec![],
        error: Some(e),
      })
    }
  };
  let cached = cache.get();
  let (result, mut diag) = tokio::select! {
    biased;
    _ = token.cancelled() => return Err("账号出站配置已变化，已停止额度请求".into()),
    response = quota::fetch_quota_v2(&client, &cookie, cached.as_deref()) => response,
  };
  if egress.is_proxy() {
    diag.insert(0, format!("出站：经代理 {}", egress.display));
  }
  if let Some(c) = cached.as_deref() {
    diag.insert(0, format!("使用缓存 action {}", &c[..8.min(c.len())]));
  }
  match result {
    Ok(q) => {
      cache.set(q.action_id.clone());
      mgr.record_native_quota(&app, &uuid, Some(&q)).await?;
      Ok(QuotaProbe {
        ok: true,
        quota: Some(q),
        diag,
        error: None,
      })
    }
    Err(e) => {
      mgr.record_native_quota(&app, &uuid, None).await?;
      // 会话失效时清缓存，避免继续用过期 id 打上游。
      if matches!(e, quota::QuotaError::SessionExpired) {
        cache.set(None);
      }
      Ok(QuotaProbe {
        ok: false,
        quota: None,
        diag,
        error: Some(e.to_string()),
      })
    }
  }
}

/// #8：VM 租约探针（只读控制面）。
#[tauri::command]
async fn muse_native_lease_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  task_id: String,
  twice: Option<bool>,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<muse_native::LeaseProbe, String> {
  if provider_id != "muse" {
    return Err("租约探针只支持 Muse 账号".to_string());
  }
  let task = uuid::Uuid::parse_str(&task_id).map_err(|_| "租约任务标识无效".to_string())?;
  let (mut token, _guard) = cancel.register_task(&uuid, "lease", task);
  tokio::select! {
    biased;
    _ = token.cancelled() => Err("租约探针已停止".into()),
    result = tokio::time::timeout(std::time::Duration::from_secs(60), async {
      let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
      muse_native::lease_probe(&app, &uuid, twice.unwrap_or(true), setting.as_ref()).await
    }) => result.map_err(|_| "租约探针达到 60 秒总时限".to_string())?,
  }
}

/// B4：发送一条消息到隔离测试会话（单发锁 + 台账在 muse_native::send_probe）。
#[tauri::command]
async fn muse_native_send_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  text: String,
  node_id: String,
  task_id: String,
  session_id: Option<String>,
  ledger: tauri::State<'_, std::sync::Arc<send_ledger::SendLedger>>,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<muse_native::SendOutcome, String> {
  if provider_id != "muse" {
    return Err("发送探针只支持 Muse 账号".to_string());
  }
  let task = uuid::Uuid::parse_str(&task_id).map_err(|_| "发送任务标识无效".to_string())?;
  let (token, _guard) = cancel.register_task(&uuid, "message", task);
  let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
  let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
  let secrets = secrets::Secrets::new(&dir);
  Ok(muse_native::send_probe(
    &app,
    &ledger,
    &uuid,
    &text,
    &node_id,
    session_id.as_deref(),
    setting.as_ref(),
    &secrets,
    token,
  )
  .await)
}

/// #18：模型指纹探针（隔离会话发 3 条计数探针 + lmfpd 本地评分）。
/// verdict 只有 scored 才是有效识别；partial 只是候选排名；indeterminate 不可判定。
#[derive(Default)]
struct FingerprintGate(tokio::sync::Mutex<()>);

#[tauri::command]
async fn set_muse_fingerprint_settings(
  app: AppHandle, uuid: String, manual_model: Option<String>, interval_days: Option<u8>,
  mgr: State<'_, Arc<SessionManager>>, cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<ManagerSnapshot, String> {
  let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
  let manual_model = manual_model.map(|name| name.trim().to_owned()).filter(|name| !name.is_empty());
  if manual_model.as_ref().is_some_and(|name| name.chars().count() > 80 || name.chars().any(char::is_control))
    || interval_days.is_some_and(|days| !(1..=90).contains(&days)) {
    return Err("模型名称最长 80 字，自动重测间隔为 1–90 天".into());
  }
  let snapshot = mgr.save_muse_fingerprint_settings(&app, &uuid, manual_model, interval_days).await?;
  if interval_days.is_none() { cancel.cancel_kind(&uuid, "fingerprint_automatic"); }
  Ok(snapshot)
}

#[tauri::command]
async fn muse_native_fingerprint_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  task_id: String,
  automatic: Option<bool>,
  ledger: tauri::State<'_, std::sync::Arc<send_ledger::SendLedger>>,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
  fingerprints: State<'_, FingerprintGate>,
  rooms_gate: State<'_, rooms::RoomGate>,
) -> Result<muse_native::FingerprintProbe, String> {
  if provider_id != "muse" {
    return Err("指纹探针只支持 Muse 账号".to_string());
  }
  let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
  let task_id = uuid::Uuid::parse_str(&task_id).map_err(|_| "指纹任务标识无效".to_string())?;
  let automatic = automatic.unwrap_or(false);
  let _exclusive = fingerprints.0.try_lock().map_err(|_| "已有房间或指纹任务运行")?;
  let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
  let proxy_guard = mgr.proxy_write_lock.lock().await;
  let snapshot = mgr.get_snapshot().await;
  let matching = snapshot.accounts.iter().find(|account| account.uuid == uuid
    && account.provider_id == providers::ProviderId::Muse && account.auth_state == storage::AuthState::Authenticated)
    .ok_or("请先登录该 Muse 账号")?;
  if snapshot.conflicts.iter().any(|item| item.uuid == uuid)
    || snapshot.pending_cleanup.iter().any(|key| key.uuid == uuid) {
    return Err("请先处理账号归属或清理状态".into());
  }
  let (token, _guard) = cancel.register_task(&uuid, if automatic { "fingerprint_automatic" } else { "fingerprint" }, task_id);
  if automatic {
    #[cfg(not(target_os = "ios"))]
    return Err("自动指纹重测需要 iOS".into());
    #[cfg(target_os = "ios")]
    {
      let _rooms = rooms_gate.0.lock().await;
      if rooms::apply(&dir.join("rooms-v1.json"), rooms::Command::List, &snapshot.accounts, false)?.has_active_room()
        || matching.quota_observation.as_ref().and_then(|quota| quota.remaining(storage::now_ms())).unwrap_or(0) == 0
      { return Err("房间活跃或尚未确认 Muse 剩余额度，自动重测暂缓".into()); }
    }
  }
  #[cfg(not(target_os = "ios"))]
  let _ = rooms_gate;
  let setting = matching.proxy.clone();
  drop(proxy_guard);
  let secrets = secrets::Secrets::new(&dir);
  #[cfg(target_os = "ios")]
  let (foreground, reason) = {
    use std::sync::atomic::{AtomicU8, Ordering};
    let reason = Arc::new(AtomicU8::new(0));
    let event_reason = Arc::clone(&reason);
    let task_account = uuid.clone();
    let cancellations = Arc::clone(cancel.inner());
    let interrupted = tauri::ipc::Channel::new(move |body| {
      if let tauri::ipc::InvokeResponseBody::Json(raw) = body {
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&raw) {
          let cause = match event["reason"].as_str() {
            Some("background") => 1,
            Some("deadline") => 2,
            _ => return Ok(()),
          };
          event_reason.store(cause, Ordering::Release);
          cancellations.cancel_task(&task_account, task_id);
        }
      }
      Ok(())
    });
    let guard = platform::ios::keep_fingerprint_foreground(&app, task_id.to_string(), interrupted)
      .await
      .map_err(|_| "无法保持指纹探针前台运行，请在叠号前台重试".to_string())?;
    (guard, reason)
  };
  #[cfg(target_os = "ios")]
  if automatic { mgr.claim_automatic_fingerprint(&uuid).await?; }

  let result = Box::pin(muse_native::fingerprint_probe(
    &app,
    &ledger,
    &uuid,
    task_id,
    setting.as_ref(),
    &secrets,
    token,
  )).await;
  #[cfg(target_os = "ios")]
  let result = {
    let mut result = result;
    drop(foreground);
    if result.samples_collected < 3 {
      use std::sync::atomic::Ordering;
      let interruption = match reason.load(Ordering::Acquire) {
        1 => Some("叠号离开前台，已停止本次指纹探针"),
        2 => Some("本次指纹探针达到总时限，已停止"),
        _ => None,
      };
      if let Some(message) = interruption {
        result.error = Some(message.to_string());
      }
    }
    result
  };
  let scored = result.result.as_ref();
  let entry = storage::FingerprintRecord {
    task_id, recorded_at_ms: storage::now_ms(), verdict: result.verdict.clone(),
    model: scored.filter(|record| !record.prediction.is_empty()).map(|record| record.prediction.clone()),
    family: scored.filter(|record| !record.family_prediction.is_empty()).map(|record| record.family_prediction.clone()),
    probability_ppm: scored.filter(|record| record.identification && record.calibrated)
      .and_then(|record| record.probability).filter(|p| (0.0..=1.0).contains(p))
      .map(|p| (p * 1_000_000.0).round() as u32),
    valid_samples: scored.map_or(0, |record| record.used_samples),
    reference_built_at: scored.filter(|record| !record.reference_built_at.is_empty()).map(|record| record.reference_built_at.clone()),
    reference_sha256: scored.and_then(|record| record.reference_sha256.clone()),
    method: scored.filter(|record| !record.method.is_empty()).map(|record| record.method.clone()),
  };
  mgr.archive_muse_fingerprint(&app, &uuid, entry).await
    .map_err(|_| "检测已结束，但账号指纹记录未能保存".to_string())?;
  Ok(result)
}

#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn hark_native_watch_probe(
  app: AppHandle,
  uuid: String,
  timezone: String,
  mgr: State<'_, Arc<SessionManager>>,
) -> Result<hark_native::HarkProbe, String> {
  #[cfg(not(target_os = "ios"))]
  {
    Err("Hark 原生诊断需要 iOS 测试包".to_string())
  }
  #[cfg(target_os = "ios")]
  {
    if !muse_native::is_spike_build(app.clone()) {
      return Err("Hark 原生诊断仅在 Spike 包开放".to_string());
    }
    if timezone.is_empty() || timezone.len() > 96
      || !timezone.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_+-".contains(&b))
    {
      return Err("时区格式无效".to_string());
    }
    let account = mgr.get_snapshot().await.accounts.into_iter()
      .find(|account| account.provider_id == providers::ProviderId::Hark && account.uuid == uuid
        && account.auth_state == storage::AuthState::Authenticated)
      .ok_or_else(|| "没有匹配的 Hark 测试账号".to_string())?;
    Ok(hark_native::diagnose(&app, &uuid, account.proxy.as_ref(), &timezone).await)
  }
}

#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn hark_isolated_send_probe(
  app: AppHandle, uuid: String, timezone: String, task_id: uuid::Uuid,
  mgr: State<'_, Arc<SessionManager>>, ledger: State<'_, Arc<send_ledger::SendLedger>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<serde_json::Value, String> {
  #[cfg(target_os = "ios")]
  {
    if !muse_native::is_spike_build(app.clone()) { return Err("Hark 隔离验证只在 Spike 开放".into()); }
    let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
    let proxy_guard = mgr.proxy_write_lock.lock().await;
    let snapshot = mgr.get_snapshot().await;
    let account = snapshot.accounts.iter().find(|account| account.uuid == uuid && account.provider_id == providers::ProviderId::Hark
      && account.auth_state == storage::AuthState::Authenticated).ok_or("请先登录该 Hark 测试账号")?;
    if snapshot.conflicts.iter().any(|item| item.uuid == uuid) || snapshot.pending_cleanup.iter().any(|key| key.uuid == uuid) {
      return Err("请先处理账号归属或清理状态".into());
    }
    let timezone = storage::normalize_account_timezone(account.timezone.as_deref().unwrap_or(&timezone))?
      .unwrap_or_else(|| "UTC".into());
    let (token, _guard) = cancel.register_task(&uuid, "hark_send", task_id);
    drop(proxy_guard);
    let (client, cookie) = hark_native::account_http(&app, &uuid, account.proxy.as_ref()).await?;
    let transport = hark_send::Transport { client, cookie, timezone:&timezone, origin:"https://app.hark.com" };
    if token.is_cancelled() { return Err("已取消，未发送".into()); }
    let report = Box::pin(hark_send::send_isolated(transport, &ledger, &uuid, "这是叠号的隔离测试。请只回复 OK。", token)).await?;
    serde_json::to_value(report).map_err(|_| "无法呈现诊断结果".into())
  }
  #[cfg(not(target_os = "ios"))]
  Err("Hark 隔离验证需要 iOS".into())
}

#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn hark_recover_replies(
  app: AppHandle, uuid: String, timezone: String, mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<hark_native::HarkRecovery, String> {
  #[cfg(target_os = "ios")]
  {
    let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
    let proxy_lock = mgr.proxy_write_lock.lock().await;
    let (mut token, _guard) = cancel.register(&uuid, "hark_recovery");
    let snapshot = mgr.get_snapshot().await;
    drop(proxy_lock);
    let account = snapshot.accounts.iter().find(|account| account.provider_id == providers::ProviderId::Hark
      && account.uuid == uuid && account.auth_state == storage::AuthState::Authenticated
    ).ok_or("请先登录该 Hark 账号")?;
    if snapshot.conflicts.iter().any(|conflict| conflict.uuid == uuid)
      || snapshot.pending_cleanup.iter().any(|key| key.uuid == uuid) {
      return Err("请先处理账号归属或清理状态".into());
    }
    let timezone = account.timezone.as_deref().unwrap_or(&timezone);
    if timezone.is_empty() || timezone.len() > 96
      || !timezone.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_+-".contains(&b)) {
      return Err("时区格式无效".into());
    }
    tokio::select! {
      biased;
      _ = token.cancelled() => Err("账号出站配置已变化，已停止读取".into()),
      result = tokio::time::timeout(std::time::Duration::from_secs(44),
        Box::pin(hark_native::recover(&app, &uuid, account.proxy.as_ref(), timezone))
      ) => result.map_err(|_| "Hark 回复补同步达到总时限".into()),
    }
  }
  #[cfg(not(target_os = "ios"))]
  { Err("Hark 回复补同步需要 iOS".into()) }
}

/// #11/#16：用**该账号已保存的代理**验证控制面 + WSS 隧道 + Noise + ping（fail-closed）。
///
/// 不再接受前端传入的 proxy URI：诊断必须与真实发送同源（ProxySetting +
/// Keychain），否则会出现「诊断 OK、发送走另一个出口」的误判，也会让代理口令
/// 经 IPC 进入前端。支持后端取消（取消后立即停止后续阶段）。
#[tauri::command]
async fn muse_native_proxy_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  connectivity_only: Option<bool>,
  task_id: Option<String>,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<muse_native::ProxyProbe, String> {
  if provider_id != "muse" {
    return Err("代理探针只支持 Muse 账号".to_string());
  }
  let (token, _guard) = match task_id {
    Some(task_id) => {
      let id = uuid::Uuid::parse_str(&task_id).map_err(|_| "代理任务标识无效".to_string())?;
      cancel.register_task(&uuid, "proxy", id)
    }
    None => cancel.register(&uuid, "proxy"),
  };
  let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
  let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
  let secrets = secrets::Secrets::new(&dir);
  Ok(muse_native::proxy_probe(
    &app,
    &uuid,
    setting.as_ref(),
    &secrets,
    token,
    connectivity_only.unwrap_or(false),
  ).await)
}

/// 平台原生 WebView 侧的代理应用结果。
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WebviewProxyView {
  pub applied: bool,
  pub session_open: bool,
  /// 下发失败时 WebView 出口停在隔离态（不可达代理 + 禁止 failover）。
  pub quarantined: bool,
  pub reason: Option<String>,
}

/// 账号代理配置的脱敏视图（不含口令）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySettingView {
  pub enabled: bool,
  pub display: String,
  pub has_credentials: bool,
  pub credential_available: Option<bool>,
  /// iOS：账号 WebView（WKWebView）侧的代理应用结果；其它平台为 `None`（不适用，
  /// 桌面 WebView 走系统代理）。`None` 不代表“已生效”，只代表“无此层”。
  pub webview: Option<WebviewProxyView>,
}

fn proxy_credential_available(setting: &storage::ProxySetting, secrets: &secrets::Secrets) -> Option<bool> {
  setting.credential_ref.as_deref().map(|reference| {
    matches!(secrets.get(reference), Ok(Some(secret)) if !secret.is_empty())
  })
}

/// #11/#16（补 W）：把账号代理下发给 iOS 侧的 WKWebView。
/// 平台没有这一层时返回 `None`（桌面 / Android），有但未生效时返回
/// `applied = false` + 原因——前端据此提示，而**阻断由 Swift 侧真正执行**。
async fn apply_webview_proxy(
  app: &AppHandle,
  uuid: &str,
  setting: Option<&storage::ProxySetting>,
) -> Option<WebviewProxyView> {
  let outcome = webview_proxy::push(app, uuid, setting).await;
  if !outcome.applicable {
    return None;
  }
  Some(WebviewProxyView {
    applied: outcome.applied,
    session_open: outcome.session_open,
    quarantined: outcome.quarantined,
    reason: outcome.reason,
  })
}

fn uri_userinfo(uri: &str) -> Option<&str> {
  let authority = uri.split_once("://")?.1.split('/').next()?;
  authority.rsplit_once('@').map(|(ui, _)| ui)
}

#[tauri::command]
async fn manage_proxy_pool(
  app: AppHandle,
  gate: State<'_, proxy_pool::ProxyPoolGate>,
  mgr: State<'_, Arc<SessionManager>>,
  action: String,
  input: Option<String>,
  entry_id: Option<uuid::Uuid>,
) -> Result<proxy_pool::PoolView, String> {
  let _guard = gate.0.lock().await;
  let dir = app.path().app_data_dir().map_err(|_| "无法读取应用目录")?;
  let secrets = secrets::Secrets::new(&dir);
  let mut pool = proxy_pool::ProxyPool::read(&secrets)?;
  let input = input.map(zeroize::Zeroizing::new);
  match action.as_str() {
    "list" => {}
    "import" => {
      pool.import(input.as_deref().ok_or("请输入代理")?)?;
      pool.save(&secrets)?;
    }
    "subscribe" => {
      pool.subscribe(input.as_deref().ok_or("请输入订阅地址")?.to_string()).await?;
      pool.save(&secrets)?;
    }
    "refresh" => {
      if pool.refresh(entry_id).await? { pool.save(&secrets)?; }
    }
    "remove" => {
      let id = entry_id.ok_or("缺少代理标识")?;
      let restored = mgr.restored_proxy_metadata().await;
      if restored.iter().any(|entry| proxy_pool::restored_entry_id(entry) == id) {
        mgr.remove_restored_proxy_metadata(id).await?;
      } else {
        pool.remove(id);
        pool.save(&secrets)?;
      }
    }
    _ => return Err("未知代理池操作".into()),
  }
  let mut view = pool.view()?;
  proxy_pool::ProxyPool::push_restored(&mut view, &mgr.restored_proxy_metadata().await);
  Ok(view)
}

#[tauri::command]
async fn apply_proxy_pool_entry(
  app: AppHandle,
  gate: State<'_, proxy_pool::ProxyPoolGate>,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
  provider_id: String,
  uuid: String,
  entry_id: uuid::Uuid,
) -> Result<ProxySettingView, String> {
  let entry = {
    let _guard = gate.0.lock().await;
    let dir = app.path().app_data_dir().map_err(|_| "无法读取应用目录")?;
    let secrets = secrets::Secrets::new(&dir);
    let pool = proxy_pool::ProxyPool::read(&secrets)?;
    match pool.selected(entry_id) {
      Ok(entry) => entry,
      Err(original) => {
        let restored = mgr.restored_proxy_metadata().await.into_iter()
          .find(|entry| proxy_pool::restored_entry_id(entry) == entry_id).ok_or(original)?;
        if restored.requires_auth {
          return Err("导入完整的代理认证信息后才能使用".into());
        }
        proxy_pool::ApplyEntry { endpoint: restored.endpoint, username: None, password: None }
      }
    }
  };
  muse_native_set_account_proxy(app, provider_id, uuid, entry.endpoint, entry.username, entry.password, true, mgr, cancel).await
}

#[tauri::command]
#[cfg_attr(target_os = "android", allow(unused_variables))]
async fn measure_account_proxy(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
  provider_id: String,
  uuid: String,
) -> Result<serde_json::Value, String> {
  #[cfg(not(target_os = "android"))]
  {
    let provider = providers::ProviderId::parse(&provider_id).ok_or("服务标识无效")?;
    let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
    let (mut token, _guard) = cancel.register(&uuid, "proxy_measurement");
    let setting = mgr.proxy_setting(provider, &uuid).await;
    let Some(setting) = setting.filter(|setting| setting.enabled) else {
      return Err("该账号尚未启用代理".into());
    };
    let client = egress::resolve_for_app(&app, Some(&setting))?
      .http_client(std::time::Duration::from_secs(15), false)
      .map_err(|_| "无法初始化代理测速")?;
    let started = std::time::Instant::now();
    tokio::select! {
      biased;
      _ = token.cancelled() => Err("代理配置已更改，已停止测量".into()),
      response = client.head(providers::get_provider(provider).info.entry_url).send() => {
        let response = response.map_err(|error| egress::safe_http_error("代理 HTTPS", &error))?;
        Ok(serde_json::json!({ "milliseconds": started.elapsed().as_millis(), "status": response.status().as_u16() }))
      }
    }
  }
  #[cfg(target_os = "android")]
  {
    Err("此操作在 Android 上不可用".into())
  }
}

/// #11/#16：保存账号代理。口令只进 Keychain，`accounts.json` 仅存引用。
#[tauri::command]
async fn muse_native_set_account_proxy(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  uri: String,
  username: Option<String>,
  password: Option<String>,
  enabled: bool,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<ProxySettingView, String> {
  let pid = providers::ProviderId::parse(&provider_id)
    .ok_or_else(|| format!("未知 provider: {provider_id}"))?;
  let _write = mgr.proxy_write_lock.lock().await;
  let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
  let snapshot = mgr.get_snapshot().await;
  if snapshot.conflicts.iter().any(|entry| entry.uuid == uuid)
    || snapshot.pending_cleanup.iter().any(|key| key.uuid == uuid)
    || !snapshot.accounts.iter().any(|account| account.provider_id == pid && account.uuid == uuid)
  {
    return Err("该账号不可用，未修改代理".to_string());
  }
  let uri = uri.trim().to_string();
  if let Some(ui) = uri_userinfo(&uri) {
    if ui.contains(':') {
      return Err("代理 URI 不得包含明文密码；请用独立字段提交并由 Keychain 保存".to_string());
    }
  }
  muse_native_client::proxy::ProxyConfig::validate_shape(&uri).map_err(|e| e.to_string())?;

  let username = username
    .map(|u| u.trim().to_string())
    .filter(|u| !u.is_empty());
  let has_user = username.is_some() || uri_userinfo(&uri).map(|u| !u.is_empty()).unwrap_or(false);
  let password = password.filter(|p| !p.is_empty()).map(zeroize::Zeroizing::new);

  let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
  let secrets = secrets::Secrets::new(&dir);
  let reference = format!("proxy-{uuid}");

  // 先构造并校验设置，再落凭据，避免把半成品写进 Keychain。
  let setting = storage::ProxySetting {
    uri: uri.clone(),
    username,
    credential_ref: if has_user { Some(reference.clone()) } else { None },
    enabled,
  };
  setting.validate()?;
  if has_user && password.is_none() {
    return Err("启用了代理认证，但没有提供代理密码".to_string());
  }

  // **先隔离，再保存**：已打开的 WebView 不能等到保存完才停旧出口。
  // 顺序是「隔离旧出口 → 落配置 → 下发新配置」：
  // - 隔离失败（设备无法在网络层阻断）→ 拒绝保存，否则会在窗口期继续走旧出口；
  // - 保存失败 → 保持隔离（fail-closed），下次打开仍按旧配置重建；
  // - 下发失败 → 保持隔离，不让 WebView 用系统网络直连。
  let quarantined = webview_proxy::quarantine(
    &app,
    &uuid,
    Some("代理配置正在变更，当前会话已停止出站（请重新打开该账号）".to_string()),
  )
  .await;
  if let Some(reason) = quarantined.failed() {
    return Err(format!("无法隔离当前 WebView 出站，已拒绝保存代理：{reason}"));
  }

  cancel.cancel_account(&uuid);
  let previous_password = if has_user {
    let existing = secrets.get(&reference)?;
    secrets.set(&reference, password.as_deref().map(String::as_str).unwrap_or(""))?;
    existing
  } else {
    None
  };
  if let Err(error) = mgr.set_account_proxy(pid, &uuid, Some(setting.clone())).await {
    if has_user {
      let restore = match previous_password {
        Some(secret) => secrets.set(&reference, &secret),
        None => secrets.delete(&reference),
      };
      if restore.is_err() {
        return Err("代理配置写入失败，旧凭据无法恢复，账号出口保持隔离".to_string());
      }
    }
    return Err(error);
  }

  // 再下发新配置：成功即替换隔离；enabled=false 时 helper 会清除 WebView 配置。
  let webview = apply_webview_proxy(&app, &uuid, Some(&setting)).await;
  if !has_user {
    secrets.delete(&reference)?;
  }

  Ok(ProxySettingView {
    enabled: setting.enabled,
    display: setting.display_safe(),
    has_credentials: setting.credential_ref.is_some(),
    credential_available: proxy_credential_available(&setting, &secrets),
    webview,
  })
}

/// #11/#16：读取账号代理的脱敏视图。
///
/// 读取配置和凭据可用性。WebView 下发发生在保存、打开会话与后台同步前；
/// 读面板不会销毁正在使用的 WebView。
#[tauri::command]
async fn muse_native_get_account_proxy(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  mgr: State<'_, Arc<SessionManager>>,
) -> Result<Option<ProxySettingView>, String> {
  let pid = providers::ProviderId::parse(&provider_id)
    .ok_or_else(|| format!("未知 provider: {provider_id}"))?;
  let setting = mgr.proxy_setting(pid, &uuid).await;
  let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
  let secrets = secrets::Secrets::new(&dir);
  Ok(setting.map(|s| ProxySettingView {
    enabled: s.enabled,
    display: s.display_safe(),
    has_credentials: s.credential_ref.is_some(),
    credential_available: proxy_credential_available(&s, &secrets),
    webview: None,
  }))
}

/// #11/#16：清除账号代理（连同 Keychain 中的口令）。
#[tauri::command]
async fn muse_native_clear_account_proxy(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<Option<WebviewProxyView>, String> {
  let pid = providers::ProviderId::parse(&provider_id)
    .ok_or_else(|| format!("未知 provider: {provider_id}"))?;
  let _write = mgr.proxy_write_lock.lock().await;
  let uuid = storage::normalize_uuid(&uuid).ok_or("账号标识无效")?;
  let snapshot = mgr.get_snapshot().await;
  if snapshot.conflicts.iter().any(|entry| entry.uuid == uuid)
    || snapshot.pending_cleanup.iter().any(|key| key.uuid == uuid)
    || !snapshot.accounts.iter().any(|account| account.provider_id == pid && account.uuid == uuid)
  {
    return Err("该账号不可用，未修改代理".to_string());
  }
  let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
  let secrets = secrets::Secrets::new(&dir);
  let reference = mgr.proxy_setting(pid, &uuid).await.and_then(|setting| setting.credential_ref);
  // 同样先隔离：清凭据到下发之间，已打开的 WebView 不能还按旧代理跑。
  let quarantined = webview_proxy::quarantine(
    &app,
    &uuid,
    Some("代理已清除，当前会话已停止出站（请重新打开该账号）".to_string()),
  )
  .await;
  if let Some(reason) = quarantined.failed() {
    return Err(format!("无法隔离当前 WebView 出站，已拒绝清除代理：{reason}"));
  }
  cancel.cancel_account(&uuid);
  mgr.set_account_proxy(pid, &uuid, None).await?;
  let webview = apply_webview_proxy(&app, &uuid, None).await;
  let default_reference = format!("proxy-{uuid}");
  secrets.delete(&default_reference)?;
  if let Some(reference) = reference.filter(|reference| reference != &default_reference) {
    secrets.delete(&reference)?;
  }
  Ok(webview)
}

/// #11/#16：请求取消该账号正在运行的长任务（指纹/发送/代理探针等）。
/// 只停止发起新请求；已发出的发送按 UNKNOWN 记账，不重发。
/// 返回命中的任务数（同账号可能有多个任务在跑，例如指纹 + 代理探针）。
#[tauri::command]
fn muse_native_cancel_probe(
  provider_id: String,
  uuid: String,
  task_id: Option<String>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<usize, String> {
  if provider_id != "muse" {
    return Err("取消探针只支持 Muse 账号".to_string());
  }
  Ok(match task_id {
    Some(task) => cancel.cancel_task(
      &uuid,
      uuid::Uuid::parse_str(&task).map_err(|_| "任务标识无效".to_string())?,
    ),
    None => cancel.cancel_account(&uuid),
  })
}

/// #10：开始一次存活探针（长连接 + 订阅，记录到断开为止）。
///
/// 返回 `Result`：async 命令若返回非 `Result`，Tauri 会把引用参数当成不可满足的
/// 约束（`AsyncCommandMustReturnResult`）；这里本就要读账号代理设置（需要 await），
/// 所以统一返回 `Result`，错误直接抛给前端。
#[tauri::command]
async fn muse_native_liveness_start(
  app: AppHandle,
  uuid: String,
  task_id: String,
  seconds: u64,
  state: State<'_, Arc<muse_native::LivenessState>>,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<muse_native::Liveness, String> {
  let task = uuid::Uuid::parse_str(&task_id).map_err(|_| "存活任务标识无效".to_string())?;
  muse_native::liveness_start(&app, &state, &uuid, seconds, mgr.inner().clone(), cancel.inner().clone(), task)?;
  Ok(state.snapshot())
}

/// #10：读取存活探针当前状态/结果。
#[tauri::command]
fn muse_native_liveness_status(
  state: tauri::State<'_, std::sync::Arc<muse_native::LivenessState>>,
) -> muse_native::Liveness {
  state.snapshot()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  let builder = tauri::Builder::default();
  let builder = builder.manage(proxy_pool::ProxyPoolGate::default());
  let builder = builder.manage(rooms::RoomGate::default());

  #[cfg(target_os = "android")]
  let builder = builder.plugin(platform::android::init());

  #[cfg(target_os = "ios")]
  let builder = builder.plugin(platform::ios::init());

  builder
    .setup(|app| {
      let app_data_dir = app.path().app_data_dir()?;
      let session_mgr = Arc::new(SessionManager::new(app_data_dir.clone()));
      app.manage(Arc::clone(&session_mgr));

      // 发送台账的状态日志：进程异常退出后，未定论的发送在重启时改判 UNKNOWN，
      // 不会被当成「没发过」而重发。日志读不了/写不了都拒绝新发送（fail-closed）。
      let (ledger, recovery) = send_ledger::SendLedger::open(app_data_dir.join("send-ledger.json"));
      if recovery.corrupt {
        eprintln!(
          "[send-ledger] 台账不可用（{}）：本次运行拒绝一切新发送（fail-closed）",
          recovery
            .load_error
            .as_deref()
            .unwrap_or("状态日志损坏或无法持久化")
        );
      } else if recovery.restored > 0 {
        eprintln!(
          "[send-ledger] 恢复 {} 条记录，其中 {} 条未定论改判 UNKNOWN",
          recovery.restored, recovery.marked_unknown
        );
      }
      app.manage(Arc::new(ledger));

      #[cfg(desktop)]
      {
        let main_window = tauri::window::WindowBuilder::new(app, "main")
          .title(APP_TITLE)
          .inner_size(1240.0, 800.0)
          .min_inner_size(760.0, 520.0)
          .build()?;

        let manager_builder =
          tauri::webview::WebviewBuilder::new("manager", tauri::WebviewUrl::App("index.html".into()))
            .on_navigation(|url| {
              url.scheme() == "tauri"
                || url.domain() == Some("tauri.localhost")
                || (cfg!(dev) && url.host_str() == Some("localhost"))
            });

        main_window.add_child(
          manager_builder,
          tauri::LogicalPosition::new(0.0, 0.0),
          tauri::LogicalSize::new(1240.0, 800.0),
        )?;

        let app_for_resize = app.handle().clone();
        let mgr_for_resize = Arc::clone(&session_mgr);
        main_window.on_window_event(move |event| match event {
          tauri::WindowEvent::Resized(_) | tauri::WindowEvent::ScaleFactorChanged { .. } => {
            let app_inner = app_for_resize.clone();
            let mgr_inner = Arc::clone(&mgr_for_resize);
            tauri::async_runtime::spawn(async move {
              mgr_inner.recompute_desktop_bounds(&app_inner).await;
            });
          }
          tauri::WindowEvent::Focused(focused) if *focused => {
            let app_inner = app_for_resize.clone();
            let mgr_inner = Arc::clone(&mgr_for_resize);
            tauri::async_runtime::spawn(async move {
              mgr_inner.trigger_silent_sync(&app_inner, false).await;
            });
          }
          _ => {}
        });
      }

      #[cfg(mobile)]
      {
        let _ = APP_TITLE;
        tauri::WebviewWindowBuilder::new(
          app,
          "manager",
          tauri::WebviewUrl::App("index.html".into()),
        )
        .build()?;
      }

      let app_for_boot = app.handle().clone();
      let mgr_for_boot = Arc::clone(&session_mgr);
      tauri::async_runtime::spawn(async move {
        mgr_for_boot.run_startup_tasks(&app_for_boot).await;
      });

      // Spike 0.1.22：启动自检已暂停（2026-11-23 诊断挂起排查）。
      // 原先 Spike 版启动后对首个已登录 Muse 账号跑一次握手诊断
      // （日常版不跑；见 muse_native::maybe_auto_diagnose）。
      // 用户实测手动诊断一直卡在「诊断中」，即使 Rust 侧已有 60 秒超时
      // 也没返回——先停掉启动自检，排除它与手动诊断互相干扰的可能，
      // 等手动诊断跑通后再恢复。
      // let app_for_diag = app.handle().clone();
      // let mgr_for_diag = Arc::clone(&session_mgr);
      // tauri::async_runtime::spawn(async move {
      //   muse_native::maybe_auto_diagnose(&app_for_diag, &mgr_for_diag).await;
      // });

      Ok(())
    })
    .manage(std::sync::Arc::new(muse_native::LivenessState::default()))
    .manage(std::sync::Arc::new(probe_cancel::ProbeCancel::new()))
    .manage(FingerprintGate::default())
    .manage(std::sync::Arc::new(quota::ActionCache::default()))
    .invoke_handler(tauri::generate_handler![
      get_manager_state,
      select_provider,
      add_account,
      manage_rooms,
      share_room_artifact,
      run_room,
      rename_account,
      remove_account,
      switch_account,
      set_modal_active,
      open_official_site,
      resolve_uuid_conflict,
      dismiss_notice,
      trigger_quota_sync,
      preview_account_cookies,
      export_account_backup,
      preview_account_backup,
      restore_account_backup,
      restore_room_backup,
      transfer_account_cookies,
      inspect_account_cookies,
      list_muse_sessions,
      open_muse_thread,
      spike_ios_quota_sync_probe,
      reorder_accounts,
      check_for_updates,
      download_and_install_update,
      open_external_url,
      muse_native_diagnose,
      muse_native_watch_probe,
      observe_account_tasks,
      set_account_task_notifications,
      hark_native_watch_probe,
      hark_isolated_send_probe,
      hark_recover_replies,
      muse_native_approvals_probe,
      decide_muse_approval,
      process_approval_notification_actions,
      muse_native_quota_probe,
      muse_native_send_probe,
      muse_native_fingerprint_probe,
      set_muse_fingerprint_settings,
      muse_native_proxy_probe,
      muse_native_set_account_proxy,
      muse_native_get_account_proxy,
      manage_proxy_pool,
      measure_account_proxy,
      apply_proxy_pool_entry,
      muse_native_clear_account_proxy,
      muse_native_cancel_probe,
      muse_native_liveness_start,
      muse_native_liveness_status,
      muse_native_lease_probe,
      muse_native::is_spike_build,
    ])
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
