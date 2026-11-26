use crate::providers::ProviderId;
use crate::storage::{normalize_account_group, normalize_uuid, AccountRecord, ProxySetting};
use base64::{engine::general_purpose::STANDARD, Engine};
use ring::{aead, pbkdf2, rand::{SecureRandom, SystemRandom}};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"MAAMBAK1";
const HEADER: usize = 8 + 16 + 12;
const MAX_BYTES: usize = 2 * 1024 * 1024;
const ROUNDS: NonZeroU32 = NonZeroU32::new(600_000).unwrap();

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountBackup {
  pub accounts: Vec<Profile>,
  pub includes_groups: bool,
  pub includes_proxy_endpoints: bool,
  #[serde(default)]
  pub proxy_pool: Vec<ProxyPoolProfile>,
  #[serde(default)]
  pub expires_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyPoolProfile {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub entry_id: Option<uuid::Uuid>,
  pub endpoint: String,
  pub requires_auth: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
  pub provider_id: ProviderId,
  pub uuid: String,
  pub alias: String,
  pub group: Option<String>,
  #[serde(default)]
  pub timezone: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub language: Option<String>,
  #[serde(default)]
  pub last_muse_thread: Option<uuid::Uuid>,
  pub created_at_ms: u64,
  pub proxy_endpoint: Option<String>,
  pub proxy_requires_auth: bool,
  #[serde(default)]
  pub proxy_enabled: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub proxy_pool_entry_id: Option<uuid::Uuid>,
}

impl Profile {
  pub fn restored_proxy(&self, existing: Option<&ProxySetting>) -> Option<ProxySetting> {
    let had_proxy = self.proxy_endpoint.is_some() || self.proxy_enabled;
    if !had_proxy { return existing.cloned(); }
    if let Some(existing) = existing.filter(|setting| self.proxy_endpoint.is_some() && safe_proxy_endpoint(setting) == self.proxy_endpoint) {
      if !self.proxy_requires_auth || existing.credential_ref.is_some() {
        let mut same = existing.clone();
        if self.proxy_pool_entry_id.is_some() { same.pool_entry_id = self.proxy_pool_entry_id; }
        // A metadata restore never disables an enabled route.
        same.enabled |= self.proxy_enabled;
        return Some(same);
      }
    }
    Some(ProxySetting {
      pool_entry_id: self.proxy_pool_entry_id,
      uri:self.proxy_endpoint.clone().unwrap_or_default(),
      username:None,
      credential_ref:self.proxy_requires_auth.then(|| format!("proxy-restore-{}", self.uuid)),
      enabled:self.proxy_enabled || existing.is_some_and(|setting| setting.enabled),
    })
  }
}

fn safe_proxy_endpoint(setting: &ProxySetting) -> Option<String> {
  let endpoint = muse_native_client::proxy::ProxyConfig::parse_endpoint(&setting.uri).ok()?;
  let host = if endpoint.host.contains(':') && !endpoint.host.starts_with('[') {
    format!("[{}]", endpoint.host)
  } else { endpoint.host };
  Some(format!("{}://{host}:{}", endpoint.scheme, endpoint.port))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePreview {
  pub added: usize,
  pub existing: usize,
  pub conflicts: Vec<String>,
  pub proxy_pool_count: usize,
  pub checksum: String,
}

pub fn preview(backup: &AccountBackup, current: &[AccountRecord]) -> RestorePreview {
  let mut added = 0;
  let mut existing = 0;
  let mut conflicts = Vec::new();
  let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
  if let Ok(payload) = serde_json::to_vec(backup) { digest.update(&payload); }
  for profile in &backup.accounts {
    let old = current.iter().find(|account| account.uuid == profile.uuid);
    if let Some(account) = old {
      existing += 1;
      let projection = serde_json::json!([
        account.uuid, account.provider_id, account.alias, account.group, account.timezone, account.language, account.last_muse_thread,
        account.proxy.as_ref().and_then(safe_proxy_endpoint), account.proxy.as_ref().and_then(|proxy| proxy.pool_entry_id),
      ]);
      digest.update(projection.to_string().as_bytes());
      if account.provider_id != profile.provider_id {
        conflicts.push(format!("{}（标识归属另一个服务）", profile.alias));
      } else if account.alias != profile.alias || account.timezone != profile.timezone || account.language != profile.language || (backup.includes_groups && account.group != profile.group) {
        conflicts.push(profile.alias.clone());
      }
      if backup.includes_proxy_endpoints && (profile.proxy_enabled || profile.proxy_endpoint.is_some())
        && account.proxy.as_ref().and_then(safe_proxy_endpoint) != profile.proxy_endpoint {
        conflicts.push(format!("{}（代理端点不同）", profile.alias));
      }
    } else {
      added += 1;
      digest.update(profile.uuid.as_bytes());
    }
  }
  RestorePreview { added, existing, conflicts, proxy_pool_count: backup.proxy_pool.len(),
    checksum: STANDARD.encode(digest.finish().as_ref()) }
}

#[cfg(target_os = "ios")]
pub fn from_accounts(
  accounts: &[AccountRecord],
  include_groups: bool,
  include_proxy_endpoints: bool,
  proxy_pool: Vec<ProxyPoolProfile>,
  expires_at_ms: Option<u64>,
) -> AccountBackup {
  let profiles = accounts.iter().map(|account| {
    let proxy = account.proxy.as_ref().filter(|_| include_proxy_endpoints);
    let endpoint = proxy.and_then(safe_proxy_endpoint);
    let pool_entry_id = proxy.and_then(|setting| setting.pool_entry_id).filter(|id| {
      proxy_pool.iter().any(|entry| crate::proxy_pool::restored_entry_id(entry) == *id && Some(&entry.endpoint) == endpoint.as_ref())
    });
    Profile {
      provider_id: account.provider_id, uuid: account.uuid.clone(), alias: account.alias.clone(),
      group: account.group.clone().filter(|_| include_groups), timezone: account.timezone.clone(), language: account.language.clone(),
      last_muse_thread: account.last_muse_thread, created_at_ms: account.created_at_ms,
      proxy_endpoint: endpoint, proxy_requires_auth: proxy.is_some_and(|setting| setting.credential_ref.is_some()),
      proxy_enabled: proxy.is_some_and(|setting| setting.enabled), proxy_pool_entry_id: pool_entry_id,
    }
  }).collect();
  AccountBackup {
    includes_groups: include_groups,
    includes_proxy_endpoints: include_proxy_endpoints,
    proxy_pool,
    expires_at_ms,
    accounts: profiles,
  }
}

fn key(password: &str, salt: &[u8]) -> Result<aead::LessSafeKey, String> {
  if !(8..=1024).contains(&password.chars().count()) {
    return Err("备份口令长度须为 8 至 1024 个字符".into());
  }
  let mut bytes = Zeroizing::new([0; 32]);
  pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA256, ROUNDS, salt, password.as_bytes(), &mut *bytes);
  aead::UnboundKey::new(&aead::AES_256_GCM, &*bytes).map(aead::LessSafeKey::new)
    .map_err(|_| "无法初始化备份加密".into())
}

pub fn encrypt(backup: &AccountBackup, password: &str) -> Result<String, String> {
  let random = SystemRandom::new();
  let mut salt = [0; 16];
  let mut nonce = [0; 12];
  random.fill(&mut salt).and_then(|_| random.fill(&mut nonce)).map_err(|_| "无法生成随机数")?;
  let mut header = Vec::with_capacity(HEADER);
  header.extend_from_slice(MAGIC);
  header.extend_from_slice(&salt);
  header.extend_from_slice(&nonce);
  let mut body = Zeroizing::new(serde_json::to_vec(backup).map_err(|_| "备份数据无法编码")?);
  if body.len() > MAX_BYTES - HEADER - 16 { return Err("备份数据过大".into()); }
  key(password, &salt)?.seal_in_place_append_tag(
    aead::Nonce::assume_unique_for_key(nonce), aead::Aad::from(&header), &mut *body,
  ).map_err(|_| "备份加密失败")?;
  header.extend_from_slice(&body);
  Ok(STANDARD.encode(header))
}

pub fn decrypt(encoded: &str, password: &str) -> Result<AccountBackup, String> {
  let encoded = encoded.trim();
  if encoded.len() > MAX_BYTES.div_ceil(3) * 4 { return Err("备份文件过大".into()); }
  let mut data = Zeroizing::new(STANDARD.decode(encoded).map_err(|_| "备份编码无效")?);
  if data.len() < HEADER + 16 || data.len() > MAX_BYTES || &data[..8] != MAGIC {
    return Err("备份版本或格式无法识别".into());
  }
  let header: [u8; HEADER] = data[..HEADER].try_into().map_err(|_| "备份头损坏")?;
  let nonce = aead::Nonce::try_assume_unique_for_key(&header[24..]).map_err(|_| "备份头损坏")?;
  let body = key(password, &header[8..24])?
    .open_in_place(nonce, aead::Aad::from(&header), &mut data[HEADER..])
    .map_err(|_| "口令错误或备份已损坏")?;
  let backup: AccountBackup = serde_json::from_slice(body).map_err(|_| "备份数据无效")?;
  if backup.expires_at_ms.is_some_and(|deadline| deadline <= crate::storage::now_ms()) {
    return Err("这份加密备份已超过设定的恢复期限".into());
  }
  if backup.proxy_pool.len() > 256 || (!backup.includes_proxy_endpoints && !backup.proxy_pool.is_empty()) {
    return Err("备份内的代理池范围无效".into());
  }
  for entry in &backup.proxy_pool {
    validate_proxy_endpoint(&entry.endpoint)?;
  }
  let mut seen = std::collections::HashSet::new();
  let mut bindings = std::collections::HashSet::new();
  for profile in &backup.accounts {
    if normalize_uuid(&profile.uuid).as_deref() != Some(&profile.uuid)
      || !seen.insert(&profile.uuid) || profile.alias.trim().is_empty() || profile.alias.chars().count() > 160
      || profile.group.as_deref().is_some_and(|group| normalize_account_group(group).is_err())
      || profile.language.as_deref().is_some_and(|value| crate::storage::normalize_account_language(value).is_err())
      || profile.timezone.as_deref().is_some_and(|timezone| crate::storage::normalize_account_timezone(timezone).is_err())
      || (profile.provider_id != ProviderId::Muse && profile.last_muse_thread.is_some())
    {
      return Err("备份含无效账号或重复标识".into());
    }
    if let Some(endpoint) = &profile.proxy_endpoint {
      validate_proxy_endpoint(endpoint)?;
    }
    if let Some(id) = profile.proxy_pool_entry_id {
      if !bindings.insert(id) || !backup.proxy_pool.iter().any(|entry| crate::proxy_pool::restored_entry_id(entry) == id
        && profile.proxy_endpoint.as_ref() == Some(&entry.endpoint) && entry.requires_auth == profile.proxy_requires_auth) {
        return Err("备份含无效或重复的账号代理引用".into());
      }
    }
  }
  Ok(backup)
}

fn validate_proxy_endpoint(endpoint: &str) -> Result<(), String> {
  let parsed = muse_native_client::proxy::ProxyConfig::parse(endpoint).map_err(|_| "备份含无效代理端点")?;
  if parsed.display_safe() != endpoint || parsed.has_credentials() {
    return Err("备份含不可用的代理元数据".into());
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn restored_proxy_never_drops_an_enabled_route_or_reuses_a_different_host_secret() {
    let mut profile: Profile = serde_json::from_value(serde_json::json!({
      "providerId":"muse","uuid":"00000000-0000-4000-8000-000000000001","alias":"test","createdAtMs":0,
      "proxyEnabled":true,"proxyRequiresAuth":true,
    })).unwrap();
    let blocked = profile.restored_proxy(None).unwrap();
    assert!(blocked.enabled);
    assert!(blocked.validate().is_err());
    let active = ProxySetting { pool_entry_id: None, uri:"socks5://old.example:1234".into(), username:Some("proxy-user".into()),
      credential_ref:Some("existing-key".into()), enabled:true };
    profile.proxy_endpoint = Some("socks5://new.example:1234".into());
    let new = profile.restored_proxy(Some(&active)).unwrap();
    assert!(new.enabled);
    assert_ne!(new.credential_ref, active.credential_ref);
    assert!(new.validate().is_err());
    profile.proxy_endpoint = Some(active.uri.clone());
    assert_eq!(profile.restored_proxy(Some(&active)).unwrap().credential_ref, active.credential_ref);
  }

  #[test]
  fn older_backups_import_accounts_and_discard_retired_keys() {
    let old = serde_json::json!({ "accounts":[{"providerId":"muse","uuid":"00000000-0000-4000-8000-000000000001",
      "alias":"test","createdAtMs":0,"proxyRequiresAuth":false,"proxyEnabled":false}],
      "includesGroups":true,"includesProxyEndpoints":false,"rooms":{"rooms":[{"text":"old data"}]} });
    let backup: AccountBackup = serde_json::from_value(old).unwrap();
    assert_eq!(backup.accounts.len(), 1);
    assert!(serde_json::to_value(&backup).unwrap().get("rooms").is_none());
  }

  #[test]
  fn account_backup_roundtrip_rejects_wrong_password_and_tampering() {
    let backup = AccountBackup { accounts: Vec::new(), includes_groups: true, includes_proxy_endpoints: false, proxy_pool: Vec::new(), expires_at_ms: None };
    let encoded = encrypt(&backup, "test-password").unwrap();
    assert!(decrypt(&encoded, "bad-password").is_err());
    assert!(decrypt(&encoded, "test-password").unwrap().includes_groups);
    let mut corrupted = STANDARD.decode(encoded).unwrap();
    *corrupted.last_mut().unwrap() ^= 1;
    assert!(decrypt(&STANDARD.encode(corrupted), "test-password").is_err());
    assert_ne!(encrypt(&backup, "test-password").unwrap(), encrypt(&backup, "test-password").unwrap());
  }
}
