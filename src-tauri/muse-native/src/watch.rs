//! B1：`chat.subscribe` 事件归一化与去重（Issue #13 / i005 #5）。
//!
//! 事件形状（对照 BotArk v0.12.0 `src/main/muse-native/chat.ts` 的实测注释）：
//! `{type:"event", event:"<name>", seq, ts_ms, payload:{...}}`，payload 里可能
//! 再带 message_id / text / status / activity_code。订阅是一次长连接，事件以
//! NDJSON 分块到达，可能跨 chunk 断开，所以解析只在“行”一级做。
//!
//! 本模块只做归一化与去重，不发请求、不碰 UI；网关的订阅读行在
//! `gateway::Subscription`。

use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

/// Muse 客户端认为“智能体仍在忙”的活动码（与网页端 tC/tT 一致）。
const BUSY_ACTIVITY: &[&str] = &[
  "responding",
  "composing",
  "making_something",
  "working",
  "waiting_for_subagents",
  "compacting",
  "needs_approval",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchKind {
  /// 用户消息出现：一轮对话开始。
  TurnStarted,
  /// 助手气泡开始（delta.message_start）。
  MessageStarted,
  /// 文本增量（delta.text_append 的 payload.text）。
  TextDelta(String),
  /// 一条助手消息结束（delta.message_done / message.assistant）。
  MessageDone { status: Option<String> },
  /// 智能体活动状态为忙。
  AgentBusy(String),
  /// 智能体活动状态为空闲。
  AgentIdle(String),
  /// 本切片不关心的事件（tool_call、presentation 等）。
  Other,
}

#[derive(Debug, Clone)]
pub struct WatchEvent {
  pub name: String,
  pub seq: i64,
  pub message_id: Option<String>,
  pub kind: WatchKind,
}

fn non_empty(v: Option<&Value>) -> Option<String> {
  v.and_then(Value::as_str)
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_string)
}

/// 解析一行 NDJSON；非 event 行、坏 JSON、缺事件名都返回 None。
pub fn parse_event(line: &str) -> Option<WatchEvent> {
  let root: Value = serde_json::from_str(line.trim()).ok()?;
  if root.get("type").and_then(Value::as_str) != Some("event") {
    return None;
  }
  let name = root
    .get("event")
    .or_else(|| root.get("event_name"))
    .and_then(Value::as_str)?
    .to_string();
  let seq = root.get("seq").and_then(Value::as_i64).unwrap_or(0);

  // 顶层字段与 payload 合并，payload 覆盖同名顶层字段（与参考实现一致）。
  let mut merged = root.clone();
  if let (Some(src), Some(dst)) = (
    root.get("payload").and_then(Value::as_object),
    merged.as_object_mut(),
  ) {
    for (k, v) in src {
      dst.insert(k.clone(), v.clone());
    }
  }
  if let Some(map) = merged.as_object_mut() {
    for k in ["type", "event", "event_name", "seq", "ts_ms", "payload"] {
      map.remove(k);
    }
  }

  let message_id = non_empty(merged.get("message_id")).or_else(|| non_empty(merged.get("id")));
  let kind = match name.as_str() {
    "message.user" => WatchKind::TurnStarted,
    "delta.message_start" => WatchKind::MessageStarted,
    "delta.text_append" | "delta.placeholder" => {
      WatchKind::TextDelta(non_empty(merged.get("text")).unwrap_or_default())
    }
    "delta.message_done" | "message.assistant" => WatchKind::MessageDone {
      status: non_empty(merged.get("status")).map(|s| s.to_lowercase()),
    },
    "agent.status" => {
      let code = non_empty(merged.get("activity_code")).unwrap_or_default();
      if BUSY_ACTIVITY.contains(&code.as_str()) {
        WatchKind::AgentBusy(code)
      } else {
        WatchKind::AgentIdle(code)
      }
    }
    _ => WatchKind::Other,
  };

  Some(WatchEvent {
    name,
    seq,
    message_id,
    kind,
  })
}

/// 对外可见的状态变化；文本增量不产生通知（由调用方自己累计）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
  TurnStarted,
  MessageStarted(String),
  Completed(String),
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchSummary {
  pub events: u64,
  pub turns_started: u64,
  pub messages_started: u64,
  pub text_deltas: u64,
  pub messages_done: u64,
  pub busy: u64,
  pub idle: u64,
  pub activity: Option<&'static str>,
  pub duplicates: u64,
  pub ignored: u64,
  pub events_by_name: BTreeMap<String, u64>,
}

/// 去重与计数：同一 (event, seq) 只处理一次；同一条消息完成只通知一次。
#[derive(Debug, Default)]
pub struct WatchTracker {
  seen: HashSet<(String, i64)>,
  completed: HashSet<String>,
  summary: WatchSummary,
}

impl WatchTracker {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn summary(&self) -> &WatchSummary {
    &self.summary
  }

  /// 处理一行 NDJSON，返回需要通知调用方的状态变化。
  pub fn observe_line(&mut self, line: &str) -> Option<Notice> {
    let ev = parse_event(line)?;
    self.observe(ev)
  }

  pub fn observe(&mut self, ev: WatchEvent) -> Option<Notice> {
    self.summary.events += 1;
    *self
      .summary
      .events_by_name
      .entry(ev.name.clone())
      .or_insert(0) += 1;

    if ev.seq > 0 && !self.seen.insert((ev.name.clone(), ev.seq)) {
      self.summary.duplicates += 1;
      return None;
    }

    match ev.kind {
      WatchKind::TurnStarted => {
        self.summary.turns_started += 1;
        Some(Notice::TurnStarted)
      }
      WatchKind::MessageStarted => {
        self.summary.messages_started += 1;
        ev.message_id.map(Notice::MessageStarted)
      }
      WatchKind::TextDelta(_) => {
        self.summary.text_deltas += 1;
        None
      }
      WatchKind::MessageDone { .. } => {
        let id = ev.message_id.unwrap_or_default();
        if id.is_empty() {
          self.summary.messages_done += 1;
          return None;
        }
        if !self.completed.insert(id.clone()) {
          self.summary.duplicates += 1;
          return None;
        }
        self.summary.messages_done += 1;
        Some(Notice::Completed(id))
      }
      WatchKind::AgentBusy(code) => {
        self.summary.busy += 1;
        self.summary.activity = Some(if code == "needs_approval" { "approval" } else { "working" });
        None
      }
      WatchKind::AgentIdle(code) => {
        self.summary.idle += 1;
        self.summary.activity = match code.as_str() {
          "online" | "idle" => Some("idle"),
          _ => None,
        };
        None
      }
      WatchKind::Other => {
        self.summary.ignored += 1;
        None
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn line(name: &str, seq: i64, payload: &str) -> String {
    format!(
      r#"{{"type":"event","event":"{name}","seq":{seq},"ts_ms":1700000000000,"payload":{payload}}}"#
    )
  }

  #[test]
  fn parses_text_delta() {
    let ev = parse_event(&line("delta.text_append", 3, r#"{"message_id":"m1","text":"你好"}"#))
      .expect("event");
    assert_eq!(ev.name, "delta.text_append");
    assert_eq!(ev.seq, 3);
    assert_eq!(ev.message_id.as_deref(), Some("m1"));
    assert_eq!(ev.kind, WatchKind::TextDelta("你好".to_string()));
  }

  #[test]
  fn parses_message_done_status_from_payload() {
    let ev = parse_event(&line(
      "delta.message_done",
      9,
      r#"{"message_id":"m1","status":"completed"}"#,
    ))
    .expect("event");
    assert_eq!(
      ev.kind,
      WatchKind::MessageDone {
        status: Some("completed".to_string())
      }
    );
  }

  #[test]
  fn agent_activity_busy_and_idle() {
    let busy = parse_event(&line("agent.status", 1, r#"{"activity_code":"responding"}"#))
      .expect("event");
    assert_eq!(busy.kind, WatchKind::AgentBusy("responding".to_string()));
    let idle = parse_event(&line("agent.status", 2, r#"{"activity_code":"online"}"#))
      .expect("event");
    assert_eq!(idle.kind, WatchKind::AgentIdle("online".to_string()));
  }

  #[test]
  fn ignores_non_event_and_malformed_lines() {
    assert!(parse_event(r#"{"type":"ack"}"#).is_none());
    assert!(parse_event("not json").is_none());
    assert!(parse_event(r#"{"type":"event","seq":1}"#).is_none());
  }

  #[test]
  fn duplicate_seq_is_counted_not_noticed() {
    let mut t = WatchTracker::new();
    let l = line("delta.message_start", 5, r#"{"message_id":"m1"}"#);
    assert_eq!(
      t.observe_line(&l),
      Some(Notice::MessageStarted("m1".to_string()))
    );
    assert_eq!(t.observe_line(&l), None);
    assert_eq!(t.summary().duplicates, 1);
    assert_eq!(t.summary().messages_started, 1);
  }

  #[test]
  fn message_done_notified_once_per_message() {
    let mut t = WatchTracker::new();
    let a = line("delta.message_done", 7, r#"{"message_id":"m1"}"#);
    let b = line("message.assistant", 8, r#"{"message_id":"m1"}"#);
    assert_eq!(t.observe_line(&a), Some(Notice::Completed("m1".to_string())));
    assert_eq!(t.observe_line(&b), None);
    assert_eq!(t.summary().messages_done, 1);
    assert_eq!(t.summary().duplicates, 1);
  }

  #[test]
  fn turn_start_and_other_events() {
    let mut t = WatchTracker::new();
    assert_eq!(
      t.observe_line(&line("message.user", 2, r#"{"session_id":"s1"}"#)),
      Some(Notice::TurnStarted)
    );
    assert_eq!(
      t.observe_line(&line("delta.tool_start", 3, r#"{"message_id":"m1"}"#)),
      None
    );
    let s = t.summary();
    assert_eq!(s.turns_started, 1);
    assert_eq!(s.ignored, 1);
    assert_eq!(s.events, 2);
  }
}
