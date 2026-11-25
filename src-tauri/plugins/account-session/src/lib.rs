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
  /// (working → idle) for a parked account (0.1.16: the active
  /// account's completions are Muse web's job and never reach here).
  #[serde(rename_all = "camelCase")]
  TaskReplyDetected {
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
pub struct FingerprintForeground {
  app: AppHandle,
  task_id: String,
}

#[cfg(target_os = "ios")]
impl Drop for FingerprintForeground {
  fn drop(&mut self) {
    let app = self.app.clone();
    let task_id = self.task_id.clone();
    tauri::async_runtime::spawn(async move {
      let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
      let _ = bridge.0.run_mobile_plugin_async::<serde_json::Value>(
        "setFingerprintForeground",
        serde_json::json!({ "taskId": task_id }),
      ).await;
    });
  }
}

#[cfg(target_os = "ios")]
pub async fn keep_fingerprint_foreground(
  app: &AppHandle,
  task_id: String,
  interrupted: Channel<serde_json::Value>,
) -> Result<FingerprintForeground, String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  bridge.0.run_mobile_plugin_async::<serde_json::Value>(
    "setFingerprintForeground",
    serde_json::json!({ "taskId": task_id, "interrupted": interrupted }),
  ).await.map_err(|error| error.to_string())?;
  Ok(FingerprintForeground { app: app.clone(), task_id })
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

/// Spike 0.1.22 (#2 Cookie 桥)：从指定账号的 WKWebsiteDataStore 取
/// muse.ai 会话 cookie（Swift 侧已按域过滤；值只在内存里传递，Rust 侧
/// 用后 zeroize，永不落盘、永不进日志）。
#[cfg(target_os = "ios")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IosSessionCookie {
  pub name: String,
  pub value: String,
}

#[cfg(target_os = "ios")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCookiesResponse {
  #[serde(default)]
  pub cookies: Vec<IosSessionCookie>,
}

#[cfg(target_os = "ios")]
pub async fn get_session_cookies(app: &AppHandle, uuid: &str) -> Result<Vec<IosSessionCookie>, String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  let res: SessionCookiesResponse = bridge
    .0
    .run_mobile_plugin_async("getSessionCookies", serde_json::json!({ "uuid": uuid }))
    .await
    .map_err(|e| e.to_string())?;
  Ok(res.cookies)
}

#[cfg(target_os = "ios")]
pub async fn get_hark_api_cookies(app: &AppHandle, uuid: &str) -> Result<Vec<IosSessionCookie>, String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  let res: SessionCookiesResponse = bridge
    .0
    .run_mobile_plugin_async("getSessionCookies", serde_json::json!({ "uuid": uuid, "provider": "hark" }))
    .await
    .map_err(|_| "读取 Hark 登录态失败".to_string())?;
  Ok(res.cookies)
}

/// 0.1.16: pushes the aggregate unread state (any account with an
/// unread reply) to the native session view, which shows/hides the
/// dot on the "← 账号" button.
#[cfg(target_os = "ios")]
pub async fn set_unread_badge(app: &AppHandle, has_unread: bool) -> Result<(), String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async::<serde_json::Value>(
      "setUnreadBadge",
      serde_json::json!({ "hasUnread": has_unread }),
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

/// #11/#16（补 W）：把每账号代理写入 iOS 侧 WKWebView 配置。
///
/// iOS 17+ 只有 `WKWebsiteDataStore.proxyConfigurations` 能影响 WKWebView 的
/// 出站，Rust 侧的 reqwest / 网关隧道都管不到它，所以必须单独下发一次。
/// 口令不走这个接口：Swift 侧按 `proxy-<uuid>` 从同一个 Keychain service
/// 读取（见主 crate `secrets.rs` 的 `SERVICE`）。
#[cfg(target_os = "ios")]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAccountProxyPayload {
  pub uuid: String,
  /// http | https | socks5 | socks5h
  pub scheme: String,
  pub host: String,
  pub port: u16,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub username: Option<String>,
  pub enabled: bool,
}

#[cfg(target_os = "ios")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAccountProxyResponse {
  /// 配置是否已实际应用（iOS < 17 或参数非法时为 false）。
  #[serde(default)]
  pub applied: bool,
  /// 该账号的会话当前正打开：新配置要重新打开会话才生效。
  #[serde(default)]
  pub session_open: bool,
  /// WebView 出口是否处于隔离态（下发失败时保持隔离）。
  #[serde(default)]
  pub quarantined: bool,
  #[serde(default)]
  pub reason: Option<String>,
}

/// #11/#16（补 W）：隔离某账号 WebView 出口的参数。
/// Rust 在保存新代理**之前**调用，用来关掉「保存与下发之间」的时间窗口。
#[cfg(target_os = "ios")]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineProxyPayload {
  pub uuid: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub reason: Option<String>,
}

#[cfg(target_os = "ios")]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineProxyResponse {
  /// 是否已把该 data store 的出口换成不可达端点。
  #[serde(default)]
  pub engaged: bool,
  /// 该账号的会话当前正打开（已被阻断）。
  #[serde(default)]
  pub session_open: bool,
  #[serde(default)]
  pub reason: Option<String>,
}

#[cfg(target_os = "ios")]
pub async fn set_account_proxy(
  app: &AppHandle,
  payload: SetAccountProxyPayload,
) -> Result<SetAccountProxyResponse, String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async("setAccountProxy", payload)
    .await
    .map_err(|e| e.to_string())
}

/// #11/#16（补 W）：在改配置前隔离该账号的 WebView 出口。
#[cfg(target_os = "ios")]
pub async fn quarantine_account_proxy(
  app: &AppHandle,
  payload: QuarantineProxyPayload,
) -> Result<QuarantineProxyResponse, String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  bridge
    .0
    .run_mobile_plugin_async("quarantineAccountProxy", payload)
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
  pub observation: String,
  pub authenticated: bool,
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
  fresh: bool,
) -> Result<Vec<SyncQuotaResult>, String> {
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  let res: SyncQuotasResponse = bridge
    .0
    .run_mobile_plugin_async(
      "syncAccountQuotas",
      serde_json::json!({ "accounts": accounts, "fresh": fresh }),
    )
    .await
    .map_err(|e| e.to_string())?;
  Ok(res.results)
}

#[cfg(target_os = "ios")]
pub async fn transfer_profile_cookies(
  app: &AppHandle,
  provider: &str,
  uuid: &str,
  cookies: Option<&[impl serde::Serialize]>,
) -> Result<usize, String> {
  #[derive(Deserialize)]
  struct ResultCount {
    count: usize,
  }
  let bridge = app.state::<IosSessionBridge<tauri::Wry>>();
  let response: ResultCount = bridge.0.run_mobile_plugin_async(
    "transferProfileCookies",
    serde_json::json!({ "provider": provider, "uuid": uuid, "cookies": cookies }),
  ).await.map_err(|e| e.to_string())?;
  Ok(response.count)
}
