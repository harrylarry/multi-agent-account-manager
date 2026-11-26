use crate::providers::ProviderId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const STORE_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountKey {
  pub provider_id: ProviderId,
  pub uuid: String,
}

/// #11/#16：每账号 1:1 代理引用。
///
/// 安全约束（开发计划 §3.5 / P2-2）：
/// - `uri` 只保存 scheme://host:port，**不含明文口令**；口令放 Keychain，
///   这里仅保存 `credential_ref`（Keychain 项的引用名）。
/// - `fail_closed` 恒为 true：代理异常只能阻断，不能自动切池或清掉代理设置。
///   反序列化端用 `#[serde(default)]` 保证老账户缺失字段时也为 true。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySetting {
  /// scheme://host[:port]，可含 user@，但**不得含 :password**。
  pub uri: String,
  /// 代理认证用户名。与 `uri` 里的 user@ 二选一（都以 `uri` 为准）。
  #[serde(default)]
  pub username: Option<String>,
  /// Keychain 中口令项的 account 名；None 表示代理无需认证。
  #[serde(default)]
  pub credential_ref: Option<String>,
  /// 用户显式启用。故障逻辑不得改写此字段。
  #[serde(default)]
  pub enabled: bool,
}

impl ProxySetting {
  /// 强制 fail-closed：无论存储里写什么，代理启用时都不允许直连回退。
  /// 这里不提供关闭它的字段——这是结构性的，不是配置开关。
  #[allow(dead_code)] // 语义常量：本类型永远 fail-closed
  pub fn fail_closed(&self) -> bool {
    true
  }

  /// `uri` 的 authority 里是否带用户名。
  fn uri_has_user(&self) -> bool {
    self
      .uri
      .split_once("://")
      .map(|(_, r)| r.split('/').next().unwrap_or(r))
      .and_then(|authority| authority.rsplit_once('@').map(|(ui, _)| !ui.is_empty()))
      .unwrap_or(false)
  }

  /// 生效的用户名（`uri` 中的 user@ 优先，其次 `username` 字段）。
  pub fn effective_username(&self) -> Option<String> {
    if let Ok(endpoint) = muse_native_client::proxy::ProxyConfig::parse_endpoint(&self.uri) {
      if endpoint.username.is_some() {
        return endpoint.username;
      }
    }
    self.username.as_deref().filter(|u| !u.is_empty()).map(str::to_string)
  }

  /// 保存/启用前校验。拒绝：
  /// - `uri` 中带明文密码（口令只允许存 Keychain）；
  /// - scheme / host / port 非法；
  /// - 用户名与 Keychain 引用不成对（否则认证必然失败）。
  pub fn validate(&self) -> Result<(), String> {
    if !self.enabled {
      return Ok(());
    }
    let authority = self
      .uri
      .split_once("://")
      .map(|(_, r)| r.split('/').next().unwrap_or(r))
      .unwrap_or("");
    if let Some((ui, _)) = authority.rsplit_once('@') {
      if ui.contains(':') {
        return Err("代理 URI 不得包含明文密码；密码只允许存入 Keychain".to_string());
      }
    }
    muse_native_client::proxy::ProxyConfig::validate_shape(&self.uri)
      .map_err(|e| e.to_string())?;

    let has_user = self.uri_has_user()
      || self
        .username
        .as_deref()
        .map(|u| !u.is_empty())
        .unwrap_or(false);
    let has_ref = self
      .credential_ref
      .as_deref()
      .map(|r| !r.is_empty())
      .unwrap_or(false);
    if has_user != has_ref {
      return Err("代理用户名与 Keychain 凭据引用必须同时设置（或同时留空）".to_string());
    }
    Ok(())
  }

  /// 脱敏展示串：去掉 userinfo 与 query，绝不含口令。
  pub fn display_safe(&self) -> String {
    let Ok(url) = url::Url::parse(&self.uri) else {
      return "代理配置损坏".to_string();
    };
    let Some(host) = url.host_str() else {
      return "代理配置损坏".to_string();
    };
    match url.port() {
      Some(port) => format!("{host}:{port}"),
      None => host.to_string(),
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QuotaDetail {
  #[serde(default)]
  pub plan_label: String,
  #[serde(default)]
  pub plan_value: String,
  #[serde(default)]
  pub plan_subtext: Option<String>,
  #[serde(default)]
  pub plan_used_percent: Option<u8>,
  #[serde(default)]
  pub extra_label: Option<String>,
  #[serde(default)]
  pub extra_value: Option<String>,
  #[serde(default)]
  pub extra_subtext: Option<String>,
  #[serde(default)]
  pub extra_used_percent: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
  #[default]
  Unknown,
  Authenticated,
  LoggedOut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaObservation {
  pub source: String,
  pub window: Option<String>,
  #[serde(default)]
  pub percent_used: Option<u8>,
  pub resets_at_ms: Option<u64>,
  pub checked_at_ms: u64,
  pub stale: bool,
}

impl QuotaObservation {
  pub fn remaining(&self, now: u64) -> Option<u8> {
    if self.stale || self.checked_at_ms > now || now - self.checked_at_ms > 10 * 60_000
      || self.resets_at_ms.is_some_and(|end| end <= now) { return None; }
    100u8.checked_sub(self.percent_used?)
  }

  pub fn comparable_remaining(&self, now: u64) -> Option<(&str, u8)> {
    let window = self.window.as_deref().map(str::trim).filter(|label| !label.is_empty())?;
    Some((window, self.remaining(now)?))
  }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FingerprintSettings {
  pub manual_model: Option<String>,
  pub interval_days: Option<u8>,
  pub last_automatic_attempt_ms: Option<u64>,
}

impl FingerprintSettings {
  pub fn automatic_due(&self, history: &[FingerprintRecord], now: u64) -> bool {
    let Some(days) = self.interval_days.filter(|days| (1..=90).contains(days)) else { return false; };
    let last = self.last_automatic_attempt_ms.into_iter().chain(history.first().map(|row| row.recorded_at_ms)).max();
    last.is_none_or(|at| now >= at.saturating_add(u64::from(days) * 86_400_000))
  }
}

#[cfg(test)]
#[test]
fn automatic_fingerprint_is_opt_in_and_not_due_again_after_restart() {
  let mut settings = FingerprintSettings::default();
  let now = 1_800_000_000_000;
  assert!(!settings.automatic_due(&[], now));
  settings.interval_days = Some(2);
  assert!(settings.automatic_due(&[], now));
  settings.last_automatic_attempt_ms = Some(now);
  let loaded: FingerprintSettings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
  assert!(!loaded.automatic_due(&[], now + 2 * 86_400_000 - 1));
  assert!(loaded.automatic_due(&[], now + 2 * 86_400_000));
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRecord {
  pub provider_id: ProviderId,
  pub uuid: String,
  pub alias: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub group: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub timezone: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub last_muse_thread: Option<Uuid>,
  pub created_at_ms: u64,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub fingerprint_history: Vec<FingerprintRecord>,
  #[serde(default)]
  pub fingerprint_settings: FingerprintSettings,
  #[serde(default)]
  pub auto_alias: bool,
  #[serde(default)]
  pub avatar_url: Option<String>,
  #[serde(default)]
  pub quota_text: Option<String>,
  #[serde(default)]
  pub quota_percent: Option<u8>,
  #[serde(default)]
  pub quota_detail: Option<QuotaDetail>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub quota_observation: Option<QuotaObservation>,
  #[serde(default)]
  pub auth_state: AuthState,
  /// 0.1.15: the iOS task watcher saw a new reply for this account
  /// while it was not active. Cleared when the account is opened.
  #[serde(default)]
  pub has_unread_reply: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub last_task_reply_at_ms: Option<u64>,
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub muse_unread_counts: BTreeMap<Uuid, u64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub muse_unread_status: Option<String>,
  #[serde(default)]
  pub task_notifications: bool,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub task_notice_keys: Vec<String>,
  #[serde(default)]
  pub pending_approval_count: Option<usize>,
  /// #11/#16：每账号 1:1 代理引用（脱敏存储，fail-closed）。
  #[serde(default)]
  pub proxy: Option<ProxySetting>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintRecord {
  pub task_id: Uuid,
  pub recorded_at_ms: u64,
  pub verdict: String,
  pub model: Option<String>,
  pub family: Option<String>,
  pub probability_ppm: Option<u32>,
  pub valid_samples: usize,
  pub reference_built_at: Option<String>,
  pub reference_sha256: Option<String>,
  pub method: Option<String>,
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictItem {
  pub uuid: String,
  pub providers: Vec<ProviderId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsStoreFile {
  pub version: u32,
  pub accounts: Vec<AccountRecord>,
  pub last_committed: Option<AccountKey>,
  pub pending_cleanup: Vec<AccountKey>,
  #[serde(default)]
  pub approval_attempts: BTreeMap<String, bool>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub restored_proxy_pool: Vec<crate::account_backup::ProxyPoolProfile>,
}

impl Default for AccountsStoreFile {
  fn default() -> Self {
    Self {
      version: STORE_VERSION,
      accounts: Vec::new(),
      last_committed: None,
      pending_cleanup: Vec::new(),
      approval_attempts: BTreeMap::new(),
      restored_proxy_pool: Vec::new(),
    }
  }
}

pub struct LoadedAccountsStore {
  pub store: AccountsStoreFile,
  pub conflicts: Vec<ConflictItem>,
  pub startup_notice: Option<String>,
}

pub fn now_ms() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_millis() as u64)
    .unwrap_or(0)
}

pub fn normalize_uuid(raw: &str) -> Option<String> {
  Uuid::parse_str(raw.trim()).ok().map(|u| u.hyphenated().to_string())
}

pub fn detect_conflicts(accounts: &[AccountRecord]) -> Vec<ConflictItem> {
  let mut by_uuid: BTreeMap<String, Vec<ProviderId>> = BTreeMap::new();
  for acc in accounts {
    let entry = by_uuid.entry(acc.uuid.clone()).or_default();
    if !entry.contains(&acc.provider_id) {
      entry.push(acc.provider_id);
    }
  }
  let mut count_by_uuid: BTreeMap<String, usize> = BTreeMap::new();
  for acc in accounts {
    *count_by_uuid.entry(acc.uuid.clone()).or_insert(0) += 1;
  }
  let mut conflicts = Vec::new();
  for (uuid, providers) in by_uuid {
    let total = count_by_uuid.get(&uuid).copied().unwrap_or(0);
    if providers.len() > 1 || total > 1 {
      conflicts.push(ConflictItem { uuid, providers });
    }
  }
  conflicts
}

pub fn normalize_account_timezone(value: &str) -> Result<Option<String>, String> {
  let value = value.trim();
  if value.is_empty() { return Ok(None); }
  if value.len() > 96 || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"/_+-".contains(&byte))
    || (value != "UTC" && value != "GMT" && (!value.contains('/') || value.split('/').any(str::is_empty))) {
    return Err("请输入有效的 IANA 时区，例如 Asia/Shanghai".into());
  }
  Ok(Some(value.to_string()))
}

pub fn normalize_account_group(value: &str) -> Result<Option<String>, String> {
  let value = value.trim();
  if value.chars().count() > 32 || value.chars().any(char::is_control) {
    return Err("分组名称最多 32 个字符，不能包含控制字符".to_string());
  }
  Ok((!value.is_empty()).then(|| value.to_string()))
}

fn parse_key_or_legacy_muse(val: &Value) -> Option<AccountKey> {
  if let Some(raw_uuid) = val.as_str() {
    return normalize_uuid(raw_uuid).map(|uuid| AccountKey {
      provider_id: ProviderId::Muse,
      uuid,
    });
  }
  let obj = val.as_object()?;
  let raw_uuid = obj
    .get("uuid")
    .or_else(|| obj.get("accountUuid"))
    .or_else(|| obj.get("id"))
    .and_then(|v| v.as_str())?;
  let uuid = normalize_uuid(raw_uuid)?;
  let provider_id = match obj.get("providerId").or_else(|| obj.get("provider_id")) {
    Some(v) => ProviderId::parse(v.as_str()?)?,
    None => ProviderId::Muse,
  };
  Some(AccountKey { provider_id, uuid })
}

fn migrate_from_value(root: Value) -> Result<(AccountsStoreFile, bool), String> {
  let obj = root
    .as_object()
    .ok_or_else(|| "accounts.json 根节点不是对象".to_string())?;

  let file_version = obj
    .get("version")
    .and_then(|v| v.as_u64())
    .map(|v| v as u32)
    .unwrap_or(1);

  let is_legacy = file_version < STORE_VERSION;
  let mut accounts = Vec::new();

  if let Some(arr) = obj.get("accounts").and_then(|v| v.as_array()) {
    for item in arr {
      let item_obj = item
        .as_object()
        .ok_or_else(|| "账号条目格式错误".to_string())?;
      let raw_uuid = item_obj
        .get("uuid")
        .or_else(|| item_obj.get("id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "账号缺少 UUID".to_string())?;
      let uuid = normalize_uuid(raw_uuid).ok_or_else(|| "账号 UUID 非法".to_string())?;
      let provider_id = match item_obj.get("providerId").or_else(|| item_obj.get("provider_id")) {
        Some(v) => {
          let s = v.as_str().ok_or_else(|| "providerId 非法".to_string())?;
          ProviderId::parse(s).ok_or_else(|| format!("未知服务标识: {s}"))?
        }
        None if is_legacy => ProviderId::Muse,
        None => return Err("新版配置账号缺少 providerId".to_string()),
      };
      let alias = item_obj
        .get("alias")
        .or_else(|| item_obj.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("账号 {}", &uuid[..8]));
      let group = item_obj.get("group").and_then(Value::as_str)
        .and_then(|value| normalize_account_group(value).ok()).flatten();
      let created_at_ms = item_obj
        .get("createdAtMs")
        .or_else(|| item_obj.get("created_at_ms"))
        .and_then(|v| v.as_u64())
        .unwrap_or_else(now_ms);

      let auto_alias = item_obj
        .get("autoAlias")
        .or_else(|| item_obj.get("auto_alias"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
      let avatar_url = item_obj
        .get("avatarUrl")
        .or_else(|| item_obj.get("avatar_url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
      let auth_state = item_obj
        .get("authState")
        .or_else(|| item_obj.get("auth_state"))
        .and_then(|v| serde_json::from_value::<AuthState>(v.clone()).ok())
        .unwrap_or_default();
      let mut quota_detail = item_obj
        .get("quotaDetail")
        .or_else(|| item_obj.get("quota_detail"))
        .and_then(|v| serde_json::from_value::<QuotaDetail>(v.clone()).ok())
        .filter(|d| !d.plan_label.trim().is_empty() || !d.plan_value.trim().is_empty());
      let mut quota_text = item_obj
        .get("quotaText")
        .or_else(|| item_obj.get("quota_text"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
      let mut quota_percent = item_obj
        .get("quotaPercent")
        .or_else(|| item_obj.get("quota_percent"))
        .and_then(|v| v.as_u64())
        .map(|n| n.min(100) as u8);
      if quota_detail.is_none() && quota_text.as_deref() == Some("剩余额度 100%") {
        quota_text = None;
        quota_percent = None;
      }
      if auth_state == AuthState::LoggedOut {
        quota_detail = None;
        quota_text = None;
        quota_percent = None;
      }

      let proxy = item_obj.get("proxy").filter(|value| !value.is_null()).map(|value| {
        // An existing but unreadable proxy must remain blocked. A missing
        // field or explicit null still represents legacy/system networking.
        if value.get("enabled").and_then(Value::as_bool).is_some() {
          if let Ok(setting) = serde_json::from_value::<ProxySetting>(value.clone()) {
            return setting;
          }
        }
        ProxySetting {
          uri: String::new(),
          username: None,
          credential_ref: None,
          enabled: true,
        }
      });

      accounts.push(AccountRecord {
        provider_id,
        uuid,
        alias,
        group,
        last_muse_thread: item_obj.get("lastMuseThread").and_then(Value::as_str).and_then(|value| Uuid::parse_str(value).ok()),
        timezone: item_obj.get("timezone").and_then(Value::as_str).and_then(|value| normalize_account_timezone(value).ok()).flatten(),
        created_at_ms,
        fingerprint_history: item_obj.get("fingerprintHistory")
          .and_then(|value| serde_json::from_value::<Vec<FingerprintRecord>>(value.clone()).ok())
          .unwrap_or_default().into_iter().take(12).collect(),
        fingerprint_settings: item_obj.get("fingerprintSettings")
          .and_then(|value| serde_json::from_value(value.clone()).ok()).unwrap_or_default(),
        auto_alias,
        avatar_url,
        quota_text,
        quota_percent,
        quota_detail,
        quota_observation: item_obj.get("quotaObservation")
          .and_then(|value| serde_json::from_value(value.clone()).ok())
          .filter(|_: &QuotaObservation| auth_state != AuthState::LoggedOut),
        auth_state,
        has_unread_reply: false,
        last_task_reply_at_ms: item_obj.get("lastTaskReplyAtMs").and_then(Value::as_u64),
        muse_unread_counts: item_obj.get("museUnreadCounts").and_then(|value| serde_json::from_value(value.clone()).ok()).unwrap_or_default(),
        muse_unread_status: item_obj.get("museUnreadStatus").and_then(Value::as_str).map(str::to_string),
        task_notifications: item_obj.get("taskNotifications").and_then(Value::as_bool).unwrap_or(false),
        task_notice_keys: item_obj.get("taskNoticeKeys")
          .and_then(|value| serde_json::from_value(value.clone()).ok()).unwrap_or_default(),
        pending_approval_count: None,
        proxy,
      });
    }
  }

  let last_committed = obj
    .get("lastCommitted")
    .or_else(|| obj.get("last_committed"))
    .or_else(|| obj.get("lastOpened"))
    .or_else(|| obj.get("last_opened"))
    .filter(|v| !v.is_null())
    .and_then(parse_key_or_legacy_muse);

  let mut pending_cleanup = Vec::new();
  if let Some(arr) = obj
    .get("pendingCleanup")
    .or_else(|| obj.get("pending_cleanup"))
    .and_then(|v| v.as_array())
  {
    for v in arr {
      if let Some(key) = parse_key_or_legacy_muse(v) {
        if !pending_cleanup.contains(&key) {
          pending_cleanup.push(key);
        }
      }
    }
  }

  let approval_attempts = match obj.get("approvalAttempts") {
    None => BTreeMap::new(),
    Some(value) => serde_json::from_value(value.clone()).map_err(|_| "审批台账损坏".to_string())?,
  };

  Ok((
    AccountsStoreFile {
      version: STORE_VERSION,
      accounts,
      last_committed,
      pending_cleanup,
      approval_attempts,
      restored_proxy_pool: obj.get("restoredProxyPool").and_then(|value| serde_json::from_value(value.clone()).ok()).unwrap_or_default(),
    },
    is_legacy,
  ))
}

pub fn save_store_atomic(path: &Path, store: &AccountsStoreFile) -> Result<(), String> {
  save_json_atomic(path, store)
}

pub(crate) fn save_json_atomic(path: &Path, document: &impl Serialize) -> Result<(), String> {
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {e}"))?;
  }
  let bytes = serde_json::to_vec_pretty(document).map_err(|e| format!("序列化配置失败: {e}"))?;
  let tmp_name = format!(".maam.tmp.{}", Uuid::new_v4().simple());
  let tmp_path = path.with_file_name(tmp_name);

  let mut options = OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
  }
  let mut file = options.open(&tmp_path)
    .map_err(|e| format!("创建临时配置文件失败: {e}"))?;
  let result = file.write_all(&bytes)
    .and_then(|_| file.sync_all())
    .map_err(|e| format!("写入临时配置文件失败: {e}"));
  drop(file);
  let result = result.and_then(|_| fs::rename(&tmp_path, path)
    .map_err(|e| format!("原子替换本地资料失败: {e}")));
  if result.is_err() {
    let _ = fs::remove_file(&tmp_path);
  }
  result
}

pub fn load_or_migrate_store(path: &Path) -> LoadedAccountsStore {
  if !path.exists() {
    return LoadedAccountsStore {
      store: AccountsStoreFile::default(),
      conflicts: Vec::new(),
      startup_notice: None,
    };
  }

  let raw = match fs::read_to_string(path) {
    Ok(content) => content,
    Err(err) => {
      return LoadedAccountsStore {
        store: AccountsStoreFile::default(),
        conflicts: Vec::new(),
        startup_notice: Some(format!("读取 accounts.json 失败，已回退管理页: {err}")),
      };
    }
  };

  let parsed: Value = match serde_json::from_str(&raw) {
    Ok(v) => v,
    Err(_) => {
      return LoadedAccountsStore {
        store: AccountsStoreFile::default(),
        conflicts: Vec::new(),
        startup_notice: Some("本地 accounts.json 配置损坏，已回退管理页".to_string()),
      };
    }
  };

  let (mut store, migrated) = match migrate_from_value(parsed) {
    Ok(res) => res,
    Err(reason) => {
      return LoadedAccountsStore {
        store: AccountsStoreFile::default(),
        conflicts: Vec::new(),
        startup_notice: Some(format!("解析 accounts.json 失败（{reason}），已回退管理页")),
      };
    }
  };

  let conflicts = detect_conflicts(&store.accounts);
  if !conflicts.is_empty() {
    store.last_committed = None;
    return LoadedAccountsStore {
      store,
      conflicts,
      startup_notice: Some("检测到账号 UUID 归属冲突，已停留在管理页等待处理".to_string()),
    };
  }

  if migrated {
    if let Err(err) = save_store_atomic(path, &store) {
      return LoadedAccountsStore {
        store,
        conflicts: Vec::new(),
        startup_notice: Some(format!("旧版 Muse 配置原子迁移写盘失败: {err}")),
      };
    }
  }

  LoadedAccountsStore {
    store,
    conflicts: Vec::new(),
    startup_notice: None,
  }
}

#[cfg(test)]
mod proxy_tests {
  use super::*;

  #[cfg(unix)]
  #[test]
  fn account_store_is_created_with_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(Uuid::new_v4().to_string());
    let path = dir.join("accounts.json");
    let store = AccountsStoreFile::default();
    save_store_atomic(&path, &store).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    fs::remove_dir_all(dir).unwrap();
  }

  #[test]
  fn proxy_display_safe_hides_userinfo() {
    let p = ProxySetting {
      uri: "http://alice:p%40ss@p.example:8080".to_string(),
      username: None,
      credential_ref: Some("kc-proxy-p.example".to_string()),
      enabled: true,
    };
    let safe = p.display_safe();
    assert!(!safe.contains("alice"), "脱敏串不得含用户名: {safe}");
    assert!(!safe.contains("p%40ss"), "脱敏串不得含口令: {safe}");
    assert!(safe.contains("p.example:8080"), "脱敏串应保留 host:port: {safe}");
  }

  #[test]
  fn proxy_is_always_fail_closed() {
    let p = ProxySetting {
      uri: "socks5://p.example".to_string(),
      username: None,
      credential_ref: None,
      enabled: true,
    };
    assert!(p.fail_closed());
  }

  #[test]
  fn validate_rejects_inline_password() {
    let p = ProxySetting {
      uri: "http://alice:secret@p.example:8080".to_string(),
      username: None,
      credential_ref: None,
      enabled: true,
    };
    let err = p.validate().expect_err("明文密码必须被拒绝");
    assert!(err.contains("明文密码"), "{err}");
  }

  #[test]
  fn validate_requires_username_and_credential_pair() {
    let orphan_user = ProxySetting {
      uri: "http://alice@p.example:8080".to_string(),
      username: None,
      credential_ref: None,
      enabled: true,
    };
    assert!(orphan_user.validate().is_err(), "有用户名却无 Keychain 引用应被拒");

    let paired = ProxySetting {
      uri: "http://alice@p.example:8080".to_string(),
      username: None,
      credential_ref: Some("proxy-account-1".to_string()),
      enabled: true,
    };
    assert!(paired.validate().is_ok(), "{:?}", paired.validate());

    let no_auth = ProxySetting {
      uri: "socks5://p.example:1080".to_string(),
      username: None,
      credential_ref: None,
      enabled: true,
    };
    assert!(no_auth.validate().is_ok());
  }

  #[test]
  fn validate_rejects_bad_host_and_port() {
    for uri in ["p.example", "ftp://p.example", "http://:8080", "http://p.example:0"] {
      let p = ProxySetting {
        uri: uri.to_string(),
        username: None,
        credential_ref: None,
        enabled: true,
      };
      assert!(p.validate().is_err(), "{uri} 应被拒绝");
    }
  }

  #[test]
  fn disabled_setting_skips_validation() {
    let p = ProxySetting {
      uri: "not-a-uri".to_string(),
      username: None,
      credential_ref: None,
      enabled: false,
    };
    assert!(p.validate().is_ok(), "未启用的配置不参与校验");
  }

  #[test]
  fn effective_username_prefers_uri_user() {
    let p = ProxySetting {
      uri: "http://alice%40corp@p.example:8080".to_string(),
      username: Some("bob".to_string()),
      credential_ref: Some("ref".to_string()),
      enabled: true,
    };
    assert_eq!(p.effective_username().as_deref(), Some("alice@corp"));
  }

  #[test]
  fn migration_retains_proxy_settings_and_blocks_damaged_proxy_entries() {
    let account = |proxy: Option<Value>| {
      let mut account = serde_json::json!({
        "providerId": "muse", "uuid": Uuid::new_v4().to_string(), "alias": "fixture",
      });
      if let Some(proxy) = proxy {
        account["proxy"] = proxy;
      }
      account
    };
    let valid = serde_json::json!({
      "uri": "socks5://proxy.example:1080", "username": "user",
      "credentialRef": "proxy-credential-reference", "enabled": true,
    });
    let (store, _) = migrate_from_value(serde_json::json!({
      "version": STORE_VERSION, "accounts": [
        account(Some(valid.clone())),
        account(Some(serde_json::json!({"enabled": true}))),
        account(Some(serde_json::json!({"uri": "socks5://proxy.example:1080"}))),
        account(Some(Value::Null)),
        account(None),
      ],
    })).unwrap();
    assert_eq!(serde_json::to_value(&store.accounts[0].proxy).unwrap(), valid);
    let secrets = crate::secrets::Secrets::new(&std::env::temp_dir());
    for record in &store.accounts[1..3] {
      let setting = record.proxy.as_ref().unwrap();
      assert!(setting.enabled);
      assert!(crate::egress::AccountEgress::resolve(Some(setting), &secrets).is_err());
    }
    assert!(store.accounts[3..].iter().all(|record| record.proxy.is_none()));
  }

  #[test]
  fn account_groups_round_trip_and_legacy_accounts_stay_ungrouped() {
    let account = |provider, name| serde_json::json!({
      "providerId": provider, "uuid": Uuid::new_v4().to_string(), "alias": name,
    });
    let mut muse = account("muse", "test-muse");
    muse["group"] = serde_json::json!("  工作  ");
    let hark = account("hark", "test-hark");
    let mut malformed = account("hark", "legacy");
    malformed["group"] = serde_json::json!({"name": "invalid"});
    let (store, _) = migrate_from_value(serde_json::json!({
      "version": STORE_VERSION, "accounts": [muse, hark, malformed],
    })).unwrap();
    let (reloaded, _) = migrate_from_value(serde_json::to_value(store).unwrap()).unwrap();
    assert_eq!(reloaded.accounts[0].group.as_deref(), Some("工作"));
    assert!(reloaded.accounts[1..].iter().all(|account| account.group.is_none()));
    assert_eq!(normalize_account_group("  ").unwrap(), None);
    assert!(normalize_account_group("a\nb").is_err());
    assert!(normalize_account_group(&"长".repeat(33)).is_err());
  }

  #[test]
  fn account_record_without_proxy_field_deserializes() {
    // 老账户 JSON 无 proxy 字段，必须能反序列化为 None（向后兼容）。
    let json = r#"{
      "providerId":"muse","uuid":"u-1","alias":"a","createdAtMs":1,
      "autoAlias":false,"avatarUrl":null,"quotaText":null,"quotaPercent":null,
      "quotaDetail":null,"authState":"unknown","hasUnreadReply":false
    }"#;
    let rec: AccountRecord = serde_json::from_str(json).expect("老账户应能反序列化");
    assert!(rec.proxy.is_none());
  }

  #[test]
  fn proxy_legacy_json_without_username_field_deserializes() {
    let json = r#"{"uri":"socks5://p.example:1080","credentialRef":null,"enabled":false}"#;
    let p: ProxySetting = serde_json::from_str(json).expect("老代理配置应能反序列化");
    assert!(p.username.is_none());
  }
}
