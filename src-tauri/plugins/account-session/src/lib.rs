#[cfg(target_os = "android")]
use serde::{Deserialize, Serialize};
#[cfg(target_os = "android")]
use tauri::{
  ipc::Channel,
  plugin::{Builder, PluginHandle, TauriPlugin},
  AppHandle, Manager, Runtime,
};

#[cfg(target_os = "android")]
pub struct AndroidSessionBridge<R: Runtime>(pub PluginHandle<R>);

#[cfg(target_os = "android")]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAndroidAccountPayload {
  pub provider_id: String,
  pub provider_name: String,
  pub uuid: String,
  pub alias: String,
  pub auto_alias: bool,
  pub entry_url: String,
  pub allowed_hosts: Vec<String>,
  pub switch_seq: u64,
  pub view_instance_id: u64,
  pub is_cold_start: bool,
}

#[cfg(target_os = "android")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidCapabilitiesResponse {
  pub multi_profile_supported: bool,
  pub profile_names: Vec<String>,
}

#[cfg(target_os = "android")]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AndroidSessionEvent {
  #[serde(rename_all = "camelCase")]
  PageCommitVisible {
    provider_id: String,
    uuid: String,
    switch_seq: u64,
    view_instance_id: u64,
    url: String,
  },
  #[serde(rename_all = "camelCase")]
  IdentityDetected {
    provider_id: String,
    uuid: String,
    switch_seq: u64,
    view_instance_id: u64,
    identity: String,
    #[serde(default)]
    avatar_url: Option<String>,
    #[serde(default)]
    quota_text: Option<String>,
    #[serde(default)]
    quota_percent: Option<u8>,
    #[serde(default)]
    quota_detail_json: Option<String>,
  },
  #[serde(rename_all = "camelCase")]
  SessionFailed {
    switch_seq: u64,
    view_instance_id: u64,
    reason: String,
  },
  #[serde(rename_all = "camelCase")]
  ActivityClosed {
    switch_seq: u64,
    view_instance_id: u64,
  },
  #[serde(rename_all = "camelCase")]
  ExternalAuthUnavailable {
    switch_seq: u64,
    view_instance_id: u64,
    reason: String,
  },
  #[serde(rename_all = "camelCase")]
  RecreateRequested {
    previous_switch_seq: u64,
  },
  #[serde(rename_all = "camelCase")]
  UpdateProgress {
    stage: String,
    downloaded: u64,
    #[serde(default)]
    total: Option<u64>,
    #[serde(default)]
    percent: Option<u8>,
    message: String,
  },
  #[serde(rename_all = "camelCase")]
  SessionLoggedOut {
    provider_id: String,
    uuid: String,
    switch_seq: u64,
    view_instance_id: u64,
  },
}

#[cfg(target_os = "android")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidFetchTextResponse {
  pub status: u16,
  pub body: String,
}

#[cfg(target_os = "android")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidInstallApkResponse {
  pub status: String,
  pub message: String,
}

#[cfg(target_os = "android")]
pub fn init<R: Runtime>() -> TauriPlugin<R> {
  Builder::new("account-session")
    .setup(|app, api| {
      let handle =
        api.register_android_plugin("com.ldjx.accountmanager", "AccountSessionPlugin")?;
      app.manage(AndroidSessionBridge(handle));
      Ok(())
    })
    .build()
}

#[cfg(target_os = "android")]
pub async fn subscribe_events(
  app: &AppHandle,
  channel: Channel<serde_json::Value>,
) -> Result<(), String> {
  let bridge = app.state::<AndroidSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async::<serde_json::Value>(
      "subscribeEvents",
      serde_json::json!({ "channel": channel }),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
pub async fn check_capabilities(app: &AppHandle) -> Result<AndroidCapabilitiesResponse, String> {
  let bridge = app.state::<AndroidSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async("checkCapabilities", serde_json::json!({}))
    .await
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
pub async fn open_account_session(
  app: &AppHandle,
  payload: OpenAndroidAccountPayload,
) -> Result<(), String> {
  let bridge = app.state::<AndroidSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async::<serde_json::Value>("openAccountSession", payload)
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
pub async fn update_account_title(
  app: &AppHandle,
  switch_seq: u64,
  title: &str,
) -> Result<(), String> {
  let bridge = app.state::<AndroidSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async::<serde_json::Value>(
      "updateAccountTitle",
      serde_json::json!({ "switchSeq": switch_seq, "title": title }),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
pub async fn close_active_session(app: &AppHandle, switch_seq: u64) -> Result<(), String> {
  let bridge = app.state::<AndroidSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async::<serde_json::Value>(
      "closeActiveSession",
      serde_json::json!({ "switchSeq": switch_seq }),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
pub async fn delete_profile(app: &AppHandle, uuid: &str) -> Result<(), String> {
  let bridge = app.state::<AndroidSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async::<serde_json::Value>(
      "deleteProfile",
      serde_json::json!({ "uuid": uuid }),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
pub async fn open_external_url(app: &AppHandle, url: &str) -> Result<(), String> {
  let bridge = app.state::<AndroidSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async::<serde_json::Value>(
      "openExternalUrl",
      serde_json::json!({ "url": url }),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
pub async fn fetch_text(
  app: &AppHandle,
  url: &str,
  user_agent: &str,
  accept: Option<&str>,
) -> Result<AndroidFetchTextResponse, String> {
  let bridge = app.state::<AndroidSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async(
      "fetchText",
      serde_json::json!({
        "url": url,
        "userAgent": user_agent,
        "accept": accept,
      }),
    )
    .await
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
pub async fn download_and_install_apk(
  app: &AppHandle,
  url: &str,
  sha256: Option<&str>,
  filename: &str,
  user_agent: &str,
) -> Result<AndroidInstallApkResponse, String> {
  let bridge = app.state::<AndroidSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async(
      "downloadAndInstallApk",
      serde_json::json!({
        "url": url,
        "sha256": sha256,
        "filename": filename,
        "userAgent": user_agent,
      }),
    )
    .await
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "ios")]
use serde::{Deserialize, Serialize};
#[cfg(target_os = "ios")]
use tauri::{
  ipc::Channel,
  plugin::{Builder, PluginHandle, TauriPlugin},
  AppHandle, Manager, Runtime,
};

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_account_session);

#[cfg(target_os = "ios")]
pub struct IosSessionBridge<R: Runtime>(pub PluginHandle<R>);

#[cfg(target_os = "ios")]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenIosAccountPayload {
  pub provider_id: String,
  pub provider_name: String,
  pub uuid: String,
  pub alias: String,
  pub auto_alias: bool,
  pub entry_url: String,
  pub allowed_hosts: Vec<String>,
  pub switch_seq: u64,
  pub view_instance_id: u64,
  pub is_cold_start: bool,
}

#[cfg(target_os = "ios")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IosCapabilitiesResponse {
  pub multi_profile_supported: bool,
  pub profile_names: Vec<String>,
}

#[cfg(target_os = "ios")]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum IosSessionEvent {
  #[serde(rename_all = "camelCase")]
  PageCommitVisible {
    provider_id: String,
    uuid: String,
    switch_seq: u64,
    view_instance_id: u64,
    url: String,
  },
  #[serde(rename_all = "camelCase")]
  IdentityDetected {
    provider_id: String,
    uuid: String,
    switch_seq: u64,
    view_instance_id: u64,
    identity: String,
    #[serde(default)]
    avatar_url: Option<String>,
    #[serde(default)]
    quota_text: Option<String>,
    #[serde(default)]
    quota_percent: Option<u8>,
    #[serde(default)]
    quota_detail_json: Option<String>,
  },
  #[serde(rename_all = "camelCase")]
  SessionFailed {
    switch_seq: u64,
    view_instance_id: u64,
    reason: String,
  },
  #[serde(rename_all = "camelCase")]
  ActivityClosed {
    switch_seq: u64,
    view_instance_id: u64,
  },
  #[serde(rename_all = "camelCase")]
  ExternalAuthUnavailable {
    switch_seq: u64,
    view_instance_id: u64,
    reason: String,
  },
  #[serde(rename_all = "camelCase")]
  RecreateRequested {
    previous_switch_seq: u64,
  },
  #[serde(rename_all = "camelCase")]
  UpdateProgress {
    stage: String,
    downloaded: u64,
    #[serde(default)]
    total: Option<u64>,
    #[serde(default)]
    percent: Option<u8>,
    message: String,
  },
  #[serde(rename_all = "camelCase")]
  SessionLoggedOut {
    provider_id: String,
    uuid: String,
    switch_seq: u64,
    view_instance_id: u64,
  },
  /// 0.1.15: the iOS task watcher detected a new assistant reply
  /// (working → idle) for an account, active or parked.
  #[serde(rename_all = "camelCase")]
  TaskReplyDetected {
    provider_id: String,
    uuid: String,
  },
  /// 0.1.15: the user tapped a task-completion notification.
  #[serde(rename_all = "camelCase")]
  NotificationTapped {
    provider_id: String,
    uuid: String,
  },
}

#[cfg(target_os = "ios")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IosFetchTextResponse {
  pub status: u16,
  pub body: String,
}

#[cfg(target_os = "ios")]
pub fn init<R: Runtime>() -> TauriPlugin<R> {
  Builder::new("account-session")
    .setup(|app, api| {
      let handle = api.register_ios_plugin(init_plugin_account_session)?;
      app.manage(IosSessionBridge(handle));
      Ok(())
    })
    .build()
}

#[cfg(target_os = "ios")]
pub async fn subscribe_events(
  app: &AppHandle,
  channel: Channel<serde_json::Value>,
) -> Result<(), String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async::<serde_json::Value>(
      "subscribeEvents",
      serde_json::json!({ "channel": channel }),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "ios")]
pub async fn check_capabilities(app: &AppHandle) -> Result<IosCapabilitiesResponse, String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async("checkCapabilities", serde_json::json!({}))
    .await
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "ios")]
pub async fn open_account_session(
  app: &AppHandle,
  payload: OpenIosAccountPayload,
) -> Result<(), String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async::<serde_json::Value>("openAccountSession", payload)
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "ios")]
pub async fn update_account_title(
  app: &AppHandle,
  switch_seq: u64,
  title: &str,
) -> Result<(), String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async::<serde_json::Value>(
      "updateAccountTitle",
      serde_json::json!({ "switchSeq": switch_seq, "title": title }),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "ios")]
pub async fn close_active_session(app: &AppHandle, switch_seq: u64) -> Result<(), String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async::<serde_json::Value>(
      "closeActiveSession",
      serde_json::json!({ "switchSeq": switch_seq }),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "ios")]
pub async fn delete_profile(app: &AppHandle, uuid: &str) -> Result<(), String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async::<serde_json::Value>(
      "deleteProfile",
      serde_json::json!({ "uuid": uuid }),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "ios")]
pub async fn open_external_url(app: &AppHandle, url: &str) -> Result<(), String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async::<serde_json::Value>(
      "openExternalUrl",
      serde_json::json!({ "url": url }),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "ios")]
pub async fn fetch_text(
  app: &AppHandle,
  url: &str,
  user_agent: &str,
  accept: Option<&str>,
) -> Result<IosFetchTextResponse, String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async(
      "fetchText",
      serde_json::json!({
        "url": url,
        "userAgent": user_agent,
        "accept": accept,
      }),
    )
    .await
    .map_err(|e| e.to_string())
}

// Batch-1: background quota sync (iOS). Rust passes the accounts to sync
// (already filtered: enabled, not the active one); Swift probes each
// account's parked page or a hidden one-shot webview and returns the
// per-account results for apply_synced_metadata.
#[cfg(target_os = "ios")]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncQuotaTarget {
  pub provider_id: String,
  pub uuid: String,
  pub entry_url: String,
}

#[cfg(target_os = "ios")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncQuotaResult {
  pub provider_id: String,
  pub uuid: String,
  #[serde(default)]
  pub identity: String,
  #[serde(default)]
  pub avatar_url: Option<String>,
  #[serde(default)]
  pub quota_text: Option<String>,
  #[serde(default)]
  pub quota_percent: Option<u8>,
  #[serde(default)]
  pub quota_detail_json: Option<String>,
  #[serde(default)]
  pub logged_out: bool,
}

#[cfg(target_os = "ios")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncQuotasResponse {
  #[serde(default)]
  pub results: Vec<SyncQuotaResult>,
}

#[cfg(target_os = "ios")]
pub async fn sync_account_quotas(
  app: &AppHandle,
  accounts: Vec<SyncQuotaTarget>,
) -> Result<Vec<SyncQuotaResult>, String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  let res: SyncQuotasResponse = bridge
    .0
    .run_mobile_plugin_async(
      "syncAccountQuotas",
      serde_json::json!({ "accounts": accounts }),
    )
    .await
    .map_err(|e| e.to_string())?;
  Ok(res.results)
}
