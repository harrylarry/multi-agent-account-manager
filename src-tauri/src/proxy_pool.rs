use crate::secrets::Secrets;
use crate::account_backup::ProxyPoolProfile;
use muse_native_client::proxy::ProxyConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const KEYCHAIN_KEY: &str = "proxy-pool-v1";
const MAX_INPUT: usize = 32 * 1024;
const MAX_ENTRIES: usize = 128;
const REFRESH_INTERVAL: u64 = 6 * 60 * 60;

#[derive(Default)]
pub struct ProxyPoolGate(pub tokio::sync::Mutex<()>);

#[derive(Default, Deserialize, Serialize)]
pub struct ProxyPool {
  entries: Vec<Entry>,
  sources: Vec<Source>,
}

#[derive(Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
struct Entry {
  #[zeroize(skip)]
  id: Uuid,
  uri: String,
  #[zeroize(skip)]
  source: Option<Uuid>,
}

#[derive(Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
struct Source {
  #[zeroize(skip)]
  id: Uuid,
  url: String,
  refreshed_at: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolView {
  entries: Vec<EntryView>,
  sources: Vec<SourceView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EntryView {
  id: Uuid,
  endpoint: String,
  requires_auth: bool,
  missing_credentials: bool,
  source_id: Option<Uuid>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceView {
  id: Uuid,
  host: String,
  refreshed_at: u64,
}

pub struct ApplyEntry {
  pub endpoint: String,
  pub username: Option<String>,
  pub password: Option<String>,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(rename_all = "camelCase")]
struct ManualEdit {
  endpoint: String,
  username: String,
  password: String,
  remove_auth: bool,
}

#[cfg(not(target_os = "android"))]
pub async fn inspect_exit(client: &reqwest::Client) -> Result<serde_json::Value, String> {
  let mut response = client.get("https://api.ipquery.io/?format=json").send().await
    .map_err(|error| crate::egress::safe_http_error("出口检测", &error))?;
  if !response.status().is_success() { return Err(format!("出口检测 HTTP {}", response.status().as_u16())); }
  let mut body = Vec::new();
  while let Some(chunk) = response.chunk().await.map_err(|_| "出口检测响应读取失败")? {
    if body.len().saturating_add(chunk.len()) > 32 * 1024 { return Err("出口检测响应过大".into()); }
    body.extend(chunk);
  }
  let payload = serde_json::from_slice(&body).map_err(|_| "出口检测响应格式无效")?;
  parse_exit(&payload).ok_or_else(|| "未取得可核验的出口地址".into())
}

#[cfg(not(target_os = "android"))]
fn parse_exit(payload: &serde_json::Value) -> Option<serde_json::Value> {
  let ip = payload["ip"].as_str()?.parse::<std::net::IpAddr>().ok()?;
  let location = &payload["location"];
  let short_text = |key: &str| location[key].as_str().map(str::trim)
    .filter(|value| !value.is_empty() && value.chars().all(|c| !c.is_control()))
    .map(|value| value.chars().take(96).collect::<String>());
  let risk = &payload["risk"];
  Some(serde_json::json!({
    "ip": ip.to_string(), "country": short_text("country"), "region": short_text("state"), "city": short_text("city"),
    "proxy": risk["is_proxy"].as_bool(), "datacenter": risk["is_datacenter"].as_bool(),
    "vpn": risk["is_vpn"].as_bool(), "tor": risk["is_tor"].as_bool(),
  }))
}

fn parse_lines(raw: &str) -> Result<Vec<String>, String> {
  if raw.len() > MAX_INPUT { return Err("代理列表过大".into()); }
  let mut unique = HashSet::new();
  let mut proxies = Vec::new();
  let mut entries = Vec::new();
  for line in raw.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
    let mut start = 0;
    for (index, separator) in line.match_indices([';', '；']) {
      let next = line[index + separator.len()..].trim_start();
      if ["http://", "https://", "socks5://", "socks5h://"].iter().any(|scheme| next.starts_with(scheme)) {
        entries.push(line[start..index].trim());
        start = index + separator.len();
      }
    }
    entries.push(line[start..].trim());
  }
  for (index, line) in entries.into_iter().enumerate() {
    let proxy = ProxyConfig::parse(line)
      .map_err(|_| format!("第 {} 条代理格式无效", index + 1))?;
    let rest = line.split_once("://").map(|(_, rest)| rest).unwrap_or_default();
    if rest.contains(['/', '?', '#']) {
      return Err(format!("第 {} 条应只有代理地址和认证信息", index + 1));
    }
    let uri = proxy.to_reqwest_proxy_uri().replacen("socks5h://", "socks5://", 1);
    if unique.insert(uri.clone()) { proxies.push(uri); }
  }
  if proxies.is_empty() || proxies.len() > MAX_ENTRIES {
    return Err("代理列表为空或超过容量".into());
  }
  Ok(proxies)
}

fn now() -> u64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_secs())
}

impl ProxyPool {
  pub fn contains(&self, id: Uuid) -> bool { self.entries.iter().any(|entry| entry.id == id) }
  pub fn read(secrets: &Secrets) -> Result<Self, String> {
    let Some(raw) = secrets.get(KEYCHAIN_KEY)?.map(Zeroizing::new) else { return Ok(Self::default()); };
    serde_json::from_str(&raw).map_err(|_| "代理池数据无法读取".into())
  }

  pub fn save(&self, secrets: &Secrets) -> Result<(), String> {
    let raw = Zeroizing::new(serde_json::to_string(self).map_err(|_| "代理池无法保存")?);
    secrets.set(KEYCHAIN_KEY, &raw)
  }

  pub fn view(&self) -> Result<PoolView, String> {
    let entries = self.entries.iter().map(|entry| {
      let proxy = ProxyConfig::parse(&entry.uri).map_err(|_| "代理池中有无效配置")?;
      Ok(EntryView {
        id: entry.id, endpoint: proxy.display_safe(),
        requires_auth: proxy.has_credentials(), missing_credentials: false, source_id: entry.source,
      })
    }).collect::<Result<Vec<_>, &str>>()?;
    let sources = self.sources.iter().map(|source| {
      let url = Url::parse(&source.url).map_err(|_| "代理订阅地址无效")?;
      Ok(SourceView {
        id: source.id, host: url.host_str().unwrap_or_default().to_string(),
        refreshed_at: source.refreshed_at,
      })
    }).collect::<Result<Vec<_>, &str>>()?;
    Ok(PoolView { entries, sources })
  }

  pub fn push_restored(view: &mut PoolView, restored: &[ProxyPoolProfile]) {
    for row in restored {
      if view.entries.iter().any(|entry| entry.id == restored_entry_id(row)
        || (row.entry_id.is_none() && entry.endpoint == row.endpoint && entry.requires_auth == row.requires_auth)) {
        continue;
      }
      view.entries.push(EntryView {
        id: restored_entry_id(row), endpoint: row.endpoint.clone(),
        requires_auth: row.requires_auth, missing_credentials: row.requires_auth, source_id: None,
      });
    }
  }

  #[cfg(target_os = "ios")]
  pub fn backup_metadata(&self) -> Result<Vec<ProxyPoolProfile>, String> {
    Ok(self.view()?.entries.into_iter().map(|entry| ProxyPoolProfile {
      entry_id:Some(entry.id), endpoint: entry.endpoint, requires_auth: entry.requires_auth,
    }).collect())
  }

  pub fn import(&mut self, raw: &str) -> Result<(), String> {
    self.replace_entries(None, parse_lines(raw)?)
  }

  pub fn edit_manual(&mut self, id: Uuid, raw: &str) -> Result<(), String> {
    let entries = Zeroizing::new(parse_lines(raw)?);
    let [uri] = entries.as_slice() else { return Err("编辑时只接受一条代理".into()); };
    let index = self.entries.iter().position(|entry| entry.id == id && entry.source.is_none())
      .ok_or("只能编辑手动添加的代理")?;
    if self.entries.iter().any(|other| other.id != id && other.uri == *uri) {
      return Err("代理池里已有相同配置".into());
    }
    let mut new_uri = uri.clone();
    std::mem::swap(&mut self.entries[index].uri, &mut new_uri);
    new_uri.zeroize();
    Ok(())
  }

  pub fn edit_details(&mut self, id: Uuid, raw: &str) -> Result<(), String> {
    let edit: ManualEdit = serde_json::from_str(raw).map_err(|_| "代理编辑资料无效")?;
    let candidates = Zeroizing::new(parse_lines(&edit.endpoint)?);
    let [endpoint] = candidates.as_slice() else { return Err("请输入一条代理地址".into()); };
    if endpoint.contains('@') { return Err("账号和密码请填写在认证栏".into()); }
    let entry = self.selected(id)?;
    if edit.remove_auth { return self.edit_manual(id, endpoint); }
    if endpoint != &entry.endpoint && entry.password.is_some()
      && (edit.username.is_empty() || edit.password.is_empty()) {
      return Err("代理地址已变更，请重新填写认证信息或明确移除认证".into());
    }
    let saved_username = entry.username.map(Zeroizing::new);
    let saved_password = entry.password.map(Zeroizing::new);
    let username = if edit.username.is_empty() { saved_username.as_deref().map(String::as_str) } else { Some(edit.username.as_str()) };
    let password = if edit.password.is_empty() { saved_password.as_deref().map(String::as_str) } else { Some(edit.password.as_str()) };
    if username.is_none() && password.is_none() { return self.edit_manual(id, endpoint); }
    let (Some(username), Some(password)) = (username, password) else { return Err("代理用户名和密码需要同时提供".into()); };
    let (scheme, authority) = endpoint.split_once("://").ok_or("代理地址无效")?;
    let encode = |text: &str| url::form_urlencoded::byte_serialize(text.as_bytes())
      .collect::<String>().replace('+', "%20");
    let encoded_user = Zeroizing::new(encode(username));
    let encoded_password = Zeroizing::new(encode(password));
    let uri = Zeroizing::new(format!("{scheme}://{encoded_user}:{encoded_password}@{authority}",
      encoded_user = encoded_user.as_str(), encoded_password = encoded_password.as_str()));
    self.edit_manual(id, &uri)
  }

  #[cfg(target_os = "ios")]
  pub fn copy_uri(&self, id: Uuid) -> Option<Zeroizing<String>> {
    self.entries.iter().find(|entry| entry.id == id).map(|entry| Zeroizing::new(entry.uri.clone()))
  }

  fn replace_entries(&mut self, source: Option<Uuid>, incoming: Vec<String>) -> Result<(), String> {
    let keep = self.entries.iter().filter(|entry| source.is_none() || entry.source != source).count();
    let to_add = incoming.iter().filter(|uri| {
      !self.entries.iter().any(|entry| entry.uri == uri.as_str() && (source.is_none() || entry.source != source))
    }).count();
    if keep + to_add > MAX_ENTRIES { return Err("代理池超过容量，保留原有配置".into()); }
    let mut previous = std::collections::HashMap::new();
    if let Some(source) = source {
      self.entries.retain(|entry| {
        if entry.source == Some(source) {
          previous.insert(entry.uri.clone(), entry.id);
          false
        } else {
          true
        }
      });
    }
    for uri in incoming {
      if self.entries.iter().any(|entry| entry.uri == uri) { continue; }
      let id = previous.remove(&uri).unwrap_or_else(Uuid::new_v4);
      self.entries.push(Entry { id, uri, source });
    }
    Ok(())
  }

  pub fn selected(&self, id: Uuid) -> Result<ApplyEntry, String> {
    let entry = self.entries.iter().find(|entry| entry.id == id).ok_or("代理条目不存在")?;
    let proxy = ProxyConfig::parse(&entry.uri).map_err(|_| "代理条目已损坏")?;
    let endpoint = ProxyConfig::parse_endpoint(&entry.uri).map_err(|_| "代理条目已损坏")?;
    Ok(ApplyEntry {
      endpoint: proxy.display_safe(),
      username: endpoint.username,
      password: proxy.password().map(str::to_string),
    })
  }

  pub fn remove(&mut self, id: Uuid) {
    self.sources.retain(|source| source.id != id);
    self.entries.retain(|entry| entry.id != id && entry.source != Some(id));
  }

  #[cfg(target_os = "android")]
  pub async fn subscribe(&mut self, _url: String) -> Result<Uuid, String> {
    Err("当前平台不支持代理订阅".into())
  }

  #[cfg(target_os = "android")]
  pub async fn refresh(&mut self, _id: Option<Uuid>) -> Result<bool, String> {
    Err("当前平台不支持代理订阅".into())
  }

  #[cfg(not(target_os = "android"))]
  pub async fn subscribe(&mut self, url: String) -> Result<Uuid, String> {
    let parsed = Url::parse(&url).map_err(|_| "订阅地址无效")?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() || !parsed.username().is_empty()
      || parsed.password().is_some() || parsed.fragment().is_some() {
      return Err("订阅需要有效的 HTTPS 地址".into());
    }
    let canonical = parsed.to_string();
    let existing = self.sources.iter().find(|source| source.url == canonical).map(|source| source.id);
    if existing.is_none() && self.sources.len() >= 16 { return Err("订阅数量已达上限".into()); }
    let entries = fetch_source(parsed).await?;
    let id = existing.unwrap_or_else(Uuid::new_v4);
    self.replace_entries(Some(id), entries)?;
    if let Some(source) = self.sources.iter_mut().find(|source| source.id == id) {
      source.refreshed_at = now();
    } else {
      self.sources.push(Source { id, url: canonical, refreshed_at: now() });
    }
    Ok(id)
  }

  #[cfg(not(target_os = "android"))]
  pub async fn refresh(&mut self, id: Option<Uuid>) -> Result<bool, String> {
    let sources: Vec<_> = self.sources.iter().filter(|source| {
      id.map_or_else(|| now().saturating_sub(source.refreshed_at) >= REFRESH_INTERVAL, |id| source.id == id)
    }).map(|source| source.url.clone()).collect();
    if sources.is_empty() && id.is_some() { return Err("订阅不存在".into()); }
    let refreshed = !sources.is_empty();
    for source in sources { self.subscribe(source).await?; }
    Ok(refreshed)
  }
}

#[cfg(not(target_os = "android"))]
async fn fetch_source(url: Url) -> Result<Vec<String>, String> {
  let mut response = reqwest::Client::builder()
    .timeout(Duration::from_secs(12)).redirect(reqwest::redirect::Policy::none())
    .build().map_err(|_| "无法初始化订阅请求")?
    .get(url).send().await.map_err(|_| "订阅联网失败，保留旧数据")?;
  if !response.status().is_success() { return Err("订阅返回异常，保留旧数据".into()); }
  let mut body = Zeroizing::new(Vec::new());
  while let Some(chunk) = response.chunk().await.map_err(|_| "订阅联网失败，保留旧数据")? {
    if body.len() + chunk.len() > MAX_INPUT { return Err("订阅过大，保留旧数据".into()); }
    body.extend_from_slice(&chunk);
  }
  let content = std::str::from_utf8(&body).map_err(|_| "订阅内容格式无效")?;
  parse_lines(content)
}

pub fn restored_entry_id(entry: &ProxyPoolProfile) -> Uuid {
  if let Some(id) = entry.entry_id { return id; }
  let key = format!("restored-proxy|{}|{}", entry.endpoint, entry.requires_auth);
  let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
  let mut bytes = [0; 16];
  bytes.copy_from_slice(&digest.as_ref()[..16]);
  Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn detailed_edit_keeps_same_route_credentials_and_requires_explicit_auth_for_a_new_route() {
    let mut pool = ProxyPool::default();
    pool.import("socks5://user:old@proxy.example:1080").unwrap();
    let id = pool.entries[0].id;
    let edit = |endpoint, username, password, remove_auth| serde_json::json!({
      "endpoint": endpoint, "username": username, "password": password, "removeAuth": remove_auth,
    }).to_string();
    let old_route = "socks5://proxy.example:1080";
    let new_route = "socks5://other.example:1080";
    pool.edit_details(id, &edit(old_route, "", "", false)).unwrap();
    assert_eq!(pool.selected(id).unwrap().password.as_deref(), Some("old"));
    assert!(pool.edit_details(id, &edit(new_route, "", "", false)).is_err());
    assert_eq!(pool.selected(id).unwrap().endpoint, old_route);
    pool.edit_details(id, &edit(new_route, "user name", "p%@;", false)).unwrap();
    assert_eq!(pool.selected(id).unwrap().username.as_deref(), Some("user name"));
    assert_eq!(pool.selected(id).unwrap().password.as_deref(), Some("p%@;"));
    pool.edit_details(id, &edit(new_route, "", "", true)).unwrap();
    assert!(pool.selected(id).unwrap().password.is_none());
  }

  #[cfg(not(target_os = "android"))]
  #[test]
  fn exit_result_keeps_missing_classifications_unknown() {
    assert!(parse_exit(&serde_json::json!({"ip":"not-an-ip"})).is_none());
    let row = parse_exit(&serde_json::json!({"ip":"192.0.2.1","location":{"country":"Test"},
      "risk":{"is_proxy":false,"is_datacenter":true}})).unwrap();
    assert_eq!(row["proxy"], false);
    assert_eq!(row["datacenter"], true);
    assert!(row["vpn"].is_null());
  }

  #[test]
  fn manual_edit_preserves_binding_id_and_rejects_duplicate_or_invalid_input() {
    let mut pool = ProxyPool::default();
    pool.import("socks5://u:old@proxy.example:1080; http://other.example:8080").unwrap();
    let id = pool.entries[0].id;
    assert!(pool.edit_manual(id, "http://other.example:8080").is_err());
    assert!(pool.edit_manual(id, "https://invalid.example:443/path").is_err());
    assert_eq!(pool.selected(id).unwrap().password.as_deref(), Some("old"));
    pool.edit_manual(id, "socks5://u:new@proxy.example:1080").unwrap();
    assert_eq!(pool.selected(id).unwrap().password.as_deref(), Some("new"));
    assert_eq!(pool.view().unwrap().entries[0].endpoint, "socks5://proxy.example:1080");
  }

  #[test]
  fn batch_import_understands_semicolons_and_preserves_userinfo() {
    let input = "# comment; https://ignored.example:80\nhttp://u:pass;word@one.example:80; socks5://two.example:1080；https://three.example:443";
    let rows = parse_lines(input).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(ProxyConfig::parse(&rows[0]).unwrap().password(), Some("pass;word"));
  }

  #[test]
  fn source_refresh_keeps_only_identical_credential_bindings() {
    let mut pool = ProxyPool::default();
    let source = Uuid::new_v4();
    let initial = parse_lines("socks5://user:one@proxy.example:1080\nhttp://other.example:8888").unwrap();
    pool.replace_entries(Some(source), initial).unwrap();
    let original = pool.entries[0].id;
    let stable = pool.entries[1].id;
    pool.replace_entries(Some(source), parse_lines("socks5://user:two@proxy.example:1080\nhttp://other.example:8888").unwrap()).unwrap();
    assert!(!pool.contains(original));
    assert!(pool.contains(stable));
  }

  #[test]
  fn validates_entire_batch_and_redacts_credentials() {
    let mut pool = ProxyPool::default();
    pool.import("socks5://a:b@proxy.example:1080\nsocks5://a:b@proxy.example:1080").unwrap();
    assert_eq!(pool.entries.len(), 1);
    let summary = serde_json::to_string(&pool.view().unwrap()).unwrap();
    assert!(!summary.contains("a:b"));
    assert!(pool.import("http://other.example:8888\nnot-a-proxy").is_err());
    assert_eq!(pool.entries.len(), 1);
    let selected = pool.selected(pool.entries[0].id).unwrap();
    assert_eq!(selected.password.as_deref(), Some("b"));
  }
}
