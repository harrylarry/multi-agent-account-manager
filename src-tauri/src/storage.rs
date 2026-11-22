use crate::providers::ProviderId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File};
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
pub struct AccountRecord {
  pub provider_id: ProviderId,
  pub uuid: String,
  pub alias: String,
  pub created_at_ms: u64,
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
  #[serde(default)]
  pub auth_state: AuthState,
  /// 0.1.15: the iOS task watcher saw a new reply for this account
  /// while it was not active. Cleared when the account is opened.
  #[serde(default)]
  pub has_unread_reply: bool,
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
}

impl Default for AccountsStoreFile {
  fn default() -> Self {
    Self {
      version: STORE_VERSION,
      accounts: Vec::new(),
      last_committed: None,
      pending_cleanup: Vec::new(),
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

      accounts.push(AccountRecord {
        provider_id,
        uuid,
        alias,
        created_at_ms,
        auto_alias,
        avatar_url,
        quota_text,
        quota_percent,
        quota_detail,
        auth_state,
        has_unread_reply: false,
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

  Ok((
    AccountsStoreFile {
      version: STORE_VERSION,
      accounts,
      last_committed,
      pending_cleanup,
    },
    is_legacy,
  ))
}

pub fn save_store_atomic(path: &Path, store: &AccountsStoreFile) -> Result<(), String> {
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {e}"))?;
  }
  let bytes = serde_json::to_vec_pretty(store).map_err(|e| format!("序列化配置失败: {e}"))?;
  let tmp_name = format!(
    ".accounts.json.tmp.{}",
    Uuid::new_v4().simple()
  );
  let tmp_path = path.with_file_name(tmp_name);

  {
    let mut file = File::create(&tmp_path).map_err(|e| format!("创建临时配置文件失败: {e}"))?;
    file
      .write_all(&bytes)
      .map_err(|e| format!("写入临时配置文件失败: {e}"))?;
    file
      .sync_all()
      .map_err(|e| format!("同步临时配置文件失败: {e}"))?;
  }

  fs::rename(&tmp_path, path).map_err(|e| {
    let _ = fs::remove_file(&tmp_path);
    format!("原子替换 accounts.json 失败: {e}")
  })
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
