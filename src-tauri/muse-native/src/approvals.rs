//! B2：`egress.approvals` 只读解析（Issue #13 / i005 #6）。
//!
//! 服务端有两种响应形状（BotArk `docs/museai/samples/approval_list_response.redacted.json`）：
//! - legacy：`{pending: [flat], recent: [flat]}`
//! - snapshot：`{pending_approvals: [...], recent_approvals: [...]}`
//! 外层可能再包一层 `result`。本模块只提取展示所需的最小字段，不碰审批内容正文，
//! 也不提供任何决定（approve/deny）能力。

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalItem {
  pub id: String,
  pub kind: String,
  pub display_name: String,
  pub requested_at_ms: i64,
  pub expires_at_ms: i64,
  pub status: String,
}

fn text(v: &Value, key: &str) -> String {
  v.get(key)
    .and_then(Value::as_str)
    .unwrap_or("")
    .trim()
    .to_string()
}

fn num(v: &Value, key: &str) -> i64 {
  v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn item_from(v: &Value) -> ApprovalItem {
  let display = v.get("display").cloned().unwrap_or(Value::Null);
  let display_name = if display.is_object() {
    text(&display, "display_name")
  } else {
    text(v, "display_name")
  };
  let mut kind = text(v, "approval_type");
  if kind.is_empty() {
    kind = text(v, "action_label");
  }
  let mut status = text(v, "status");
  if status.is_empty() {
    status = "pending".to_string();
  }
  let expires = {
    let e = num(v, "request_expires_at_ms");
    if e > 0 {
      e
    } else {
      num(v, "expires_at_ms")
    }
  };
  ApprovalItem {
    id: text(v, "approval_id"),
    kind,
    display_name,
    requested_at_ms: num(v, "requested_at_ms"),
    expires_at_ms: expires,
    status,
  }
}

/// 解析待审批列表；兼容 snapshot / legacy 与 `result` 包装。
pub fn parse_pending(raw: &Value) -> Option<Vec<ApprovalItem>> {
  let root = raw.get("result").unwrap_or(raw);
  let items = root.get("pending_approvals").or_else(|| root.get("pending"))?.as_array()?;
  let mut seen = std::collections::HashSet::new();
  let mut pending = Vec::with_capacity(items.len());
  for item in items {
    let item = item_from(item);
    if item.id.is_empty() || !seen.insert(item.id.clone()) {
      return None;
    }
    pending.push(item);
  }
  Some(pending)
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn parses_snapshot_shape() {
    let v = json!({
      "pending_approvals": [
        {
          "approval_id": "a1",
          "requested_at_ms": 1762100000000i64,
          "request_expires_at_ms": 1762100300000i64,
          "status": "pending",
          "approval_type": "connector",
          "display": {"display_name": "GitHub", "action_label": "create_repository"}
        }
      ]
    });
    let list = parse_pending(&v).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "a1");
    assert_eq!(list[0].display_name, "GitHub");
    assert_eq!(list[0].kind, "connector");
    assert_eq!(list[0].expires_at_ms, 1762100300000);
  }

  #[test]
  fn parses_legacy_shape_with_result_wrapper() {
    let v = json!({
      "result": {
        "pending": [
          {
            "approval_id": "a2",
            "display_name": "api.github.com",
            "action_label": "create_repository",
            "expires_at_ms": 1762100300000i64
          }
        ]
      }
    });
    let list = parse_pending(&v).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].display_name, "api.github.com");
    assert_eq!(list[0].kind, "create_repository");
    assert_eq!(list[0].status, "pending");
  }

  #[test]
  fn empty_list_is_distinct_from_an_unrecognized_response() {
    assert_eq!(parse_pending(&json!({"pending_approvals": []})), Some(Vec::new()));
    assert!(parse_pending(&json!({})).is_none());
    assert!(parse_pending(&json!({"pending": [{}]})).is_none());
    assert!(parse_pending(&json!({"pending": [{"approval_id":"x"}, {"approval_id":"x"}]})).is_none());
  }
}
