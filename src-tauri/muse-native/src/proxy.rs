//! 原生侧逐客户端代理（Issue #14 #11 / 开发计划 C3、补充 W）。
//!
//! 范围：HTTP CONNECT（http://）、经 TLS 的 HTTP CONNECT（https://）、
//! SOCKS5（socks5://，含用户名密码子协商）。本模块只负责「打通到目标的字节
//! 隧道」，目标站点的 TLS / WebSocket 握手由调用方在返回的 TunnelStream 上完成
//! （见后续 gateway 接入）。
//!
//! 安全红线（与开发计划 §3.5 / §4 一致）：
//! - **结构性 fail-closed**：`EgressPolicy::Proxy` 分支只调用隧道连接，任何失败
//!   一律 `Err`，代码里没有「代理失败后改用 Direct」的回退路径。测试断言代理
//!   拒绝时目标不会收到任何直连。
//! - 认证密码用 `Zeroizing<String>` 持有，配置与日志只输出脱敏串（display_safe）。
//! - 不解析、不执行目标数据；不读取代理返回的 body（逐字节读到 \r\n\r\n 即止）。

use std::fmt;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HEAD: usize = 64 * 1024;
/// SOCKS5 用户名/密码子协商的长度上限（RFC 1929：ULEN/PLEN 各 1 字节）。
/// HTTP Basic 无硬限制，但统一按此校验可避免 `len as u8` 截断出错误认证包。
pub const MAX_CREDENTIAL_LEN: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyScheme {
  Http,
  Https,
  Socks5,
}

/// 解析后的代理配置。密码只在内存中以 Zeroizing 持有；Debug / Display 均脱敏。
#[derive(Clone)]
pub struct ProxyConfig {
  scheme: ProxyScheme,
  host: String,
  port: u16,
  username: Option<String>,
  password: Option<Zeroizing<String>>,
}

impl fmt::Debug for ProxyConfig {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ProxyConfig")
      .field("scheme", &self.scheme)
      .field("host", &self.host)
      .field("port", &self.port)
      .field("has_credentials", &self.has_credentials())
      .field("password", &"<redacted>")
      .finish()
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyError {
  InvalidUri(String),
  UnsupportedScheme(String),
  MissingCredentials,
  /// 用户名/密码超过 SOCKS5 单字节长度上限。
  CredentialTooLong(usize),
  AuthFailed,
  ConnectRejected(u16),
  Socks5Rejected(u8),
  Socks5NoAcceptableMethod,
  Io(String),
  Tls(String),
  Timeout(&'static str),
  HeadTooLarge,
  UnexpectedEof,
  BadStatus(String),
}

impl fmt::Display for ProxyError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ProxyError::InvalidUri(m) => write!(f, "代理 URI 非法: {m}"),
      ProxyError::UnsupportedScheme(s) => write!(f, "不支持的代理协议: {s}"),
      ProxyError::MissingCredentials => write!(f, "代理要求认证但未提供用户名/密码"),
      ProxyError::CredentialTooLong(n) => {
        write!(f, "代理用户名/密码超过 {MAX_CREDENTIAL_LEN} 字节（{n} 字节）")
      }
      ProxyError::AuthFailed => write!(f, "代理认证失败（407 / SOCKS5 认证未通过）"),
      ProxyError::ConnectRejected(s) => write!(f, "代理拒绝 CONNECT: HTTP {s}"),
      ProxyError::Socks5Rejected(c) => write!(f, "SOCKS5 目标连接被拒绝: REP={c}"),
      ProxyError::Socks5NoAcceptableMethod => write!(f, "SOCKS5 无双方都可接受的认证方法"),
      ProxyError::Io(m) => write!(f, "代理 I/O 错误: {m}"),
      ProxyError::Tls(m) => write!(f, "到代理的 TLS 建立失败: {m}"),
      ProxyError::Timeout(w) => write!(f, "{w} 超时"),
      ProxyError::HeadTooLarge => write!(f, "代理响应头过大"),
      ProxyError::UnexpectedEof => write!(f, "代理在响应完成前关闭连接"),
      ProxyError::BadStatus(s) => write!(f, "代理响应状态行无法解析: {s}"),
    }
  }
}

impl std::error::Error for ProxyError {}

impl From<io::Error> for ProxyError {
  fn from(e: io::Error) -> Self {
    ProxyError::Io(e.to_string())
  }
}

/// 代理端点（不含任何凭据），用于把配置下发给平台原生层。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyEndpoint {
  /// `http` | `https` | `socks5`
  pub scheme: &'static str,
  pub host: String,
  pub port: u16,
  pub username: Option<String>,
}

impl ProxyConfig {
  /// 解析 `http[s]://[user:pass@]host[:port]` 或 `socks5://[...]`。
  pub fn parse(uri: &str) -> Result<Self, ProxyError> {
    let (scheme, rest) = uri
      .split_once("://")
      .ok_or_else(|| ProxyError::InvalidUri("缺少 :// 分隔符".to_string()))?;
    let scheme = scheme_from_str(scheme)?;
    let rest = rest
      .split('/')
      .next()
      .ok_or_else(|| ProxyError::InvalidUri("缺少权限部分".to_string()))?;
    if rest.is_empty() {
      return Err(ProxyError::InvalidUri("缺少 host".to_string()));
    }

    let (authority, userinfo) = match rest.rsplit_once('@') {
      Some((u, a)) => (a, Some(u)),
      None => (rest, None),
    };

    let (host, port) = parse_host_port(authority, scheme)?;

    let (username, password) = match userinfo {
      Some(ui) => {
        let (u, p) = ui
          .split_once(':')
          .ok_or_else(|| ProxyError::InvalidUri("认证信息缺少 ':'".to_string()))?;
        // 凭据按 RFC 3986 百分号编码传入；这里解码回真实字节再发认证。
        let u = percent_decode(u)?;
        let p = percent_decode(p)?;
        if u.is_empty() || p.is_empty() {
          return Err(ProxyError::InvalidUri("认证用户名或密码为空".to_string()));
        }
        // SOCKS5 的 ULEN/PLEN 是单字节；超长会被 `as u8` 截断，发出错误认证帧。
        // 这里在解析阶段就拒绝，避免「看起来配置了、实际认证包是坏的」。
        if u.len() > MAX_CREDENTIAL_LEN {
          return Err(ProxyError::InvalidUri(format!(
            "认证用户名超过 {MAX_CREDENTIAL_LEN} 字节（{} 字节）",
            u.len()
          )));
        }
        if p.len() > MAX_CREDENTIAL_LEN {
          return Err(ProxyError::InvalidUri(format!(
            "认证密码超过 {MAX_CREDENTIAL_LEN} 字节（{} 字节）",
            p.len()
          )));
        }
        (
          Some(u),
          Some(Zeroizing::new(p)),
        )
      }
      None => (None, None),
    };

    Ok(Self {
      scheme,
      host,
      port,
      username,
      password,
    })
  }

  /// 仅校验 URI 形状（scheme / host / port / userinfo 语法），不要求提供密码。
  /// 供配置保存前调用：密码存在 Keychain，`uri` 里只允许出现用户名。
  pub fn validate_shape(uri: &str) -> Result<(), ProxyError> {
    let (scheme, rest) = uri
      .split_once("://")
      .ok_or_else(|| ProxyError::InvalidUri("缺少 :// 分隔符".to_string()))?;
    let scheme = scheme_from_str(scheme)?;
    let rest = rest.split('/').next().unwrap_or("");
    if rest.is_empty() {
      return Err(ProxyError::InvalidUri("缺少 host".to_string()));
    }
    let (authority, userinfo) = match rest.rsplit_once('@') {
      Some((u, a)) => (a, Some(u)),
      None => (rest, None),
    };
    if let Some(ui) = userinfo {
      if ui.is_empty() {
        return Err(ProxyError::InvalidUri("代理用户名为空".to_string()));
      }
      if let Some((u, p)) = ui.split_once(':') {
        if u.is_empty() || p.is_empty() {
          return Err(ProxyError::InvalidUri("代理用户名或密码为空".to_string()));
        }
        if u.len() > MAX_CREDENTIAL_LEN || p.len() > MAX_CREDENTIAL_LEN {
          return Err(ProxyError::CredentialTooLong(u.len().max(p.len())));
        }
      }
    }
    parse_host_port(authority, scheme)?;
    Ok(())
  }

  /// 解析出**端点信息**（scheme / host / port / 用户名），不需要也不保留密码。
  ///
  /// 用于把代理配置下发给平台原生层（如 iOS 的
  /// `WKWebsiteDataStore.proxyConfigurations`）：原生层拿不到也不需要口令，
  /// 口令由它自己从同一个 Keychain 条目读取。
  pub fn parse_endpoint(uri: &str) -> Result<ProxyEndpoint, ProxyError> {
    Self::validate_shape(uri)?;
    let (scheme, rest) = uri
      .split_once("://")
      .ok_or_else(|| ProxyError::InvalidUri("缺少 :// 分隔符".to_string()))?;
    let scheme = scheme_from_str(scheme)?;
    let rest = rest.split('/').next().unwrap_or("");
    let (authority, userinfo) = match rest.rsplit_once('@') {
      Some((u, a)) => (a, Some(u)),
      None => (rest, None),
    };
    let (host, port) = parse_host_port(authority, scheme)?;
    let username = match userinfo {
      Some(ui) => {
        let (u, _p) = ui.split_once(':').unwrap_or((ui, ""));
        if u.is_empty() {
          return Err(ProxyError::InvalidUri("代理用户名为空".to_string()));
        }
        Some(percent_decode(u)?)
      }
      None => None,
    };
    Ok(ProxyEndpoint {
      scheme: scheme_name(scheme),
      host,
      port,
      username,
    })
  }

  pub fn scheme(&self) -> ProxyScheme {
    self.scheme
  }

  pub fn host(&self) -> &str {
    &self.host
  }

  pub fn port(&self) -> u16 {
    self.port
  }

  /// `http` | `https` | `socks5`。
  pub fn scheme_name(&self) -> &'static str {
    scheme_name(self.scheme)
  }

  /// 脱敏串，可安全进日志 / UI，省略全部认证信息。
  pub fn display_safe(&self) -> String {
    format!("{}://{}", self.scheme_name(), self.authority())
  }

  fn authority(&self) -> String {
    if self.host.contains(':') {
      format!("[{}]:{}", self.host, self.port)
    } else {
      format!("{}:{}", self.host, self.port)
    }
  }

  /// 组装**含凭据**的 reqwest 代理 URI。SOCKS 目标域名始终由代理解析，
  /// 与原生 WSS 隧道及 WKWebView 的 SOCKS5 策略一致。
  /// 返回值含明文密码：不得写日志、不得进 UI、不得持久化。
  pub fn to_reqwest_proxy_uri(&self) -> String {
    let scheme = match self.scheme {
      ProxyScheme::Socks5 => "socks5h",
      _ => self.scheme_name(),
    };
    let user = match (&self.username, &self.password) {
      (Some(u), Some(p)) => format!("{}:{}@", percent_encode(u), percent_encode(p.as_str())),
      (Some(u), None) => format!("{}@", percent_encode(u)),
      _ => String::new(),
    };
    format!("{scheme}://{user}{}", self.authority())
  }

  pub fn has_credentials(&self) -> bool {
    self.username.is_some() && self.password.is_some()
  }

  /// 打通到目标的隧道。任何失败都返回 Err；调用方不得回退直连。
  pub async fn connect(&self, target_host: &str, target_port: u16) -> Result<TunnelStream, ProxyError> {
    tokio::time::timeout(CONNECT_TIMEOUT, self.connect_inner(target_host, target_port))
      .await
      .map_err(|_| ProxyError::Timeout("代理连接"))?
  }

  async fn connect_inner(&self, target_host: &str, target_port: u16) -> Result<TunnelStream, ProxyError> {
    match self.scheme {
      ProxyScheme::Http | ProxyScheme::Https => {
        let tcp = TcpStream::connect((self.host.as_str(), self.port)).await?;
        let mut stream = if self.scheme == ProxyScheme::Https {
          TunnelStream::Tls(Box::new(tls_to_proxy(tcp, &self.host).await?))
        } else {
          TunnelStream::Plain(tcp)
        };
        let req = self.connect_request(target_host, target_port);
        stream.write_all(req.as_bytes()).await?;
        let head = read_head(&mut stream).await?;
        let status = parse_status(&head)?;
        if status == 407 {
          return Err(ProxyError::AuthFailed);
        }
        if status != 200 {
          return Err(ProxyError::ConnectRejected(status));
        }
        Ok(stream)
      }
      ProxyScheme::Socks5 => {
        let tcp = TcpStream::connect((self.host.as_str(), self.port)).await?;
        socks5_handshake(tcp, self, target_host, target_port).await
      }
    }
  }

  fn connect_request(&self, target_host: &str, target_port: u16) -> String {
    let authority = format!("{target_host}:{target_port}");
    let mut req = format!(
      "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: Keep-Alive\r\n"
    );
    if let (Some(u), Some(p)) = (&self.username, &self.password) {
      let token = base64_encode(format!("{u}:{}", p.as_str()).as_bytes());
      req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    req.push_str("\r\n");
    req
  }
}

/// 出站策略。`Proxy` 是 fail-closed 的：没有从它回退到 `Direct` 的代码路径。
#[derive(Clone)]
pub enum EgressPolicy {
  /// 仅供未配置代理的账号显式直连（由上层按配置选择，故障逻辑不得切换到此值）。
  Direct,
  /// 必须经代理；隧道失败即返回 Err。
  Proxy(ProxyConfig),
}

impl EgressPolicy {
  pub async fn tunnel(
    &self,
    target_host: &str,
    target_port: u16,
  ) -> Result<TunnelStream, ProxyError> {
    match self {
      EgressPolicy::Direct => {
        let tcp = TcpStream::connect((target_host, target_port)).await?;
        Ok(TunnelStream::Plain(tcp))
      }
      EgressPolicy::Proxy(cfg) => cfg.connect(target_host, target_port).await,
    }
  }
}

/// 代理隧道的统一字节流。调用方在其上做目标 TLS + WebSocket 握手。
#[derive(Debug)]
pub enum TunnelStream {
  Plain(TcpStream),
  Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl AsyncRead for TunnelStream {
  fn poll_read(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    match self.get_mut() {
      TunnelStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
      TunnelStream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
    }
  }
}

impl AsyncWrite for TunnelStream {
  fn poll_write(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &[u8],
  ) -> Poll<io::Result<usize>> {
    match self.get_mut() {
      TunnelStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
      TunnelStream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
    }
  }

  fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    match self.get_mut() {
      TunnelStream::Plain(s) => Pin::new(s).poll_flush(cx),
      TunnelStream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
    }
  }

  fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    match self.get_mut() {
      TunnelStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
      TunnelStream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
    }
  }
}

// ---- 内部工具 ----

fn scheme_name(s: ProxyScheme) -> &'static str {
  match s {
    ProxyScheme::Http => "http",
    ProxyScheme::Https => "https",
    ProxyScheme::Socks5 => "socks5",
  }
}

fn scheme_from_str(s: &str) -> Result<ProxyScheme, ProxyError> {
  match s.to_ascii_lowercase().as_str() {
    "http" => Ok(ProxyScheme::Http),
    "https" => Ok(ProxyScheme::Https),
    "socks5" | "socks5h" => Ok(ProxyScheme::Socks5),
    other => Err(ProxyError::UnsupportedScheme(other.to_string())),
  }
}

/// 解码 `%XX`。非法编码直接报错，避免把坏口令静默发出去。
fn percent_decode(s: &str) -> Result<String, ProxyError> {
  if !s.contains('%') {
    return Ok(s.to_string());
  }
  let bytes = s.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'%' {
      if i + 2 >= bytes.len() {
        return Err(ProxyError::InvalidUri("百分号编码不完整".to_string()));
      }
      let hi = hex_val(bytes[i + 1])
        .ok_or_else(|| ProxyError::InvalidUri("百分号编码非法".to_string()))?;
      let lo = hex_val(bytes[i + 2])
        .ok_or_else(|| ProxyError::InvalidUri("百分号编码非法".to_string()))?;
      out.push(hi * 16 + lo);
      i += 3;
    } else {
      out.push(bytes[i]);
      i += 1;
    }
  }
  String::from_utf8(out)
    .map_err(|_| ProxyError::InvalidUri("百分号解码后不是合法 UTF-8".to_string()))
}

fn hex_val(b: u8) -> Option<u8> {
  match b {
    b'0'..=b'9' => Some(b - b'0'),
    b'a'..=b'f' => Some(b - b'a' + 10),
    b'A'..=b'F' => Some(b - b'A' + 10),
    _ => None,
  }
}

/// userinfo 的百分号编码：只保留 RFC 3986 unreserved 字符。
fn percent_encode(s: &str) -> String {
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

fn parse_host_port(authority: &str, scheme: ProxyScheme) -> Result<(String, u16), ProxyError> {
  let default_port = match scheme {
    ProxyScheme::Http => 80,
    ProxyScheme::Https => 443,
    ProxyScheme::Socks5 => 1080,
  };
  // IPv6 字面量：[::1]:port
  if authority.starts_with('[') {
    let end = authority
      .find(']')
      .ok_or_else(|| ProxyError::InvalidUri("IPv6 缺少 ']'".to_string()))?;
    let host = authority[1..end].to_string();
    if host.is_empty() {
      return Err(ProxyError::InvalidUri("IPv6 host 为空".to_string()));
    }
    let port = if authority.len() > end + 1 {
      let rest = &authority[end + 1..];
      let port_str = rest
        .strip_prefix(':')
        .ok_or_else(|| ProxyError::InvalidUri("']' 后应为 ':port'".to_string()))?;
      parse_port(port_str)?
    } else {
      default_port
    };
    return Ok((host, port));
  }
  match authority.rsplit_once(':') {
    Some((h, p)) => {
      if h.is_empty() {
        return Err(ProxyError::InvalidUri("host 为空".to_string()));
      }
      Ok((h.to_string(), parse_port(p)?))
    }
    None => {
      if authority.is_empty() {
        Err(ProxyError::InvalidUri("host 为空".to_string()))
      } else {
        Ok((authority.to_string(), default_port))
      }
    }
  }
}

fn parse_port(s: &str) -> Result<u16, ProxyError> {
  if s.is_empty() {
    return Err(ProxyError::InvalidUri("端口为空".to_string()));
  }
  match s.parse::<u16>() {
    Ok(0) => Err(ProxyError::InvalidUri("端口不能为 0".to_string())),
    Ok(p) => Ok(p),
    Err(_) => Err(ProxyError::InvalidUri(format!("非法端口: {s}"))),
  }
}

fn ensure_crypto_provider() {
  static INIT: std::sync::Once = std::sync::Once::new();
  INIT.call_once(|| {
    let _ = rustls::crypto::ring::default_provider().install_default();
  });
}

async fn tls_to_proxy(
  tcp: TcpStream,
  host: &str,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, ProxyError> {
  ensure_crypto_provider();
  let mut roots = rustls::RootCertStore::empty();
  roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
  let config = rustls::ClientConfig::builder()
    .with_root_certificates(roots)
    .with_no_client_auth();
  let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config));
  let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
    .map_err(|e| ProxyError::Tls(format!("非法代理主机名: {e}")))?;
  connector
    .connect(server_name, tcp)
    .await
    .map_err(|e| ProxyError::Tls(e.to_string()))
}

/// 逐字节读到 \r\n\r\n，返回响应头；stream 位置恰好停在 body 首字节（不吞隧道数据）。
async fn read_head<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>, ProxyError> {
  let mut head = Vec::with_capacity(512);
  let mut byte = [0u8; 1];
  loop {
    let n = stream.read(&mut byte).await?;
    if n == 0 {
      return Err(ProxyError::UnexpectedEof);
    }
    head.push(byte[0]);
    if head.len() >= 4 && &head[head.len() - 4..] == b"\r\n\r\n" {
      return Ok(head);
    }
    if head.len() > MAX_HEAD {
      return Err(ProxyError::HeadTooLarge);
    }
  }
}

fn parse_status(head: &[u8]) -> Result<u16, ProxyError> {
  let text = std::str::from_utf8(head).unwrap_or("");
  let line = text.lines().next().unwrap_or("");
  let mut parts = line.split_whitespace();
  let _version = parts.next();
  let code = parts
    .next()
    .ok_or_else(|| ProxyError::BadStatus(line.to_string()))?;
  code.parse::<u16>()
    .map_err(|_| ProxyError::BadStatus(line.to_string()))
}

async fn socks5_handshake(
  tcp: TcpStream,
  cfg: &ProxyConfig,
  target_host: &str,
  target_port: u16,
) -> Result<TunnelStream, ProxyError> {
  let mut stream = tcp;
  let has_auth = cfg.username.is_some() && cfg.password.is_some();
  // greeting
  if has_auth {
    stream.write_all(&[0x05, 0x01, 0x02]).await?;
  } else {
    stream.write_all(&[0x05, 0x01, 0x00]).await?;
  }
  let method_reply = read_exact_2(&mut stream).await?;
  if method_reply[0] != 0x05 {
    return Err(ProxyError::Socks5Rejected(method_reply[0]));
  }
  match method_reply[1] {
    0x00 => {}
    0x02 => {
      let (Some(u), Some(p)) = (&cfg.username, &cfg.password) else {
        return Err(ProxyError::MissingCredentials);
      };
      // parse() 已校验长度；这里是最后一道防线，避免 `as u8` 静默截断。
      if u.len() > MAX_CREDENTIAL_LEN || p.len() > MAX_CREDENTIAL_LEN {
        return Err(ProxyError::CredentialTooLong(u.len().max(p.len())));
      }
      let mut auth = vec![0x01, u.len() as u8];
      auth.extend_from_slice(u.as_bytes());
      auth.push(p.len() as u8);
      auth.extend_from_slice(p.as_bytes());
      stream.write_all(&auth).await?;
      let auth_reply = read_exact_2(&mut stream).await?;
      if auth_reply != [0x01, 0x00] {
        return Err(ProxyError::AuthFailed);
      }
    }
    0xFF => return Err(ProxyError::Socks5NoAcceptableMethod),
    other => return Err(ProxyError::Socks5Rejected(other)),
  }

  // CONNECT 请求：VER CMD RSV ATYP ADDR PORT
  let mut req = vec![0x05, 0x01, 0x00];
  req.extend_from_slice(&encode_socks5_addr(target_host)?);
  req.extend_from_slice(&target_port.to_be_bytes());
  stream.write_all(&req).await?;

  // reply：VER REP RSV ATYP BND.ADDR BND.PORT
  let head = read_exact_n(&mut stream, 4).await?;
  if head[0] != 0x05 {
    return Err(ProxyError::Socks5Rejected(head[0]));
  }
  if head[1] != 0x00 {
    return Err(ProxyError::Socks5Rejected(head[1]));
  }
  let addr_len = match head[3] {
    0x01 => 4,
    0x04 => 16,
    0x03 => read_exact_n(&mut stream, 1).await?[0] as usize,
    other => return Err(ProxyError::Socks5Rejected(other)),
  };
  read_exact_n(&mut stream, addr_len + 2).await?; // BND.ADDR + BND.PORT
  Ok(TunnelStream::Plain(stream))
}

fn encode_socks5_addr(host: &str) -> Result<Vec<u8>, ProxyError> {
  if let Ok(ip) = host.parse::<IpAddr>() {
    match ip {
      IpAddr::V4(v4) => {
        let mut out = vec![0x01];
        out.extend_from_slice(&v4.octets());
        Ok(out)
      }
      IpAddr::V6(v6) => {
        let mut out = vec![0x04];
        out.extend_from_slice(&v6.octets());
        Ok(out)
      }
    }
  } else {
    let bytes = host.as_bytes();
    if bytes.len() > 255 {
      return Err(ProxyError::InvalidUri("SOCKS5 域名过长".to_string()));
    }
    let mut out = vec![0x03, bytes.len() as u8];
    out.extend_from_slice(bytes);
    Ok(out)
  }
}

async fn read_exact_2<S: AsyncRead + Unpin>(s: &mut S) -> Result<[u8; 2], ProxyError> {
  let mut buf = [0u8; 2];
  s.read_exact(&mut buf).await?;
  Ok(buf)
}

async fn read_exact_n<S: AsyncRead + Unpin>(s: &mut S, n: usize) -> Result<Vec<u8>, ProxyError> {
  let mut buf = vec![0u8; n];
  s.read_exact(&mut buf).await?;
  Ok(buf)
}

fn base64_encode(data: &[u8]) -> String {
  const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
  let mut i = 0;
  while i + 3 <= data.len() {
    let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | data[i + 2] as u32;
    out.push(T[((n >> 18) & 63) as usize] as char);
    out.push(T[((n >> 12) & 63) as usize] as char);
    out.push(T[((n >> 6) & 63) as usize] as char);
    out.push(T[(n & 63) as usize] as char);
    i += 3;
  }
  let rem = &data[i..];
  if rem.len() == 1 {
    let n = (rem[0] as u32) << 16;
    out.push(T[((n >> 18) & 63) as usize] as char);
    out.push(T[((n >> 12) & 63) as usize] as char);
    out.push_str("==");
  } else if rem.len() == 2 {
    let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
    out.push(T[((n >> 18) & 63) as usize] as char);
    out.push(T[((n >> 12) & 63) as usize] as char);
    out.push(T[((n >> 6) & 63) as usize] as char);
    out.push('=');
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::io::AsyncWriteExt;
  use tokio::net::TcpListener;

  async fn mock<F, Fut>(handler: F) -> (u16, tokio::task::JoinHandle<()>)
  where
    F: FnOnce(TcpStream) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
  {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
      let (stream, _) = listener.accept().await.unwrap();
      handler(stream).await;
    });
    (port, handle)
  }

  async fn read_http_request(stream: &mut TcpStream) -> String {
    let head = read_head(stream).await.unwrap();
    String::from_utf8_lossy(&head).to_string()
  }

  #[test]
  fn parse_schemes_default_ports() {
    assert_eq!(ProxyConfig::parse("http://p.example").unwrap().scheme(), ProxyScheme::Http);
    assert_eq!(ProxyConfig::parse("http://p.example").unwrap().port, 80);
    assert_eq!(ProxyConfig::parse("https://p.example:8443").unwrap().port, 8443);
    assert_eq!(ProxyConfig::parse("socks5://p.example").unwrap().port, 1080);
    assert_eq!(ProxyConfig::parse("socks5h://p.example").unwrap().scheme(), ProxyScheme::Socks5);
  }

  #[test]
  fn parse_auth_and_display_safe_redacts_credentials() {
    let cfg = ProxyConfig::parse("http://alice:s3cr3t@p.example:8080").unwrap();
    assert_eq!(cfg.username.as_deref(), Some("alice"));
    assert_eq!(cfg.password.as_deref().map(|p| p.as_str()), Some("s3cr3t"));
    let safe = cfg.display_safe();
    assert_eq!(safe, "http://p.example:8080");
    let dbg = format!("{cfg:?}");
    assert!(!dbg.contains("alice"));
    assert!(!dbg.contains("s3cr3t"));
  }

  #[test]
  fn reqwest_proxy_uri_preserves_connect_and_uses_remote_socks_dns() {
    for scheme in ["http", "https", "socks5", "socks5h"] {
      let cfg = ProxyConfig::parse(&format!("{scheme}://u%40x:p%3Ass@[::1]:1080")).unwrap();
      let expected = if scheme == "socks5" { "socks5h" } else { scheme };
      assert_eq!(cfg.to_reqwest_proxy_uri(), format!("{expected}://u%40x:p%3Ass@[::1]:1080"));
    }
  }

  #[test]
  fn parse_ipv6_host() {
    let cfg = ProxyConfig::parse("http://[::1]:3128").unwrap();
    assert_eq!(cfg.host, "::1");
    assert_eq!(cfg.port, 3128);
  }

  #[test]
  fn parse_rejects_bad_input() {
    assert!(matches!(ProxyConfig::parse("p.example"), Err(ProxyError::InvalidUri(_))));
    assert!(matches!(
      ProxyConfig::parse("ftp://p.example"),
      Err(ProxyError::UnsupportedScheme(_))
    ));
    assert!(matches!(
      ProxyConfig::parse("http://p.example:99999"),
      Err(ProxyError::InvalidUri(_))
    ));
    assert!(matches!(
      ProxyConfig::parse("http://alice@p.example"),
      Err(ProxyError::InvalidUri(_))
    ));
  }

  #[test]
  fn base64_known_vectors() {
    assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
    assert_eq!(base64_encode(b"a"), "YQ==");
    assert_eq!(base64_encode(b"ab"), "YWI=");
    assert_eq!(base64_encode(b""), "");
  }

  #[test]
  fn credential_length_boundary() {
    let ok_user = "u".repeat(MAX_CREDENTIAL_LEN);
    let ok_pass = "p".repeat(MAX_CREDENTIAL_LEN);
    assert!(
      ProxyConfig::parse(&format!("socks5://{ok_user}:{ok_pass}@p.example:1080")).is_ok(),
      "255 字节用户名/密码应被接受"
    );
    let too_long = "x".repeat(MAX_CREDENTIAL_LEN + 1);
    match ProxyConfig::parse(&format!("socks5://{too_long}:p@p.example:1080")) {
      Err(ProxyError::InvalidUri(m)) => assert!(m.contains("用户名"), "{m}"),
      other => panic!("超长用户名必须被拒绝，实际: {other:?}"),
    }
    match ProxyConfig::parse(&format!("socks5://u:{too_long}@p.example:1080")) {
      Err(ProxyError::InvalidUri(m)) => assert!(m.contains("密码"), "{m}"),
      other => panic!("超长密码必须被拒绝，实际: {other:?}"),
    }
    // UTF-8 多字节按字节计：128 个中文字符 = 384 字节，超限。
    let cjk = "密".repeat(128);
    assert!(ProxyConfig::parse(&format!("socks5://u:{cjk}@p.example:1080")).is_err());
  }

  #[test]
  fn parse_endpoint_never_requires_or_returns_password() {
    // 只有用户名（无密码）：endpoint 能解析出来，供原生层下发。
    let ep = ProxyConfig::parse_endpoint("http://user@p.example:8080").unwrap();
    assert_eq!(ep.scheme, "http");
    assert_eq!(ep.host, "p.example");
    assert_eq!(ep.port, 8080);
    assert_eq!(ep.username.as_deref(), Some("user"));

    // 带密码的 URI 也允许：endpoint 只取用户名，绝不带出口令。
    let ep = ProxyConfig::parse_endpoint("socks5://u%40c:p%40ss@p.example:1080").unwrap();
    assert_eq!(ep.scheme, "socks5");
    assert_eq!(ep.username.as_deref(), Some("u@c"));

    // 无认证：username 为 None。
    let ep = ProxyConfig::parse_endpoint("https://p.example:3128").unwrap();
    assert!(ep.username.is_none());

    // 形状非法一律拒绝，避免把坏配置下发给原生层。
    assert!(ProxyConfig::parse_endpoint("p.example:8080").is_err());
    assert!(ProxyConfig::parse_endpoint("http://p.example:0").is_err());
    assert!(ProxyConfig::parse_endpoint("ftp://p.example:21").is_err());
  }

  #[test]
  fn empty_credential_rejected() {
    assert!(matches!(
      ProxyConfig::parse("socks5://:pass@p.example:1080"),
      Err(ProxyError::InvalidUri(_))
    ));
    assert!(matches!(
      ProxyConfig::parse("socks5://user:@p.example:1080"),
      Err(ProxyError::InvalidUri(_))
    ));
  }

  #[test]
  fn percent_encoded_credentials_are_decoded() {
    let cfg =
      ProxyConfig::parse("socks5://user%40corp:p%40ss%3Aw0rd%21@p.example:1080").unwrap();
    assert_eq!(cfg.username.as_deref(), Some("user@corp"));
    assert_eq!(
      cfg.password.as_deref().map(|p| p.as_str()),
      Some("p@ss:w0rd!")
    );
  }

  #[test]
  fn bad_percent_encoding_rejected() {
    assert!(matches!(
      ProxyConfig::parse("socks5://u:%ZZ@p.example:1080"),
      Err(ProxyError::InvalidUri(_))
    ));
    assert!(matches!(
      ProxyConfig::parse("socks5://u:%4@p.example:1080"),
      Err(ProxyError::InvalidUri(_))
    ));
  }

  #[test]
  fn validate_shape_allows_user_only_without_password() {
    assert!(ProxyConfig::validate_shape("socks5://alice@socks.example:1080").is_ok());
    assert!(ProxyConfig::validate_shape("http://p.example:8080").is_ok());
    assert!(ProxyConfig::validate_shape("https://[::1]:8443").is_ok());
    assert!(ProxyConfig::validate_shape("http://:8080").is_err());
    assert!(ProxyConfig::validate_shape("ftp://p.example").is_err());
    assert!(ProxyConfig::validate_shape("http://p.example:0").is_err());
    assert!(ProxyConfig::validate_shape("http://:pass@p.example:8080").is_err());
  }

  #[tokio::test]
  async fn http_connect_request_format_no_auth() {
    let (port, server) = mock(|mut s: TcpStream| async move {
      let req = read_http_request(&mut s).await;
      assert!(req.starts_with("CONNECT target.example:443 HTTP/1.1\r\n"), "{req}");
      assert!(req.contains("Host: target.example:443\r\n"));
      assert!(!req.to_ascii_lowercase().contains("proxy-authorization"));
      s.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .await
        .unwrap();
    })
    .await;
    let cfg = ProxyConfig::parse(&format!("http://127.0.0.1:{port}")).unwrap();
    assert!(cfg.connect("target.example", 443).await.is_ok());
    server.await.unwrap();
  }

  #[tokio::test]
  async fn http_connect_request_includes_basic_auth() {
    let (port, server) = mock(|mut s: TcpStream| async move {
      let req = read_http_request(&mut s).await;
      assert!(req.contains("Proxy-Authorization: Basic dXNlcjpwYXNz\r\n"), "{req}");
      s.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
    })
    .await;
    let cfg = ProxyConfig::parse(&format!("http://user:pass@127.0.0.1:{port}")).unwrap();
    assert!(cfg.connect("target.example", 443).await.is_ok());
    server.await.unwrap();
  }

  #[tokio::test]
  async fn http_connect_does_not_swallow_tunnel_body() {
    let (port, server) = mock(|mut s: TcpStream| async move {
      let _ = read_http_request(&mut s).await;
      s.write_all(b"HTTP/1.1 200 OK\r\n\r\nPAYLOAD").await.unwrap();
    })
    .await;
    let cfg = ProxyConfig::parse(&format!("http://127.0.0.1:{port}")).unwrap();
    let mut stream = cfg.connect("target.example", 443).await.unwrap();
    let mut buf = [0u8; 7];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"PAYLOAD");
    server.await.unwrap();
  }

  #[tokio::test]
  async fn http_connect_407_is_auth_failure() {
    let (port, server) = mock(|mut s: TcpStream| async move {
      let _ = read_http_request(&mut s).await;
      s.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
        .await
        .unwrap();
    })
    .await;
    let cfg = ProxyConfig::parse(&format!("http://127.0.0.1:{port}")).unwrap();
    let err = cfg.connect("target.example", 443).await.unwrap_err();
    assert_eq!(err, ProxyError::AuthFailed);
    server.await.unwrap();
  }

  #[tokio::test]
  async fn http_connect_non_200_is_rejected() {
    let (port, server) = mock(|mut s: TcpStream| async move {
      let _ = read_http_request(&mut s).await;
      s.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await.unwrap();
    })
    .await;
    let cfg = ProxyConfig::parse(&format!("http://127.0.0.1:{port}")).unwrap();
    let err = cfg.connect("target.example", 443).await.unwrap_err();
    assert_eq!(err, ProxyError::ConnectRejected(403));
    server.await.unwrap();
  }

  #[tokio::test]
  async fn socks5_no_auth_domain() {
    let (port, server) = mock(|mut s: TcpStream| async move {
      let greeting = read_exact_2(&mut s).await.unwrap();
      let methods = read_exact_n(&mut s, greeting[1] as usize).await.unwrap();
      assert_eq!(greeting, [0x05, 0x01]);
      assert_eq!(methods, vec![0x00]);
      s.write_all(&[0x05, 0x00]).await.unwrap();
      let req = read_exact_n(&mut s, 4).await.unwrap();
      assert_eq!(&req[..3], &[0x05, 0x01, 0x00]);
      assert_eq!(req[3], 0x03); // domain
      let len = read_exact_n(&mut s, 1).await.unwrap()[0] as usize;
      let host = read_exact_n(&mut s, len).await.unwrap();
      assert_eq!(host, b"target.example");
      let p = read_exact_2(&mut s).await.unwrap();
      assert_eq!(p, 443u16.to_be_bytes());
      s.write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
        .await
        .unwrap();
    })
    .await;
    let cfg = ProxyConfig::parse(&format!("socks5://127.0.0.1:{port}")).unwrap();
    assert!(cfg.connect("target.example", 443).await.is_ok());
    server.await.unwrap();
  }

  #[tokio::test]
  async fn socks5_userpass_auth_and_ipv4() {
    let (port, server) = mock(|mut s: TcpStream| async move {
      assert_eq!(read_exact_2(&mut s).await.unwrap(), [0x05, 0x01]);
      assert_eq!(read_exact_n(&mut s, 1).await.unwrap(), vec![0x02]);
      s.write_all(&[0x05, 0x02]).await.unwrap();
      // 认证帧：VER=1, ULEN=2, "up", PLEN=6, "secret"
      assert_eq!(read_exact_n(&mut s, 2).await.unwrap(), vec![0x01, 2]);
      assert_eq!(read_exact_n(&mut s, 2).await.unwrap(), b"up");
      assert_eq!(read_exact_n(&mut s, 1).await.unwrap(), vec![6]);
      assert_eq!(read_exact_n(&mut s, 6).await.unwrap(), b"secret");
      s.write_all(&[0x01, 0x00]).await.unwrap();
      let req = read_exact_n(&mut s, 4).await.unwrap();
      assert_eq!(req, vec![0x05, 0x01, 0x00, 0x01]);
      assert_eq!(read_exact_n(&mut s, 4).await.unwrap(), vec![10, 0, 0, 7]);
      assert_eq!(read_exact_2(&mut s).await.unwrap(), 8080u16.to_be_bytes());
      s.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
        .unwrap();
    })
    .await;
    let cfg = ProxyConfig::parse(&format!("socks5://up:secret@127.0.0.1:{port}")).unwrap();
    assert!(cfg.connect("10.0.0.7", 8080).await.is_ok());
    server.await.unwrap();
  }

  /// 255 字节用户名的认证帧不能被 `as u8` 截断：ULEN 必须正好是 255。
  #[tokio::test]
  async fn socks5_max_length_user_is_not_truncated() {
    let user = "u".repeat(MAX_CREDENTIAL_LEN);
    let user_for_mock = user.clone();
    let (port, server) = mock(move |mut s: TcpStream| async move {
      assert_eq!(read_exact_2(&mut s).await.unwrap(), [0x05, 0x01]);
      assert_eq!(read_exact_n(&mut s, 1).await.unwrap(), vec![0x02]);
      s.write_all(&[0x05, 0x02]).await.unwrap();
      let head = read_exact_n(&mut s, 2).await.unwrap();
      assert_eq!(head[0], 0x01);
      assert_eq!(head[1], 255, "ULEN 必须为 255，不能被截断为 0 或其他值");
      assert_eq!(
        read_exact_n(&mut s, 255).await.unwrap(),
        user_for_mock.as_bytes()
      );
      assert_eq!(read_exact_n(&mut s, 1).await.unwrap(), vec![3]);
      assert_eq!(read_exact_n(&mut s, 3).await.unwrap(), b"pwd");
      s.write_all(&[0x01, 0x00]).await.unwrap();
      let _ = read_exact_n(&mut s, 4).await.unwrap();
      let _ = read_exact_n(&mut s, 1).await.unwrap();
      let _ = read_exact_n(&mut s, 14).await.unwrap();
      let _ = read_exact_2(&mut s).await.unwrap();
      s.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
        .unwrap();
    })
    .await;
    let cfg = ProxyConfig::parse(&format!("socks5://{user}:pwd@127.0.0.1:{port}")).unwrap();
    assert!(cfg.connect("target.example", 443).await.is_ok());
    server.await.unwrap();
  }

  #[tokio::test]
  async fn socks5_auth_rejected() {
    let (port, server) = mock(|mut s: TcpStream| async move {
      let g = read_exact_2(&mut s).await.unwrap();
      let _ = read_exact_n(&mut s, g[1] as usize).await.unwrap();
      s.write_all(&[0x05, 0x02]).await.unwrap();
      // 认证帧：VER, ULEN, user(2), PLEN, pass(6) = 2+2+1+6 = 11 字节
      let _ = read_exact_n(&mut s, 2).await.unwrap();
      let _ = read_exact_n(&mut s, 2).await.unwrap();
      let _ = read_exact_n(&mut s, 1).await.unwrap();
      let _ = read_exact_n(&mut s, 6).await.unwrap();
      s.write_all(&[0x01, 0x01]).await.unwrap(); // auth failed
    })
    .await;
    let cfg = ProxyConfig::parse(&format!("socks5://up:secret@127.0.0.1:{port}")).unwrap();
    let err = cfg.connect("target.example", 443).await.unwrap_err();
    assert_eq!(err, ProxyError::AuthFailed);
    server.await.unwrap();
  }

  #[tokio::test]
  async fn socks5_target_rejected() {
    let (port, server) = mock(|mut s: TcpStream| async move {
      let g = read_exact_2(&mut s).await.unwrap();
      let _ = read_exact_n(&mut s, g[1] as usize).await.unwrap();
      s.write_all(&[0x05, 0x00]).await.unwrap();
      let head = read_exact_n(&mut s, 4).await.unwrap();
      assert_eq!(head[3], 0x03); // ATYP = domain
      let domain_len = read_exact_n(&mut s, 1).await.unwrap()[0] as usize;
      let _ = read_exact_n(&mut s, domain_len).await.unwrap();
      let _ = read_exact_2(&mut s).await.unwrap();
      s.write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
        .unwrap();
    })
    .await;
    let cfg = ProxyConfig::parse(&format!("socks5://127.0.0.1:{port}")).unwrap();
    let err = cfg.connect("target.example", 443).await.unwrap_err();
    assert_eq!(err, ProxyError::Socks5Rejected(5));
    server.await.unwrap();
  }

  /// fail-closed：代理策略下代理拒绝 CONNECT 时，目标不会收到任何直连。
  #[tokio::test]
  async fn proxy_failure_never_falls_back_to_direct() {
    // 假目标：记录是否有直连到达。
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_port = target.local_addr().unwrap().port();
    let (direct_tx, mut direct_rx) = tokio::sync::oneshot::channel::<()>();
    let target_task = tokio::spawn(async move {
      tokio::select! {
        _ = target.accept() => { let _ = direct_tx.send(()); }
        _ = tokio::time::sleep(Duration::from_millis(300)) => {}
      }
    });

    // 假代理：一律 407 拒绝。
    let (proxy_port, proxy_task) = mock(|mut s: TcpStream| async move {
      let _ = read_http_request(&mut s).await;
      s.write_all(b"HTTP/1.1 407 Need Auth\r\n\r\n").await.unwrap();
    })
    .await;

    let policy = EgressPolicy::Proxy(
      ProxyConfig::parse(&format!("http://127.0.0.1:{proxy_port}")).unwrap(),
    );
    let err = policy
      .tunnel("127.0.0.1", target_port)
      .await
      .unwrap_err();
    assert_eq!(err, ProxyError::AuthFailed);

    target_task.await.unwrap();
    assert!(
      direct_rx.try_recv().is_err(),
      "代理失败后不得直连目标"
    );
    proxy_task.await.unwrap();
  }
}
