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
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BadgeTiming {
  pub resumed_at_ms: u64,
  pub handler_at_ms: u64,
  pub query_at_ms: Option<u64>,
  pub gateway_at_ms: Option<u64>,
  pub history_request_at_ms: Option<u64>,
  pub history_response_at_ms: Option<u64>,
  pub result_at_ms: Option<u64>,
  pub snapshot_at_ms: Option<u64>,
  pub server_reply_at_ms: Option<u64>,
  pub expected_dot: Option<bool>,
  pub state: &'static str,
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

#[cfg(any(target_os = "ios", test))]
pub(crate) fn parse_unread_total(raw: &Value) -> Option<u64> {
  if raw["ok"] == false || !raw["error"].is_null() { return None; }
  let body = raw.get("result").unwrap_or(raw);
  ["count", "unread_count", "unreadCount"].iter().find_map(|key| body[*key].as_u64())
}

#[cfg(target_os = "ios")]
pub(crate) async fn fetch_unread_total(
  app: &tauri::AppHandle, uuid: &str, setting: Option<&crate::storage::ProxySetting>,
) -> Result<Option<u64>, String> {
  let header = zeroize::Zeroizing::new(crate::muse_native::cookie_header_for(app, uuid).await?);
  if header.is_empty() { return Ok(None); }
  let egress = crate::egress::resolve_for_app(app, setting)?;
  let cookies = muse_native_client::cookie::SessionCookies::from_header(&header);
  let mut gateway = Box::pin(egress.connect(&cookies)).await.map_err(|_| "无法连接 Muse 网关")?;
  let body = Box::pin(request(&mut gateway, "/chat/unread-count")).await?;
  Ok(serde_json::from_slice::<Value>(&body).ok().as_ref().and_then(parse_unread_total))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MuseThreadArtifacts {
  names: Vec<String>,
  unscoped: usize,
}

#[cfg(target_os = "ios")]
pub async fn fetch_thread_artifacts(
  egress: &crate::egress::AccountEgress, cookies: &muse_native_client::cookie::SessionCookies,
  thread: Uuid,
) -> Result<MuseThreadArtifacts, String> {
  let mut gateway = Box::pin(egress.connect(cookies)).await.map_err(|_| "无法连接 Muse 会话网关")?;
  parse_thread_artifacts(&request(&mut gateway, "/api/artifacts").await?, thread)
}

#[cfg(any(target_os = "ios", test))]
fn parse_thread_artifacts(body: &[u8], thread: Uuid) -> Result<MuseThreadArtifacts, String> {
  let raw: Value = serde_json::from_slice(body).map_err(|_| "产物响应格式无效")?;
  if raw["ok"] == false || !raw["error"].is_null() { return Err("服务未返回产物列表".into()); }
  let envelope = raw.get("result").unwrap_or(&raw);
  let rows = envelope.as_array().or_else(|| ["artifacts", "items", "files", "results"].into_iter()
    .find_map(|key| envelope[key].as_array())).ok_or("无法核验产物列表结构")?;
  let mut names = Vec::new();
  let mut unscoped = 0;
  let mut ids = std::collections::HashSet::new();
  for row in rows {
    let scopes: Vec<_> = ["session_id", "thread_id"].into_iter().filter_map(|key| row[key].as_str())
      .collect();
    if scopes.is_empty() || scopes.iter().any(|id| Uuid::parse_str(id).is_err()) {
      unscoped += 1;
      continue;
    }
    if scopes.iter().any(|id| Uuid::parse_str(id).ok() != Some(thread)) { continue; }
    let identifier = ["artifact_id", "id", "file_id"].into_iter().find_map(|key| row[key].as_str());
    let Some(identifier) = identifier.filter(|id| !id.is_empty()) else { unscoped += 1; continue; };
    if !ids.insert(identifier) { continue; }
    let display = ["name", "title", "filename", "display_name"].into_iter().find_map(|key| row[key].as_str());
    let Some(display) = display.filter(|name| !name.trim().is_empty()) else { unscoped += 1; continue; };
    let name: String = display.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("")
      .trim().chars().filter(|character| !character.is_control()).take(160).collect();
    if !name.is_empty() { names.push(name); }
    if names.len() >= 100 { break; }
  }
  Ok(MuseThreadArtifacts { names, unscoped })
}

#[cfg(target_os = "ios")]
pub async fn verify_first_message(
  egress: &crate::egress::AccountEgress, cookies: &muse_native_client::cookie::SessionCookies,
  session_id: Uuid, digest: &str,
) -> Result<bool, String> {
  let mut gateway = Box::pin(egress.connect(cookies)).await.map_err(|_| "无法连接 Muse 会话网关")?;
  let path = format!("/chat/history?limit=30&transcript_mode=messages&session_id={session_id}");
  Ok(matches_first_message(&request(&mut gateway, &path).await?, session_id, digest))
}

#[cfg(any(target_os = "ios", test))]
fn matches_first_message(body: &[u8], session_id: Uuid, digest: &str) -> bool {
  let Ok(raw) = serde_json::from_slice::<Value>(body) else { return false; };
  let result = raw.get("result").unwrap_or(&raw);
  if raw["ok"] == false || raw["has_more"] == true || result["has_more"] == true || !raw["error"].is_null() {
    return false;
  }
  let session = session_id.to_string();
  if result["session_id"].as_str().is_some_and(|id| id != session) { return false; }
  let Some(rows) = ["chat_events", "messages"].iter().find_map(|key| result[*key].as_array())
    else { return false; };
  let mut matched = None;
  for row in rows {
    let payload = row.get("payload").unwrap_or(row);
    let is_user = row["event_name"] == "message.user" || payload["role"] == "user";
    if !is_user { continue; }
    if payload["session_id"].as_str().or_else(|| result["session_id"].as_str()) != Some(session.as_str()) {
      return false;
    }
    let Some(id) = row["message_id"].as_str().or_else(|| payload["id"].as_str()).filter(|id| !id.is_empty())
      else { return false; };
    let parts = payload["content"].as_array().filter(|parts| !parts.is_empty()).and_then(|parts| {
      parts.iter().map(|part| (part["type"] == "text").then(|| part["text"].as_str()).flatten())
        .collect::<Option<Vec<_>>>().map(|parts| parts.concat())
    });
    let text = payload["text"].as_str().or_else(|| payload["content"].as_str()).or(parts.as_deref());
    if text.map(crate::send_ledger::SendLedger::new_muse_conversation_digest).as_deref() != Some(digest)
      || matched.is_some_and(|previous| previous != id) { return false; }
    matched = Some(id);
  }
  matched.is_some()
}

#[cfg(target_os = "ios")]
pub async fn fetch_unread(
  egress: &crate::egress::AccountEgress,
  cookies: &muse_native_client::cookie::SessionCookies,
  thread: Option<Uuid>,
  primary: bool,
  parked_at: u64,
  timing: &mut BadgeTiming,
) -> Result<(Option<std::collections::BTreeMap<Uuid, u64>>, Option<String>, &'static str), String> {
  let mut gateway = Box::pin(egress.connect(cookies)).await.map_err(|_| "无法连接 Muse 会话网关")?;
  timing.gateway_at_ms = Some(crate::storage::now_ms());
  let has_history_target = thread.is_some() || primary;
  let status = if has_history_target {
    let mut path = "/chat/history?limit=30&transcript_mode=messages".to_string();
    if let Some(thread) = thread { path.push_str(&format!("&session_id={thread}")); }
    timing.history_request_at_ms = Some(crate::storage::now_ms());
    let response = Box::pin(request(&mut gateway, &path)).await;
    timing.history_response_at_ms = Some(crate::storage::now_ms());
    match response {
      Ok(body) => match completed_since_park(&body, thread, parked_at) {
        Some(Some((id, at))) => {
          timing.server_reply_at_ms = Some(at);
          return Ok((None, Some(id), "history_completed"));
        }
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
fn completed_since_park(body: &[u8], thread: Option<Uuid>, parked_at: u64) -> Option<Option<(String, u64)>> {
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
    if matches!(payload["status"].as_str().or_else(|| row["status"].as_str()), Some("completed" | "complete" | "done" | "finished")) {
      if let Some(at) = row["occurred_at_ms"].as_u64().filter(|time| *time >= parked_at) {
        completed = Some((id.to_owned(), at));
      }
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
  fn unread_totals_require_explicit_counts() {
    assert_eq!(parse_unread_total(&serde_json::json!({"count": 0})), Some(0));
    assert_eq!(parse_unread_total(&serde_json::json!({"ok": true, "result": {"unread_count": 7}})), Some(7));
    assert_eq!(parse_unread_total(&serde_json::json!({"ok": false, "count": 0})), None);
    assert_eq!(parse_unread_total(&serde_json::json!({"sessions": []})), None);
  }

  #[test]
  fn artifacts_require_explicit_thread_ownership_and_hide_download_urls() {
    let thread = Uuid::new_v4();
    let raw = serde_json::json!({"ok":true,"result":{"artifacts":[
      {"id":"same","session_id":thread,"name":"brief.md", "url":"https://private.example/file?sig=secret"},
      {"id":"same","thread_id":thread,"name":"brief.md"},
      {"id":"other","session_id":Uuid::new_v4(),"name":"other.md"},
      {"id":"unbound","name":"not-attributed.md"}
    ]}});
    let items = parse_thread_artifacts(raw.to_string().as_bytes(), thread).unwrap();
    assert_eq!(items.names, vec!["brief.md"]);
    assert_eq!(items.unscoped, 1);
    assert!(!serde_json::to_string(&items).unwrap().contains("secret"));
  }

  #[test]
  fn reconciliation_requires_exact_text_and_the_allocated_session() {
    let session = Uuid::new_v4();
    let digest = crate::send_ledger::SendLedger::new_muse_conversation_digest("test message");
    let mut response = serde_json::json!({"ok":true,"result":{"session_id":session,"chat_events":[
      {"event_name":"message.user","message_id":"user-1","payload":{"text":"test message"}}
    ]}});
    let verify = |response: &Value, id| matches_first_message(response.to_string().as_bytes(), id, &digest);
    assert!(verify(&response, session));
    assert!(!verify(&response, Uuid::new_v4()));
    response["result"]["chat_events"][0]["payload"]["text"] = "another message".into();
    assert!(!verify(&response, session));
    response["result"]["chat_events"][0]["payload"]["text"] = "test message".into();
    response["result"]["has_more"] = true.into();
    assert!(!verify(&response, session));
  }

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
    assert_eq!(parse(&body, Some(thread), 180), Some(Some(("a".into(), 200))));
    assert_eq!(parse(&body, None, 180), Some(Some(("a".into(), 200))));
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
