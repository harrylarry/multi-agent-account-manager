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
      .field("username", &self.username.as_deref().unwrap_or("<none>"))
      .field("password", &"<redacted>")
      .finish()
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyError {
  InvalidUri(String),
  UnsupportedScheme(String),
  MissingCredentials,
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

impl ProxyConfig {
  /// 解析 `http[s]://[user:pass@]host[:port]` 或 `socks5://[...]`。
  pub fn parse(uri: &str) -> Result<Self, ProxyError> {
    let (scheme, rest) = uri
      .split_once("://")
      .ok_or_else(|| ProxyError::InvalidUri("缺少 :// 分隔符".to_string()))?;
    let scheme = match scheme.to_ascii_lowercase().as_str() {
      "http" => ProxyScheme::Http,
      "https" => ProxyScheme::Https,
      "socks5" | "socks5h" => ProxyScheme::Socks5,
      other => return Err(ProxyError::UnsupportedScheme(other.to_string())),
    };
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
        if u.is_empty() || p.is_empty() {
          return Err(ProxyError::InvalidUri("认证用户名或密码为空".to_string()));
        }
        (
          Some(u.to_string()),
          Some(Zeroizing::new(p.to_string())),
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

  pub fn scheme(&self) -> ProxyScheme {
    self.scheme
  }

  /// 脱敏串，可安全进日志 / UI：scheme://[user@]host:port，绝不含密码。
  pub fn display_safe(&self) -> String {
    let user = self
      .username
      .as_ref()
      .map(|u| format!("{u}@"))
      .unwrap_or_default();
    format!("{}://{}{}:{}", scheme_name(self.scheme), user, self.host, self.port)
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
  s.parse::<u16>()
    .map_err(|_| ProxyError::InvalidUri(format!("非法端口: {s}")))
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
  fn parse_auth_and_display_safe_redacts_password() {
    let cfg = ProxyConfig::parse("http://alice:s3cr3t@p.example:8080").unwrap();
    assert_eq!(cfg.username.as_deref(), Some("alice"));
    assert_eq!(cfg.password.as_deref().map(|p| p.as_str()), Some("s3cr3t"));
    let safe = cfg.display_safe();
    assert!(safe.contains("alice@"));
    assert!(!safe.contains("s3cr3t"));
    let dbg = format!("{cfg:?}");
    assert!(!dbg.contains("s3cr3t"));
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
