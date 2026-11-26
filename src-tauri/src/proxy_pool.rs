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
