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
  #[serde(skip)]
  pub unread_count: Option<u64>,
}

#[cfg(target_os = "ios")]
pub async fn fetch(
  egress: &crate::egress::AccountEgress,
  cookies: &muse_native_client::cookie::SessionCookies,
) -> Result<Vec<MuseSession>, String> {
  fetch_list(egress, cookies, "/api/session/list").await
}

#[cfg(target_os = "ios")]
async fn fetch_list(
  egress: &crate::egress::AccountEgress,
  cookies: &muse_native_client::cookie::SessionCookies,
  path: &str,
) -> Result<Vec<MuseSession>, String> {
  use muse_native_client::transport::{HttpRequest, Service};
  let mut gateway = Box::pin(egress.connect(cookies)).await.map_err(|_| "无法连接 Muse 会话网关")?;
  let (status, body) = gateway.request(HttpRequest {
    service: Service::Daemon,
    verb: "GET".into(),
    path: path.into(),
    headers: Vec::new(),
    body: Vec::new(),
  }).await.map_err(|_| "Muse 会话查询未完成")?;
  if !(200..300).contains(&status) { return Err(format!("会话列表请求失败（HTTP {status}）")); }
  if body.len() > 2 * 1024 * 1024 { return Err("会话列表响应过大".into()); }
  parse_list(&body)
}

#[cfg(target_os = "ios")]
pub async fn fetch_unread(
  egress: &crate::egress::AccountEgress,
  cookies: &muse_native_client::cookie::SessionCookies,
) -> Result<Option<std::collections::BTreeMap<Uuid, u64>>, String> {
  let sessions = Box::pin(fetch_list(egress, cookies, "/api/session/list?include_peer_sidechats=true")).await?;
  if !sessions.iter().any(|session| session.unread_count.is_some()) {
    return Ok(None);
  }
  Ok(Some(sessions.into_iter().filter_map(|session| {
    session.unread_count.map(|count| (session.id, count))
  }).collect()))
}

#[cfg(any(target_os = "ios", test))]
pub fn update_unread_counts(
  previous: &mut std::collections::BTreeMap<Uuid, u64>,
  observed: &std::collections::BTreeMap<Uuid, u64>,
) -> bool {
  let mut increased = false;
  for (&id, &count) in observed {
    if count == 0 {
      previous.remove(&id);
    } else {
      increased |= count > previous.insert(id, count).unwrap_or(0);
    }
  }
  // Keep a bounded baseline. Eviction is only for sessions absent from this sample.
  if previous.len() > 2048 {
    previous.retain(|id, _| observed.contains_key(id));
  }
  increased
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
      unread_count: item["unread_count"].as_u64(),
    });
  }
  sessions.sort_by_key(|session| (session.archived, !session.pinned, std::cmp::Reverse(session.updated_at_ms)));
  Ok(sessions)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn catches_new_server_unread_and_deduplicates_repeated_reads() {
    let id = Uuid::from_u128(1);
    let other = Uuid::from_u128(2);
    let mut previous = Default::default();
    assert!(update_unread_counts(&mut previous, &[(id, 1)].into()));
    assert!(!update_unread_counts(&mut previous, &[(id, 1)].into()));
    assert!(!update_unread_counts(&mut previous, &[(other, 0)].into()));
    assert!(!update_unread_counts(&mut previous, &[(id, 0)].into()));
    assert!(update_unread_counts(&mut previous, &[(id, 1)].into()));
    assert!(update_unread_counts(&mut previous, &[(other, 1)].into()));
  }

  #[test]
  fn parses_valid_sessions_without_treating_an_error_as_empty() {
    let raw = br#"{"ok":true,"result":{"sessions":[
      {"session_id":"6ea0907b-13b5-448b-bcc1-5659db9c7ae6","title":"regular"},
      {"sessionId":"bd097124-f7a3-46b4-8eb5-370db511c53d","displayTitle":"pinned","updatedAt":1777000000,"isPinned":true,"unread_count":2}
    ]}}"#;
    let result = parse_list(raw).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].title, "pinned");
    assert_eq!(result[0].updated_at_ms, Some(1_777_000_000_000));
    assert_eq!(result[0].unread_count, Some(2));
    assert_eq!(result[1].unread_count, None);
    assert!(parse_list(br#"{"sessions":[]}"#).unwrap().is_empty());
    assert!(parse_list(br#"{"ok":false,"sessions":[]}"#).is_err());
    assert!(parse_list(br#"{"sessions":[{"id":"../../"}]}"#).is_err());
  }
}
