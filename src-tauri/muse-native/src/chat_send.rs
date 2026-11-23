//! B4：`chat.stream` 原生发送（Issue #13 / i005 #15）。
//!
//! Wire 形状（BotArk `chat.ts` streamChat 实测注释）：
//! - `chat.stream` 是**订阅**而非 unary：`gw.subscribe('chat.stream', params)`；
//! - params: `{items:[{type:"text",text}], node_id, capabilities:[...delta_stream]}`，
//!   可选 `session_id`；
//! - 首个非 event 行是 ack（`{message_id, session_id, channel…}`），随后事件在
//!   `chat.subscribe` 上继续。
//!
//! 安全：本模块只负责「发一次并拿到 ack」，其余判断（单发锁、不重发、UNKNOWN）
//! 由 `send_ledger` 决策。

use crate::gateway::{ConnectedGateway, GatewayError};
use crate::transport::{HttpRequest, Service};
use serde_json::{json, Value};
use std::time::Duration;

/// 网页端广告的能力，`delta_stream` 决定服务端是否推文本增量。
pub const CHAT_CAPABILITIES: &[&str] = &[
  "hotel_results",
  "chat_cancel",
  "delta_stream",
  "custom_reactions",
  "custom_reactions_facebook_thumbs_up_v1",
];

pub fn build_params(text: &str, node_id: &str, session_id: Option<&str>) -> Value {
  let mut p = json!({
    "items": [{ "type": "text", "text": text }],
    "node_id": node_id,
    "capabilities": CHAT_CAPABILITIES,
  });
  if let Some(sid) = session_id {
    if !sid.is_empty() {
      p["session_id"] = json!(sid);
    }
  }
  p
}

/// 发送一次并等待 ack（首个非 event 行）。返回 (ack JSON, session_id)。
pub async fn send_message(
  gw: &mut ConnectedGateway,
  text: &str,
  node_id: &str,
  session_id: Option<&str>,
  timeout: Duration,
) -> Result<(Value, Option<String>), GatewayError> {
  let params = build_params(text, node_id, session_id);
  let req = HttpRequest {
    service: Service::Daemon,
    verb: "POST".to_string(),
    path: "/chat/stream".to_string(),
    headers: vec![
      ("Content-Type".to_string(), "application/json".to_string()),
      ("x-request-id".to_string(), uuid::Uuid::new_v4().to_string()),
      ("x-app-id".to_string(), "hatch-web".to_string()),
      ("Accept-Language".to_string(), "en-US".to_string()),
    ],
    body: serde_json::to_vec(&params).unwrap_or_default(),
  };
  let mut sub = gw.subscribe(req).await?;
  loop {
    match sub.next_line(timeout).await? {
      None => {
        return Err(GatewayError::Closed(
          "chat.stream ended without ack".to_string(),
        ))
      }
      Some(line) => {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
          continue;
        };
        // 事件行不是 ack；ack 是普通 JSON 对象。
        if v.get("type").and_then(Value::as_str) == Some("event") {
          continue;
        }
        let sid = v
          .get("session_id")
          .and_then(Value::as_str)
          .map(str::to_string);
        return Ok((v, sid));
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn params_include_text_item_and_capabilities() {
    let p = build_params("hello", "node-1", None);
    assert_eq!(p["items"][0]["type"], "text");
    assert_eq!(p["items"][0]["text"], "hello");
    assert_eq!(p["node_id"], "node-1");
    assert!(p["capabilities"].as_array().unwrap().iter().any(|c| c == "delta_stream"));
    assert!(p.get("session_id").is_none());
  }

  #[test]
  fn params_include_session_when_given() {
    let p = build_params("x", "n", Some("sess-9"));
    assert_eq!(p["session_id"], "sess-9");
    let p2 = build_params("x", "n", Some(""));
    assert!(p2.get("session_id").is_none());
  }
}
