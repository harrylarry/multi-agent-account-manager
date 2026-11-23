mod muse_native;
mod platform;
mod providers;
mod quota;
mod send_ledger;
mod session_manager;
mod storage;
mod updater;

use session_manager::{ManagerSnapshot, SessionManager, APP_TITLE};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use updater::{InstallUpdateOutcome, InstallUpdateRequest, UpdateCheckResult};

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
) -> Result<muse_native::SpikeDiagnose, String> {
  muse_native::diagnose(&app, &provider_id, &uuid).await
}

/// B1：`chat.subscribe` 探针（Issue #13 #5）。订阅 N 秒，只返回事件计数与去重结果。
#[tauri::command]
async fn muse_native_watch_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
  seconds: u64,
) -> Result<muse_native::WatchProbe, String> {
  muse_native::watch_probe(&app, &provider_id, &uuid, seconds).await
}

/// B2：只读拉取 `egress.approvals`。
#[tauri::command]
async fn muse_native_approvals_probe(
  app: AppHandle,
  provider_id: String,
  uuid: String,
) -> Result<muse_native::ApprovalsProbe, String> {
  muse_native::approvals_probe(&app, &provider_id, &uuid).await
}

/// B3：只读拉一次额度（Server Action 优先，REST 兜底），失败时带每步诊断。
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
) -> Result<QuotaProbe, String> {
  if provider_id != "muse" {
    return Err("额度探针只支持 Muse 账号".to_string());
  }
  let cookie = match muse_native::cookie_header_for(&app, &uuid).await {
    Ok(c) => c,
    Err(e) => {
      return Ok(QuotaProbe { ok: false, quota: None, diag: vec![], error: Some(e) })
    }
  };
  let (quota, diag) = quota::fetch_quota(&cookie, None).await;
  Ok(QuotaProbe {
    ok: quota.is_some(),
    error: if quota.is_some() {
      None
    } else {
      Some("额度获取失败（见下方每步诊断）".to_string())
    },
    quota,
    diag,
  })
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
) -> Result<muse_native::SendOutcome, String> {
  if provider_id != "muse" {
    return Err("发送探针只支持 Muse 账号".to_string());
  }
  Ok(muse_native::send_probe(
    &app,
    &ledger,
    &uuid,
    &text,
    &node_id,
    session_id.as_deref(),
  )
  .await)
}

/// #10：开始一次存活探针（长连接 + 订阅，记录到断开为止）。
#[tauri::command]
async fn muse_native_liveness_start(
  app: AppHandle,
  uuid: String,
  seconds: u64,
  state: tauri::State<'_, std::sync::Arc<muse_native::LivenessState>>,
) -> muse_native::Liveness {
  muse_native::liveness_start(&app, &state, &uuid, seconds).await;
  state.snapshot()
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
      let session_mgr = Arc::new(SessionManager::new(app_data_dir));
      app.manage(Arc::clone(&session_mgr));

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
    .manage(std::sync::Arc::new(send_ledger::SendLedger::new()))
    .manage(std::sync::Arc::new(muse_native::LivenessState::default()))
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
      muse_native_approvals_probe,
      muse_native_quota_probe,
      muse_native_send_probe,
      muse_native_liveness_start,
      muse_native_liveness_status,
      muse_native::is_spike_build,
    ])
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
