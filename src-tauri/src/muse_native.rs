// Spike 原生客户端的 App 接线（Issue #12 #2–#4）：
// - Cookie 桥：iOS 上经 account-session 插件从 Swift 取各账号
//   WKWebsiteDataStore 的 muse.ai 会话 cookie（内存传递，见下）；
// - HTTP 客户端：控制面用的 reqwest（桌面与 iOS 均为 native-tls）；
// - 诊断命令 muse_native_diagnose：跑一次“取凭证 → WSS → Noise 握手 →
//   connection.ping”，返回不含任何密钥的结果；
// - Spike 版启动自检：只在 bundle id 不是日常版时跑一次，日常版绝不自动跑。
//
// 协议实现在 muse-native crate；这里只有平台接线，不含协议逻辑。

use muse_native_client::control_plane::ControlPlaneFetch;
use muse_native_client::cookie::{SessionCookie, SessionCookies};
use muse_native_client::gateway::{GatewayError, MuseGateway};
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use zeroize::Zeroize;

use crate::providers::ProviderId;
use crate::session_manager::SessionManager;
use crate::storage::AuthState;

/// 日常版 bundle id：自检只在 Spike 版（换了 bundle id 的包）里跑。
const DAILY_IDENTIFIER: &str = "com.ldjx.accountmanager";

/// 诊断结果——只含阶段与状态，不含 cookie / token / 凭证值。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpikeDiagnose {
  pub ok: bool,
  pub phase: String,
  pub handshake_completed: bool,
  pub ping_status: Option<u16>,
  pub cvm_detected: bool,
  pub error: Option<String>,
}

impl SpikeDiagnose {
  fn fail(phase: &str, error: impl Into<String>) -> Self {
    Self {
      ok: false,
      phase: phase.to_string(),
      handshake_completed: false,
      ping_status: None,
      cvm_detected: false,
      error: Some(error.into()),
    }
  }
}

#[cfg(not(target_os = "android"))]
struct ReqwestFetch {
  client: reqwest::Client,
}

#[cfg(not(target_os = "android"))]
impl ReqwestFetch {
  fn new() -> Result<Self, String> {
    let client = reqwest::Client::builder()
      .timeout(Duration::from_secs(20))
      .build()
      .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    Ok(Self { client })
  }
}

#[cfg(not(target_os = "android"))]
impl ControlPlaneFetch for ReqwestFetch {
  async fn post(
    &self,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
  ) -> Result<(u16, Vec<u8>), String> {
    let mut req = self.client.post(url).body(body.to_vec());
    for (k, v) in headers {
      req = req.header(*k, *v);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    Ok((status, bytes.to_vec()))
  }
}

/// 跑一次原生握手诊断：取凭证 → WSS → Noise 握手 → connection.ping。
/// 只做诊断，不发聊天、不做 watcher、不做通知、不改 UI。
/// cookie 只在内存里用，控制面跑完立即 zeroize。
pub async fn diagnose(
  app: &AppHandle,
  provider_id: &str,
  uuid: &str,
) -> Result<SpikeDiagnose, String> {
  if provider_id != ProviderId::Muse.as_str() {
    return Err("Spike 原生诊断只支持 Muse 账号".to_string());
  }
  #[cfg(target_os = "android")]
  {
    let _ = (app, uuid);
    return Err(
      "Spike 原生诊断在 Android 上不可用（Cookie 桥只接了 iOS WKWebsiteDataStore）".to_string(),
    );
  }
  #[cfg(not(target_os = "android"))]
  {
    let mut cookies = fetch_session_cookies(app, uuid).await?;
    if cookies.is_empty() {
      return Ok(SpikeDiagnose::fail(
        "no_cookies",
        "该账号在 Spike App 里还没有 muse.ai 登录态：请先在 Spike App 里打开 Muse 账号完成登录，再跑诊断。",
      ));
    }
    eprintln!(
      "[muse-native] diagnose: {} cookie(s) from WKWebsiteDataStore, names={:?}",
      cookies.len(),
      cookies.names()
    );
    let fetch = ReqwestFetch::new()?;
    let gateway = MuseGateway::new(fetch);
    let mut connected = match gateway.connect(&cookies).await {
      Ok(c) => c,
      Err(e) => {
        cookies.zeroize();
        return Ok(report_connect_error(e));
      }
    };
    // 控制面已跑完：cookie 不再需要，立即清零。
    cookies.zeroize();
    eprintln!("[muse-native] diagnose: noise handshake completed");
    match connected.ping().await {
      Ok(status) => {
        connected.close().await;
        eprintln!("[muse-native] diagnose: connection.ping status={status}");
        Ok(SpikeDiagnose {
          ok: status < 400,
          phase: "ready".to_string(),
          handshake_completed: true,
          ping_status: Some(status),
          cvm_detected: false,
          error: if status < 400 {
            None
          } else {
            Some(format!("connection.ping HTTP {status}"))
          },
        })
      }
      Err(e) => {
        connected.close().await;
        Ok(SpikeDiagnose {
          ok: false,
          phase: "ping".to_string(),
          handshake_completed: true,
          ping_status: None,
          cvm_detected: false,
          error: Some(e.to_string()),
        })
      }
    }
  }
}

#[cfg(not(target_os = "android"))]
fn report_connect_error(e: GatewayError) -> SpikeDiagnose {
  match e {
    GatewayError::SignedOut => SpikeDiagnose::fail(
      "signed_out",
      "控制面 401/403：账号未登录或登录已过期，请在 Spike App 里重新登录 Muse 测试账号。",
    ),
    GatewayError::CvmRequired => SpikeDiagnose {
      ok: false,
      phase: "cvm_required".to_string(),
      handshake_completed: false,
      ping_status: None,
      cvm_detected: true,
      error: Some(
        "该 VM 是保密 VM（Owner RV challenge）：请在浏览器里打开 muse.ai 完成注册，或换标准 VM。"
          .to_string(),
      ),
    },
    GatewayError::ControlPlane(_) => SpikeDiagnose::fail("credentials", e.to_string()),
    GatewayError::Timeout(what) => SpikeDiagnose::fail("handshake", format!("{what} timed out")),
    other => SpikeDiagnose::fail("handshake", other.to_string()),
  }
}

#[cfg(target_os = "ios")]
async fn fetch_session_cookies(app: &AppHandle, uuid: &str) -> Result<SessionCookies, String> {
  let entries = crate::platform::ios::get_session_cookies(app, uuid).await?;
  Ok(SessionCookies {
    cookies: entries
      .into_iter()
      .map(|e| SessionCookie {
        name: e.name,
        value: e.value,
      })
      .collect(),
  })
}

#[cfg(all(not(target_os = "android"), not(target_os = "ios")))]
async fn fetch_session_cookies(_app: &AppHandle, _uuid: &str) -> Result<SessionCookies, String> {
  Err("Spike 原生诊断只在 iOS 可用（Cookie 桥只接了 WKWebsiteDataStore）".to_string())
}

/// Spike 版启动自检：bundle id 不是日常版时，对第一个已登录的 Muse 账号
/// 跑一次握手诊断，结果只打到诊断日志（不含密钥）。日常版直接返回。
pub async fn maybe_auto_diagnose(app: &AppHandle, mgr: &Arc<SessionManager>) {
  if app.config().identifier == DAILY_IDENTIFIER {
    return;
  }
  // 等启动任务就绪（插件桥、账号快照）。
  tokio::time::sleep(Duration::from_secs(3)).await;
  let snap = mgr.get_snapshot().await;
  let Some(acc) = snap
    .accounts
    .iter()
    .find(|a| a.provider_id == ProviderId::Muse && a.auth_state == AuthState::Authenticated)
  else {
    eprintln!("[muse-native] spike auto-diagnose: no authenticated Muse account, skip");
    return;
  };
  let uuid = acc.uuid.clone();
  match diagnose(app, ProviderId::Muse.as_str(), &uuid).await {
    Ok(r) => eprintln!(
      "[muse-native] spike auto-diagnose: ok={} phase={} handshake={} ping={:?} cvm={} error={:?}",
      r.ok, r.phase, r.handshake_completed, r.ping_status, r.cvm_detected, r.error
    ),
    Err(e) => eprintln!("[muse-native] spike auto-diagnose failed: {e}"),
  }
}
