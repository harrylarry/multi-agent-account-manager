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
use crate::protobuf::{pb_decode, pb_get, PbWriter};
use crate::transport::{HttpRequest, InboundFrame, NoiseTransport, TransportError};
use futures_util::{SinkExt, StreamExt};
use std::fmt;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use zeroize::Zeroize;

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
    GatewayError::Ws(e.to_string())
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

  async fn handshake(
    ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
  ) -> Result<NoiseTransport, GatewayError> {
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

pub struct ConnectedGateway {
  ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
  transport: NoiseTransport,
}

impl ConnectedGateway {
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

#[cfg(test)]
mod tests {
  use super::*;

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
}
