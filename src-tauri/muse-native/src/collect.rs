//! Collect a correlated assistant turn on the same Noise connection as the send.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use crate::chat_send::{Ack, LineSource};
use crate::gateway::{ConnectedGateway, GatewayError, Subscription};
use crate::transport::{HttpRequest, Service};
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};

/// 网页端广告的能力集合（含 `delta_stream`，否则服务端不推正文增量）。
pub const SUBSCRIBE_CAPABILITIES: &[&str] = &[
  "hotel_results",
  "chat_cancel",
  "delta_stream",
  "custom_reactions",
  "custom_reactions_facebook_thumbs_up_v1",
];

/// 一次采集的结果。`text` 只包含我们真正收到并累积的增量。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Collected {
  pub send_events: StreamEventCounts,
  pub subscription_events: StreamEventCounts,
  pub first_matched_delta_ms: Option<u64>,
  pub first_delta_to_message_done_ms: Option<u64>,
  /// 累积到的助手正文（按到达顺序拼接）。
  pub text: String,
  /// 收到的文本增量事件数。
  pub text_deltas: u64,
  /// 收到的全部事件行数。
  pub lines: u64,
  /// 解析失败的行数。
  pub parse_errors: u64,
  /// `delta.message_done` 是否到达（false 表示超时或断流）。
  pub done: bool,
  /// 结束原因：`done` / `timeout` / `closed`。
  pub ended_by: String,
  /// 是否出现过智能体忙态（用于判断这轮是否真的开始了）。
  pub read_timeouts: u64,
  pub unmatched_events: u64,
  pub outside_turn_events: u64,
  pub conflicting_events: u64,
  pub missing_message_id_events: u64,
  pub unlinked_events: u64,
  pub saw_user: bool,
  pub saw_idle: bool,
  pub assistant_messages: usize,
}

/// Counts use a fixed event allowlist and include events rejected by correlation.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEventCounts {
  pub user: u64,
  pub start: u64,
  pub delta: u64,
  pub done: u64,
  pub activity: u64,
  pub other: u64,
}

impl StreamEventCounts {
  fn observe(&mut self, name: &str) {
    *match name {
      "message.user" => &mut self.user,
      "delta.message_start" => &mut self.start,
      "delta.text_append" => &mut self.delta,
      "delta.message_done" | "message.assistant" => &mut self.done,
      "agent.status" => &mut self.activity,
      _ => &mut self.other,
    } += 1;
  }
}

/// 构造 `chat.subscribe` 请求（供直连与代理两条路径复用）。
pub fn subscription_request() -> HttpRequest {
  let body = serde_json::json!({ "capabilities": SUBSCRIBE_CAPABILITIES });
  subscription_with_body(body)
}

pub(crate) fn isolated_subscription_request(session_id: &str) -> HttpRequest {
  subscription_with_body(serde_json::json!({
    "session_id": session_id,
    "after_stream_seq": 0,
    "after_chat_event_seq": 0,
    "capabilities": SUBSCRIBE_CAPABILITIES,
  }))
}

fn subscription_with_body(body: Value) -> HttpRequest {
  HttpRequest {
    service: Service::Daemon,
    verb: "POST".to_string(),
    path: "/chat/subscribe".to_string(),
    headers: vec![
      ("Content-Type".to_string(), "application/json".to_string()),
      ("x-request-id".to_string(), uuid::Uuid::new_v4().to_string()),
      ("x-app-id".to_string(), "hatch-web".to_string()),
      ("Accept-Language".to_string(), "en-US".to_string()),
    ],
    body: body.to_string().into_bytes(),
  }
}

/// 建立 `chat.subscribe` 长连接。返回的 `Subscription` 必须在本轮发送**之前**创建。
pub async fn open_subscription<S>(gw: ConnectedGateway<S>) -> Result<Subscription<S>, GatewayError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  gw.subscribe(subscription_request()).await
}

#[derive(Default)]
struct AssistantMessage {
  text: String,
  done: bool,
}

fn text_field<'a>(objects: &[&'a Value], keys: &[&str]) -> Option<&'a str> {
  objects.iter().find_map(|value| {
    keys
      .iter()
      .find_map(|key| value.get(key)?.as_str().filter(|text| !text.is_empty()))
  })
}

fn event_time_ms(root: &Value, payload: &Value) -> Option<u64> {
  [root, payload].into_iter().find_map(|record| {
    ["occurred_at_ms", "ts_ms"].into_iter().find_map(|key| record[key].as_u64().filter(|ms| *ms > 0))
  })
}

fn final_text(payload: &Value, root: &Value, assistant_id: &str) -> Option<String> {
  if let Some(messages) = payload["transcript"]["messages"].as_array() {
    let text: Vec<&str> = messages
      .iter()
      .filter(|message| message["role"] == "assistant")
      .filter(|message| {
        text_field(&[message], &["message_id", "id"]).is_none_or(|id| id == assistant_id)
      })
      .flat_map(|message| {
        let content = &message["content"];
        content.as_str().into_iter().chain(
          content
            .as_array()
            .into_iter()
            .flatten()
            .filter(|part| {
              part["type"]
                .as_str()
                .is_none_or(|kind| matches!(kind, "text" | "markdown"))
            })
            .filter_map(|part| text_field(&[part], &["text", "markdown"])),
        )
      })
      .collect();
    if !text.is_empty() {
      return Some(text.join("\n\n"));
    }
  }
  if let Some(parts) = payload["content"]
    .as_array()
    .or_else(|| root["content"].as_array())
  {
    let text: Vec<_> = parts
      .iter()
      .filter(|part| {
        part["type"]
          .as_str()
          .is_none_or(|kind| matches!(kind, "text" | "markdown"))
      })
      .filter_map(|part| text_field(&[part], &["text", "markdown"]))
      .collect();
    if !text.is_empty() {
      return Some(text.join("\n\n"));
    }
  }
  text_field(
    &[payload, &payload["message"], root],
    &["display_text", "text", "content"],
  )
  .map(str::to_string)
}

/// Recover a persisted, completed reply from a session-scoped history response.
/// Only the latest user turn is eligible, and its ID must match the send ACK.
pub fn completed_history_reply(body: &[u8], session_id: &str, user_id: &str) -> Option<String> {
  let raw: Value = serde_json::from_slice(body).ok()?;
  if raw["ok"] == false || !raw["error"].is_null() || raw["has_more"] == true {
    return None;
  }
  let result = raw.get("result").unwrap_or(&raw);
  if result["has_more"] == true
    || result["session_id"]
      .as_str()
      .is_some_and(|id| id != session_id)
  {
    return None;
  }
  let rows = ["events", "chat_events", "messages"]
    .iter()
    .find_map(|key| result[*key].as_array())
    .or_else(|| result.as_array())?;
  let mut ordered: Vec<_> = rows.iter().collect();
  let sequenced = ordered.iter().all(|row| row["seq"].is_u64());
  if sequenced {
    ordered.sort_by_key(|row| row["seq"].as_u64());
  }
  let mut current = false;
  let mut foreign_user = false;
  let mut unlinked = false;
  let mut pending = HashSet::new();
  let mut completed = HashMap::new();
  for row in ordered {
    let payload = &row["payload"];
    let fields = [payload, row, &payload["chat_context"]];
    if text_field(&fields, &["session_id", "thread_id"]).is_some_and(|id| id != session_id) {
      return None;
    }
    let kind = text_field(&[row], &["event_name", "event", "type"]);
    let role = text_field(&fields, &["role"]);
    let id = text_field(&fields, &["message_id", "id"]);
    if role == Some("user") || kind == Some("message.user") {
      if !current || id != Some(user_id) {
        pending.clear();
        completed.clear();
        unlinked = false;
      }
      current = id == Some(user_id);
      foreign_user |= !current;
      continue;
    }
    if !current
      || !(role == Some("assistant")
        || matches!(
          kind,
          Some(
            "message.assistant"
              | "delta.message_done"
              | "delta.message_start"
              | "delta.text_append"
          )
        ))
    {
      continue;
    }
    let parent = text_field(
      &fields,
      &[
        "user_message_id",
        "parent_message_id",
        "reply_to_message_id",
      ],
    );
    if parent.is_some_and(|id| id != user_id) {
      return None;
    }
    unlinked |= parent.is_none();
    let id = id.filter(|id| *id != user_id)?;
    let terminal = matches!(
      text_field(&fields, &["status"]),
      Some("complete" | "completed" | "done" | "finished")
    );
    if terminal {
      let text = final_text(payload, row, id)
        .or_else(|| final_text(row, row, id))
        .filter(|text| !text.trim().is_empty())?;
      pending.remove(id);
      completed.insert(id, text);
    } else if !completed.contains_key(id) {
      pending.insert(id);
    }
  }
  if current
    && pending.is_empty()
    && completed.len() == 1
    && (sequenced || !foreign_user || !unlinked)
  {
    completed.into_values().next()
  } else {
    None
  }
}

/// Gathering a bubble and establishing the end of a turn are independent.
/// A matching completion must be followed by an observed idle state. A short
/// settle window allows another bubble to arrive before the turn is released.
pub async fn collect_text<L: LineSource>(
  sub: &mut L,
  ack: &Ack,
  session_id: &str,
  total: Duration,
  per_line: Duration,
  mut on_first_delta: impl FnMut(),
) -> Collected {
  use tokio::time::{timeout_at, Instant};
  let started = Instant::now();
  let wall_started = SystemTime::now();
  let deadline = started + total;
  let mut out = Collected::default();
  let mut messages = HashMap::<String, AssistantMessage>::new();
  let mut order = Vec::new();
  let mut seen = HashSet::new();
  let scoped_subscription = sub.is_scoped_to(session_id);
  let mut scoped_user_seen = false;
  let mut user_time_ms = None;
  let mut earlier_assistants = HashSet::new();
  let mut idle_after_output = false;
  let fields = ack.raw.get("result").unwrap_or(&ack.raw);
  let mut turn = text_field(&[fields], &["turn_id"]).map(str::to_string);
  let mut task = text_field(&[fields], &["task_id"]).map(str::to_string);
  let mut settle_at: Option<Instant> = None;

  out.ended_by = loop {
    let now = Instant::now();
    // A monotonic timer alone may undercount elapsed time when iOS suspends the app.
    if wall_started.elapsed().is_ok_and(|elapsed| elapsed >= total) {
      break "timeout";
    }
    if settle_at.is_some_and(|end| end <= now && end <= deadline) {
      out.done = true;
      break "done";
    }
    if deadline <= now {
      break "timeout";
    }
    let wake = (now + per_line)
      .min(deadline)
      .min(settle_at.unwrap_or(deadline));
    let line = match timeout_at(wake, sub.next_line(wake - now)).await {
      Ok(Ok(Some(line))) => line,
      Ok(Ok(None)) if sub.stream_ended() => break "stream_ended",
      Ok(Ok(None) | Err(GatewayError::Closed(_))) => break "closed",
      Ok(Err(GatewayError::Cancelled)) => break "cancelled",
      Ok(Err(GatewayError::Reset(..))) => break "reset",
      Ok(Err(GatewayError::Ws(_))) => break "websocket_error",
      Ok(Err(GatewayError::Timeout(_))) | Err(_) => {
        if Some(wake) != settle_at && wake < deadline {
          out.read_timeouts += 1;
        }
        continue;
      }
      Ok(Err(_)) => break "protocol_error",
    };
    if wall_started.elapsed().is_ok_and(|elapsed| elapsed >= total) {
      break "timeout";
    }
    out.lines += 1;
    let Ok(root) = serde_json::from_str::<Value>(&line) else {
      out.parse_errors += 1;
      break "protocol_error";
    };
    if root["type"] != "event" {
      continue;
    }
    let Some(event) = text_field(&[&root], &["event", "event_name"]) else {
      out.parse_errors += 1;
      break "protocol_error";
    };
    let from_send = sub.last_line_from_send();
    let counts = if from_send {
      &mut out.send_events
    } else {
      &mut out.subscription_events
    };
    counts.observe(event);
    let payload = &root["payload"];
    let contexts = [
      payload,
      &root,
      &payload["chat_context"],
      &root["chat_context"],
      &payload["message"],
    ];
    let session = text_field(
      &contexts,
      &["session_id", "sessionId", "thread_id", "threadId"],
    );
    let event_turn = text_field(&contexts, &["turn_id"]);
    let event_task = text_field(&contexts, &["task_id"]);
    let parent = text_field(
      &contexts,
      &[
        "user_message_id",
        "reply_to_message_id",
        "parent_message_id",
      ],
    );
    if session.is_some_and(|id| id != session_id) {
      out.unmatched_events += 1;
      out.conflicting_events += 1;
      continue;
    }
    let before_user = event_time_ms(&root, payload)
      .zip(user_time_ms)
      .is_some_and(|(event, user)| event < user);
    if event == "message.user" {
      let msg = text_field(&[payload, &root], &["message_id", "user_message_id"])
        .or_else(|| text_field(&[&payload["message"]], &["message_id", "id"]));
      if msg.or(parent) == Some(ack.message_id.as_str()) {
        if user_time_ms.is_none() {
          user_time_ms = event_time_ms(&root, payload);
        }
        out.saw_user = true;
        scoped_user_seen |= scoped_subscription && !from_send;
        if turn.is_none() {
          turn = event_turn.map(str::to_string);
        }
        if task.is_none() {
          task = event_task.map(str::to_string);
        }
      } else if !before_user && (out.saw_user || (!scoped_subscription && session == Some(session_id))) {
        break "competing_turn";
      }
      continue;
    }
    let scoped_current = scoped_user_seen && !from_send && !before_user;
    if event_turn.zip(turn.as_deref()).is_some_and(|(a, b)| a != b)
      || event_task.zip(task.as_deref()).is_some_and(|(a, b)| a != b)
      || parent.is_some_and(|id| id != ack.message_id)
    {
      out.unmatched_events += 1;
      out.conflicting_events += 1;
      continue;
    }
    if event == "agent.status" {
      match text_field(&[payload, &root], &["activity_code"]) {
        Some("idle" | "online") if !before_user && !messages.is_empty() => {
          if messages.values().all(|m| m.done) {
            out.saw_idle = true;
            settle_at.get_or_insert(Instant::now() + Duration::from_secs(1));
          } else if scoped_current {
            idle_after_output = true;
          }
        }
        Some("idle" | "online") | None => {}
        Some(_) => {
          if !before_user {
            idle_after_output = false;
            settle_at = None;
          }
        }
      }
      continue;
    }
    if !matches!(
      event,
      "delta.message_start" | "delta.text_append" | "delta.message_done" | "message.assistant"
    ) {
      continue;
    }
    let Some(id) = [&root["assistant_message_id"], &payload["assistant_message_id"], &root["message_id"],
      &payload["message_id"], &payload["message"]["message_id"], &payload["message"]["id"]]
      .into_iter().filter_map(Value::as_str).find(|id| !id.is_empty() && *id != ack.message_id) else {
      out.unmatched_events += 1;
      out.missing_message_id_events += 1;
      continue;
    };
    let explicit_link = parent == Some(ack.message_id.as_str())
      || event_turn.zip(turn.as_deref()).is_some_and(|(a, b)| a == b)
      || event_task.zip(task.as_deref()).is_some_and(|(a, b)| a == b);
    // The subscription can still deliver the previous turn while we wait for the current user event.
    if scoped_subscription && !from_send && !scoped_current && !explicit_link {
      if earlier_assistants.len() >= 64 && !earlier_assistants.contains(id) {
        break "response_limit";
      }
      earlier_assistants.insert(id.to_string());
      out.unmatched_events += 1;
      out.outside_turn_events += 1;
      continue;
    }
    let linked = explicit_link
      || (out.saw_user && session == Some(session_id))
      || messages.contains_key(id)
      // Only an assistant start on the isolated subscription can claim a new sparse event chain.
      || (scoped_current && event == "delta.message_start");
    if before_user || earlier_assistants.contains(id) {
      out.unmatched_events += 1;
      out.outside_turn_events += 1;
      continue;
    }
    if !linked {
      out.unmatched_events += 1;
      out.unlinked_events += 1;
      continue;
    }
    if let Some(seq) = root["seq"].as_u64().filter(|seq| *seq > 0) {
      if !seen.insert((event.to_string(), id.to_string(), seq)) {
        continue;
      }
    }
    if !messages.contains_key(id) {
      order.push(id.to_string());
    }
    let message = messages.entry(id.to_string()).or_default();
    match event {
      "delta.message_start" => {
        idle_after_output = false;
        settle_at = None;
      }
      "delta.text_append" if !message.done => {
        idle_after_output = false;
        settle_at = None;
        if let Some(text) = text_field(&[payload, &root], &["text", "delta"]) {
          message.text.push_str(text);
          out.text_deltas += 1;
          if out.first_matched_delta_ms.is_none() {
            out.first_matched_delta_ms = Some(started.elapsed().as_millis() as u64);
            on_first_delta();
          }
        }
      }
      "delta.message_done" | "message.assistant" => {
        if event == "message.assistant" && (payload["display_text_ready"] == false || root["display_text_ready"] == false) {
          continue;
        }
        let status = text_field(&[payload, &root], &["status"]);
        match status {
          Some("completed" | "complete" | "done" | "finished" | "ok") | None => {}
          Some("failed" | "error" | "cancelled" | "canceled" | "interrupted") => {
            break "remote_failure"
          }
          Some(_) => continue,
        }
        if let Some(text) = final_text(payload, &root, id) {
          message.text = text;
        }
        if !message.done {
          out.first_delta_to_message_done_ms = out.first_matched_delta_ms.map(|first| {
            (started.elapsed().as_millis() as u64).saturating_sub(first)
          });
          settle_at = None;
          message.done = true;
        }
      }
      _ => {}
    }
    if out.lines > 16_384 || message.text.len() > 512 * 1024 || order.len() > 64 {
      break "response_limit";
    }
    if idle_after_output && messages.values().all(|m| m.done) {
      out.saw_idle = true;
      settle_at.get_or_insert(Instant::now() + Duration::from_secs(1));
    }
  }
  .to_string();
  out.assistant_messages = messages.len();
  out.text = order
    .iter()
    .map(|id| messages[id].text.as_str())
    .collect::<Vec<_>>()
    .join("\n\n");
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn history_requires_completed_reply_for_latest_acknowledged_turn() {
    let user = serde_json::json!({"role":"user","id":"u","seq":1});
    let reply = serde_json::json!({"role":"assistant","id":"a","seq":2,
      "status":"completed","content":[{"type":"text","text":"verified reply"}]});
    let read = |rows: Vec<Value>| {
      completed_history_reply(
        serde_json::json!({"ok":true,"result":{
          "session_id":"s","messages":rows
        }})
        .to_string()
        .as_bytes(),
        "s",
        "u",
      )
    };
    assert_eq!(
      read(vec![reply.clone(), user.clone()]).as_deref(),
      Some("verified reply")
    );
    assert!(read(vec![reply.clone()]).is_none());
    assert!(read(vec![
      user.clone(),
      reply.clone(),
      serde_json::json!({"role":"user","id":"other","seq":3})
    ])
    .is_none());
    let mut partial = reply.clone();
    partial["status"] = serde_json::json!("running");
    assert!(read(vec![user.clone(), partial]).is_none());
    let mut unrelated = reply;
    unrelated["parent_message_id"] = serde_json::json!("other");
    assert!(read(vec![user, unrelated]).is_none());
  }
  use std::collections::VecDeque;

  struct Scripted(VecDeque<Result<Option<String>, GatewayError>>, bool);

  impl LineSource for Scripted {
    fn http_status(&self) -> Option<u16> {
      Some(200)
    }
    fn is_scoped_to(&self, session_id: &str) -> bool {
      self.1 && session_id == "test-session"
    }
    async fn next_line(&mut self, _wait: Duration) -> Result<Option<String>, GatewayError> {
      if let Some(item) = self.0.pop_front() {
        return item;
      }
      std::future::pending().await
    }
  }

  fn ack() -> Ack {
    Ack {
      message_id: "user-1".to_string(),
      session_id: Some("test-session".to_string()),
      raw: serde_json::json!({"message_id":"user-1","session_id":"test-session"}),
    }
  }

  fn lines(values: &[&str]) -> Scripted {
    Scripted(
      values
        .iter()
        .map(|line| Ok(Some((*line).to_string())))
        .collect(),
      false,
    )
  }

  #[tokio::test]
  async fn scoped_reply_uses_acked_user_anchor_and_ignores_the_previous_burst() {
    let mut source = lines(&[
      r#"{"type":"event","event":"message.user","message_id":"prior-user","payload":{"session_id":"test-session"}}"#,
      r#"{"type":"event","event":"delta.message_start","message_id":"prior-reply"}"#,
      r#"{"type":"event","event":"delta.text_append","message_id":"prior-reply","payload":{"text":"previous reply"}}"#,
      r#"{"type":"event","event":"delta.message_done","message_id":"prior-reply"}"#,
      r#"{"type":"event","event":"message.user","message_id":"user-1","ts_ms":200}"#,
      r#"{"type":"event","event":"delta.message_start","message_id":"prior-reply"}"#,
      r#"{"type":"event","event":"delta.message_start","message_id":"older-unknown","occurred_at_ms":100}"#,
      r#"{"type":"event","event":"delta.message_start","message_id":"wrong-session","payload":{"session_id":"another"}}"#,
      r#"{"type":"event","event":"delta.message_start","message_id":"assistant","payload":{"message_id":"user-1"}}"#,
      r#"{"type":"event","event":"delta.text_append","message_id":"assistant","payload":{"text":"current "}}"#,
      r#"{"type":"event","event":"message.assistant","message_id":"assistant","payload":{"display_text_ready":false}}"#,
      r#"{"type":"event","event":"delta.text_append","message_id":"assistant","payload":{"delta":"reply"}}"#,
      r#"{"type":"event","event":"agent.status","payload":{"activity_code":"idle"}}"#,
      r#"{"type":"event","event":"delta.message_done","message_id":"assistant"}"#,
    ]);
    source.1 = true;
    let mut notified = 0;
    let result = collect_text(&mut source, &ack(), "test-session", Duration::from_secs(2), Duration::from_millis(100), || notified += 1).await;
    assert_eq!(result.ended_by, "done");
    assert_eq!(result.text, "current reply");
    assert_eq!(result.text_deltas, 2);
    assert!(result.first_delta_to_message_done_ms.is_some());
    assert_eq!(result.outside_turn_events, 5);
    assert_eq!(result.conflicting_events, 1);
    assert_eq!(notified, 1);
  }

  #[tokio::test]
  async fn unscoped_or_unanchored_events_do_not_create_a_reply() {
    for (scoped, user) in [(false, "user-1"), (true, "different-user")] {
      let user = format!(r#"{{"type":"event","event":"message.user","message_id":"{user}"}}"#);
      let mut source = lines(&[
        &user,
        r#"{"type":"event","event":"delta.message_start","message_id":"assistant"}"#,
        r#"{"type":"event","event":"delta.text_append","message_id":"assistant","payload":{"text":"unlinked"}}"#,
      ]);
      source.1 = scoped;
      let result = collect_text(&mut source, &ack(), "test-session", Duration::from_millis(30), Duration::from_millis(5), || panic!("unlinked notification")).await;
      assert!(result.text.is_empty());
      assert_eq!(result.text_deltas, 0);
    }
  }

  #[tokio::test]
  async fn collects_only_matching_assistant_and_preserves_whitespace_after_read_timeout() {
    let mut src = lines(&[
      r#"{"type":"event","event":"message.user","message_id":"user-1","payload":{"session_id":"test-session","turn_id":"turn-1"}}"#,
      r#"{"type":"event","event":"delta.message_done","message_id":"other","payload":{"status":"complete","text":"wrong"}}"#,
      r#"{"type":"event","event":"delta.text_append","message_id":"assistant","seq":3,"payload":{"turn_id":"turn-1","text":"one "}}"#,
      r#"{"type":"event","event":"delta.text_append","message_id":"assistant","seq":3,"payload":{"text":"duplicate"}}"#,
      r#"{"type":"event","event":"delta.text_append","message_id":"assistant","seq":4,"payload":{"text":"two"}}"#,
      r#"{"type":"event","event":"delta.message_done","message_id":"assistant","payload":{"status":"complete"}}"#,
      r#"{"type":"event","event":"agent.status","payload":{"activity_code":"idle"}}"#,
    ]);
    src
      .0
      .push_front(Err(GatewayError::Timeout("subscription read")));
    let mut live_notifications = 0;
    let result = collect_text(
      &mut src,
      &ack(),
      "test-session",
      Duration::from_secs(2),
      Duration::from_millis(100),
      || live_notifications += 1,
    )
    .await;
    assert!(result.done);
    assert_eq!(result.ended_by, "done");
    assert_eq!(result.text, "one two");
    assert_eq!(result.text_deltas, 2);
    assert_eq!(live_notifications, 1);
    assert_eq!(result.subscription_events.delta, 3);
    assert!(result.first_matched_delta_ms.is_some());
    assert!(result.read_timeouts > 0);
    assert_eq!(result.unmatched_events, 1);
  }

  #[tokio::test]
  async fn message_done_without_turn_end_does_not_advance() {
    let mut src = lines(&[
      r#"{"type":"event","event":"delta.message_done","message_id":"assistant","payload":{"parent_message_id":"user-1","status":"complete","text":"reply"}}"#,
    ]);
    let result = collect_text(
      &mut src,
      &ack(),
      "test-session",
      Duration::from_millis(20),
      Duration::from_millis(5),
      || panic!("completion must not emit a live delta notification"),
    )
    .await;
    assert!(!result.done);
    assert_eq!(result.ended_by, "timeout");
  }

  #[tokio::test]
  async fn distinguishes_cancellation_close_and_protocol_error() {
    for (event, expected) in [
      (Ok(None), "closed"),
      (Err(GatewayError::Cancelled), "cancelled"),
      (
        Err(GatewayError::Protocol("invalid frame".to_string())),
        "protocol_error",
      ),
    ] {
      let mut src = Scripted(VecDeque::from([event]), false);
      let result = collect_text(
        &mut src,
        &ack(),
        "test-session",
        Duration::from_secs(1),
        Duration::from_secs(1),
        || panic!("a failed read cannot produce a live delta"),
      )
      .await;
      assert!(!result.done);
      assert_eq!(result.ended_by, expected);
    }
  }
}
