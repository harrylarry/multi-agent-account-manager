// 原生 Muse 网关：WSS + Noise 握手 + 单次 unary RPC。
// 移植自 BotArk v0.12.0 src/main/muse-native/gateway.ts（MIT）的 Spike 子集。
//
// Spike 范围（Issue #12 #4）：connect() 一次性完成
// 控制面取凭证 → WSS → Noise 握手 → transport 就绪，
// 再以 connection.ping 验证 RPC 通路。完成后即关闭。
// 不做：心跳、自动重连、订阅、流式事件——那是 Task B / Phase 1 的范围。
// 密钥（auth_token / notary）只出现在内存里的 WS URL 中，建好请求后
// 立即 zeroize；日志只记阶段，不记凭证值。

use crate::control_plane::{
  ControlPlaneCode, ControlPlaneError, ControlPlaneFetch, MuseControlPlane,
};
use crate::cookie::SessionCookies;
use crate::methods;
use crate::noise::{NoiseError, NoiseXXInitiator};
use crate::proxy::{ProxyConfig, ProxyError, TunnelStream};
use crate::protobuf::{pb_decode, pb_get, PbWriter};
use crate::transport::{HttpRequest, InboundFrame, NoiseTransport, TransportError};
use futures_util::{SinkExt, StreamExt};
use std::fmt;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_tungstenite::client_async;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use zeroize::Zeroize;

/// 经代理隧道、再包一层到目标站点 TLS 的字节流。
pub type ProxyTlsStream = tokio_rustls::client::TlsStream<TunnelStream>;

/// 直连（未配置代理）时的底层 WS 流类型。
pub type DirectStream = MaybeTlsStream<TcpStream>;

pub const NOISE_URL: &str = "wss://hatch.metaaivm.com/v1/noise";
pub const APP_ID: &str = "hatch-web";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const PING_TIMEOUT: Duration = Duration::from_secs(10);
const GATEWAY_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36";

#[derive(Debug)]
pub enum GatewayError {
  ControlPlane(ControlPlaneError),
  SignedOut,
  CvmRequired,
  HandshakeFailed(String),
  Timeout(&'static str),
  Closed(String),
  Transport(TransportError),
  Ws(String),
  Http(u16),
  Reset(u32, String),
  /// #11/#16：代理隧道建立失败。fail-closed——绝不回退直连。
  Proxy(ProxyError),
  /// 请求在写入过程中被本地取消。已写出的帧无法撤回，调用方按结果不明处理。
  Cancelled,
  /// 收到了响应，但它不是一条合法的发送 ack（错误体 / 缺 message_id）。
  Protocol(String),
}

impl fmt::Display for GatewayError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      GatewayError::ControlPlane(e) => write!(f, "{e}"),
      GatewayError::SignedOut => write!(f, "not signed in (control plane 401/403)"),
      GatewayError::CvmRequired => write!(
        f,
        "confidential VM (Owner RV challenge): open muse.ai in the browser to enroll it, or use a standard VM"
      ),
      GatewayError::HandshakeFailed(m) => write!(f, "noise handshake failed: {m}"),
      GatewayError::Timeout(what) => write!(f, "{what} timed out"),
      GatewayError::Closed(m) => write!(f, "gateway closed: {m}"),
      GatewayError::Transport(e) => write!(f, "{e}"),
      GatewayError::Ws(m) => write!(f, "websocket error: {m}"),
      GatewayError::Http(s) => write!(f, "HTTP {s}"),
      GatewayError::Reset(code, reason) => write!(f, "stream reset ({code}{reason})"),
      GatewayError::Proxy(e) => write!(f, "proxy tunnel failed (no direct fallback): {e}"),
      GatewayError::Cancelled => write!(f, "cancelled before the request finished writing"),
      GatewayError::Protocol(m) => write!(f, "unexpected response: {m}"),
    }
  }
}

impl std::error::Error for GatewayError {}

impl From<ControlPlaneError> for GatewayError {
  fn from(e: ControlPlaneError) -> Self {
    if e.code == Some(ControlPlaneCode::SignedOut) {
      GatewayError::SignedOut
    } else {
      GatewayError::ControlPlane(e)
    }
  }
}

impl From<TransportError> for GatewayError {
  fn from(e: TransportError) -> Self {
    GatewayError::Transport(e)
  }
}

impl From<NoiseError> for GatewayError {
  fn from(e: NoiseError) -> Self {
    GatewayError::HandshakeFailed(e.to_string())
  }
}

impl From<tokio_tungstenite::tungstenite::Error> for GatewayError {
  fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
    use tokio_tungstenite::tungstenite::Error;
    match e {
      Error::ConnectionClosed
      | Error::AlreadyClosed
      | Error::Protocol(
        tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
      ) => GatewayError::Closed("websocket disconnected".to_string()),
      _ => GatewayError::Ws(e.to_string()),
    }
  }
}

impl From<ProxyError> for GatewayError {
  fn from(e: ProxyError) -> Self {
    GatewayError::Proxy(e)
  }
}

/// RV challenge 检测（BotArk gateway.ts isOwnerRvChallenge）：
/// Msg2 payload 解析为 field1 bytes、field2 为 1 或 2、field3 为 32 字节。
pub fn is_owner_rv_challenge(payload: &[u8]) -> bool {
  let Ok(fields) = pb_decode(payload) else {
    return false;
  };
  let att = pb_get(&fields, 1);
  let state = pb_get(&fields, 2);
  let nonce = pb_get(&fields, 3);
  matches!(att, Some(f) if f.wire == 2)
    && matches!(state, Some(f) if f.wire == 0 && matches!(f.int, Some(1) | Some(2)))
    && matches!(nonce, Some(f) if f.wire == 2 && f.bytes.as_ref().map(|b| b.len()) == Some(32))
}

/// WS 查询参数的百分号编码（只保留 RFC 3986 unreserved 字符）。
fn percent_encode(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for b in s.bytes() {
    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
      out.push(b as char);
    } else {
      out.push_str(&format!("%{b:02X}"));
    }
  }
  out
}

pub struct MuseGateway<F: ControlPlaneFetch> {
  fetch: F,
}

/// rustls 0.23 的进程级 CryptoProvider：依赖图里 tokio-tungstenite 的
/// rustls-tls-webpki-roots 只带 TLS 管道，Cargo.lock 中 rustls 未启用
/// ring / aws-lc-rs 任一 provider feature，首次建 TLS 会 panic：
/// "Could not automatically determine the process-level CryptoProvider..."。
/// 这里用 Once 显式装 ring（rustls-webpki 已带 ring），重复调用无副作用。
fn ensure_crypto_provider() {
  static INIT: std::sync::Once = std::sync::Once::new();
  INIT.call_once(|| {
    let _ = rustls::crypto::ring::default_provider().install_default();
  });
}

/// 在代理隧道之上与目标站点做 TLS，得到可交给 client_async 的 TLS 流。
async fn tls_over_tunnel(tunnel: TunnelStream, host: &str) -> Result<ProxyTlsStream, GatewayError> {
  let mut roots = rustls::RootCertStore::empty();
  roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
  let config = rustls::ClientConfig::builder()
    .with_root_certificates(roots)
    .with_no_client_auth();
  let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config));
  let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
    .map_err(|e| GatewayError::Proxy(ProxyError::Tls(format!("非法目标主机名: {e}"))))?;
  connector
    .connect(server_name, tunnel)
    .await
    .map_err(|e| GatewayError::Proxy(ProxyError::Tls(e.to_string())))
}

impl<F: ControlPlaneFetch> MuseGateway<F> {
  pub fn new(fetch: F) -> Self {
    Self { fetch }
  }

  /// 取凭证 → WSS → Noise 握手。成功返回已连接的网关（transport 就绪）。
  pub async fn connect(self, cookies: &SessionCookies) -> Result<ConnectedGateway, GatewayError> {
    ensure_crypto_provider();
    let mut cp = MuseControlPlane::new(self.fetch, cookies.clone());
    let mut creds = cp.credentials(false).await?;
    drop(cp);

    let mut url = format!(
      "{NOISE_URL}?vm_id={}&auth_token={}",
      percent_encode(&creds.vm_id),
      percent_encode(&creds.auth_token)
    );
    if let Some(notary) = &creds.notary_token {
      url.push_str(&format!("&notary_token={}", percent_encode(notary)));
    }
    url.push_str(&format!(
      "&app_id={APP_ID}&request_id={}",
      uuid::Uuid::new_v4()
    ));
    creds.zeroize();
    drop(creds);

    // tungstenite 的 connect_async 接到预建 Request 时不会自动补 WebSocket
    // 握手头（只在传 URL 字符串时 generate_request 才会补），缺了就报
    // "Missing, duplicated or incorrect header sec-websocket-key"。
    // 须手动带齐 5 个强制头（Host / Connection / Upgrade /
    // Sec-WebSocket-Version / Sec-WebSocket-Key）。
    let request = http::Request::builder()
      .uri(url)
      .header("Host", "hatch.metaaivm.com")
      .header("Connection", "Upgrade")
      .header("Upgrade", "websocket")
      .header("Sec-WebSocket-Version", "13")
      .header("Sec-WebSocket-Key", generate_key())
      .header("Origin", "https://muse.ai")
      .header("User-Agent", GATEWAY_UA)
      .body(())
      .map_err(|e| GatewayError::HandshakeFailed(format!("bad noise url: {e}")))?;
    let (mut ws, _) = tokio::time::timeout(HANDSHAKE_TIMEOUT, connect_async(request))
      .await
      .map_err(|_| GatewayError::Timeout("websocket open"))?
      .map_err(|e| match e {
        tokio_tungstenite::tungstenite::Error::Http(r) => {
          let status = r.status().as_u16();
          if status == 401 || status == 403 {
            GatewayError::SignedOut
          } else {
            GatewayError::HandshakeFailed(format!("websocket rejected: HTTP {status}"))
          }
        }
        other => GatewayError::Ws(other.to_string()),
      })?;

    let transport = Self::handshake(&mut ws).await?;
    Ok(ConnectedGateway { ws, transport })
  }

  /// #11/#16：经逐客户端代理建立 WSS + Noise 连接。
  ///
  /// fail-closed：代理隧道建立失败立即返回 `GatewayError::Proxy`，本函数内
  /// 没有任何「改用直连」的分支；调用方配置的策略即最终策略。
  ///
  /// 链路：控制面取凭证（HTTP 侧代理由注入的 `F: ControlPlaneFetch` 负责）
  /// → 代理 CONNECT/SOCKS5 隧道 → 隧道上与目标站点做 TLS → WS 握手 → Noise。
  pub async fn connect_via_proxy(
    self,
    cookies: &SessionCookies,
    proxy: &ProxyConfig,
  ) -> Result<ConnectedGateway<ProxyTlsStream>, GatewayError> {
    ensure_crypto_provider();
    let mut cp = MuseControlPlane::new(self.fetch, cookies.clone());
    let mut creds = cp.credentials(false).await?;
    drop(cp);

    let mut url = format!(
      "{NOISE_URL}?vm_id={}&auth_token={}",
      percent_encode(&creds.vm_id),
      percent_encode(&creds.auth_token)
    );
    if let Some(notary) = &creds.notary_token {
      url.push_str(&format!("&notary_token={}", percent_encode(notary)));
    }
    url.push_str(&format!(
      "&app_id={APP_ID}&request_id={}",
      uuid::Uuid::new_v4()
    ));
    creds.zeroize();
    drop(creds);

    // 与直连同样的 WS 握手头（connect_async 补不齐，手动带 5 个强制头）。
    let request = http::Request::builder()
      .uri(url)
      .header("Host", "hatch.metaaivm.com")
      .header("Connection", "Upgrade")
      .header("Upgrade", "websocket")
      .header("Sec-WebSocket-Version", "13")
      .header("Sec-WebSocket-Key", generate_key())
      .header("Origin", "https://muse.ai")
      .header("User-Agent", GATEWAY_UA)
      .body(())
      .map_err(|e| GatewayError::HandshakeFailed(format!("bad noise url: {e}")))?;

    // 1) 代理隧道到目标 host:443；任何失败直接返回，绝不回退直连。
    let tunnel = proxy.connect("hatch.metaaivm.com", 443).await?;
    // 2) 隧道上与目标站点做 TLS（wss:// 的 TLS 层）。
    let tls = tls_over_tunnel(tunnel, "hatch.metaaivm.com").await?;
    // 3) 在 TLS 流上做 WebSocket 握手（client_async 不再自行 TLS）。
    let (mut ws, _) = tokio::time::timeout(HANDSHAKE_TIMEOUT, client_async(request, tls))
      .await
      .map_err(|_| GatewayError::Timeout("websocket open (proxy)"))?
      .map_err(|e| match e {
        tokio_tungstenite::tungstenite::Error::Http(r) => {
          let status = r.status().as_u16();
          if status == 401 || status == 403 {
            GatewayError::SignedOut
          } else {
            GatewayError::HandshakeFailed(format!("websocket rejected: HTTP {status}"))
          }
        }
        other => GatewayError::Ws(other.to_string()),
      })?;
    let transport = Self::handshake(&mut ws).await?;
    Ok(ConnectedGateway { ws, transport })
  }

  async fn handshake<S>(ws: &mut WebSocketStream<S>) -> Result<NoiseTransport, GatewayError>
  where
    S: AsyncRead + AsyncWrite + Unpin,
  {
    let mut hs = NoiseXXInitiator::new();
    // hatch_vm：field1 = 32 字节 freshness nonce。
    let mut nonce = [0u8; 32];
    getrandom::fill(&mut nonce).map_err(|e| GatewayError::HandshakeFailed(e.to_string()))?;
    let mut p1 = PbWriter::new();
    p1.bytes(1, &nonce);
    let msg1 = hs.write_message1(&p1.finish())?;
    ws.send(Message::Binary(msg1.into())).await?;

    let msg2 = tokio::time::timeout(HANDSHAKE_TIMEOUT, ws.next())
      .await
      .map_err(|_| GatewayError::Timeout("noise handshake"))?
      .ok_or_else(|| GatewayError::Closed("closed during handshake".to_string()))??;
    let Message::Binary(data) = msg2 else {
      return Err(GatewayError::HandshakeFailed(
        "plaintext frame during handshake".to_string(),
      ));
    };
    if data.len() > 65535 {
      return Err(GatewayError::HandshakeFailed(
        "handshake message too large".to_string(),
      ));
    }
    let payload2 = hs.read_message2(&data)?;
    if is_owner_rv_challenge(&payload2) {
      return Err(GatewayError::CvmRequired);
    }
    // 标准 personal VM：payload2 是 attestation bundle，按 BotArk v1 策略
    // 接受 NON_TEE（auth_token 绑定）；完整 SEV-SNP 校验是 P3，不做。
    let msg3 = hs.write_message3(&[])?;
    ws.send(Message::Binary(msg3.into())).await?;
    let (send, recv) = hs.split()?;
    Ok(NoiseTransport::new(send, recv))
  }
}

/// 已连接的网关。默认类型参数保持直连（MaybeTlsStream<TcpStream>），因此
/// 直连 `connect()` 与既有调用点零改动；代理路径实例化为 `<ProxyTlsStream>`。
pub struct ConnectedGateway<S = MaybeTlsStream<TcpStream>> {
  ws: WebSocketStream<S>,
  transport: NoiseTransport,
}

impl<S> ConnectedGateway<S>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  /// 通用 unary 请求：发送后按帧收齐响应体，返回 (HTTP 状态, body)。
  pub async fn request(&mut self, req: HttpRequest) -> Result<(u16, Vec<u8>), GatewayError> {
    let (stream_id, frames) = self.transport.encrypt_request(&req)?;
    for f in frames {
      self.ws.send(Message::Binary(f.into())).await?;
    }
    let mut status: Option<u16> = None;
    let mut body: Vec<u8> = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), async {
      loop {
        let msg = self
          .ws
          .next()
          .await
          .ok_or_else(|| GatewayError::Closed("closed during request".to_string()))??;
        let Message::Binary(data) = msg else { continue };
        let Some(frame) = self.transport.decrypt(&data)? else { continue };
        match frame {
          InboundFrame::Response { stream_id: sid, status: st, body: b, end_body, .. } if sid == stream_id => {
            status = Some(st);
            if !b.is_empty() { body.extend_from_slice(&b); }
            if end_body { return Ok(()); }
          }
          InboundFrame::BodyChunk { stream_id: sid, data, end_body } if sid == stream_id => {
            body.extend_from_slice(&data);
            if end_body { return Ok(()); }
          }
          InboundFrame::Reset { stream_id: sid, code, reason } if sid == stream_id => {
            return Err(GatewayError::Reset(code, reason));
          }
          _ => {}
        }
      }
    })
    .await
    .map_err(|_| GatewayError::Timeout("unary request"))??;
    Ok((status.unwrap_or(0), body))
  }

  /// 发起长连接订阅（一次连接一个订阅），把连接移交给 `Subscription` 按行读取。
  ///
  /// 不知道写没写出去时不要用这个：用 [`Self::subscribe_tracked`]，
  /// 它会把「已经写了几帧」带回来。
  pub async fn subscribe(self, req: HttpRequest) -> Result<Subscription<S>, GatewayError> {
    self.subscribe_tracked(req, None).await.result
  }

  /// 与 [`Self::subscribe`] 相同，但报告**是否已经进入网络写阶段**。
  ///
  /// 一条请求会被 Noise 切成多帧（`encrypt_request`）。调用方**不能**把
  /// `Err` 等同于「请求没发出去」：`ws.send` 的 `Err` 可能发生在写了一半
  /// 之后，前几帧甚至同一帧的前半段已经在 TCP 缓冲区里。
  ///
  /// 更关键的是：`wrote_frames == 0` 也**不**代表没写过。首帧的
  /// `send_frame` 被 `select!` 取消时 Future 被直接丢弃，计数仍是 0，
  /// 但底层很可能已经写出了字节。所以判定必须看独立的 `entered_write`
  /// 状态：它在我们**开始调用底层写操作之前**就置位。
  ///
  /// `cancel`：写入过程中允许被中断（每帧之间检查一次，写入中也可打断）。
  pub async fn subscribe_tracked(
    mut self,
    req: HttpRequest,
    cancel: Option<&mut watch::Receiver<bool>>,
  ) -> SubscribeAttempt<S> {
    let (stream_id, frames) = match self.transport.encrypt_request(&req) {
      Ok(v) => v,
      // 加密/序列化阶段失败：一个字节都没写出去。
      Err(e) => {
        return SubscribeAttempt {
          entered_write: false,
          wrote_frames: 0,
          result: Err(e.into()),
        }
      }
    };
    let mut sink = WsSink { ws: &mut self.ws };
    let wrote = write_frames(&mut sink, frames, cancel).await;
    if let Some(err) = wrote.error {
      return SubscribeAttempt {
        entered_write: wrote.entered_write,
        wrote_frames: wrote.wrote,
        result: Err(err),
      };
    }
    SubscribeAttempt {
      entered_write: wrote.entered_write,
      wrote_frames: wrote.wrote,
      result: Ok(Subscription {
        ws: self.ws,
        transport: self.transport,
        stream_id,
        response: ResponseLines::default(),
        send_response: None,
        early_send_events: std::collections::VecDeque::new(),
        last_line_from_send: false,
        socket_closed: false,
      }),
    }
  }
}

/// 一次订阅尝试的结果。
///
/// `entered_write` 是**权威判定**：一旦为 true，底层写操作已被调用过，
/// 无法证明没有字节离开本机；`wrote_frames` 只是完整写完的帧数，仅作诊断。
pub struct SubscribeAttempt<S> {
  pub entered_write: bool,
  pub wrote_frames: usize,
  pub result: Result<Subscription<S>, GatewayError>,
}

impl<S> SubscribeAttempt<S> {
  /// 请求是否可能已经到达服务端。`true` 时任何失败都只能记 UNKNOWN。
  pub fn possibly_on_wire(&self) -> bool {
    self.entered_write
  }
}

/// 一条进行中的订阅流。事件是 NDJSON，可能被拆到多个正文块，所以按缓冲区
/// 拼行；`end_body` 之后把剩余内容作为最后一行返回，再返回 `None`。
pub struct Subscription<S = MaybeTlsStream<TcpStream>> {
  ws: WebSocketStream<S>,
  transport: NoiseTransport,
  stream_id: u64,
  response: ResponseLines,
  send_response: Option<(u64, ResponseLines)>,
  early_send_events: std::collections::VecDeque<String>,
  last_line_from_send: bool,
  socket_closed: bool,
}

#[derive(Default)]
struct ResponseLines {
  status: Option<u16>,
  buf: Vec<u8>,
  ended: bool,
  reset: Option<(u32, String)>,
}

impl ResponseLines {
  fn accept(&mut self, frame: InboundFrame) -> Result<(), GatewayError> {
    let body = match frame {
      InboundFrame::Response {
        status,
        body,
        end_body,
        ..
      } => {
        self.status = Some(status);
        self.ended = end_body;
        body
      }
      InboundFrame::BodyChunk { data, end_body, .. } => {
        self.ended = end_body;
        data
      }
      InboundFrame::Reset { code, reason, .. } => {
        self.reset = Some((code, reason));
        return Ok(());
      }
    };
    if self.buf.len().saturating_add(body.len()) > 2 * 1024 * 1024 {
      return Err(GatewayError::Protocol(
        "buffered response exceeds limit".to_string(),
      ));
    }
    self.buf.extend(body);
    Ok(())
  }

  fn line(&mut self) -> Result<Option<String>, GatewayError> {
    loop {
      let len = match self.buf.iter().position(|b| *b == b'\n') {
        Some(pos) => pos + 1,
        None if self.ended => self.buf.len(),
        None => 0,
      };
      if len == 0 {
        break;
      }
      let line = String::from_utf8(self.buf.drain(..len).collect())
        .map_err(|_| GatewayError::Protocol("invalid NDJSON UTF-8".to_string()))?;
      let line = line.trim_end_matches(['\r', '\n']);
      if !line.is_empty() {
        return Ok(Some(line.to_string()));
      }
    }
    if let Some((code, reason)) = self.reset.take() {
      return Err(GatewayError::Reset(code, reason));
    }
    Ok(None)
  }
}

/// A second RPC on the existing Noise connection. While it is being read,
/// frames for the original event subscription are buffered separately.
pub(crate) struct ParallelResponse<'a, S> {
  subscription: &'a mut Subscription<S>,
  id: u64,
  response: ResponseLines,
  early_events: std::collections::VecDeque<String>,
  early_event_bytes: usize,
}

impl<S: AsyncRead + AsyncWrite + Unpin> ParallelResponse<'_, S> {
  pub(crate) fn retain_for_collection(self) {
    self.subscription.early_send_events = self.early_events;
    self.subscription.send_response = Some((self.id, self.response));
  }

  pub(crate) fn status(&self) -> Option<u16> {
    self.response.status
  }

  pub(crate) async fn next_line(
    &mut self,
    timeout: Duration,
  ) -> Result<Option<String>, GatewayError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
      if let Some(line) = self.response.line()? {
        if serde_json::from_str::<serde_json::Value>(&line)
          .is_ok_and(|value| value["type"] == "event")
        {
          self.early_event_bytes += line.len();
          if self.early_events.len() == 128 || self.early_event_bytes > 2 * 1024 * 1024 {
            return Err(GatewayError::Protocol("too many events before send ACK".to_string()));
          }
          self.early_events.push_back(line);
          continue;
        }
        return Ok(Some(line));
      }
      if self.response.ended || self.subscription.socket_closed {
        return Ok(None);
      }
      self
        .subscription
        .receive(deadline, Some((self.id, &mut self.response)))
        .await?;
    }
  }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Subscription<S> {
  pub fn stream_id(&self) -> u64 {
    self.stream_id
  }

  pub fn status(&self) -> Option<u16> {
    self.response.status
  }

  pub fn stream_ended(&self) -> bool {
    self.response.ended
  }

  pub async fn wait_ready(&mut self, timeout: Duration) -> Result<(), GatewayError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
      if self.socket_closed || self.response.ended {
        return Err(GatewayError::Closed(
          "event subscription closed before send".to_string(),
        ));
      }
      if let Some((code, reason)) = self.response.reset.take() {
        return Err(GatewayError::Reset(code, reason));
      }
      if let Some(status) = self.status() {
        if !(200..300).contains(&status) {
          return Err(GatewayError::Http(status));
        }
        if let Some(line) = self.response.line()? {
          let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
          };
          if !value.is_object() || value["type"] == "event" {
            continue;
          }
          if value["ok"] == false || value["success"] == false || !value["error"].is_null() {
            return Err(GatewayError::Protocol(
              "event subscription rejected".to_string(),
            ));
          }
          return Ok(());
        }
      }
      self.receive(deadline, None).await?;
    }
  }

  pub(crate) async fn open_parallel(
    &mut self,
    req: HttpRequest,
    cancel: Option<&mut watch::Receiver<bool>>,
  ) -> (bool, usize, Result<ParallelResponse<'_, S>, GatewayError>) {
    let (id, frames) = match self.transport.encrypt_request(&req) {
      Ok(v) => v,
      Err(e) => return (false, 0, Err(e.into())),
    };
    let wrote = write_frames(&mut WsSink { ws: &mut self.ws }, frames, cancel).await;
    let response = match wrote.error {
      Some(e) => Err(e),
      None => Ok(ParallelResponse {
        subscription: self,
        id,
        response: ResponseLines::default(),
        early_events: Default::default(),
        early_event_bytes: 0,
      }),
    };
    (wrote.entered_write, wrote.wrote, response)
  }

  pub(crate) fn last_line_from_send(&self) -> bool {
    self.last_line_from_send
  }

  pub async fn next_line(&mut self, timeout: Duration) -> Result<Option<String>, GatewayError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
      if let Some(line) = self.early_send_events.pop_front() {
        self.last_line_from_send = true;
        return Ok(Some(line));
      }
      if let Some(line) = self.response.line()? {
        self.last_line_from_send = false;
        return Ok(Some(line));
      }
      if let Some((_, response)) = &mut self.send_response {
        if let Some(line) = response.line()? {
          self.last_line_from_send = true;
          return Ok(Some(line));
        }
        if response.ended {
          self.send_response = None;
        }
      }
      if (self.response.ended && self.send_response.is_none()) || self.socket_closed {
        return Ok(None);
      }
      self.receive(deadline, None).await?;
    }
  }

  async fn receive(
    &mut self,
    deadline: tokio::time::Instant,
    parallel: Option<(u64, &mut ResponseLines)>,
  ) -> Result<(), GatewayError> {
    match tokio::time::timeout_at(deadline, self.ws.next()).await {
      Err(_) => return Err(GatewayError::Timeout("subscription read")),
      Ok(None | Some(Ok(Message::Close(_)))) => self.socket_closed = true,
      Ok(Some(Err(e))) => return Err(e.into()),
      Ok(Some(Ok(Message::Binary(data)))) => {
        if let Some(frame) = self.transport.decrypt(&data)? {
          let id = match &frame {
            InboundFrame::Response { stream_id, .. }
            | InboundFrame::BodyChunk { stream_id, .. }
            | InboundFrame::Reset { stream_id, .. } => *stream_id,
          };
          if id == self.stream_id {
            self.response.accept(frame)?;
          } else if let Some((_, response)) = parallel.filter(|(aux, _)| *aux == id) {
            response.accept(frame)?;
          } else if let Some((_, response)) = self.send_response.as_mut().filter(|(aux, _)| *aux == id) {
            response.accept(frame)?;
          }
        }
      }
      Ok(Some(Ok(_))) => {}
    }
    Ok(())
  }
}

impl<S> ConnectedGateway<S>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  /// 握手探针：connection.ping（POST /api/ping），返回 HTTP 状态码。
  pub async fn ping(&mut self) -> Result<u16, GatewayError> {
    let spec = methods::connection_ping();
    let req = HttpRequest {
      service: spec.service,
      verb: spec.http_method.to_string(),
      path: spec.path.to_string(),
      headers: vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("x-request-id".to_string(), uuid::Uuid::new_v4().to_string()),
        ("x-app-id".to_string(), APP_ID.to_string()),
        ("Accept-Language".to_string(), "en-US".to_string()),
      ],
      body: br#"{"presence":"visible"}"#.to_vec(),
    };
    let (stream_id, frames) = self.transport.encrypt_request(&req)?;
    for frame in frames {
      self.ws.send(Message::Binary(frame.into())).await?;
    }
    // 一次 unary 响应的结束方式有两种（对齐 BotArk finishStream）：
    // 响应头帧自带 end_body，或响应头帧 end_body=false、随后由正文块 end_body=true 收尾。
    // 记下先到的状态码，任何一个结束帧都算流结束；只有「一个响应头帧都没收到、
    // 却以正文块结束」才算异常（否则会把正常响应误判成 failed）。
    let mut status: Option<u16> = None;
    let mut body_chunks = 0usize;
    let deadline = tokio::time::timeout(PING_TIMEOUT, async {
      loop {
        let msg = self
          .ws
          .next()
          .await
          .ok_or_else(|| GatewayError::Closed("closed during ping".to_string()))??;
        let Message::Binary(data) = msg else {
          return Err(GatewayError::Closed("plaintext frame".to_string()));
        };
        let Some(frame) = self.transport.decrypt(&data)? else {
          continue;
        };
        match frame {
          InboundFrame::Response {
            stream_id: sid,
            status: st,
            end_body,
            ..
          } if sid == stream_id => {
            status = Some(st);
            if end_body {
              return Ok(st);
            }
          }
          InboundFrame::BodyChunk {
            stream_id: sid,
            end_body,
            ..
          } if sid == stream_id => {
            body_chunks += 1;
            if end_body {
              return match status {
                Some(st) => Ok(st),
                None => Err(GatewayError::Closed(format!(
                  "ping ended without response frame (body chunks: {body_chunks})"
                ))),
              };
            }
          }
          InboundFrame::Reset {
            stream_id: sid,
            code,
            reason,
          } if sid == stream_id => {
            return Err(GatewayError::Reset(code, reason));
          }
          _ => {}
        }
      }
    })
    .await
    .map_err(|_| GatewayError::Timeout("connection.ping"))??;
    Ok(deadline)
  }

  pub fn is_dead(&self) -> bool {
    self.transport.is_dead()
  }

  pub async fn close(mut self) {
    let _ = self.ws.send(Message::Close(None)).await;
  }
}


/// 逐帧写入的抽象。抽出来的唯一目的是让「写到一半失败 / 写到一半被取消」
/// 可被单测覆盖——这是 `on_wire` 判定更容易出错的地方，不能只靠肉眼推理。
#[allow(async_fn_in_trait)]
trait FrameSink {
  async fn send_frame(&mut self, frame: Vec<u8>) -> Result<(), GatewayError>;
}

struct WsSink<'a, S> {
  ws: &'a mut WebSocketStream<S>,
}

impl<S> FrameSink for WsSink<'_, S>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  async fn send_frame(&mut self, frame: Vec<u8>) -> Result<(), GatewayError> {
    self.ws.send(Message::Binary(frame.into())).await.map_err(Into::into)
  }
}

struct WriteOutcome {
  /// 是否已经调用过底层写操作（`send_frame` 至少被轮询一次）。
  ///
  /// 这是 on_wire 的**唯一依据**：一旦进入写操作，即使一帧都没写完、
  /// 甚至第一次写就返回 Err，也无法排除「半帧已经出去了」。
  entered_write: bool,
  /// 完整写完的帧数。只作诊断，**不能**用来判「没发出去」。
  wrote: usize,
  error: Option<GatewayError>,
}

/// 把请求切成帧逐条写出，并在每帧之间检查取消。
///
/// `entered_write` 在**调用 `send_frame` 之前**置位——被 `select!` 取消时
/// `send_frame` 的 Future 会被直接丢弃，`wrote` 不会增加，但底层可能已经
/// 写出了字节。所以「有没有开始写」和「写完了几个完整帧」必须分开记。
async fn write_frames<K: FrameSink>(
  sink: &mut K,
  frames: Vec<Vec<u8>>,
  mut cancel: Option<&mut watch::Receiver<bool>>,
) -> WriteOutcome {
  let mut wrote = 0usize;
  let mut entered_write = false;
  for frame in frames {
    // 进写入前的检查：只有这里命中才能确定「一个字节都没写」。
    if let Some(rx) = cancel.as_deref_mut() {
      if *rx.borrow_and_update() {
        return WriteOutcome {
          entered_write,
          wrote,
          error: Some(GatewayError::Cancelled),
        };
      }
    }
    // 关键：置位发生在 poll 之前。之后无论成功、失败还是被取消，
    // 都不能再断言「请求没有开始写」。
    entered_write = true;
    let sent = match cancel.as_deref_mut() {
      Some(rx) => tokio::select! {
        r = sink.send_frame(frame) => Some(r),
        _ = rx.changed() => None,
      },
      None => Some(sink.send_frame(frame).await),
    };
    match sent {
      None => {
        return WriteOutcome {
          entered_write,
          wrote,
          error: Some(GatewayError::Cancelled),
        }
      }
      Some(Err(e)) => {
        return WriteOutcome {
          entered_write,
          wrote,
          error: Some(e),
        }
      }
      Some(Ok(())) => wrote += 1,
    }
  }
  WriteOutcome {
    entered_write,
    wrote,
    error: None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn subscription_routes_interleaved_ack_and_events_by_noise_stream() {
    use crate::chat_send::send_on_subscription;
    use crate::collect::subscription_request;
    use crate::noise::CipherState;
    use tokio_tungstenite::tungstenite::protocol::Role;

    fn response(cipher: &mut CipherState, id: u64, body: &[u8], ended: bool) -> Message {
      let mut r = PbWriter::new();
      r.uint(1, 200).bytes(3, body).bool(4, ended);
      let mut stream = PbWriter::new();
      stream.uint(1, id).bytes(3, &r.finish());
      let mut service = PbWriter::new();
      service.bytes(1, &stream.finish());
      let mut frame = PbWriter::new();
      frame
        .uint(1, id)
        .uint(2, 0)
        .uint(3, 1)
        .bytes(4, &service.finish());
      Message::Binary(cipher.encrypt_with_ad(&[], &frame.finish()).unwrap().into())
    }

    let (client, server) = tokio::io::duplex(8192);
    let mut server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
    let client = ConnectedGateway {
      ws: WebSocketStream::from_raw_socket(client, Role::Client, None).await,
      transport: NoiseTransport::new(
        CipherState::new(Some([1; 32])),
        CipherState::new(Some([2; 32])),
      ),
    };
    let peer = tokio::spawn(async move {
      let mut cipher = CipherState::new(Some([2; 32]));
      assert!(server.next().await.unwrap().unwrap().is_binary());
      server
        .send(response(&mut cipher, 1, b"{\"type\":\"res\"}\n", false))
        .await
        .unwrap();
      assert!(server.next().await.unwrap().unwrap().is_binary());
      server
        .send(response(
          &mut cipher,
          99,
          b"{\"message_id\":\"wrong\"}",
          true,
        ))
        .await
        .unwrap();
      server
        .send(response(
          &mut cipher,
          1,
          b"{\"type\":\"event\",\"event\":\"message.user\"}\n",
          false,
        ))
        .await
        .unwrap();
      server
        .send(response(
          &mut cipher,
          2,
          b"{\"type\":\"event\",\"event\":\"delta.message_start\"}\n{\"message_id\":\"accepted\",\"session_id\":\"new\"}\n",
          false,
        ))
        .await
        .unwrap();
      server.send(response(
        &mut cipher, 2, b"{\"type\":\"event\",\"event\":\"delta.text_append\"}\n", true,
      )).await.unwrap();
    });

    let mut sub = client.subscribe(subscription_request()).await.unwrap();
    let wait = Duration::from_secs(1);
    sub.wait_ready(wait).await.unwrap();
    let (_tx, mut rx) = watch::channel(false);
    let sent = send_on_subscription(&mut sub, "test", "new", wait, &mut rx).await;
    assert!(sent.on_wire);
    assert_eq!(sent.outcome.unwrap().message_id, "accepted");
    assert!(sub.next_line(wait).await.unwrap().unwrap().contains("delta.message_start"));
    assert!(sub.last_line_from_send());
    let event = sub.next_line(wait).await.unwrap().unwrap();
    assert!(event.contains("message.user"));
    assert!(!sub.last_line_from_send());
    assert!(sub.next_line(wait).await.unwrap().unwrap().contains("delta.text_append"));
    assert!(sub.last_line_from_send());
    assert!(!sub.stream_ended());
    peer.await.unwrap();
  }

  #[test]
  fn split_utf8_stays_intact_at_ndjson_chunk_boundary() {
    let bytes = "回复\n".as_bytes();
    let mut lines = ResponseLines::default();
    lines
      .accept(InboundFrame::BodyChunk {
        stream_id: 1,
        data: bytes[..2].to_vec(),
        end_body: false,
      })
      .unwrap();
    assert!(lines.line().unwrap().is_none());
    lines
      .accept(InboundFrame::BodyChunk {
        stream_id: 1,
        data: bytes[2..].to_vec(),
        end_body: true,
      })
      .unwrap();
    assert_eq!(lines.line().unwrap().as_deref(), Some("回复"));
  }

  #[test]
  fn rv_challenge_detection() {
    // field1 bytes(4B) + field2 varint 1 + field3 bytes(32B) → challenge。
    let mut w = PbWriter::new();
    w.bytes(1, b"att").uint(2, 1).bytes(3, &[0u8; 32]);
    assert!(is_owner_rv_challenge(&w.finish()));
    let mut w2 = PbWriter::new();
    w2.bytes(1, b"att").uint(2, 2).bytes(3, &[0u8; 32]);
    assert!(is_owner_rv_challenge(&w2.finish()));
    // field2 = 3 不是 challenge；field3 长度不对也不是。
    let mut w3 = PbWriter::new();
    w3.bytes(1, b"att").uint(2, 3).bytes(3, &[0u8; 32]);
    assert!(!is_owner_rv_challenge(&w3.finish()));
    let mut w4 = PbWriter::new();
    w4.bytes(1, b"att").uint(2, 1).bytes(3, &[0u8; 16]);
    assert!(!is_owner_rv_challenge(&w4.finish()));
    // attestation bundle（普通 bytes）不是 challenge。
    assert!(!is_owner_rv_challenge(b"attestation-test"));
    assert!(!is_owner_rv_challenge(&[]));
  }

  #[test]
  fn percent_encoding() {
    assert_eq!(percent_encode("abc-._~123"), "abc-._~123");
    assert_eq!(percent_encode("s0:abc/def?x=1"), "s0%3Aabc%2Fdef%3Fx%3D1");
  }

  // ---- #11/#16 代理接线测试 ----

  use crate::control_plane::ControlPlaneFetch;
  use std::collections::VecDeque;
  use std::sync::Mutex;
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  struct MockFetch {
    responses: Mutex<VecDeque<(u16, String)>>,
  }

  impl MockFetch {
    fn new(responses: Vec<(u16, &str)>) -> Self {
      Self {
        responses: Mutex::new(
          responses
            .into_iter()
            .map(|(s, b)| (s, b.to_string()))
            .collect(),
        ),
      }
    }
  }

  impl ControlPlaneFetch for MockFetch {
    async fn post(
      &self,
      _url: &str,
      _headers: &[(&str, &str)],
      _body: &[u8],
    ) -> Result<(u16, Vec<u8>), String> {
      let (s, b) = self
        .responses
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or((500, "{}".to_string()));
      Ok((s, b.into_bytes()))
    }
  }

  fn test_cookies() -> SessionCookies {
    SessionCookies {
      cookies: vec![crate::cookie::SessionCookie {
        name: "hatch_sess".to_string(),
        value: "test".to_string(),
      }],
    }
  }

  fn creds_responses() -> Vec<(u16, &'static str)> {
    vec![
      (200, "{}"), // wake
      (
        200,
        r#"{"status":"assigned","vmId":"vm-9","gatewayUrl":"wss://vm-9.metaaivm.com/","vmName":"vm-9"}"#,
      ), // lease
      (200, r#"{"token":"s0:testtoken"}"#), // token（带 notary_token 避免额外请求）
    ]
  }

  /// 代理拒绝 CONNECT 时，connect_via_proxy 必须返回 Proxy 错误，
  /// 不得静默回退直连（否则会去连真实 hatch.metaaivm.com 得到别的错误/超时）。
  #[tokio::test]
  async fn proxy_connect_fail_closed_on_407() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    let proxy_task = tokio::spawn(async move {
      let (mut s, _) = listener.accept().await.unwrap();
      // 逐字节读 CONNECT 请求头直到 \r\n\r\n，然后回 407。
      let mut head: Vec<u8> = Vec::new();
      let mut buf = [0u8; 1];
      while head.len() < 8192 {
        if s.read(&mut buf).await.unwrap_or(0) == 0 {
          break;
        }
        head.push(buf[0]);
        if head.ends_with(b"\r\n\r\n") {
          break;
        }
      }
      let _ = s
        .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
        .await;
    });

    let cfg = ProxyConfig::parse(&format!("http://127.0.0.1:{proxy_port}")).unwrap();
    let gateway = MuseGateway::new(MockFetch::new(creds_responses()));
    let result = gateway.connect_via_proxy(&test_cookies(), &cfg).await;
    // 无论 mock 读取细节如何，结果必须是 Proxy 变体，绝不能是直连路径的 Ws/Timeout。
    assert!(
      matches!(&result, Err(GatewayError::Proxy(_))),
      "代理失败必须传播为 GatewayError::Proxy"
    );
    proxy_task.abort();
  }

  /// 代理不可达（连接被拒）时同样是 Proxy 错误，而非直连。
  #[tokio::test]
  async fn proxy_connect_fail_closed_when_unreachable() {
    // 绑定后立即释放，得到一个几乎肯定无人监听的端口。
    let tmp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tmp.local_addr().unwrap().port();
    drop(tmp);

    let cfg = ProxyConfig::parse(&format!("http://127.0.0.1:{port}")).unwrap();
    let gateway = MuseGateway::new(MockFetch::new(creds_responses()));
    let result = gateway.connect_via_proxy(&test_cookies(), &cfg).await;
    assert!(
      matches!(&result, Err(GatewayError::Proxy(_))),
      "代理不可达必须传播为 GatewayError::Proxy"
    );
  }

  // ---- 写入阶段的 on_wire 判定（不依赖真实 WSS）----------------------

  /// 假 sink：模拟四种底层写行为。
  /// - `fail_at(n)`：第 n 帧直接返回 Err（可能已经写了半帧）；
  /// - `partial_fail_at(n)`：第 n 帧先「写出半帧」再返回 Err——真实 socket
  ///   的部分写就是这个形态，帧计数不会增加，但字节已经在线上；
  /// - `stall_at(n)`：第 n 帧永久挂起（用于在写入中被取消）。
  struct MockSink {
    fail_at: Option<usize>,
    partial_fail_at: Option<usize>,
    stall_at: Option<usize>,
    seen: usize,
    /// 已经「写进 socket」但没构成完整帧的字节数。
    partial_bytes: usize,
  }

  impl MockSink {
    fn new() -> Self {
      Self {
        fail_at: None,
        partial_fail_at: None,
        stall_at: None,
        seen: 0,
        partial_bytes: 0,
      }
    }
  }

  impl FrameSink for MockSink {
    async fn send_frame(&mut self, frame: Vec<u8>) -> Result<(), GatewayError> {
      self.seen += 1;
      if Some(self.seen) == self.stall_at {
        std::future::pending::<()>().await;
      }
      if Some(self.seen) == self.partial_fail_at {
        // 半帧已经交给 socket，然后连接断掉。
        self.partial_bytes += frame.len() / 2 + 1;
        return Err(GatewayError::Ws("connection reset mid-frame".to_string()));
      }
      if Some(self.seen) == self.fail_at {
        return Err(GatewayError::Ws("write failed".to_string()));
      }
      Ok(())
    }
  }

  fn frames(n: usize) -> Vec<Vec<u8>> {
    (0..n).map(|i| vec![i as u8]).collect()
  }

  #[tokio::test]
  async fn write_frames_all_ok() {
    let mut sink = MockSink::new();
    let out = write_frames(&mut sink, frames(3), None).await;
    assert!(out.entered_write);
    assert_eq!(out.wrote, 3);
    assert!(out.error.is_none());
  }

  /// 关键回归：第 2 帧写失败时，第 1 帧已经在线上 → 不能报告「没发出去」。
  #[tokio::test]
  async fn write_frames_failure_after_partial_write_keeps_count() {
    let mut sink = MockSink {
      fail_at: Some(2),
      ..MockSink::new()
    };
    let out = write_frames(&mut sink, frames(3), None).await;
    assert!(out.entered_write);
    assert_eq!(out.wrote, 1, "第 1 帧已经写出去，必须计入");
    assert!(matches!(out.error, Some(GatewayError::Ws(_))));
  }

  /// 首帧就返回 Err：帧计数是 0，但底层写已经被调用过——不能当「没开始」。
  #[tokio::test]
  async fn write_frames_first_frame_error_is_indeterminate() {
    let mut sink = MockSink {
      fail_at: Some(1),
      ..MockSink::new()
    };
    let out = write_frames(&mut sink, frames(2), None).await;
    assert_eq!(out.wrote, 0);
    assert!(
      out.entered_write,
      "send_frame 已被轮询，可能已写出字节，必须视为已进入网络写入"
    );
    assert!(matches!(out.error, Some(GatewayError::Ws(_))));
  }

  /// 首帧写了一半后失败：这是本轮复审点名的场景。
  #[tokio::test]
  async fn write_frames_first_frame_partial_write_then_error_is_indeterminate() {
    let mut sink = MockSink {
      partial_fail_at: Some(1),
      ..MockSink::new()
    };
    let out = write_frames(&mut sink, frames(2), None).await;
    assert!(sink.partial_bytes > 0, "模拟必须真的写了半帧");
    assert_eq!(out.wrote, 0);
    assert!(
      out.entered_write,
      "半帧已在线上：绝不能记 DEFINITIVELY_FAILED"
    );
  }

  /// 首帧写入中被取消：Future 被丢弃，计数为 0，但字节可能已经写出去了。
  #[tokio::test]
  async fn write_frames_cancel_during_first_frame_write_is_indeterminate() {
    let mut sink = MockSink {
      stall_at: Some(1),
      ..MockSink::new()
    };
    let (tx, rx) = watch::channel(false);
    let mut rx = rx;
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(10)).await;
      let _ = tx.send(true);
    });
    let out = write_frames(&mut sink, frames(1), Some(&mut rx)).await;
    assert_eq!(out.wrote, 0);
    assert!(
      out.entered_write,
      "取消发生在首帧写入中，不能当成「没开始」"
    );
    assert!(matches!(out.error, Some(GatewayError::Cancelled)));
  }

  #[tokio::test]
  async fn write_frames_cancel_mid_write_keeps_written_count() {
    let mut sink = MockSink {
      stall_at: Some(2),
      ..MockSink::new()
    };
    let (tx, rx) = watch::channel(false);
    let mut rx = rx;
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(10)).await;
      let _ = tx.send(true);
    });
    let out = write_frames(&mut sink, frames(3), Some(&mut rx)).await;
    assert!(out.entered_write);
    assert_eq!(out.wrote, 1);
    assert!(matches!(out.error, Some(GatewayError::Cancelled)));
  }

  /// 唯一能确定「没开始」的取消：进写入之前就已经被取消。
  #[tokio::test]
  async fn write_frames_cancel_before_start_is_not_entered() {
    let mut sink = MockSink::new();
    let (tx, rx) = watch::channel(true);
    let mut rx = rx;
    drop(tx);
    let out = write_frames(&mut sink, frames(2), Some(&mut rx)).await;
    assert!(!out.entered_write);
    assert_eq!(out.wrote, 0);
    assert!(matches!(out.error, Some(GatewayError::Cancelled)));
  }

  /// 空请求（无帧）不会碰网络。
  #[tokio::test]
  async fn write_frames_empty_request_never_enters_write() {
    let mut sink = MockSink::new();
    let out = write_frames(&mut sink, frames(0), None).await;
    assert!(!out.entered_write);
    assert_eq!(out.wrote, 0);
    assert!(out.error.is_none());
    assert_eq!(sink.seen, 0);
  }
}
