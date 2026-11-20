use std::path::Path;
use uuid::Uuid;

pub fn current_platform_name() -> &'static str {
  #[cfg(target_os = "macos")]
  {
    "macos"
  }
  #[cfg(windows)]
  {
    "windows"
  }
  #[cfg(target_os = "android")]
  {
    "android"
  }
  #[cfg(target_os = "ios")]
  {
    "ios"
  }
  #[cfg(all(
    not(target_os = "macos"),
    not(windows),
    not(target_os = "android"),
    not(target_os = "ios")
  ))]
  {
    "linux"
  }
}

#[cfg(desktop)]
pub fn open_external_browser(url: &str) -> Result<(), String> {
  open::that_detached(url).map_err(|e| format!("打开系统浏览器失败: {e}"))
}

#[cfg(desktop)]
pub fn verify_desktop_profile_binding(
  webview: &tauri::Webview,
  expected_uuid: Uuid,
  expected_dir: &Path,
) -> impl std::future::Future<Output = Result<(), String>> + Send {
  #[cfg(target_os = "macos")]
  {
    let _ = expected_dir;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let res = webview
      .with_webview(move |platform_wv| unsafe {
        let wk: &objc2_web_kit::WKWebView = &*platform_wv.inner().cast();
        let store = wk.configuration().websiteDataStore();
        let check = match store.identifier() {
          Some(ns_uuid) => {
            let actual = ns_uuid.UUIDString().to_string();
            let expected = expected_uuid.hyphenated().to_string();
            if actual.eq_ignore_ascii_case(&expected) {
              Ok(())
            } else {
              Err(format!(
                "WKWebsiteDataStore 标识不匹配（期望 {expected}，实际 {actual}）"
              ))
            }
          }
          None => Err("WKWebsiteDataStore 未绑定独立 UUID（落入默认资料），已阻止加载".to_string()),
        };
        let _ = tx.send(check);
      });

    async move {
      res.map_err(|e| format!("访问 macOS WebView 失败: {e}"))?;
      rx
        .await
        .map_err(|_| "核对 macOS WKWebsiteDataStore 超时或中断".to_string())?
    }
  }

  #[cfg(windows)]
  {
    let _ = expected_uuid;
    let expected_owned = expected_dir.to_path_buf();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let res = webview
      .with_webview(move |platform_wv| unsafe {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
          ICoreWebView2Environment7, ICoreWebView2_2,
        };
        use windows::core::Interface;

        let check = (|| -> Result<(), String> {
          let core = platform_wv
            .controller()
            .CoreWebView2()
            .map_err(|e| format!("获取 CoreWebView2 失败: {e}"))?;
          let core2: ICoreWebView2_2 = core
            .cast()
            .map_err(|e| format!("转换 ICoreWebView2_2 失败: {e}"))?;
          let env = core2
            .Environment()
            .map_err(|e| format!("获取 WebView2 Environment 失败: {e}"))?;
          let env7: ICoreWebView2Environment7 = env
            .cast()
            .map_err(|e| format!("转换 ICoreWebView2Environment7 失败: {e}"))?;
          let mut raw_folder = windows::core::PWSTR::null();
          env7
            .UserDataFolder(&mut raw_folder)
            .map_err(|e| format!("读取 WebView2 UserDataFolder 失败: {e}"))?;
          let actual_str = raw_folder
            .to_string()
            .map_err(|e| format!("解析 UserDataFolder 失败: {e}"))?;
          windows::Win32::System::Com::CoTaskMemFree(Some(raw_folder.0 as *const _));

          let actual_norm = normalize_win_path(Path::new(&actual_str));
          let expected_norm = normalize_win_path(&expected_owned);
          if actual_norm == expected_norm {
            Ok(())
          } else {
            Err(format!(
              "Windows WebView2 实际数据目录被系统配置覆盖（实际: {actual_str}），已阻止打开以防串号"
            ))
          }
        })();
        let _ = tx.send(check);
      });

    async move {
      res.map_err(|e| format!("访问 Windows WebView2 失败: {e}"))?;
      rx
        .await
        .map_err(|_| "核对 Windows WebView2 数据目录中断".to_string())?
    }
  }

  #[cfg(all(not(target_os = "macos"), not(windows)))]
  {
    let _ = (webview, expected_uuid, expected_dir);
    async move { Ok(()) }
  }
}

#[cfg(windows)]
fn normalize_win_path(path: &Path) -> String {
  let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
  resolved
    .to_string_lossy()
    .trim_start_matches(r"\\?\")
    .trim_end_matches(['\\', '/'])
    .to_ascii_lowercase()
}

#[cfg(desktop)]
pub async fn delete_desktop_profile(
  app: &tauri::AppHandle,
  app_data_dir: &Path,
  uuid_str: &str,
) -> Result<(), String> {
  let parsed = Uuid::parse_str(uuid_str).map_err(|e| format!("非法 UUID: {e}"))?;

  #[cfg(target_os = "macos")]
  {
    let _ = app_data_dir;
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    let uuid_formatted = parsed.hyphenated().to_string();
    let tx_cell = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));

    app
      .run_on_main_thread(move || {
        use objc2::AnyThread;
        use objc2_foundation::{NSError, NSString, NSUUID};
        use objc2_web_kit::WKWebsiteDataStore;

        let ns_str = NSString::from_str(&uuid_formatted);
        let Some(ns_uuid) = NSUUID::initWithUUIDString(NSUUID::alloc(), &ns_str) else {
          if let Some(sender) = tx_cell.lock().unwrap().take() {
            let _ = sender.send(Err("构造 NSUUID 失败".to_string()));
          }
          return;
        };

        let Some(mtm) = objc2::MainThreadMarker::new() else {
          if let Some(sender) = tx_cell.lock().unwrap().take() {
            let _ = sender.send(Err("当前不在 macOS 主线程".to_string()));
          }
          return;
        };

        let tx_inner = tx_cell.clone();
        let block = block2::RcBlock::new(move |err: *mut NSError| {
          let outcome = if err.is_null() {
            Ok(())
          } else {
            let msg = unsafe { (&*err).localizedDescription().to_string() };
            if msg.to_ascii_lowercase().contains("not exist")
              || msg.to_ascii_lowercase().contains("not found")
            {
              Ok(())
            } else {
              Err(format!("移除 WKWebsiteDataStore 失败: {msg}"))
            }
          };
          if let Some(sender) = tx_inner.lock().unwrap().take() {
            let _ = sender.send(outcome);
          }
        });

        unsafe {
          WKWebsiteDataStore::removeDataStoreForIdentifier_completionHandler(&ns_uuid, &block, mtm);
        }
      })
      .map_err(|e| format!("调度主线程清理 WKWebsiteDataStore 失败: {e}"))?;

    rx
      .await
      .map_err(|_| "等待 WKWebsiteDataStore 清理完成中断".to_string())?
  }

  #[cfg(not(target_os = "macos"))]
  {
    let _ = (app, parsed);
    let profile_dir = app_data_dir.join("profiles").join(uuid_str);
    if profile_dir.exists() {
      std::fs::remove_dir_all(&profile_dir)
        .map_err(|e| format!("清理资料目录被占用或失败: {e}"))?;
    }
    Ok(())
  }
}

#[cfg(target_os = "android")]
pub use tauri_plugin_account_session as android;

#[cfg(target_os = "ios")]
pub use tauri_plugin_account_session as ios;
