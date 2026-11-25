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
  let mut gateway = Box::pin(egress.connect(cookies)).await.map_err(|_| "无法连接 Muse 会话网关")?;
  parse_list(&request(&mut gateway, path).await?)
}

#[cfg(target_os = "ios")]
async fn request(gateway: &mut crate::egress::AccountGateway, path: &str) -> Result<Vec<u8>, String> {
  use muse_native_client::transport::{HttpRequest, Service};
  let (status, body) = gateway.request(HttpRequest {
    service: Service::Daemon,
    verb: "GET".into(),
    path: path.into(),
    headers: Vec::new(),
    body: Vec::new(),
  }).await.map_err(|_| "Muse 会话查询未完成")?;
  if !(200..300).contains(&status) { return Err(format!("会话列表请求失败（HTTP {status}）")); }
  if body.len() > 2 * 1024 * 1024 { return Err("会话列表响应过大".into()); }
  Ok(body)
}

#[cfg(target_os = "ios")]
pub async fn fetch_unread(
  egress: &crate::egress::AccountEgress,
  cookies: &muse_native_client::cookie::SessionCookies,
  thread: Option<Uuid>,
  primary: bool,
  parked_at: u64,
) -> Result<(Option<std::collections::BTreeMap<Uuid, u64>>, Option<String>, &'static str), String> {
  let mut gateway = Box::pin(egress.connect(cookies)).await.map_err(|_| "无法连接 Muse 会话网关")?;
  let has_history_target = thread.is_some() || primary;
  let status = if has_history_target {
    let mut path = "/chat/history?limit=30&transcript_mode=messages".to_string();
    if let Some(thread) = thread { path.push_str(&format!("&session_id={thread}")); }
    match Box::pin(request(&mut gateway, &path)).await {
      Ok(body) => match completed_since_park(&body, thread, parked_at) {
        Some(Some(id)) => return Ok((None, Some(id), "history_completed")),
        Some(None) => "history_no_completion",
        None => "history_unavailable",
      },
      Err(_) => "history_unavailable",
    }
  } else { "no_thread" };
  let list = async {
    let body = Box::pin(request(&mut gateway, "/api/session/list?include_peer_sidechats=true")).await?;
    parse_list(&body)
  };
  let sessions = match list.await {
    Ok(sessions) => sessions,
    Err(_) if has_history_target => return Ok((None, None, status)),
    Err(error) => return Err(error),
  };
  let counts: std::collections::BTreeMap<_, _> = sessions.into_iter().filter_map(|session| {
    session.unread_count.map(|count| (session.id, count))
  }).collect();
  Ok(((!counts.is_empty()).then_some(counts), None, status))
}

#[cfg(any(target_os = "ios", test))]
fn completed_since_park(body: &[u8], thread: Option<Uuid>, parked_at: u64) -> Option<Option<String>> {
  let raw: Value = serde_json::from_slice(body).ok()?;
  if raw["ok"] == false || !raw["error"].is_null() { return None; }
  let result = raw.get("result").unwrap_or(&raw);
  let parse_thread = |value: &Value| value.as_str().and_then(|id| Uuid::parse_str(id).ok());
  let envelope_thread = parse_thread(&result["session_id"]);
  if matches!((thread, envelope_thread), (Some(expected), Some(actual)) if expected != actual) { return None; }
  let mut expected = thread.or(envelope_thread);
  let mut rows: Vec<&Value> = result["chat_events"].as_array()?.iter().collect();
  rows.sort_by_key(|row| row["seq"].as_u64().unwrap_or(0));
  let mut current_user = None;
  let mut completed = None;
  for row in rows {
    let payload = &row["payload"];
    let kind = row["event_name"].as_str();
    if !matches!(kind, Some("message.user" | "message.assistant")) { continue; }
    let observed = parse_thread(&payload["session_id"]).or(envelope_thread)?;
    if expected.is_some_and(|id| id != observed) { return None; }
    expected = Some(observed);
    let id = row["message_id"].as_str().filter(|id| !id.is_empty() && id.len() <= 128)?;
    if kind == Some("message.user") {
      current_user = Some(id);
      completed = None;
      continue;
    }
    let Some(user) = current_user else { continue; };
    let reply_to = row["reply_to_message_id"].as_str().filter(|id| !id.is_empty())
      .or_else(|| payload["reply_to_message_id"].as_str().filter(|id| !id.is_empty()));
    if reply_to.is_some_and(|parent| parent != user) || row["visibility"] != "public" || id == user {
      continue;
    }
    if matches!(payload["status"].as_str().or_else(|| row["status"].as_str()), Some("completed" | "complete" | "done" | "finished"))
      && row["occurred_at_ms"].as_u64().is_some_and(|time| time >= parked_at)
    {
      completed = Some(id.to_owned());
    }
  }
  Some(completed)
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
  fn history_requires_a_public_completed_reply_in_the_parked_thread_after_leaving() {
    use serde_json::json;
    let thread = Uuid::from_u128(1);
    let mut body = json!({"ok":true,"result":{"chat_events":[
      {"seq":20, "event_name":"message.assistant", "visibility":"public", "message_id":"a",
       "reply_to_message_id":"u", "occurred_at_ms":200, "payload":{"session_id":thread,"status":"completed"}},
      {"seq":19, "event_name":"message.user", "message_id":"u", "payload":{"session_id":thread}},
    ]}});
    let parse = |body: &Value, target, time| completed_since_park(&serde_json::to_vec(body).unwrap(), target, time);
    assert_eq!(parse(&body, Some(thread), 180), Some(Some("a".into())));
    assert_eq!(parse(&body, None, 180), Some(Some("a".into())));
    assert_eq!(parse(&body, Some(thread), 201), Some(None));
    assert_eq!(parse(&body, Some(Uuid::from_u128(2)), 180), None);
    body["result"]["chat_events"][0]["payload"]["status"] = json!("running");
    assert_eq!(parse(&body, Some(thread), 180), Some(None));
    body["result"]["chat_events"][0]["payload"]["status"] = json!("completed");
    body["result"]["chat_events"][0]["reply_to_message_id"] = json!("other");
    assert_eq!(parse(&body, Some(thread), 180), Some(None));
    body["result"]["chat_events"][0]["reply_to_message_id"] = json!("u");
    body["result"]["chat_events"][0]["visibility"] = json!("private");
    assert_eq!(parse(&body, Some(thread), 180), Some(None));
  }

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
