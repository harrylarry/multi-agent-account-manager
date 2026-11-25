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
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
  pub provider_id: ProviderId,
  pub uuid: String,
  pub alias: String,
  pub group: Option<String>,
  pub created_at_ms: u64,
  pub proxy_endpoint: Option<String>,
  pub proxy_requires_auth: bool,
  #[serde(default)]
  pub proxy_enabled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePreview {
  pub added: usize,
  pub existing: usize,
  pub conflicts: Vec<String>,
  pub checksum: String,
}

pub fn preview(backup: &AccountBackup, current: &[AccountRecord]) -> RestorePreview {
  let mut added = 0;
  let mut existing = 0;
  let mut conflicts = Vec::new();
  let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
  for profile in &backup.accounts {
    let old = current.iter().find(|account| account.uuid == profile.uuid);
    if let Some(account) = old {
      existing += 1;
      let projection = serde_json::json!([
        account.uuid, account.provider_id, account.alias, account.group,
        account.proxy.as_ref().map(ProxySetting::display_safe),
      ]);
      digest.update(projection.to_string().as_bytes());
      if account.provider_id != profile.provider_id {
        conflicts.push(format!("{}（标识归属另一个服务）", profile.alias));
      } else if account.alias != profile.alias || (backup.includes_groups && account.group != profile.group) {
        conflicts.push(profile.alias.clone());
      }
    } else {
      added += 1;
      digest.update(profile.uuid.as_bytes());
    }
  }
  RestorePreview { added, existing, conflicts, checksum: STANDARD.encode(digest.finish().as_ref()) }
}

#[cfg(target_os = "ios")]
pub fn from_accounts(
  accounts: &[AccountRecord],
  include_groups: bool,
  include_proxy_endpoints: bool,
) -> AccountBackup {
  AccountBackup {
    includes_groups: include_groups,
    includes_proxy_endpoints: include_proxy_endpoints,
    accounts: accounts.iter().map(|account| {
      let proxy = account.proxy.as_ref().filter(|_| include_proxy_endpoints);
      Profile {
        provider_id: account.provider_id,
        uuid: account.uuid.clone(),
        alias: account.alias.clone(),
        group: account.group.clone().filter(|_| include_groups),
        created_at_ms: account.created_at_ms,
        proxy_endpoint: proxy.filter(|setting| setting.validate().is_ok()).map(ProxySetting::display_safe),
        proxy_requires_auth: proxy.is_some_and(|setting| setting.credential_ref.is_some()),
        proxy_enabled: proxy.is_some_and(|setting| setting.enabled),
      }
    }).collect(),
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
  let mut seen = std::collections::HashSet::new();
  for profile in &backup.accounts {
    if normalize_uuid(&profile.uuid).as_deref() != Some(&profile.uuid)
      || !seen.insert(&profile.uuid) || profile.alias.trim().is_empty() || profile.alias.chars().count() > 160
      || profile.group.as_deref().is_some_and(|group| normalize_account_group(group).is_err())
    {
      return Err("备份含无效账号或重复标识".into());
    }
    if let Some(endpoint) = &profile.proxy_endpoint {
      let parsed = muse_native_client::proxy::ProxyConfig::parse(endpoint).map_err(|_| "备份含无效代理端点")?;
      if parsed.display_safe() != *endpoint || parsed.has_credentials() {
        return Err("备份含不可用的代理元数据".into());
      }
    }
  }
  Ok(backup)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn account_backup_roundtrip_rejects_wrong_password_and_tampering() {
    let backup = AccountBackup { accounts: Vec::new(), includes_groups: true, includes_proxy_endpoints: false };
    let encoded = encrypt(&backup, "test-password").unwrap();
    assert!(decrypt(&encoded, "bad-password").is_err());
    assert!(decrypt(&encoded, "test-password").unwrap().includes_groups);
    let mut corrupted = STANDARD.decode(encoded).unwrap();
    *corrupted.last_mut().unwrap() ^= 1;
    assert!(decrypt(&STANDARD.encode(corrupted), "test-password").is_err());
    assert_ne!(encrypt(&backup, "test-password").unwrap(), encrypt(&backup, "test-password").unwrap());
  }
}
