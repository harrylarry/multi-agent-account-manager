// HTTP-over-Noise 帧编解码，移植自 BotArk v0.12.0
// src/main/muse-native/transport.ts（MIT）。
//
// 帧结构（NoiseTransportFrame → ServiceRequest/Response → ServiceFrame）：
// - 请求：ApplicationRequest{verb, path, headers, body?, end_body=true}
//   包进 ServiceFrame{stream_id, request}，再包进
//   ServiceRequest{service?, payload}（daemon=0 时 service 字段省略）；
// - 响应：ServiceResponse{field1: ServiceFrame}；ServiceFrame 内
//   field3=response / field4=body_chunk / field5=reset，field2=请求是非法的；
// - 载荷按 65489 字节分片，每片一个 NoiseTransportFrame{chunk_id, index,
//   total, part}，每片单独用 send cipher 加密（空 AD），一片即一条 WS 消息。

use crate::noise::{CipherState, NoiseError};
use crate::protobuf::{pb_decode, pb_get, PbWriter};
use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

const EMPTY: &[u8] = &[];
const MAX_CHUNK: usize = 65489;
const MAX_TOTAL_CHUNKS: usize = 256;
const MAX_ASSEMBLIES: usize = 16;
const ASSEMBLY_TTL: Duration = Duration::from_secs(60);
const ASSEMBLY_BYTE_BUDGET: usize = 0x1000000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
  Dead,
  Noise(NoiseError),
  Proto(crate::protobuf::ProtoError),
  PayloadTooLarge,
  BadChunkHeader,
  TooManyAssemblies,
  InconsistentChunks,
  AssemblyTooLarge,
  EmptyServiceResponse,
  UnexpectedRequestFrame,
}

impl fmt::Display for TransportError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      TransportError::Dead => write!(f, "NoiseTransport: dead after prior failure"),
      TransportError::Noise(e) => write!(f, "{e}"),
      TransportError::Proto(e) => write!(f, "{e}"),
      TransportError::PayloadTooLarge => write!(f, "payload too large for noise framing"),
      TransportError::BadChunkHeader => write!(f, "bad noise chunk header"),
      TransportError::TooManyAssemblies => write!(f, "too many pending assemblies"),
      TransportError::InconsistentChunks => write!(f, "inconsistent noise chunks"),
      TransportError::AssemblyTooLarge => write!(f, "assembly exceeded byte budget"),
      TransportError::EmptyServiceResponse => write!(f, "empty ServiceResponse payload"),
      TransportError::UnexpectedRequestFrame => {
        write!(f, "NoiseTransport: unexpected request frame from server")
      }
    }
  }
}

impl std::error::Error for TransportError {}

impl From<NoiseError> for TransportError {
  fn from(e: NoiseError) -> Self {
    TransportError::Noise(e)
  }
}

impl From<crate::protobuf::ProtoError> for TransportError {
  fn from(e: crate::protobuf::ProtoError) -> Self {
    TransportError::Proto(e)
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
  Daemon = 0,
  Sentinel = 1,
  Vault = 2,
  Authd = 3,
  SshAccess = 4,
}

impl Service {
  pub fn as_u32(self) -> u32 {
    self as u32
  }
}

#[derive(Debug, Clone)]
pub struct HttpRequest {
  pub service: Service,
  pub verb: String,
  pub path: String,
  pub headers: Vec<(String, String)>,
  pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundFrame {
  Response {
    stream_id: u64,
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    end_body: bool,
  },
  BodyChunk {
    stream_id: u64,
    data: Vec<u8>,
    end_body: bool,
  },
  Reset {
    stream_id: u64,
    code: u32,
    reason: String,
  },
}

fn encode_headers(w: &mut PbWriter, field: u32, headers: &[(String, String)]) {
  for (k, v) in headers {
    let mut h = PbWriter::new();
    h.string(1, k).string(2, v);
    w.bytes(field, &h.finish());
  }
}

fn decode_headers(fields: &[crate::protobuf::PbField], field: u32) -> Vec<(String, String)> {
  fields
    .iter()
    .filter(|f| f.field == field && f.bytes.is_some())
    .filter_map(|f| {
      let h = pb_decode(f.bytes.as_ref().unwrap()).ok()?;
      let k = pb_get(&h, 1)?.bytes.as_ref()?;
      let v = pb_get(&h, 2)?.bytes.as_ref()?;
      Some((
        String::from_utf8_lossy(k).into_owned(),
        String::from_utf8_lossy(v).into_owned(),
      ))
    })
    .collect()
}

struct Assembly {
  total: usize,
  chunks: HashMap<usize, Vec<u8>>,
  bytes: usize,
  at: Instant,
}

/// Noise 传输层：一次握手产生一对 cipher，之后所有 RPC 走它。
/// 任一加解密失败即 dead——绝不复用已损坏的传输（BotArk 同款语义）。
pub struct NoiseTransport {
  next_stream_id: u64,
  dead: bool,
  assemblies: HashMap<u64, Assembly>,
  send: CipherState,
  recv: CipherState,
  #[cfg(test)]
  test_chunk_id: Option<u64>,
}

impl NoiseTransport {
  pub fn new(send: CipherState, recv: CipherState) -> Self {
    Self {
      next_stream_id: 1,
      dead: false,
      assemblies: HashMap::new(),
      send,
      recv,
      #[cfg(test)]
      test_chunk_id: None,
    }
  }

  pub fn is_dead(&self) -> bool {
    self.dead
  }

  /// 仅测试用：固定分片 chunk_id，以复现帧向量。
  #[cfg(test)]
  pub fn set_test_chunk_id(&mut self, id: u64) {
    self.test_chunk_id = Some(id);
  }

  fn guard<T>(
    &mut self,
    f: impl FnOnce(&mut Self) -> Result<T, TransportError>,
  ) -> Result<T, TransportError> {
    if self.dead {
      return Err(TransportError::Dead);
    }
    match f(self) {
      Ok(v) => Ok(v),
      Err(e) => {
        self.dead = true;
        Err(e)
      }
    }
  }

  /// 加密一条 unary 请求；返回 (stream_id, 加密后的 WS 消息列表)。
  pub fn encrypt_request(
    &mut self,
    req: &HttpRequest,
  ) -> Result<(u64, Vec<Vec<u8>>), TransportError> {
    self.guard(|t| {
      let stream_id = t.next_stream_id;
      t.next_stream_id += 1;
      let mut app = PbWriter::new();
      app.string(1, &req.verb).string(2, &req.path);
      encode_headers(&mut app, 3, &req.headers);
      if !req.body.is_empty() {
        app.bytes(4, &req.body);
      }
      app.bool(5, true);
      let mut frame = PbWriter::new();
      frame.uint(1, stream_id).bytes(2, &app.finish());
      Ok((stream_id, t.wrap(req.service, &frame.finish())?))
    })
  }

  pub fn encrypt_reset(
    &mut self,
    service: Service,
    stream_id: u64,
    reason: &str,
  ) -> Result<Vec<Vec<u8>>, TransportError> {
    self.guard(|t| {
      let mut reset = PbWriter::new();
      reset.uint(1, 1); // CANCELLED
      if !reason.is_empty() {
        reset.string(2, reason);
      }
      let mut frame = PbWriter::new();
      frame.uint(1, stream_id).bytes(5, &reset.finish());
      t.wrap(service, &frame.finish())
    })
  }

  fn wrap(
    &mut self,
    service: Service,
    service_frame: &[u8],
  ) -> Result<Vec<Vec<u8>>, TransportError> {
    let mut sr = PbWriter::new();
    if service.as_u32() != 0 {
      sr.uint(1, service.as_u32() as u64);
    }
    sr.bytes(2, service_frame);
    let payload = sr.finish();
    #[cfg(test)]
    let chunk_id = match self.test_chunk_id {
      Some(id) => id,
      None => random_chunk_id()?,
    };
    #[cfg(not(test))]
    let chunk_id = random_chunk_id()?;
    let total = payload.len().div_ceil(MAX_CHUNK).max(1);
    if total > MAX_TOTAL_CHUNKS {
      return Err(TransportError::PayloadTooLarge);
    }
    let mut out = Vec::with_capacity(total);
    for i in 0..total {
      let part = &payload[i * MAX_CHUNK..((i + 1) * MAX_CHUNK).min(payload.len())];
      let mut f = PbWriter::new();
      f.uint(1, chunk_id)
        .uint(2, i as u64)
        .uint(3, total as u64)
        .bytes(4, part);
      out.push(self.send.encrypt_with_ad(EMPTY, &f.finish())?);
    }
    Ok(out)
  }

  /// 解密一条 WS 消息；多分片未集齐时返回 Ok(None)。
  pub fn decrypt(&mut self, msg: &[u8]) -> Result<Option<InboundFrame>, TransportError> {
    self.guard(|t| {
      let plain = t.recv.decrypt_with_ad(EMPTY, msg)?;
      let nf = pb_decode(&plain)?;
      let chunk_id = pb_get(&nf, 1).and_then(|f| f.int).unwrap_or(0);
      let index = pb_get(&nf, 2).and_then(|f| f.int).unwrap_or(0) as usize;
      let total = pb_get(&nf, 3).and_then(|f| f.int).unwrap_or(1) as usize;
      let payload = pb_get(&nf, 4)
        .and_then(|f| f.bytes.clone())
        .unwrap_or_default();
      if !(1..=MAX_TOTAL_CHUNKS).contains(&total) || index >= total {
        return Err(TransportError::BadChunkHeader);
      }
      let whole: Vec<u8> = if total == 1 {
        payload
      } else {
        let now = Instant::now();
        t.assemblies
          .retain(|_, a| now.duration_since(a.at) <= ASSEMBLY_TTL);
        if !t.assemblies.contains_key(&chunk_id) && t.assemblies.len() >= MAX_ASSEMBLIES {
          return Err(TransportError::TooManyAssemblies);
        }
        let a = t.assemblies.entry(chunk_id).or_insert_with(|| Assembly {
          total,
          chunks: HashMap::new(),
          bytes: 0,
          at: now,
        });
        if a.total != total || a.chunks.contains_key(&index) {
          return Err(TransportError::InconsistentChunks);
        }
        a.bytes += payload.len();
        if a.bytes > ASSEMBLY_BYTE_BUDGET {
          return Err(TransportError::AssemblyTooLarge);
        }
        a.chunks.insert(index, payload);
        if a.chunks.len() < total {
          return Ok(None);
        }
        let a = t.assemblies.remove(&chunk_id).unwrap();
        let mut whole = Vec::with_capacity(a.bytes);
        for i in 0..total {
          whole.extend_from_slice(&a.chunks[&i]);
        }
        whole
      };
      let resp = pb_get(&pb_decode(&whole)?, 1)
        .and_then(|f| f.bytes.clone())
        .filter(|b| !b.is_empty())
        .ok_or(TransportError::EmptyServiceResponse)?;
      let sf = pb_decode(&resp)?;
      let stream_id = pb_get(&sf, 1).and_then(|f| f.int).unwrap_or(0);
      if let Some(r) = pb_get(&sf, 3).and_then(|f| f.bytes.clone()) {
        let f = pb_decode(&r)?;
        return Ok(Some(InboundFrame::Response {
          stream_id,
          status: pb_get(&f, 1).and_then(|x| x.int).unwrap_or(0) as u16,
          headers: decode_headers(&f, 2),
          body: pb_get(&f, 3)
            .and_then(|x| x.bytes.clone())
            .unwrap_or_default(),
          end_body: pb_get(&f, 4).and_then(|x| x.int).unwrap_or(0) != 0,
        }));
      }
      if let Some(bc) = pb_get(&sf, 4).and_then(|f| f.bytes.clone()) {
        let f = pb_decode(&bc)?;
        return Ok(Some(InboundFrame::BodyChunk {
          stream_id,
          data: pb_get(&f, 1)
            .and_then(|x| x.bytes.clone())
            .unwrap_or_default(),
          end_body: pb_get(&f, 2).and_then(|x| x.int).unwrap_or(0) != 0,
        }));
      }
      if let Some(rs) = pb_get(&sf, 5).and_then(|f| f.bytes.clone()) {
        let f = pb_decode(&rs)?;
        return Ok(Some(InboundFrame::Reset {
          stream_id,
          code: pb_get(&f, 1).and_then(|x| x.int).unwrap_or(0) as u32,
          reason: pb_get(&f, 2)
            .and_then(|x| x.bytes.clone())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default(),
        }));
      }
      if pb_get(&sf, 2).is_some() {
        return Err(TransportError::UnexpectedRequestFrame);
      }
      Ok(None)
    })
  }
}

fn random_chunk_id() -> Result<u64, TransportError> {
  let mut b = [0u8; 8];
  getrandom::fill(&mut b).map_err(|e| TransportError::Noise(NoiseError::Random(e.to_string())))?;
  Ok(u64::from_le_bytes(b))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::noise::CipherState;

  /// 用固定密钥握手得到一对 transport（发起方/响应方视角互换）。
  fn paired_transports() -> (NoiseTransport, NoiseTransport) {
    // 复用 noise.rs 的测试响应方逻辑太重；这里直接用固定 cipher key，
    // 帧编解码本身不依赖握手（握手正确性由 noise.rs 测试覆盖）。
    let (send, recv) = (
      CipherState::new(Some([0x51; 32])),
      CipherState::new(Some([0x52; 32])),
    );
    let (rsend, rrecv) = (
      CipherState::new(Some([0x52; 32])),
      CipherState::new(Some([0x51; 32])),
    );
    (
      NoiseTransport::new(send, recv),
      NoiseTransport::new(rsend, rrecv),
    )
  }

  #[test]
  fn request_response_round_trip() {
    let (mut tx, rx) = paired_transports();
    let req = HttpRequest {
      service: Service::Daemon,
      verb: "POST".to_string(),
      path: "/api/ping".to_string(),
      headers: vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        (
          "x-request-id".to_string(),
          "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        ("x-app-id".to_string(), "hatch-web".to_string()),
        ("Accept-Language".to_string(), "en-US".to_string()),
      ],
      body: br#"{"presence":"visible"}"#.to_vec(),
    };
    let (stream_id, frames) = tx.encrypt_request(&req).unwrap();
    assert_eq!(stream_id, 1);
    assert_eq!(frames.len(), 1, "小请求应为单分片");
    // 接收方是“服务器”视角：它用自己的 transport 解请求会走到
    // ServiceRequest 分支——这里只验证加密帧可被对端 cipher 解开并
    // 且 protobuf 结构完整（解密后的 NoiseTransportFrame 可解码）。
    let mut peer_recv = rx.recv.clone();
    let plain = peer_recv.decrypt_with_ad(EMPTY, &frames[0]).unwrap();
    let nf = pb_decode(&plain).unwrap();
    assert_eq!(pb_get(&nf, 3).unwrap().int, Some(1)); // total = 1
    assert_eq!(pb_get(&nf, 2).unwrap().int, Some(0)); // index = 0
    let payload = pb_get(&nf, 4).unwrap().bytes.clone().unwrap();
    // ServiceRequest：field2 = ServiceFrame（daemon 时无 field1）。
    let sr = pb_decode(&payload).unwrap();
    assert!(pb_get(&sr, 1).is_none());
    let sf = pb_decode(pb_get(&sr, 2).unwrap().bytes.as_ref().unwrap()).unwrap();
    assert_eq!(pb_get(&sf, 1).unwrap().int, Some(1)); // stream_id
    let app = pb_decode(pb_get(&sf, 2).unwrap().bytes.as_ref().unwrap()).unwrap();
    assert_eq!(
      pb_get(&app, 1).unwrap().bytes.as_deref(),
      Some(b"POST".as_slice())
    );
    assert_eq!(
      pb_get(&app, 2).unwrap().bytes.as_deref(),
      Some(b"/api/ping".as_slice())
    );
    assert_eq!(pb_get(&app, 5).unwrap().int, Some(1)); // end_body
  }

  fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
      .step_by(2)
      .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
      .collect()
  }

  /// 与 Node 参考实现（BotArk transport.ts 逻辑 + 固定密钥/固定 chunk_id）
  /// 逐字节一致的帧向量：请求帧用发起方 send key（K1）加密，
  /// 响应帧用响应方 send key（K2）加密后由本端解密。
  #[test]
  fn cross_implementation_frame_vectors() {
    const K1: &str = "eb2fe0bed643c43335b1354ec654dc80e37916a5b95f315e54435ddd476ef060";
    const K2: &str = "f6363bb26e6e9cb20ac77b14b6ec4ee9c92f3f1d9d1397468bb4ea23bed9d1d8";
    const REQ_FRAME: &str = "13190a523823dfbc0a6937205cf1ea5ef4f67af57e05b62b6f1f3325637490c52b1424a6f8cef26b530ecf52a5c984a4c24b549d1a80f5a2731c76de547c1e7f1f43832bc99e6266ebbba906a95f7c2f654c4f0608689fcfdfa609703f5c16aa7212bf3fdf6d089f1171342a4519b0e23cdc713424ee45fb8efffbeeb13c5738b59e15c7d29aa47084bc193218afaac09b21d92d47f2daf75df962db1e1aaf1442a707f15dc5cb1c501b87bdf83297fcaf3bf829488ff40f799b00899c1c83e979f2e62163ce4d5447405f864f4a7b039e524962f1869138ef020d77da";
    const RESP_FRAME: &str =
      "7bcaf57c1d8f0be31c2813731c6096f1bcbfa9fbe7d6b52a268a62512485af22abcd8ac9dfd70b";
    let k1: [u8; 32] = unhex(K1).try_into().unwrap();
    let k2: [u8; 32] = unhex(K2).try_into().unwrap();

    let mut tx = NoiseTransport::new(CipherState::new(Some(k1)), CipherState::new(Some(k2)));
    tx.set_test_chunk_id(0x0807060504030201);
    let req = HttpRequest {
      service: Service::Daemon,
      verb: "POST".to_string(),
      path: "/api/ping".to_string(),
      headers: vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        (
          "x-request-id".to_string(),
          "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        ("x-app-id".to_string(), "hatch-web".to_string()),
        ("Accept-Language".to_string(), "en-US".to_string()),
      ],
      body: br#"{"presence":"visible"}"#.to_vec(),
    };
    let (stream_id, frames) = tx.encrypt_request(&req).unwrap();
    assert_eq!(stream_id, 1);
    assert_eq!(frames.len(), 1);
    assert_eq!(
      frames[0]
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect::<String>(),
      REQ_FRAME,
      "请求帧与 BotArk 参考实现不一致"
    );

    // 响应由响应方 send key（K2）加密；发起方用 recv key（K2）解密。
    let mut rx = NoiseTransport::new(CipherState::new(Some(k1)), CipherState::new(Some(k2)));
    let frame = rx
      .decrypt(&unhex(RESP_FRAME))
      .unwrap()
      .expect("response frame");
    assert_eq!(
      frame,
      InboundFrame::Response {
        stream_id: 1,
        status: 200,
        headers: vec![],
        body: b"{}".to_vec(),
        end_body: true
      }
    );
  }
  #[test]
  fn decrypt_response_and_reset() {
    let (_, mut rx) = paired_transports();
    // 构造一条服务器响应帧：ServiceResponse{field1: ServiceFrame}，
    // ServiceFrame{field1: stream_id, field3: Response{status, body, end_body}}。
    // 注意：响应由“服务器”用它的 send cipher（= 本端 recv）加密。
    let mut resp_body = PbWriter::new();
    resp_body.uint(1, 200).bytes(3, b"{}").bool(4, true);
    let mut sf = PbWriter::new();
    sf.uint(1, 7).bytes(3, &resp_body.finish());
    let mut whole = PbWriter::new();
    whole.bytes(1, &sf.finish());
    // 响应方向的分片载荷就是 whole 本身（ServiceResponse{field1: ServiceFrame}），
    // 没有请求方向那层 ServiceRequest。
    let mut nf = PbWriter::new();
    nf.uint(1, 0x0807060504030201)
      .uint(2, 0)
      .uint(3, 1)
      .bytes(4, &whole.finish());
    // 服务器视角的 transport（send=本端 recv key）：
    let mut server = NoiseTransport::new(
      CipherState::new(Some([0x51; 32])),
      CipherState::new(Some([0x52; 32])),
    );
    // 借 server 的 send cipher 加密（不经过 wrap 的 service 层）：
    let enc = server.send.encrypt_with_ad(EMPTY, &nf.finish()).unwrap();
    let frame = rx.decrypt(&enc).unwrap().expect("response frame");
    assert_eq!(
      frame,
      InboundFrame::Response {
        stream_id: 7,
        status: 200,
        headers: vec![],
        body: b"{}".to_vec(),
        end_body: true
      }
    );

    // reset 帧。
    let mut rst = PbWriter::new();
    rst.uint(1, 1).string(2, "gone");
    let mut sf2 = PbWriter::new();
    sf2.uint(1, 7).bytes(5, &rst.finish());
    let mut whole2 = PbWriter::new();
    whole2.bytes(1, &sf2.finish());
    let mut nf2 = PbWriter::new();
    nf2
      .uint(1, 99)
      .uint(2, 0)
      .uint(3, 1)
      .bytes(4, &whole2.finish());
    let enc2 = server.send.encrypt_with_ad(EMPTY, &nf2.finish()).unwrap();
    assert_eq!(
      rx.decrypt(&enc2).unwrap().unwrap(),
      InboundFrame::Reset {
        stream_id: 7,
        code: 1,
        reason: "gone".to_string()
      }
    );
  }

  #[test]
  fn multi_chunk_assembly_and_dead_on_tamper() {
    let (mut tx, mut rx) = paired_transports();
    let big = HttpRequest {
      service: Service::Sentinel,
      verb: "POST".to_string(),
      path: "/x".to_string(),
      headers: vec![],
      body: vec![b'a'; 70_000],
    };
    let (_, frames) = tx.encrypt_request(&big).unwrap();
    assert_eq!(frames.len(), 2, "70000+ 字节应分两片");
    // 乱序到达：先第二片应返回 None（未集齐），再第一片应集齐。
    // 直接测分片重组逻辑（响应帧的载荷就是 ServiceResponse 本身）。
    let mut server = NoiseTransport::new(
      CipherState::new(Some([0x51; 32])),
      CipherState::new(Some([0x52; 32])),
    );
    let mut resp_body = PbWriter::new();
    resp_body
      .uint(1, 200)
      .bytes(3, &vec![b'b'; 70_000])
      .bool(4, true);
    let mut sf = PbWriter::new();
    sf.uint(1, 3).bytes(3, &resp_body.finish());
    let mut whole = PbWriter::new();
    whole.bytes(1, &sf.finish());
    // 手工分片 + 逐片加密（chunk_id 固定 42）。
    let payload = whole.finish();
    let mut chunks = Vec::new();
    for (i, part) in payload.chunks(MAX_CHUNK).enumerate() {
      let mut nf = PbWriter::new();
      nf.uint(1, 42)
        .uint(2, i as u64)
        .uint(3, payload.len().div_ceil(MAX_CHUNK) as u64)
        .bytes(4, part);
      chunks.push(server.send.encrypt_with_ad(EMPTY, &nf.finish()).unwrap());
    }
    // Noise nonce 是顺序的，分片必须按序解密（WS 本身按序投递）：
    // 先到第一片应返回 None（未集齐），再到第二片应出帧。
    assert!(
      rx.decrypt(&chunks[0]).unwrap().is_none(),
      "先到第一片应等第二片"
    );
    let frame = rx.decrypt(&chunks[1]).unwrap().expect("集齐后应出帧");
    match frame {
      InboundFrame::Response { body, .. } => assert_eq!(body.len(), 70_000),
      other => panic!("unexpected {other:?}"),
    }

    // 篡改密文 → decrypt 失败且 transport 变 dead。
    let mut bad = chunks[0].clone();
    bad[0] ^= 1;
    assert!(rx.decrypt(&bad).is_err());
    assert!(rx.is_dead());
    assert!(rx.decrypt(&chunks[0]).is_err());
  }
}
