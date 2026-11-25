mod muse_native;
mod platform;
mod hark_native;
mod probe_cancel;
mod webview_proxy;
mod providers;
mod quota;
mod secrets;
mod send_ledger;
mod session_manager;
mod storage;
mod updater;

#[cfg(not(target_os = "android"))]
mod egress;

use session_manager::{ManagerSnapshot, SessionManager, APP_TITLE};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use updater::{InstallUpdateOutcome, InstallUpdateRequest, UpdateCheckResult};

/// #11/#16：取账号的代理设置，交给原生探针做统一出站决策。
async fn account_proxy_setting(
  mgr: &SessionManager,
  provider_id: &str,
  uuid: &str,
) -> Option<storage::ProxySetting> {
  let pid = providers::ProviderId::parse(provider_id)?;
  mgr.proxy_setting(pid, uuid).await
}

#[tauri::command]
async fn get_manager_state(mgr: State<'_, Arc<SessionManager>>) -> Result<ManagerSnapshot, String> {
  Ok(mgr.get_snapshot().await)
}

#[tauri::command]
async fn select_provider(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
) -> Result<ManagerSnapshot, String> {
  mgr.select_provider(&app, &provider_id).await
}

#[tauri::command]
async fn add_account(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
  alias: String,
) -> Result<ManagerSnapshot, String> {
  mgr.add_account(&app, &provider_id, &alias).await
}

#[tauri::command]
async fn rename_account(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
  uuid: String,
  alias: String,
) -> Result<ManagerSnapshot, String> {
  mgr.rename_account(&app, &provider_id, &uuid, &alias).await
}

#[tauri::command]
async fn remove_account(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
  uuid: String,
) -> Result<ManagerSnapshot, String> {
  mgr.remove_account(&app, &provider_id, &uuid).await
}

#[tauri::command]
async fn switch_account(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
  uuid: String,
) -> Result<ManagerSnapshot, String> {
  mgr.switch_account(&app, &provider_id, &uuid).await
}

#[tauri::command]
async fn set_modal_active(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  active: bool,
) -> Result<ManagerSnapshot, String> {
  mgr.set_modal_active(&app, active).await
}

#[tauri::command]
async fn open_official_site(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
) -> Result<ManagerSnapshot, String> {
  mgr.open_official_site(&app, &provider_id).await
}

#[tauri::command]
async fn resolve_uuid_conflict(
  mgr: State<'_, Arc<SessionManager>>,
  keep_provider_id: String,
  uuid: String,
) -> Result<ManagerSnapshot, String> {
  mgr.resolve_uuid_conflict(&keep_provider_id, &uuid).await
}

#[tauri::command]
async fn dismiss_notice(mgr: State<'_, Arc<SessionManager>>) -> Result<ManagerSnapshot, String> {
  Ok(mgr.dismiss_notice().await)
}

#[tauri::command]
async fn trigger_quota_sync(
  app: AppHandle,
  mgr: State<'_, Arc<SessionManager>>,
  force: Option<bool>,
) -> Result<ManagerSnapshot, String> {
  // Batch-1: auto-triggers pass force=false (the 30s throttle applies);
  // the desktop button omits it (force=true, as before).
  mgr.trigger_silent_sync(&app, force.unwrap_or(true)).await;
  Ok(mgr.get_snapshot().await)
}

/// Batch-1: reorder one provider's accounts (long-press + drag in the list).
#[tauri::command]
async fn reorder_accounts(
  mgr: State<'_, Arc<SessionManager>>,
  provider_id: String,
  uuids: Vec<String>,
) -> Result<ManagerSnapshot, String> {
  mgr.reorder_accounts(&provider_id, uuids).await
}

#[tauri::command]
async fn check_for_updates(app: AppHandle) -> Result<UpdateCheckResult, String> {
  updater::check_for_updates(&app).await
}

#[tauri::command]
async fn download_and_install_update(
  app: AppHandle,
  request: InstallUpdateRequest,
) -> Result<InstallUpdateOutcome, String> {
  updater::download_and_install_update(&app, request).await
}

#[tauri::command]
async fn open_external_url(app: AppHandle, url: String) -> Result<(), String> {
  updater::open_external_url(&app, &url).await
}

/// Spike 0.1.22：原生握手诊断（取凭证 → WSS → Noise 握手 → ping）。
/// 只做诊断，不发聊天；结果不含任何密钥。由 Spike 版启动自检或手动调用。
#[tauri::command]
async fn muse_native_diagnose(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  mgr: State<'_, Arc<SessionManager>>,
) -> Result<muse_native::SpikeDiagnose, String> {
  let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
  muse_native::diagnose(&app, &provider_id, &uuid, setting).await
}

/// B1：`chat.subscribe` 探针（Issue #13 #5）。订阅 N 秒，只返回事件计数与去重结果。
#[tauri::command]
async fn muse_native_watch_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  seconds: u64,
  mgr: State<'_, Arc<SessionManager>>,
) -> Result<muse_native::WatchProbe, String> {
  let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
  muse_native::watch_probe(&app, &provider_id, &uuid, seconds, setting.as_ref()).await
}

/// B2：只读拉取 `egress.approvals`。
#[tauri::command]
async fn muse_native_approvals_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  mgr: State<'_, Arc<SessionManager>>,
) -> Result<muse_native::ApprovalsProbe, String> {
  let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
  muse_native::approvals_probe(&app, &provider_id, &uuid, setting.as_ref()).await
}

/// B3：只读拉一次额度（Server Action 冷发现 + 重试），失败时带每步诊断。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaProbe {
  pub ok: bool,
  pub quota: Option<quota::QuotaSnapshot>,
  pub diag: Vec<String>,
  pub error: Option<String>,
}

#[tauri::command]
async fn muse_native_quota_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  cache: tauri::State<'_, std::sync::Arc<quota::ActionCache>>,
  mgr: State<'_, Arc<SessionManager>>,
) -> Result<QuotaProbe, String> {
  if provider_id != "muse" {
    return Err("额度探针只支持 Muse 账号".to_string());
  }
  // 额度请求也属于账号出站流量：启用代理的账号必须经代理，解析失败即失败。
  let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
  let egress = match egress::resolve_for_app(&app, setting.as_ref()) {
    Ok(e) => e,
    Err(e) => {
      return Ok(QuotaProbe {
        ok: false,
        quota: None,
        diag: vec![format!("出站策略不可用（未回退直连）: {e}")],
        error: Some(e),
      })
    }
  };
  let client = match egress.http_client(std::time::Duration::from_secs(15), false) {
    Ok(c) => c,
    Err(e) => {
      return Ok(QuotaProbe {
        ok: false,
        quota: None,
        diag: vec![format!("HTTP 客户端不可用: {e}")],
        error: Some(e.to_string()),
      })
    }
  };
  let cookie = match muse_native::cookie_header_for(&app, &uuid).await {
    Ok(c) => c,
    Err(e) => {
      return Ok(QuotaProbe {
        ok: false,
        quota: None,
        diag: vec![],
        error: Some(e),
      })
    }
  };
  let cached = cache.get();
  let (result, mut diag) = quota::fetch_quota_v2(&client, &cookie, cached.as_deref()).await;
  if egress.is_proxy() {
    diag.insert(0, format!("出站：经代理 {}", egress.display));
  }
  if let Some(c) = cached.as_deref() {
    diag.insert(0, format!("使用缓存 action {}", &c[..8.min(c.len())]));
  }
  match result {
    Ok(q) => {
      cache.set(q.action_id.clone());
      Ok(QuotaProbe {
        ok: true,
        quota: Some(q),
        diag,
        error: None,
      })
    }
    Err(e) => {
      // 会话失效时清缓存，避免继续用过期 id 打上游。
      if matches!(e, quota::QuotaError::SessionExpired) {
        cache.set(None);
      }
      Ok(QuotaProbe {
        ok: false,
        quota: None,
        diag,
        error: Some(e.to_string()),
      })
    }
  }
}

/// #8：VM 租约探针（只读控制面）。
#[tauri::command]
async fn muse_native_lease_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  twice: Option<bool>,
  mgr: State<'_, Arc<SessionManager>>,
) -> Result<muse_native::LeaseProbe, String> {
  if provider_id != "muse" {
    return Err("租约探针只支持 Muse 账号".to_string());
  }
  let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
  muse_native::lease_probe(&app, &uuid, twice.unwrap_or(true), setting.as_ref()).await
}

/// B4：发送一条消息到隔离测试会话（单发锁 + 台账在 muse_native::send_probe）。
#[tauri::command]
async fn muse_native_send_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  text: String,
  node_id: String,
  session_id: Option<String>,
  ledger: tauri::State<'_, std::sync::Arc<send_ledger::SendLedger>>,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<muse_native::SendOutcome, String> {
  if provider_id != "muse" {
    return Err("发送探针只支持 Muse 账号".to_string());
  }
  let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
  let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
  let secrets = secrets::Secrets::new(&dir);
  Ok(muse_native::send_probe(
    &app,
    &ledger,
    &uuid,
    &text,
    &node_id,
    session_id.as_deref(),
    setting.as_ref(),
    &secrets,
    &cancel,
  )
  .await)
}

/// #18：模型指纹探针（隔离会话发 3 条计数探针 + lmfpd 本地评分）。
/// verdict 只有 scored 才是有效识别；partial 只是候选排名；indeterminate 不可判定。
#[tauri::command]
async fn muse_native_fingerprint_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  task_id: String,
  ledger: tauri::State<'_, std::sync::Arc<send_ledger::SendLedger>>,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<muse_native::FingerprintProbe, String> {
  if provider_id != "muse" {
    return Err("指纹探针只支持 Muse 账号".to_string());
  }
  let task_id = uuid::Uuid::parse_str(&task_id).map_err(|_| "指纹任务标识无效".to_string())?;
  let (token, _guard) = cancel.register_task(&uuid, "fingerprint", task_id);
  let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
  let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
  let secrets = secrets::Secrets::new(&dir);
  #[cfg(target_os = "ios")]
  let (foreground, reason) = {
    use std::sync::atomic::{AtomicU8, Ordering};
    let reason = Arc::new(AtomicU8::new(0));
    let event_reason = Arc::clone(&reason);
    let task_account = uuid.clone();
    let cancellations = Arc::clone(cancel.inner());
    let interrupted = tauri::ipc::Channel::new(move |body| {
      if let tauri::ipc::InvokeResponseBody::Json(raw) = body {
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&raw) {
          let cause = match event["reason"].as_str() {
            Some("background") => 1,
            Some("deadline") => 2,
            _ => return Ok(()),
          };
          event_reason.store(cause, Ordering::Release);
          cancellations.cancel_task(&task_account, task_id);
        }
      }
      Ok(())
    });
    let guard = platform::ios::keep_fingerprint_foreground(&app, task_id.to_string(), interrupted)
      .await
      .map_err(|_| "无法保持指纹探针前台运行，请在叠号前台重试".to_string())?;
    (guard, reason)
  };

  let result = muse_native::fingerprint_probe(
    &app,
    &ledger,
    &uuid,
    task_id,
    setting.as_ref(),
    &secrets,
    token,
  ).await;
  #[cfg(target_os = "ios")]
  let result = {
    let mut result = result;
    drop(foreground);
    if result.samples_collected < 3 {
      use std::sync::atomic::Ordering;
      let interruption = match reason.load(Ordering::Acquire) {
        1 => Some("叠号离开前台，已停止本次指纹探针"),
        2 => Some("本次指纹探针达到总时限，已停止"),
        _ => None,
      };
      if let Some(message) = interruption {
        result.error = Some(message.to_string());
      }
    }
    result
  };
  Ok(result)
}

#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn hark_native_watch_probe(
  app: AppHandle,
  uuid: String,
  timezone: String,
  mgr: State<'_, Arc<SessionManager>>,
) -> Result<hark_native::HarkProbe, String> {
  #[cfg(not(target_os = "ios"))]
  {
    Err("Hark 原生诊断需要 iOS 测试包".to_string())
  }
  #[cfg(target_os = "ios")]
  {
    if !muse_native::is_spike_build(app.clone()) {
      return Err("Hark 原生诊断仅在 Spike 包开放".to_string());
    }
    if timezone.is_empty() || timezone.len() > 96
      || !timezone.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_+-".contains(&b))
    {
      return Err("时区格式无效".to_string());
    }
    let account = mgr.get_snapshot().await.accounts.into_iter()
      .find(|account| account.provider_id == providers::ProviderId::Hark && account.uuid == uuid
        && account.auth_state == storage::AuthState::Authenticated)
      .ok_or_else(|| "没有匹配的 Hark 测试账号".to_string())?;
    Ok(hark_native::diagnose(&app, &uuid, account.proxy.as_ref(), &timezone).await)
  }
}

/// #11/#16：用**该账号已保存的代理**验证控制面 + WSS 隧道 + Noise + ping（fail-closed）。
///
/// 不再接受前端传入的 proxy URI：诊断必须与真实发送同源（ProxySetting +
/// Keychain），否则会出现「诊断 OK、发送走另一个出口」的误判，也会让代理口令
/// 经 IPC 进入前端。支持后端取消（取消后立即停止后续阶段）。
#[tauri::command]
async fn muse_native_proxy_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  connectivity_only: Option<bool>,
  task_id: Option<String>,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<muse_native::ProxyProbe, String> {
  if provider_id != "muse" {
    return Err("代理探针只支持 Muse 账号".to_string());
  }
  let (token, _guard) = match task_id {
    Some(task_id) => {
      let id = uuid::Uuid::parse_str(&task_id).map_err(|_| "代理任务标识无效".to_string())?;
      cancel.register_task(&uuid, "proxy", id)
    }
    None => cancel.register(&uuid, "proxy"),
  };
  let setting = account_proxy_setting(&mgr, &provider_id, &uuid).await;
  let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
  let secrets = secrets::Secrets::new(&dir);
  Ok(muse_native::proxy_probe(
    &app,
    &uuid,
    setting.as_ref(),
    &secrets,
    token,
    connectivity_only.unwrap_or(false),
  ).await)
}

/// 平台原生 WebView 侧的代理应用结果。
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WebviewProxyView {
  pub applied: bool,
  pub session_open: bool,
  /// 下发失败时 WebView 出口停在隔离态（不可达代理 + 禁止 failover）。
  pub quarantined: bool,
  pub reason: Option<String>,
}

/// 账号代理配置的脱敏视图（不含口令）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySettingView {
  pub enabled: bool,
  pub display: String,
  pub has_credentials: bool,
  pub credential_available: Option<bool>,
  /// iOS：账号 WebView（WKWebView）侧的代理应用结果；其它平台为 `None`（不适用，
  /// 桌面 WebView 走系统代理）。`None` 不代表“已生效”，只代表“无此层”。
  pub webview: Option<WebviewProxyView>,
}

fn proxy_credential_available(setting: &storage::ProxySetting, secrets: &secrets::Secrets) -> Option<bool> {
  setting.credential_ref.as_deref().map(|reference| {
    matches!(secrets.get(reference), Ok(Some(secret)) if !secret.is_empty())
  })
}

/// #11/#16（补 W）：把账号代理下发给 iOS 侧的 WKWebView。
/// 平台没有这一层时返回 `None`（桌面 / Android），有但未生效时返回
/// `applied = false` + 原因——前端据此提示，而**阻断由 Swift 侧真正执行**。
async fn apply_webview_proxy(
  app: &AppHandle,
  uuid: &str,
  setting: Option<&storage::ProxySetting>,
) -> Option<WebviewProxyView> {
  let outcome = webview_proxy::push(app, uuid, setting).await;
  if !outcome.applicable {
    return None;
  }
  Some(WebviewProxyView {
    applied: outcome.applied,
    session_open: outcome.session_open,
    quarantined: outcome.quarantined,
    reason: outcome.reason,
  })
}

fn uri_userinfo(uri: &str) -> Option<&str> {
  let authority = uri.split_once("://")?.1.split('/').next()?;
  authority.rsplit_once('@').map(|(ui, _)| ui)
}

/// #11/#16：保存账号代理。口令只进 Keychain，`accounts.json` 仅存引用。
#[tauri::command]
async fn muse_native_set_account_proxy(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  uri: String,
  username: Option<String>,
  password: Option<String>,
  enabled: bool,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<ProxySettingView, String> {
  let pid = providers::ProviderId::parse(&provider_id)
    .ok_or_else(|| format!("未知 provider: {provider_id}"))?;
  if pid != providers::ProviderId::Muse {
    return Err("代理配置只支持 Muse 账号".to_string());
  }
  let uri = uri.trim().to_string();
  if let Some(ui) = uri_userinfo(&uri) {
    if ui.contains(':') {
      return Err("代理 URI 不得包含明文密码；请用独立字段提交并由 Keychain 保存".to_string());
    }
  }
  muse_native_client::proxy::ProxyConfig::validate_shape(&uri).map_err(|e| e.to_string())?;

  let username = username
    .map(|u| u.trim().to_string())
    .filter(|u| !u.is_empty());
  let has_user = username.is_some() || uri_userinfo(&uri).map(|u| !u.is_empty()).unwrap_or(false);
  let password = password.filter(|p| !p.is_empty()).map(zeroize::Zeroizing::new);

  let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
  let secrets = secrets::Secrets::new(&dir);
  let reference = format!("proxy-{uuid}");

  // 先构造并校验设置，再落凭据，避免把半成品写进 Keychain。
  let setting = storage::ProxySetting {
    uri: uri.clone(),
    username,
    credential_ref: if has_user { Some(reference.clone()) } else { None },
    enabled,
  };
  setting.validate()?;
  if has_user && password.is_none() {
    return Err("启用了代理认证，但没有提供代理密码".to_string());
  }

  // **先隔离，再保存**：已打开的 WebView 不能等到保存完才停旧出口。
  // 顺序是「隔离旧出口 → 落配置 → 下发新配置」：
  // - 隔离失败（设备无法在网络层阻断）→ 拒绝保存，否则会在窗口期继续走旧出口；
  // - 保存失败 → 保持隔离（fail-closed），下次打开仍按旧配置重建；
  // - 下发失败 → 保持隔离，不让 WebView 用系统网络直连。
  let quarantined = webview_proxy::quarantine(
    &app,
    &uuid,
    Some("代理配置正在变更，当前会话已停止出站（请重新打开该账号）".to_string()),
  )
  .await;
  if let Some(reason) = quarantined.failed() {
    return Err(format!("无法隔离当前 WebView 出站，已拒绝保存代理：{reason}"));
  }

  cancel.cancel_account(&uuid);
  let previous_password = if has_user {
    let existing = secrets.get(&reference)?;
    secrets.set(&reference, password.as_deref().map(String::as_str).unwrap_or(""))?;
    existing
  } else {
    None
  };
  if let Err(error) = mgr.set_account_proxy(pid, &uuid, Some(setting.clone())).await {
    if has_user {
      let restore = match previous_password {
        Some(secret) => secrets.set(&reference, &secret),
        None => secrets.delete(&reference),
      };
      if restore.is_err() {
        return Err("代理配置写入失败，旧凭据无法恢复，账号出口保持隔离".to_string());
      }
    }
    return Err(error);
  }

  // 再下发新配置：成功即替换隔离；enabled=false 时 helper 会清除 WebView 配置。
  let webview = apply_webview_proxy(&app, &uuid, Some(&setting)).await;
  if !has_user {
    secrets.delete(&reference)?;
  }

  Ok(ProxySettingView {
    enabled: setting.enabled,
    display: setting.display_safe(),
    has_credentials: setting.credential_ref.is_some(),
    credential_available: proxy_credential_available(&setting, &secrets),
    webview,
  })
}

/// #11/#16：读取账号代理的脱敏视图。
///
/// 读取配置和凭据可用性。WebView 下发发生在保存、打开会话与后台同步前；
/// 读面板不会销毁正在使用的 WebView。
#[tauri::command]
async fn muse_native_get_account_proxy(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  mgr: State<'_, Arc<SessionManager>>,
) -> Result<Option<ProxySettingView>, String> {
  let pid = providers::ProviderId::parse(&provider_id)
    .ok_or_else(|| format!("未知 provider: {provider_id}"))?;
  let setting = mgr.proxy_setting(pid, &uuid).await;
  let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
  let secrets = secrets::Secrets::new(&dir);
  Ok(setting.map(|s| ProxySettingView {
    enabled: s.enabled,
    display: s.display_safe(),
    has_credentials: s.credential_ref.is_some(),
    credential_available: proxy_credential_available(&s, &secrets),
    webview: None,
  }))
}

/// #11/#16：清除账号代理（连同 Keychain 中的口令）。
#[tauri::command]
async fn muse_native_clear_account_proxy(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  mgr: State<'_, Arc<SessionManager>>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<Option<WebviewProxyView>, String> {
  let pid = providers::ProviderId::parse(&provider_id)
    .ok_or_else(|| format!("未知 provider: {provider_id}"))?;
  let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
  let secrets = secrets::Secrets::new(&dir);
  let reference = mgr.proxy_setting(pid, &uuid).await.and_then(|setting| setting.credential_ref);
  // 同样先隔离：清凭据到下发之间，已打开的 WebView 不能还按旧代理跑。
  let quarantined = webview_proxy::quarantine(
    &app,
    &uuid,
    Some("代理已清除，当前会话已停止出站（请重新打开该账号）".to_string()),
  )
  .await;
  if let Some(reason) = quarantined.failed() {
    return Err(format!("无法隔离当前 WebView 出站，已拒绝清除代理：{reason}"));
  }
  cancel.cancel_account(&uuid);
  mgr.set_account_proxy(pid, &uuid, None).await?;
  let webview = apply_webview_proxy(&app, &uuid, None).await;
  let default_reference = format!("proxy-{uuid}");
  secrets.delete(&default_reference)?;
  if let Some(reference) = reference.filter(|reference| reference != &default_reference) {
    secrets.delete(&reference)?;
  }
  Ok(webview)
}

/// #11/#16：请求取消该账号正在运行的长任务（指纹/发送/代理探针等）。
/// 只停止发起新请求；已发出的发送按 UNKNOWN 记账，不重发。
/// 返回命中的任务数（同账号可能有多个任务在跑，例如指纹 + 代理探针）。
#[tauri::command]
fn muse_native_cancel_probe(
  provider_id: String,
  uuid: String,
  task_id: Option<String>,
  cancel: State<'_, Arc<probe_cancel::ProbeCancel>>,
) -> Result<usize, String> {
  if provider_id != "muse" {
    return Err("取消探针只支持 Muse 账号".to_string());
  }
  Ok(match task_id {
    Some(task) => cancel.cancel_task(
      &uuid,
      uuid::Uuid::parse_str(&task).map_err(|_| "任务标识无效".to_string())?,
    ),
    None => cancel.cancel_account(&uuid),
  })
}

/// #10：开始一次存活探针（长连接 + 订阅，记录到断开为止）。
///
/// 返回 `Result`：async 命令若返回非 `Result`，Tauri 会把引用参数当成不可满足的
/// 约束（`AsyncCommandMustReturnResult`）；这里本就要读账号代理设置（需要 await），
/// 所以统一返回 `Result`，错误直接抛给前端。
#[tauri::command]
async fn muse_native_liveness_start(
  app: AppHandle,
  uuid: String,
  seconds: u64,
  state: tauri::State<'_, std::sync::Arc<muse_native::LivenessState>>,
  mgr: State<'_, Arc<SessionManager>>,
) -> Result<muse_native::Liveness, String> {
  let setting = account_proxy_setting(&mgr, "muse", &uuid).await;
  muse_native::liveness_start(&app, &state, &uuid, seconds, setting);
  Ok(state.snapshot())
}

/// #10：读取存活探针当前状态/结果。
#[tauri::command]
fn muse_native_liveness_status(
  state: tauri::State<'_, std::sync::Arc<muse_native::LivenessState>>,
) -> muse_native::Liveness {
  state.snapshot()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  let builder = tauri::Builder::default();

  #[cfg(target_os = "android")]
  let builder = builder.plugin(platform::android::init());

  #[cfg(target_os = "ios")]
  let builder = builder.plugin(platform::ios::init());

  builder
    .setup(|app| {
      let app_data_dir = app.path().app_data_dir()?;
      let session_mgr = Arc::new(SessionManager::new(app_data_dir.clone()));
      app.manage(Arc::clone(&session_mgr));

      // 发送台账的状态日志：进程异常退出后，未定论的发送在重启时改判 UNKNOWN，
      // 不会被当成「没发过」而重发。日志读不了/写不了都拒绝新发送（fail-closed）。
      let (ledger, recovery) = send_ledger::SendLedger::open(app_data_dir.join("send-ledger.json"));
      if recovery.corrupt {
        eprintln!(
          "[send-ledger] 台账不可用（{}）：本次运行拒绝一切新发送（fail-closed）",
          recovery
            .load_error
            .as_deref()
            .unwrap_or("状态日志损坏或无法持久化")
        );
      } else if recovery.restored > 0 {
        eprintln!(
          "[send-ledger] 恢复 {} 条记录，其中 {} 条未定论改判 UNKNOWN",
          recovery.restored, recovery.marked_unknown
        );
      }
      app.manage(Arc::new(ledger));

      #[cfg(desktop)]
      {
        let main_window = tauri::window::WindowBuilder::new(app, "main")
          .title(APP_TITLE)
          .inner_size(1240.0, 800.0)
          .min_inner_size(760.0, 520.0)
          .build()?;

        let manager_builder =
          tauri::webview::WebviewBuilder::new("manager", tauri::WebviewUrl::App("index.html".into()))
            .on_navigation(|url| {
              url.scheme() == "tauri"
                || url.domain() == Some("tauri.localhost")
                || (cfg!(dev) && url.host_str() == Some("localhost"))
            });

        main_window.add_child(
          manager_builder,
          tauri::LogicalPosition::new(0.0, 0.0),
          tauri::LogicalSize::new(1240.0, 800.0),
        )?;

        let app_for_resize = app.handle().clone();
        let mgr_for_resize = Arc::clone(&session_mgr);
        main_window.on_window_event(move |event| match event {
          tauri::WindowEvent::Resized(_) | tauri::WindowEvent::ScaleFactorChanged { .. } => {
            let app_inner = app_for_resize.clone();
            let mgr_inner = Arc::clone(&mgr_for_resize);
            tauri::async_runtime::spawn(async move {
              mgr_inner.recompute_desktop_bounds(&app_inner).await;
            });
          }
          tauri::WindowEvent::Focused(focused) if *focused => {
            let app_inner = app_for_resize.clone();
            let mgr_inner = Arc::clone(&mgr_for_resize);
            tauri::async_runtime::spawn(async move {
              mgr_inner.trigger_silent_sync(&app_inner, false).await;
            });
          }
          _ => {}
        });
      }

      #[cfg(mobile)]
      {
        let _ = APP_TITLE;
        tauri::WebviewWindowBuilder::new(
          app,
          "manager",
          tauri::WebviewUrl::App("index.html".into()),
        )
        .build()?;
      }

      let app_for_boot = app.handle().clone();
      let mgr_for_boot = Arc::clone(&session_mgr);
      tauri::async_runtime::spawn(async move {
        mgr_for_boot.run_startup_tasks(&app_for_boot).await;
      });

      // Spike 0.1.22：启动自检已暂停（2026-11-23 诊断挂起排查）。
      // 原先 Spike 版启动后对首个已登录 Muse 账号跑一次握手诊断
      // （日常版不跑；见 muse_native::maybe_auto_diagnose）。
      // 用户实测手动诊断一直卡在「诊断中」，即使 Rust 侧已有 60 秒超时
      // 也没返回——先停掉启动自检，排除它与手动诊断互相干扰的可能，
      // 等手动诊断跑通后再恢复。
      // let app_for_diag = app.handle().clone();
      // let mgr_for_diag = Arc::clone(&session_mgr);
      // tauri::async_runtime::spawn(async move {
      //   muse_native::maybe_auto_diagnose(&app_for_diag, &mgr_for_diag).await;
      // });

      Ok(())
    })
    .manage(std::sync::Arc::new(muse_native::LivenessState::default()))
    .manage(std::sync::Arc::new(probe_cancel::ProbeCancel::new()))
    .manage(std::sync::Arc::new(quota::ActionCache::default()))
    .invoke_handler(tauri::generate_handler![
      get_manager_state,
      select_provider,
      add_account,
      rename_account,
      remove_account,
      switch_account,
      set_modal_active,
      open_official_site,
      resolve_uuid_conflict,
      dismiss_notice,
      trigger_quota_sync,
      reorder_accounts,
      check_for_updates,
      download_and_install_update,
      open_external_url,
      muse_native_diagnose,
      muse_native_watch_probe,
      hark_native_watch_probe,
      muse_native_approvals_probe,
      muse_native_quota_probe,
      muse_native_send_probe,
      muse_native_fingerprint_probe,
      muse_native_proxy_probe,
      muse_native_set_account_proxy,
      muse_native_get_account_proxy,
      muse_native_clear_account_proxy,
      muse_native_cancel_probe,
      muse_native_liveness_start,
      muse_native_liveness_status,
      muse_native_lease_probe,
      muse_native::is_spike_build,
    ])
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
