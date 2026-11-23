//! 每账号统一出站策略入口（#11/#16）。
//!
//! 账号相关的原生网络调用（认证取凭证、额度、检测、审批、发送、指纹、诊断）
//! 都应先经 [`AccountEgress::resolve`] 得到本账号的出站策略：
//!
//! - 未配置或未启用代理 → [`EgressKind::Direct`]：走系统网络（保持既有行为）；
//! - 已启用代理 → [`EgressKind::Proxy`]：控制面 HTTP 与 WSS **都**经该代理，
//!   任何失败直接返回 `Err`。这里不存在从 `Proxy` 回退到 `Direct` 的代码路径。
//!
//! `resolve` 自身失败（URI 非法、Keychain 缺凭据）同样返回 `Err`，调用方不得
//! 自行降级直连，否则就破坏了账号级 fail-closed。
//!
//! 未接入本入口的路径（见呼叫点清单）在真机验收前标记为 UNVERIFIED。

use crate::secrets::Secrets;
use crate::storage::ProxySetting;
use muse_native_client::collect::{collect_text, subscription_request, Collected};
use muse_native_client::control_plane::ControlPlaneFetch;
use muse_native_client::cookie::SessionCookies;
use muse_native_client::gateway::{
  ConnectedGateway, DirectStream, GatewayError, MuseGateway, ProxyTlsStream, Subscription,
};
use muse_native_client::proxy::ProxyConfig;
use muse_native_client::transport::HttpRequest;
use std::time::Duration;

pub enum EgressKind {
  Direct,
  Proxy(ProxyConfig),
}

#[derive(Debug)]
pub enum EgressError {
  /// 代理配置本身不合法（URI / 凭据配对）。
  Invalid(String),
  /// Keychain 读写失败或凭据缺失。
  Secret(String),
  /// 构造 HTTP 客户端失败。
  Http(String),
  /// 建立网关连接失败（含代理隧道失败）。
  Gateway(GatewayError),
}

impl std::fmt::Display for EgressError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      EgressError::Invalid(m) => write!(f, "代理配置无效: {m}"),
      EgressError::Secret(m) => write!(f, "代理凭据不可用: {m}"),
      EgressError::Http(m) => write!(f, "HTTP 客户端失败: {m}"),
      EgressError::Gateway(e) => write!(f, "网关连接失败: {e}"),
    }
  }
}

/// 一个账号本次出站使用的策略与脱敏描述。
pub struct AccountEgress {
  pub kind: EgressKind,
  /// scheme://[user@]host:port（绝不含口令），可安全用于 UI / 台账。
  pub display: String,
}

impl AccountEgress {
  pub fn direct() -> Self {
    Self {
      kind: EgressKind::Direct,
      display: "direct".to_string(),
    }
  }

  /// 由账号的代理设置 + Keychain 凭据解析。未启用/未配置 → Direct。
  ///
  /// 注意：设置为 enabled 但解析失败时返回 Err，**绝不**退回 Direct。
  pub fn resolve(setting: Option<&ProxySetting>, secrets: &Secrets) -> Result<Self, EgressError> {
    let Some(s) = setting else {
      return Ok(Self::direct());
    };
    if !s.enabled {
      return Ok(Self::direct());
    }
    s.validate().map_err(EgressError::Invalid)?;
    let username = s.effective_username();
    let password = match s.credential_ref.as_deref().filter(|r| !r.is_empty()) {
      Some(reference) => {
        let found = secrets.get(reference).map_err(EgressError::Secret)?;
        Some(found.ok_or_else(|| {
          EgressError::Secret(format!("Keychain 中找不到凭据引用 {reference}"))
        })?)
      }
      None => None,
    };
    // 组装含凭据的进程内 URI；随后立即 parse 掉，不保留明文副本。
    let mut uri = compose_uri(&s.uri, username.as_deref(), password.as_deref());
    let cfg = ProxyConfig::parse(&uri).map_err(|e| EgressError::Invalid(e.to_string()));
    uri.clear();
    let cfg = cfg?;
    let display = cfg.display_safe();
    Ok(Self {
      kind: EgressKind::Proxy(cfg),
      display,
    })
  }

  pub fn is_proxy(&self) -> bool {
    matches!(self.kind, EgressKind::Proxy(_))
  }

  /// 本账号的控制面 HTTP 客户端（Proxy 时绑定 reqwest 代理）。
  pub fn http(&self) -> Result<ReqwestFetch, EgressError> {
    match &self.kind {
      EgressKind::Direct => ReqwestFetch::new().map_err(EgressError::Http),
      EgressKind::Proxy(cfg) => ReqwestFetch::for_proxy(cfg).map_err(EgressError::Http),
    }
  }

  /// 建立到 Muse 网关的连接。Direct 走系统网络；Proxy 走代理隧道，
  /// 隧道失败即 Err，不直连。
  pub async fn connect(&self, cookies: &SessionCookies) -> Result<AccountGateway, EgressError> {
    let fetch = self.http()?;
    match &self.kind {
      EgressKind::Direct => MuseGateway::new(fetch)
        .connect(cookies)
        .await
        .map(AccountGateway::Direct)
        .map_err(EgressError::Gateway),
      EgressKind::Proxy(cfg) => MuseGateway::new(fetch)
        .connect_via_proxy(cookies, cfg)
        .await
        .map(AccountGateway::Proxied)
        .map_err(EgressError::Gateway),
    }
  }
}

/// 从 App 数据目录构造凭据存储并解析账号出站策略。
pub fn resolve_for_app(
  app: &tauri::AppHandle,
  setting: Option<&ProxySetting>,
) -> Result<AccountEgress, String> {
  use tauri::Manager;
  let dir = app
    .path()
    .app_data_dir()
    .map_err(|e| format!("获取应用数据目录失败: {e}"))?;
  let secrets = Secrets::new(&dir);
  AccountEgress::resolve(setting, &secrets).map_err(|e| e.to_string())
}

/// 直连与代理两种底层流统一成一个句柄，调用方无需关心具体类型。
pub enum AccountGateway {
  Direct(ConnectedGateway<DirectStream>),
  Proxied(ConnectedGateway<ProxyTlsStream>),
}

impl AccountGateway {
  pub fn is_proxied(&self) -> bool {
    matches!(self, AccountGateway::Proxied(_))
  }

  pub async fn request(&mut self, req: HttpRequest) -> Result<(u16, Vec<u8>), GatewayError> {
    match self {
      AccountGateway::Direct(g) => g.request(req).await,
      AccountGateway::Proxied(g) => g.request(req).await,
    }
  }

  pub async fn ping(&mut self) -> Result<u16, GatewayError> {
    match self {
      AccountGateway::Direct(g) => g.ping().await,
      AccountGateway::Proxied(g) => g.ping().await,
    }
  }

  pub async fn close(self) {
    match self {
      AccountGateway::Direct(g) => g.close().await,
      AccountGateway::Proxied(g) => g.close().await,
    }
  }

  /// 发一条聊天消息并等 ack（直连与代理共用同一协议实现）。
  pub async fn send_message(
    self,
    text: &str,
    node_id: &str,
    session_id: Option<&str>,
    timeout: Duration,
  ) -> Result<(serde_json::Value, Option<String>), GatewayError> {
    match self {
      AccountGateway::Direct(g) => {
        muse_native_client::chat_send::send_message(g, text, node_id, session_id, timeout).await
      }
      AccountGateway::Proxied(g) => {
        muse_native_client::chat_send::send_message(g, text, node_id, session_id, timeout).await
      }
    }
  }

  /// 建立 `chat.subscribe` 长连接。
  pub async fn subscribe(self) -> Result<AccountSubscription, GatewayError> {
    let req = subscription_request();
    match self {
      AccountGateway::Direct(g) => Ok(AccountSubscription::Direct(g.subscribe(req).await?)),
      AccountGateway::Proxied(g) => Ok(AccountSubscription::Proxied(g.subscribe(req).await?)),
    }
  }
}

pub enum AccountSubscription {
  Direct(Subscription<DirectStream>),
  Proxied(Subscription<ProxyTlsStream>),
}

impl AccountSubscription {
  /// 订阅请求的 HTTP 状态（`None` 表示尚未拿到响应头）。
  pub fn status(&self) -> Option<u16> {
    match self {
      AccountSubscription::Direct(s) => s.status(),
      AccountSubscription::Proxied(s) => s.status(),
    }
  }

  /// 读下一行（NDJSON 事件或空行保活）；`None` 表示流已关闭。
  pub async fn next_line(&mut self, wait: Duration) -> Result<Option<String>, GatewayError> {
    match self {
      AccountSubscription::Direct(s) => s.next_line(wait).await,
      AccountSubscription::Proxied(s) => s.next_line(wait).await,
    }
  }

  /// 收一条助手回复，直到 message_done / 超时 / 断流。
  pub async fn collect(
    mut self,
    message_id: Option<&str>,
    total: Duration,
    per_line: Duration,
  ) -> Collected {
    match &mut self {
      AccountSubscription::Direct(s) => collect_text(s, message_id, total, per_line).await,
      AccountSubscription::Proxied(s) => collect_text(s, message_id, total, per_line).await,
    }
  }
}

/// 控制面 HTTP 客户端。`proxy_uri` 含认证时由 reqwest 从 URI 解析凭证；
/// 构造失败一律 Err（fail-closed）。
pub(crate) struct ReqwestFetch {
  client: reqwest::Client,
}

impl ReqwestFetch {
  pub fn new() -> Result<Self, String> {
    Self::build(None)
  }

  pub fn new_with_proxy(proxy_uri: Option<&str>) -> Result<Self, String> {
    Self::build(proxy_uri)
  }

  pub fn for_proxy(cfg: &ProxyConfig) -> Result<Self, String> {
    // 只在进程内传递含凭据的 URI，构造完 client 后立即丢弃字符串。
    let mut uri = cfg.to_credentialed_uri();
    let out = Self::build(Some(&uri));
    uri.clear();
    out
  }

  fn build(proxy_uri: Option<&str>) -> Result<Self, String> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(20));
    if let Some(uri) = proxy_uri {
      let proxy = reqwest::Proxy::all(uri)
        .map_err(|e| format!("代理 URI 无效（不会回退直连）: {e}"))?;
      builder = builder.proxy(proxy);
    }
    let client = builder
      .build()
      .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    Ok(Self { client })
  }
}

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

/// 把 base（scheme://host[:port]）与凭据拼成完整 URI。userinfo 做百分号编码，
/// 避免密码里的 `@` / `:` 破坏解析。
fn compose_uri(base: &str, user: Option<&str>, pass: Option<&str>) -> String {
  let (scheme, rest) = base.split_once("://").unwrap_or(("http", base));
  let authority = rest.split('/').next().unwrap_or(rest);
  let hostport = authority
    .rsplit_once('@')
    .map(|(_, h)| h)
    .unwrap_or(authority);
  match (user, pass) {
    (Some(u), Some(p)) => format!(
      "{scheme}://{}:{}@{hostport}",
      pct_encode(u),
      pct_encode(p)
    ),
    (Some(u), None) => format!("{scheme}://{}@{hostport}", pct_encode(u)),
    _ => format!("{scheme}://{hostport}"),
  }
}

fn pct_encode(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for b in s.as_bytes() {
    match b {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
        out.push(*b as char)
      }
      _ => out.push_str(&format!("%{b:02X}")),
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn compose_uri_encodes_special_chars() {
    let uri = compose_uri("socks5://host:1080", Some("u@x"), Some("p@ss:w0rd"));
    assert_eq!(uri, "socks5://u%40x:p%40ss%3Aw0rd@host:1080");
    let parsed = ProxyConfig::parse(&uri).unwrap();
    assert_eq!(parsed.display_safe(), "socks5://u@x@host:1080");
  }

  #[test]
  fn resolve_without_setting_is_direct() {
    let dir = std::env::temp_dir().join(format!("egress-test-{}", uuid::Uuid::new_v4()));
    let secrets = Secrets::new(&dir);
    assert!(!AccountEgress::resolve(None, &secrets).unwrap().is_proxy());
    let disabled = ProxySetting {
      uri: "socks5://h:1080".to_string(),
      username: None,
      credential_ref: None,
      enabled: false,
    };
    assert!(!AccountEgress::resolve(Some(&disabled), &secrets).unwrap().is_proxy());
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn enabled_proxy_never_falls_back_when_secret_missing() {
    let dir = std::env::temp_dir().join(format!("egress-test-{}", uuid::Uuid::new_v4()));
    let secrets = Secrets::new(&dir);
    let setting = ProxySetting {
      uri: "socks5://alice@p.example:1080".to_string(),
      username: None,
      credential_ref: Some("missing-ref".to_string()),
      enabled: true,
    };
    // 缺少 Keychain 凭据必须报错，不能静默直连。
    assert!(AccountEgress::resolve(Some(&setting), &secrets).is_err());
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn enabled_proxy_with_secret_resolves_proxied() {
    let dir = std::env::temp_dir().join(format!("egress-test-{}", uuid::Uuid::new_v4()));
    let secrets = Secrets::new(&dir);
    secrets.set("ref-1", "p@ss").unwrap();
    let setting = ProxySetting {
      uri: "socks5://alice@p.example:1080".to_string(),
      username: None,
      credential_ref: Some("ref-1".to_string()),
      enabled: true,
    };
    let egress = AccountEgress::resolve(Some(&setting), &secrets).unwrap();
    assert!(egress.is_proxy());
    assert!(egress.display.contains("alice@p.example:1080"));
    assert!(!egress.display.contains("p%40ss"));
    let _ = std::fs::remove_dir_all(&dir);
  }
}
