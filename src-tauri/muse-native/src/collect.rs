//! Collect a correlated assistant turn on the same Noise connection as the send.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

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
  pub saw_user: bool,
  pub saw_idle: bool,
  pub assistant_messages: usize,
}

/// 构造 `chat.subscribe` 请求（供直连与代理两条路径复用）。
pub fn subscription_request() -> HttpRequest {
  let body = serde_json::json!({ "capabilities": SUBSCRIBE_CAPABILITIES })
    .to_string()
    .into_bytes();
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
    body,
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
  text_field(
    &[payload, &payload["message"], root],
    &["display_text", "text", "content"],
  )
  .map(str::to_string)
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
) -> Collected {
  use tokio::time::{timeout_at, Instant};
  let deadline = Instant::now() + total;
  let mut out = Collected::default();
  let mut messages = HashMap::<String, AssistantMessage>::new();
  let mut order = Vec::new();
  let mut seen = HashSet::new();
  let fields = ack.raw.get("result").unwrap_or(&ack.raw);
  let mut turn = text_field(&[fields], &["turn_id"]).map(str::to_string);
  let mut task = text_field(&[fields], &["task_id"]).map(str::to_string);
  let mut settle_at: Option<Instant> = None;

  out.ended_by = loop {
    let now = Instant::now();
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
    let msg = text_field(&[payload, &root], &["message_id", "assistant_message_id"])
      .or_else(|| text_field(&[&payload["message"]], &["message_id", "id"]));
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
      continue;
    }
    if event == "message.user" {
      if msg.or(parent) == Some(ack.message_id.as_str()) {
        out.saw_user = true;
        if turn.is_none() {
          turn = event_turn.map(str::to_string);
        }
        if task.is_none() {
          task = event_task.map(str::to_string);
        }
      } else if session == Some(session_id) || (session.is_none() && out.saw_user) {
        break "competing_turn";
      }
      continue;
    }
    if event_turn.zip(turn.as_deref()).is_some_and(|(a, b)| a != b)
      || event_task.zip(task.as_deref()).is_some_and(|(a, b)| a != b)
      || parent.is_some_and(|id| id != ack.message_id)
    {
      out.unmatched_events += 1;
      continue;
    }
    if event == "agent.status" {
      match text_field(&[payload, &root], &["activity_code"]) {
        Some("idle" | "online") if !messages.is_empty() && messages.values().all(|m| m.done) => {
          out.saw_idle = true;
          settle_at.get_or_insert(Instant::now() + Duration::from_secs(1));
        }
        Some("idle" | "online") | None => {}
        Some(_) => {
          settle_at = None;
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
    let Some(id) = msg.filter(|id| *id != ack.message_id) else {
      out.unmatched_events += 1;
      continue;
    };
    let linked = parent == Some(ack.message_id.as_str())
      || event_turn.zip(turn.as_deref()).is_some_and(|(a, b)| a == b)
      || event_task.zip(task.as_deref()).is_some_and(|(a, b)| a == b)
      || (out.saw_user && session == Some(session_id))
      || messages.contains_key(id);
    if !linked {
      out.unmatched_events += 1;
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
      "delta.message_start" => settle_at = None,
      "delta.text_append" if !message.done => {
        settle_at = None;
        if let Some(text) = text_field(&[payload, &root], &["text"]) {
          message.text.push_str(text);
          out.text_deltas += 1;
        }
      }
      "delta.message_done" | "message.assistant" => {
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
          settle_at = None;
          message.done = true;
        }
      }
      _ => {}
    }
    if out.lines > 16_384 || message.text.len() > 512 * 1024 || order.len() > 64 {
      break "response_limit";
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
  use std::collections::VecDeque;

  struct Scripted(VecDeque<Result<Option<String>, GatewayError>>);

  impl LineSource for Scripted {
    fn http_status(&self) -> Option<u16> {
      Some(200)
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
    )
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
    let result = collect_text(
      &mut src,
      &ack(),
      "test-session",
      Duration::from_secs(2),
      Duration::from_millis(100),
    )
    .await;
    assert!(result.done);
    assert_eq!(result.ended_by, "done");
    assert_eq!(result.text, "one two");
    assert_eq!(result.text_deltas, 2);
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
      let mut src = Scripted(VecDeque::from([event]));
      let result = collect_text(
        &mut src,
        &ack(),
        "test-session",
        Duration::from_secs(1),
        Duration::from_secs(1),
      )
      .await;
      assert!(!result.done);
      assert_eq!(result.ended_by, expected);
    }
  }
}
