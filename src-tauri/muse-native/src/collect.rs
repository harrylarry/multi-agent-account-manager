//! #18 探针采集：把一条 `chat.stream` 的助手回复完整收下来。
//!
//! 为什么需要单独一层：`chat.stream` 是**订阅**，ack 之后正文并不在同一路
//! 流里继续，而是在 `chat.subscribe` 的长连接上以 `delta.text_append` 事件到达。
//! 所以一次探针要两条连接：
//! 1. 订阅连接：先开 `chat.subscribe`，从第一帧起就开始收事件（先订后发，避免漏头）；
//! 2. 发送连接：`chat.stream` 只负责投递与 ack（复用 `chat_send::send_message`）。
//!
//! 本模块只做「收集」，不做评分、不做判定：拿不到就报拿不到，绝不猜文本。

use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::gateway::{ConnectedGateway, GatewayError, Subscription};
use crate::transport::{HttpRequest, Service};
use crate::watch::{parse_event, WatchKind};
use serde::Serialize;

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
  pub saw_busy: bool,
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

/// 从订阅里收文本，直到 `message_done`、超时或断流。
///
/// `total` 是整轮预算；`per_line` 是单行等待上限（避免一次 read 卡死整轮）。
/// `message_id` 若给定，只累积该消息的增量；否则累积第一份到达的正文。
pub async fn collect_text<S>(
  sub: &mut Subscription<S>,
  message_id: Option<&str>,
  total: Duration,
  per_line: Duration,
) -> Collected
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  let deadline = tokio::time::Instant::now() + total;
  let mut out = Collected::default();
  let mut target = message_id.map(str::to_string);

  loop {
    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
    if left.is_zero() {
      out.ended_by = "timeout".to_string();
      return out;
    }
    let wait = left.min(per_line);
    match sub.next_line(wait).await {
      Ok(Some(line)) => {
        out.lines += 1;
        if parse_event(&line).is_none() {
          // 非 event 行（ack/心跳）不算错误，但也不计入解析失败。
          if serde_json::from_str::<serde_json::Value>(line.trim()).is_err() {
            out.parse_errors += 1;
          }
          continue;
        }
        let Some(ev) = parse_event(&line) else {
          continue;
        };
        // 只看目标消息：第一条正文出现时把它定为目标。
        let belongs = match (&target, &ev.message_id) {
          (None, _) => true,
          (Some(t), Some(m)) => t == m,
          (Some(_), None) => true,
        };
        match ev.kind {
          WatchKind::AgentBusy(_) => {
            out.saw_busy = true;
          }
          WatchKind::MessageStarted => {
            if target.is_none() {
              target = ev.message_id.clone();
            }
          }
          WatchKind::TextDelta(delta) => {
            if target.is_none() {
              target = ev.message_id.clone();
            }
            if belongs {
              out.text.push_str(&delta);
              out.text_deltas += 1;
            }
          }
          WatchKind::MessageDone { .. } => {
            if belongs {
              out.done = true;
              out.ended_by = "done".to_string();
              return out;
            }
          }
          WatchKind::TurnStarted | WatchKind::AgentIdle(_) | WatchKind::Other => {}
        }
      }
      Ok(None) => {
        out.ended_by = "closed".to_string();
        return out;
      }
      Err(_) => {
        out.ended_by = "closed".to_string();
        return out;
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn default_collected_is_empty_and_not_done() {
    let c = Collected::default();
    assert_eq!(c.text, "");
    assert!(!c.done);
    assert_eq!(c.ended_by, "");
  }

  #[test]
  fn capabilities_include_delta_stream() {
    assert!(SUBSCRIBE_CAPABILITIES.contains(&"delta_stream"));
  }
}