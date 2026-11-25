use serde::Serialize;
#[cfg(any(target_os = "ios", test))]
use serde_json::Value;
use uuid::Uuid;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MuseSession {
  pub id: Uuid,
  pub title: String,
  pub updated_at_ms: Option<u64>,
  pub pinned: bool,
  pub archived: bool,
}

#[cfg(any(target_os = "ios", test))]
pub fn parse_list(raw: &[u8]) -> Result<Vec<MuseSession>, String> {
  let raw: Value = serde_json::from_slice(raw).map_err(|_| "会话列表格式无效")?;
  if raw["ok"] == false || raw["success"] == false || !raw["error"].is_null() {
    return Err("服务未返回会话列表".into());
  }
  let root = raw.get("result").unwrap_or(&raw);
  let list = root.as_array().or_else(|| {
    ["sessions", "items", "results"].iter().find_map(|key| root[*key].as_array())
  }).ok_or("无法识别会话列表格式")?;
  let mut seen = std::collections::HashSet::new();
  let mut sessions = Vec::with_capacity(list.len());
  for item in list {
    let field = |keys: &[&str]| keys.iter().filter_map(|key| item.get(*key))
      .find_map(|value| value.as_str().filter(|text| !text.is_empty()));
    let id = field(&["id", "session_id", "sessionId"])
      .and_then(|value| Uuid::parse_str(value).ok())
      .ok_or("列表含有无法核对的会话标识")?;
    if !seen.insert(id) { continue; }
    let title = field(&["title", "name", "display_title", "displayTitle"])
      .unwrap_or("无标题会话").chars().filter(|ch| !ch.is_control()).take(160).collect();
    let timestamp = ["updated_at_ms", "updatedAtMs", "updated_at", "updatedAt"].iter()
      .find_map(|key| item[*key].as_u64()).filter(|value| *value > 0);
    let updated_at_ms = timestamp.map(|value| {
      if value < 1_000_000_000_000 { value.saturating_mul(1000) } else { value }
    }).filter(|value| *value < 253_402_300_800_000);
    sessions.push(MuseSession {
      id, title, updated_at_ms,
      pinned: ["pinned", "is_pinned", "isPinned"].iter().any(|key| item[*key] == true)
        || field(&["pin_state", "pinState"]).is_some_and(|value| value.eq_ignore_ascii_case("pinned")),
      archived: ["archived", "is_archived", "isArchived"].iter().any(|key| item[*key] == true)
        || field(&["status", "state"]).is_some_and(|value| value.eq_ignore_ascii_case("archived")),
    });
  }
  sessions.sort_by_key(|session| (session.archived, !session.pinned, std::cmp::Reverse(session.updated_at_ms)));
  Ok(sessions)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_valid_sessions_without_treating_an_error_as_empty() {
    let raw = br#"{"ok":true,"result":{"sessions":[
      {"session_id":"6ea0907b-13b5-448b-bcc1-5659db9c7ae6","title":"regular"},
      {"sessionId":"bd097124-f7a3-46b4-8eb5-370db511c53d","displayTitle":"pinned","updatedAt":1777000000,"isPinned":true}
    ]}}"#;
    let result = parse_list(raw).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].title, "pinned");
    assert_eq!(result[0].updated_at_ms, Some(1_777_000_000_000));
    assert!(parse_list(br#"{"sessions":[]}"#).unwrap().is_empty());
    assert!(parse_list(br#"{"ok":false,"sessions":[]}"#).is_err());
    assert!(parse_list(br#"{"sessions":[{"id":"../../"}]}"#).is_err());
  }
}
