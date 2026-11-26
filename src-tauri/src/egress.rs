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
//! 未接入本入口的路径（真机验收前标记为 UNVERIFIED）：
//!
//! - `updater.rs` 的更新检查/下载：**不是账号流量**（走 GitHub Releases），
//!   刻意不经账号出站策略；如需代理应在系统层配置。
//! - `platform::ios` 的 WebView 出站（WKWebView / 后台额度同步的隐藏 WebView）：
//!   Rust 管不到 WebKit 内部请求，改由 `AccountProxy.swift`
//!   （`WKWebsiteDataStore.proxyConfigurations`，`allowFailover = false`）承担，
//!   配置由 `lib.rs::apply_webview_proxy` 下发。**iOS 真机未验证**。
//! - Android：主 crate 没有 reqwest 依赖（`quota.rs` 等），该目标当前整体
//!   不可编译——本切片之前就如此，未在本次修复范围内，Android 一律 BLOCKED。

use crate::secrets::Secrets;
use crate::storage::ProxySetting;
use muse_native_client::chat_send::{self, Ack, IsolatedProbeRoute, ProbeRouteError, SendAttemptResult};
use muse_native_client::collect::{collect_text, subscription_request, Collected};
use muse_native_client::control_plane::ControlPlaneFetch;
use muse_native_client::cookie::SessionCookies;
use muse_native_client::gateway::{
  ConnectedGateway, DirectStream, GatewayError, MuseGateway, ProxyTlsStream, Subscription,
};
use muse_native_client::proxy::ProxyConfig;
use muse_native_client::transport::HttpRequest;
use std::error::Error as _;
use std::time::Duration;
use zeroize::Zeroize;

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
  /// scheme://host:port，省略认证信息，可安全用于 UI / 台账。
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
    uri.zeroize();
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

  /// 需要自定义超时/重定向策略的调用方（如额度探针）用这个入口。
  /// 代理绑定与 [`Self::http`] 完全一致，构造失败一律 Err（fail-closed）。
  pub fn http_client(
    &self,
    timeout: Duration,
    follow_redirects: bool,
  ) -> Result<reqwest::Client, EgressError> {
    self.http_client_with_policy(timeout, follow_redirects, true)
  }

  #[cfg(target_os = "ios")]
  pub fn http_client_no_retry(&self, timeout: Duration) -> Result<reqwest::Client, EgressError> {
    self.http_client_with_policy(timeout, false, false)
  }

  fn http_client_with_policy(&self, timeout: Duration, follow_redirects: bool, allow_retry: bool) -> Result<reqwest::Client, EgressError> {
    let mut builder = reqwest::Client::builder().timeout(timeout);
    if !allow_retry { builder = builder.retry(reqwest::retry::never()); }
    if !follow_redirects {
      builder = builder.redirect(reqwest::redirect::Policy::none());
    }
    if let EgressKind::Proxy(cfg) = &self.kind {
      builder = builder.proxy(reqwest_proxy(cfg).map_err(EgressError::Http)?);
    }
    builder
      .build()
      .map_err(|e| EgressError::Http(safe_http_error("创建 HTTP 客户端", &e)))
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
  #[allow(dead_code)] // 供日志/断言使用
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
  /// 返回值区分「是否已进入网络写入」——台账据此决定 UNKNOWN 还是未开始。
  #[allow(dead_code)] // 取消路径走 `send_message_cancellable`
  pub async fn send_message(
    self,
    text: &str,
    node_id: &str,
    session_id: Option<&str>,
    timeout: Duration,
  ) -> SendAttemptResult {
    self
      .send_message_cancellable(text, node_id, session_id, timeout, None)
      .await
  }

  /// 与 [`Self::send_message`] 相同，但写入阶段与等 ack 阶段都可被取消。
  /// 取消**不会**让调用方拿到「没发出去」的结论：`on_wire` 才是依据——
  /// 首帧写入中被取消时帧计数是 0，但字节可能已经写出去了。
  pub async fn send_message_cancellable(
    self,
    text: &str,
    node_id: &str,
    session_id: Option<&str>,
    timeout: Duration,
    cancel: Option<&mut tokio::sync::watch::Receiver<bool>>,
  ) -> SendAttemptResult {
    match self {
      AccountGateway::Direct(g) => {
        muse_native_client::chat_send::send_message_tracked(
          g, text, node_id, session_id, timeout, cancel,
        )
        .await
      }
      AccountGateway::Proxied(g) => {
        muse_native_client::chat_send::send_message_tracked(
          g, text, node_id, session_id, timeout, cancel,
        )
        .await
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

  pub async fn subscribe_isolated_probe(
    self,
    route: &IsolatedProbeRoute,
  ) -> Result<AccountSubscription, ProbeRouteError> {
    let version = env!("CARGO_PKG_VERSION");
    match self {
      Self::Direct(g) => route.subscribe(g, version).await.map(AccountSubscription::Direct),
      Self::Proxied(g) => route.subscribe(g, version).await.map(AccountSubscription::Proxied),
    }
  }
}

pub enum AccountSubscription {
  Direct(Subscription<DirectStream>),
  Proxied(Subscription<ProxyTlsStream>),
}

impl AccountSubscription {
  #[cfg(target_os = "ios")]
  pub async fn send_room_message(
    &mut self, text: &str, route: &IsolatedProbeRoute, attachments: &[chat_send::ChatAttachment],
    timeout: Duration, cancel: &mut tokio::sync::watch::Receiver<bool>,
  ) -> SendAttemptResult {
    match self {
      Self::Direct(s) => chat_send::send_with_attachments_on_subscription(s, text, route, attachments, timeout, cancel).await,
      Self::Proxied(s) => chat_send::send_with_attachments_on_subscription(s, text, route, attachments, timeout, cancel).await,
    }
  }
  #[cfg(target_os = "ios")]
  pub async fn cancel_isolated_turn(&mut self, route: &IsolatedProbeRoute) -> bool {
    match self {
      Self::Direct(s) => chat_send::cancel_isolated_turn(s, route).await,
      Self::Proxied(s) => chat_send::cancel_isolated_turn(s, route).await,
    }
  }

  pub fn enable_chat_wire_trace(&mut self) {
    match self {
      Self::Direct(s) => s.enable_chat_wire_trace(),
      Self::Proxied(s) => s.enable_chat_wire_trace(),
    }
  }

  pub fn take_chat_wire_trace(&mut self) -> Vec<String> {
    match self {
      Self::Direct(s) => s.take_chat_wire_trace(),
      Self::Proxied(s) => s.take_chat_wire_trace(),
    }
  }

  pub async fn wait_ready(&mut self, timeout: Duration) -> Result<(), GatewayError> {
    match self {
      Self::Direct(s) => s.wait_ready(timeout).await,
      Self::Proxied(s) => s.wait_ready(timeout).await,
    }
  }

  pub async fn send_on_subscription(
    &mut self,
    text: &str,
    route: &IsolatedProbeRoute,
    timeout: Duration,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
  ) -> SendAttemptResult {
    match self {
      Self::Direct(s) => {
        chat_send::send_on_subscription(s, text, route, timeout, cancel).await
      }
      Self::Proxied(s) => {
        chat_send::send_on_subscription(s, text, route, timeout, cancel).await
      }
    }
  }

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
    &mut self,
    ack: &Ack,
    session_id: &str,
    total: Duration,
    per_line: Duration,
    on_first_delta: impl FnMut(),
  ) -> Collected {
    match self {
      AccountSubscription::Direct(s) => collect_text(s, ack, session_id, total, per_line, on_first_delta).await,
      AccountSubscription::Proxied(s) => collect_text(s, ack, session_id, total, per_line, on_first_delta).await,
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

  pub fn for_proxy(cfg: &ProxyConfig) -> Result<Self, String> {
    Self::build(Some(cfg))
  }

  fn build(config: Option<&ProxyConfig>) -> Result<Self, String> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(20));
    if let Some(config) = config {
      builder = builder.proxy(reqwest_proxy(config)?);
    }
    let client = builder
      .build()
      .map_err(|e| safe_http_error("创建 HTTP 客户端", &e))?;
    Ok(Self { client })
  }
}

impl ReqwestFetch {
  async fn send(
    &self,
    mut req: reqwest::RequestBuilder,
    headers: &[(&str, &str)],
  ) -> Result<(u16, Vec<u8>), String> {
    for (k, v) in headers {
      req = req.header(*k, *v);
    }
    let resp = req.send().await.map_err(|e| safe_http_error("控制面 HTTP 请求", &e))?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.map_err(|e| safe_http_error("读取控制面响应", &e))?;
    Ok((status, bytes.to_vec()))
  }
}

impl ControlPlaneFetch for ReqwestFetch {
  async fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, Vec<u8>), String> {
    self.send(self.client.get(url), headers).await
  }

  async fn post(&self, url: &str, headers: &[(&str, &str)], body: &[u8]) -> Result<(u16, Vec<u8>), String> {
    self.send(self.client.post(url).body(body.to_vec()), headers).await
  }
}

fn reqwest_proxy(config: &ProxyConfig) -> Result<reqwest::Proxy, String> {
  let mut uri = config.to_reqwest_proxy_uri();
  let proxy = reqwest::Proxy::all(&uri);
  uri.zeroize();
  proxy.map_err(|e| safe_http_error("构造 HTTP 代理", &e))
}

/// 只输出错误类别和底层 I/O 类型。第三方错误的自由文本可能含 URI 或凭据。
pub(crate) fn safe_http_error(stage: &str, error: &reqwest::Error) -> String {
  let category = if error.is_timeout() {
    "timeout"
  } else if error.is_connect() {
    "connect"
  } else if error.is_builder() {
    "client_configuration"
  } else if error.is_body() || error.is_decode() {
    "response_body"
  } else if error.is_redirect() {
    "redirect"
  } else {
    "request"
  };
  let mut detail = format!("{stage}: {category}");
  let mut cause = error.source();
  while let Some(err) = cause {
    // reqwest and hyper-util keep their SOCKS error types private.
    // Match only fixed library messages and emit controlled labels.
    let proxy_cause = match err.to_string().as_str() {
      "error resolving for socks proxy" => Some("socks_target_dns"),
      "error connecting to socks proxy" => Some("socks_connect"),
      "credentials not accepted" => Some("socks_auth_rejected"),
      "server does not support user/pass authentication"
      | "server implements authentication incorrectly" => Some("socks_auth_negotiation"),
      _ => None,
    };
    if let Some(label) = proxy_cause {
      detail.push_str(" / ");
      detail.push_str(label);
    }
    if let Some(io) = err.downcast_ref::<std::io::Error>() {
      detail.push_str(&format!(" / io:{:?}", io.kind()));
      if let Some(code) = io.raw_os_error() {
        detail.push_str(&format!("({code})"));
      }
    }
    cause = err.source();
  }
  detail
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
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  #[tokio::test]
  async fn control_plane_reqwest_sends_domain_and_credentials_to_socks_proxy() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = ProxyConfig::parse(&format!(
      "socks5://user%40test:pass%3Aword@{}",
      proxy.local_addr().unwrap()
    ))
    .unwrap();
    let server = tokio::spawn(async move {
      let (mut stream, _) = proxy.accept().await.unwrap();
      assert_eq!(stream.read_u8().await.unwrap(), 5);
      let mut methods = vec![0; stream.read_u8().await.unwrap() as usize];
      stream.read_exact(&mut methods).await.unwrap();
      assert!(methods.contains(&2));
      stream.write_all(&[5, 2]).await.unwrap();
      assert_eq!(stream.read_u8().await.unwrap(), 1);
      let mut username = vec![0; stream.read_u8().await.unwrap() as usize];
      stream.read_exact(&mut username).await.unwrap();
      assert_eq!(username, b"user@test");
      let mut password = vec![0; stream.read_u8().await.unwrap() as usize];
      stream.read_exact(&mut password).await.unwrap();
      assert_eq!(password, b"pass:word");
      stream.write_all(&[1, 0]).await.unwrap();
      let mut header = [0; 4];
      stream.read_exact(&mut header).await.unwrap();
      assert_eq!(header, [5, 1, 0, 3]);
      let mut domain = vec![0; stream.read_u8().await.unwrap() as usize];
      stream.read_exact(&mut domain).await.unwrap();
      assert_eq!(domain, b"lease-target.invalid");
      assert_eq!(stream.read_u16().await.unwrap(), 80);
      stream.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await.unwrap();
      let mut request = Vec::new();
      while !request.ends_with(b"\r\n\r\n") {
        assert!(request.len() < 4096);
        request.push(stream.read_u8().await.unwrap());
      }
      assert!(request.starts_with(b"GET /api/session?vm_state=live HTTP/1.1\r\n"));
      stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
        .await
        .unwrap();
    });
    let fetch = ReqwestFetch::for_proxy(&config).unwrap();
    let result = fetch.get("http://lease-target.invalid/api/session?vm_state=live", &[]).await;
    assert_eq!(result.unwrap(), (200, b"{}".to_vec()));
    server.await.unwrap();
  }

  #[tokio::test]
  async fn control_plane_reqwest_does_not_fall_back_after_socks_rejection() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = ProxyConfig::parse(&format!("socks5://{}", proxy.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
      let (mut stream, _) = proxy.accept().await.unwrap();
      let mut greeting = [0; 3];
      stream.read_exact(&mut greeting).await.unwrap();
      assert_eq!(greeting, [5, 1, 0]);
      stream.write_all(&[5, 0xff]).await.unwrap();
    });
    let fetch = ReqwestFetch::for_proxy(&config).unwrap();
    let url = format!("http://{}/api/hatch/lease-vm", target.local_addr().unwrap());
    let error = tokio::select! {
      result = fetch.post(&url, &[], b"") => result.unwrap_err(),
      result = target.accept() => panic!("unexpected direct connection: {result:?}"),
    };
    assert!(error.contains("socks_connect"), "{error}");
    assert!(!error.contains(&url), "{error}");
    server.await.unwrap();
  }

  #[test]
  fn compose_uri_encodes_special_chars() {
    let uri = compose_uri("socks5://host:1080", Some("u@x"), Some("p@ss:w0rd"));
    assert_eq!(uri, "socks5://u%40x:p%40ss%3Aw0rd@host:1080");
    let parsed = ProxyConfig::parse(&uri).unwrap();
    assert_eq!(parsed.display_safe(), "socks5://host:1080");
  }

  #[test]
  fn resolve_without_setting_is_direct() {
    let dir = std::env::temp_dir().join(format!("egress-test-{}", uuid::Uuid::new_v4()));
    let secrets = Secrets::new(&dir);
    assert!(!AccountEgress::resolve(None, &secrets).unwrap().is_proxy());
    let disabled = ProxySetting {
      pool_entry_id: None,
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
      pool_entry_id: None,
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
      pool_entry_id: None,
      uri: "socks5://alice@p.example:1080".to_string(),
      username: None,
      credential_ref: Some("ref-1".to_string()),
      enabled: true,
    };
    let egress = AccountEgress::resolve(Some(&setting), &secrets).unwrap();
    assert!(egress.is_proxy());
    assert_eq!(egress.display, "socks5://p.example:1080");
    let _ = std::fs::remove_dir_all(&dir);
  }
}
