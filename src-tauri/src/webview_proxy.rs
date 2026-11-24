//! 账号级代理 → 平台原生 WebView 的同步层（#11/#16，补 W）。
//!
//! Rust 侧的 reqwest / WS 隧道管不到 WKWebView 内部请求，iOS 只能通过
//! `WKWebsiteDataStore.proxyConfigurations` 设置（见插件里的 `AccountProxy.swift`）。
//!
//! 这一层有三个使用点，缺一不可：
//! 1. **改配置之前**（`quarantine`）：先把该账号的 data store 出口换成不可达
//!    端点并销毁正在打开的 WebView，关掉「Rust 已保存、Swift 还没下发」的
//!    时间窗口。此后旧出口（可能是直连）再也发不出去。
//! 2. 保存/清除账号代理后（`lib.rs` 的命令）——把新配置推下去；
//! 3. **每次打开会话前**（`session_manager`）与**每次后台额度同步前**
//!    （`trigger_silent_sync`）——Rust 是权威源，App 重启、重装、Swift 本地
//!    配置损坏或过期时都以这里为准重新下发；下发失败就拒绝打开会话/跳过
//!    同步，而不是让 WebView 用系统网络直接连出去。
//!
//! 平台差异：桌面 / Android 没有「每账号 WebView 代理」这一层，`applicable`
//! 返回 false，调用方据此跳过（不是「已生效」）。

use crate::storage::ProxySetting;

/// 一次下发的执行结果。`applicable == false` 表示该平台没有这一层。
#[derive(Debug, Clone)]
pub struct PushOutcome {
  pub applicable: bool,
  pub applied: bool,
  /// 该账号的会话当时正打开（已被阻断，需要重开）。
  pub session_open: bool,
  /// WebView 出口是否停留在隔离态（下发失败时为 true）。iOS 侧读取。
  #[cfg_attr(not(target_os = "ios"), allow(dead_code))]
  pub quarantined: bool,
  pub reason: Option<String>,
}

impl PushOutcome {
  fn not_applicable() -> Self {
    Self {
      applicable: false,
      applied: false,
      session_open: false,
      quarantined: false,
      reason: None,
    }
  }

  /// 该平台有这一层但没有应用成功——调用方必须据此阻断，不得放行。
  #[cfg_attr(not(target_os = "ios"), allow(dead_code))]
  pub fn blocked(&self) -> Option<&str> {
    if self.applicable && !self.applied {
      Some(self.reason.as_deref().unwrap_or("WebView 代理未生效"))
    } else {
      None
    }
  }
}

/// 隔离结果。`applicable && !engaged` 表示这台设备无法在网络层隔离出口
/// （iOS < 17），调用方必须拒绝改配置，而不是假定「销毁视图就够了」。
#[derive(Debug, Clone)]
pub struct QuarantineOutcome {
  pub applicable: bool,
  pub engaged: bool,
  /// 该账号的会话当时正打开（已被阻断，需要重开）。iOS 侧读取。
  #[cfg_attr(not(target_os = "ios"), allow(dead_code))]
  pub session_open: bool,
  pub reason: Option<String>,
}

impl QuarantineOutcome {
  #[cfg_attr(not(target_os = "ios"), allow(dead_code))]
  pub fn failed(&self) -> Option<&str> {
    if self.applicable && !self.engaged {
      Some(self.reason.as_deref().unwrap_or("无法隔离 WebView 出口"))
    } else {
      None
    }
  }
}

#[cfg(target_os = "ios")]
pub async fn quarantine(app: &tauri::AppHandle, uuid: &str, reason: Option<String>) -> QuarantineOutcome {
  use crate::platform::ios::{QuarantineProxyPayload, quarantine_account_proxy};

  let payload = QuarantineProxyPayload {
    uuid: uuid.to_string(),
    reason,
  };
  match quarantine_account_proxy(app, payload).await {
    Ok(r) => QuarantineOutcome {
      applicable: true,
      engaged: r.engaged,
      session_open: r.session_open,
      reason: r.reason,
    },
    Err(e) => QuarantineOutcome {
      applicable: true,
      engaged: false,
      session_open: false,
      reason: Some(format!("隔离 WebView 出口失败: {e}")),
    },
  }
}

#[cfg(not(target_os = "ios"))]
pub async fn quarantine(
  _app: &tauri::AppHandle,
  _uuid: &str,
  _reason: Option<String>,
) -> QuarantineOutcome {
  QuarantineOutcome {
    applicable: false,
    engaged: false,
    session_open: false,
    reason: None,
  }
}

#[cfg(target_os = "ios")]
pub async fn push(
  app: &tauri::AppHandle,
  uuid: &str,
  setting: Option<&ProxySetting>,
) -> PushOutcome {
  use crate::platform::ios::{SetAccountProxyPayload, set_account_proxy};

  let payload = match setting {
    Some(s) if s.enabled => match s.validate().and_then(|_| {
      muse_native_client::proxy::ProxyConfig::parse_endpoint(&s.uri).map_err(|e| e.to_string())
    }) {
      Ok(ep) => SetAccountProxyPayload {
        uuid: uuid.to_string(),
        scheme: ep.scheme.to_string(),
        host: ep.host,
        port: ep.port,
        // 端点里的用户名优先；单独填的 username 字段兜底（uri 不带 userinfo 的情形）。
        username: ep.username.or_else(|| s.effective_username()),
        enabled: true,
      },
      Err(e) => {
        let reason = format!("代理端点无法下发给 WebView: {e}");
        let isolation = quarantine(app, uuid, Some(reason.clone())).await;
        return PushOutcome {
          applicable: true,
          applied: false,
          session_open: isolation.session_open,
          quarantined: isolation.engaged,
          reason: Some(reason),
        }
      }
    },
    // 未配置 / 未启用：清除 WebView 侧配置（同时解除隔离）。
    _ => SetAccountProxyPayload {
      uuid: uuid.to_string(),
      scheme: "http".to_string(),
      host: String::new(),
      port: 0,
      username: None,
      enabled: false,
    },
  };

  match set_account_proxy(app, payload).await {
    Ok(r) => PushOutcome {
      applicable: true,
      applied: r.applied,
      session_open: r.session_open,
      quarantined: r.quarantined,
      reason: r.reason,
    },
    Err(e) => PushOutcome {
      applicable: true,
      applied: false,
      session_open: false,
      // 下发失败：Swift 侧仍停在上一步的隔离态，绝不会回退直连。
      quarantined: true,
      reason: Some(format!("下发 WebView 代理失败: {e}")),
    },
  }
}

#[cfg(not(target_os = "ios"))]
pub async fn push(
  _app: &tauri::AppHandle,
  _uuid: &str,
  _setting: Option<&ProxySetting>,
) -> PushOutcome {
  PushOutcome::not_applicable()
}
