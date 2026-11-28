// Noise_XX_25519_AESGCM_SHA256 发起方，移植自 BotArk v0.12.0
// src/main/muse-native/noise.ts（MIT）。
//
// 与 BotArk 逐行对应的关键语义（不要“改进”——握手字节必须与官方客户端一致）：
// - 协议名恰好 28 字节，h 初值 = 协议名零填充到 32 字节；prologue 为空；
// - AES-GCM，空 AD（transport 层）；nonce = 12 字节，u64 BE 在 bytes 4..11；
// - Msg3 的 static key 是每次现生成的 ephemeral（官方客户端行为），不是持久身份；
// - CipherState 出错即 poison，后续调用一律失败（绝不复用已损坏的 cipher）；
// - split() 返回 [k1, k2]，发起方 k1 发、k2 收（Noise 规范）。
//
// 密码原语来自 x25519-dalek / aes-gcm / sha2 / hmac（纯 Rust，可上 iOS）；
// 随机数来自 getrandom。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::fmt;
use x25519_dalek::{PublicKey, StaticSecret};

pub const PROTOCOL_NAME: &str = "Noise_XX_25519_AESGCM_SHA256";

const EMPTY: &[u8] = &[];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoiseError {
  WrongPhase(&'static str),
  MessageTooShort(usize),
  CiphertextTooShort,
  NonceExhausted,
  Poisoned,
  Crypto(String),
  Random(String),
}

impl fmt::Display for NoiseError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      NoiseError::WrongPhase(m) => write!(f, "NoiseXX: {m}"),
      NoiseError::MessageTooShort(n) => write!(f, "NoiseXX: message too short ({n})"),
      NoiseError::CiphertextTooShort => write!(f, "CipherState: ciphertext too short"),
      NoiseError::NonceExhausted => write!(f, "CipherState: nonce exhausted"),
      NoiseError::Poisoned => write!(f, "CipherState: poisoned"),
      NoiseError::Crypto(m) => write!(f, "NoiseXX: crypto error: {m}"),
      NoiseError::Random(m) => write!(f, "NoiseXX: random error: {m}"),
    }
  }
}

impl std::error::Error for NoiseError {}

fn random_32() -> Result<[u8; 32], NoiseError> {
  let mut b = [0u8; 32];
  getrandom::fill(&mut b).map_err(|e| NoiseError::Random(e.to_string()))?;
  Ok(b)
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
  let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC key length is always valid");
  m.update(data);
  m.finalize().into_bytes().into()
}

/// Noise HKDF（双输出），与 BotArk hkdf2 完全一致。
fn hkdf2(ck: &[u8; 32], ikm: &[u8]) -> ([u8; 32], [u8; 32]) {
  let temp = hmac_sha256(ck, ikm);
  let o1 = hmac_sha256(&temp, &[1]);
  let mut o2_in = Vec::with_capacity(33);
  o2_in.extend_from_slice(&o1);
  o2_in.push(2);
  let o2 = hmac_sha256(&temp, &o2_in);
  (o1, o2)
}

/// X25519 密钥对（私钥 32 字节；公钥由私钥派生）。
#[derive(Debug, Clone)]
struct X25519KeyPair {
  priv_bytes: [u8; 32],
  pub_bytes: [u8; 32],
}

impl X25519KeyPair {
  fn generate() -> Result<Self, NoiseError> {
    Self::from_private(random_32()?)
  }

  fn from_private(priv_bytes: [u8; 32]) -> Result<Self, NoiseError> {
    let secret = StaticSecret::from(priv_bytes);
    let pub_bytes: [u8; 32] = PublicKey::from(&secret).to_bytes();
    Ok(Self {
      priv_bytes,
      pub_bytes,
    })
  }

  fn dh(&self, remote_pub: &[u8; 32]) -> [u8; 32] {
    let secret = StaticSecret::from(self.priv_bytes);
    let pubkey = PublicKey::from(*remote_pub);
    *secret.diffie_hellman(&pubkey).as_bytes()
  }
}

/// AES-256-GCM cipher state（BotArk CipherState）。
#[derive(Debug, Clone)]
pub struct CipherState {
  key: Option<[u8; 32]>,
  n: u64,
  poisoned: bool,
}

impl CipherState {
  pub fn new(key: Option<[u8; 32]>) -> Self {
    Self {
      key,
      n: 0,
      poisoned: false,
    }
  }

  pub fn has_key(&self) -> bool {
    self.key.is_some()
  }

  fn iv(&mut self) -> Result<[u8; 12], NoiseError> {
    if self.poisoned {
      return Err(NoiseError::Poisoned);
    }
    if self.n == u64::MAX {
      self.poisoned = true;
      return Err(NoiseError::NonceExhausted);
    }
    let mut iv = [0u8; 12];
    iv[4..12].copy_from_slice(&self.n.to_be_bytes());
    self.n += 1;
    Ok(iv)
  }

  pub fn encrypt_with_ad(&mut self, ad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, NoiseError> {
    if self.poisoned {
      return Err(NoiseError::Poisoned);
    }
    let Some(key) = self.key else {
      return Ok(plaintext.to_vec());
    };
    let iv = self.iv()?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| NoiseError::Crypto(e.to_string()))?;
    match cipher.encrypt(
      Nonce::from_slice(&iv),
      Payload {
        msg: plaintext,
        aad: ad,
      },
    ) {
      Ok(out) => Ok(out),
      Err(e) => {
        self.poisoned = true;
        Err(NoiseError::Crypto(e.to_string()))
      }
    }
  }

  pub fn decrypt_with_ad(&mut self, ad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, NoiseError> {
    if self.poisoned {
      return Err(NoiseError::Poisoned);
    }
    let Some(key) = self.key else {
      return Ok(ciphertext.to_vec());
    };
    if ciphertext.len() < 16 {
      self.poisoned = true;
      return Err(NoiseError::CiphertextTooShort);
    }
    let iv = self.iv()?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| NoiseError::Crypto(e.to_string()))?;
    match cipher.decrypt(
      Nonce::from_slice(&iv),
      Payload {
        msg: ciphertext,
        aad: ad,
      },
    ) {
      Ok(out) => Ok(out),
      Err(e) => {
        self.poisoned = true;
        Err(NoiseError::Crypto(e.to_string()))
      }
    }
  }
}

/// Noise SymmetricState（BotArk SymmetricState）。
#[derive(Debug)]
struct SymmetricState {
  h: [u8; 32],
  ck: [u8; 32],
  cs: CipherState,
}

impl SymmetricState {
  fn new() -> Self {
    let name = PROTOCOL_NAME.as_bytes();
    debug_assert_eq!(name.len(), 28, "protocol name must be exactly 28 bytes");
    let mut h = [0u8; 32];
    h[..name.len()].copy_from_slice(name);
    let mut st = Self {
      h,
      ck: h,
      cs: CipherState::new(None),
    };
    st.mix_hash(EMPTY);
    st
  }

  fn mix_hash(&mut self, data: &[u8]) {
    let mut hasher = Sha256::new();
    hasher.update(self.h);
    hasher.update(data);
    self.h = hasher.finalize().into();
  }

  fn mix_key(&mut self, ikm: &[u8]) {
    let (ck, k) = hkdf2(&self.ck, ikm);
    self.ck = ck;
    self.cs = CipherState::new(Some(k));
  }

  fn encrypt_and_hash(&mut self, p: &[u8]) -> Result<Vec<u8>, NoiseError> {
    let c = self.cs.encrypt_with_ad(&self.h, p)?;
    self.mix_hash(&c);
    Ok(c)
  }

  fn decrypt_and_hash(&mut self, c: &[u8]) -> Result<Vec<u8>, NoiseError> {
    let p = self.cs.decrypt_with_ad(&self.h, c)?;
    self.mix_hash(c);
    Ok(p)
  }

  fn split(&self) -> (CipherState, CipherState) {
    let (k1, k2) = hkdf2(&self.ck, EMPTY);
    (CipherState::new(Some(k1)), CipherState::new(Some(k2)))
  }

  fn handshake_hash(&self) -> [u8; 32] {
    self.h
  }
}

/// XX 发起方：`-> e ; <- e, ee, s, es ; -> s, se`（BotArk NoiseXXInitiator）。
#[derive(Debug)]
pub struct NoiseXXInitiator {
  ss: SymmetricState,
  e: Option<X25519KeyPair>,
  re: Option<[u8; 32]>,
  rs: Option<[u8; 32]>,
  phase: u8,
  #[cfg(test)]
  fixed_e: Option<[u8; 32]>,
  #[cfg(test)]
  fixed_s: Option<[u8; 32]>,
}

impl Default for NoiseXXInitiator {
  fn default() -> Self {
    Self::new()
  }
}

impl NoiseXXInitiator {
  pub fn new() -> Self {
    Self {
      ss: SymmetricState::new(),
      e: None,
      re: None,
      rs: None,
      phase: 1,
      #[cfg(test)]
      fixed_e: None,
      #[cfg(test)]
      fixed_s: None,
    }
  }

  /// 仅测试用：固定 Msg1/Msg3 的密钥，以复现握手向量。
  #[cfg(test)]
  pub fn with_fixed_keys(e_priv: [u8; 32], s_priv: [u8; 32]) -> Self {
    let mut it = Self::new();
    it.fixed_e = Some(e_priv);
    it.fixed_s = Some(s_priv);
    it
  }

  pub fn write_message1(&mut self, payload: &[u8]) -> Result<Vec<u8>, NoiseError> {
    if self.phase != 1 {
      return Err(NoiseError::WrongPhase("write_message1 wrong phase"));
    }
    #[cfg(test)]
    let e = match self.fixed_e {
      Some(priv_bytes) => X25519KeyPair::from_private(priv_bytes)?,
      None => X25519KeyPair::generate()?,
    };
    #[cfg(not(test))]
    let e = X25519KeyPair::generate()?;
    self.ss.mix_hash(&e.pub_bytes);
    let c = self.ss.encrypt_and_hash(payload)?;
    self.e = Some(e);
    self.phase = 2;
    let mut out = Vec::with_capacity(32 + c.len());
    out.extend_from_slice(&self.e.as_ref().unwrap().pub_bytes);
    out.extend_from_slice(&c);
    Ok(out)
  }

  pub fn read_message2(&mut self, msg: &[u8]) -> Result<Vec<u8>, NoiseError> {
    if self.phase != 2 || self.e.is_none() {
      return Err(NoiseError::WrongPhase("read_message2 wrong phase"));
    }
    if msg.len() < 96 {
      return Err(NoiseError::MessageTooShort(msg.len()));
    }
    let mut re = [0u8; 32];
    re.copy_from_slice(&msg[..32]);
    self.ss.mix_hash(&re);
    self.ss.mix_key(&self.e.as_ref().unwrap().dh(&re));
    let rs_bytes = self.ss.decrypt_and_hash(&msg[32..80])?;
    let mut rs = [0u8; 32];
    rs.copy_from_slice(&rs_bytes);
    self.ss.mix_key(&self.e.as_ref().unwrap().dh(&rs));
    let payload = self.ss.decrypt_and_hash(&msg[80..])?;
    self.re = Some(re);
    self.rs = Some(rs);
    self.phase = 3;
    Ok(payload)
  }

  pub fn write_message3(&mut self, payload: &[u8]) -> Result<Vec<u8>, NoiseError> {
    if self.phase != 3 || self.re.is_none() {
      return Err(NoiseError::WrongPhase("write_message3 wrong phase"));
    }
    #[cfg(test)]
    let s = match self.fixed_s {
      Some(priv_bytes) => X25519KeyPair::from_private(priv_bytes)?,
      None => X25519KeyPair::generate()?,
    };
    #[cfg(not(test))]
    let s = X25519KeyPair::generate()?;
    let c1 = self.ss.encrypt_and_hash(&s.pub_bytes)?;
    self.ss.mix_key(&s.dh(self.re.as_ref().unwrap()));
    let c2 = self.ss.encrypt_and_hash(payload)?;
    self.phase = 4;
    let mut out = Vec::with_capacity(c1.len() + c2.len());
    out.extend_from_slice(&c1);
    out.extend_from_slice(&c2);
    Ok(out)
  }

  /// 返回 [send, recv]（发起方视角）。
  pub fn split(&mut self) -> Result<(CipherState, CipherState), NoiseError> {
    if self.phase != 4 {
      return Err(NoiseError::WrongPhase("split wrong phase"));
    }
    self.phase = 5;
    Ok(self.ss.split())
  }

  pub fn remote_static(&self) -> Option<[u8; 32]> {
    self.rs
  }

  pub fn handshake_hash(&self) -> [u8; 32] {
    self.ss.handshake_hash()
  }
}

#[cfg(any(test, feature = "swift-noise-tests"))]
mod test_responder {
  use super::*;

  /// 仅测试用的 XX 响应方（BotArk 只有发起方；响应方按 Noise 规范镜像实现，
  /// 与 Node 参考脚本交叉验证，见 transport 注释与 docs）。
  pub(super) struct TestResponder {
    pub(super) ss: SymmetricState,
    e: Option<X25519KeyPair>,
    re: Option<[u8; 32]>,
    fixed_e: [u8; 32],
    fixed_s: [u8; 32],
  }

  impl TestResponder {
    pub(super) fn new(e_priv: [u8; 32], s_priv: [u8; 32]) -> Self {
      Self {
        ss: SymmetricState::new(),
        e: None,
        re: None,
        fixed_e: e_priv,
        fixed_s: s_priv,
      }
    }

    pub(super) fn read_message1(&mut self, msg: &[u8]) -> Result<Vec<u8>, NoiseError> {
      let mut re = [0u8; 32];
      re.copy_from_slice(&msg[..32]);
      self.ss.mix_hash(&re);
      self.re = Some(re);
      self.ss.decrypt_and_hash(&msg[32..])
    }

    pub(super) fn write_message2(&mut self, payload: &[u8]) -> Result<Vec<u8>, NoiseError> {
      let e = X25519KeyPair::from_private(self.fixed_e)?;
      let s = X25519KeyPair::from_private(self.fixed_s)?;
      self.ss.mix_hash(&e.pub_bytes);
      self.ss.mix_key(&e.dh(self.re.as_ref().unwrap()));
      let c1 = self.ss.encrypt_and_hash(&s.pub_bytes)?;
      // 'es' = DH(响应方 static, 发起方 ephemeral)。
      self.ss.mix_key(&s.dh(self.re.as_ref().unwrap()));
      let c2 = self.ss.encrypt_and_hash(payload)?;
      self.e = Some(e);
      let mut out = Vec::with_capacity(32 + c1.len() + c2.len());
      out.extend_from_slice(&self.e.as_ref().unwrap().pub_bytes);
      out.extend_from_slice(&c1);
      out.extend_from_slice(&c2);
      Ok(out)
    }

    pub(super) fn read_message3(&mut self, msg: &[u8]) -> Result<Vec<u8>, NoiseError> {
      let rs_bytes = self.ss.decrypt_and_hash(&msg[..48])?;
      let mut rs = [0u8; 32];
      rs.copy_from_slice(&rs_bytes);
      // 'se' = DH(发起方 static, 响应方 ephemeral)。
      self.ss.mix_key(&self.e.as_ref().unwrap().dh(&rs));
      self.ss.decrypt_and_hash(&msg[48..])
    }

    /// 返回响应方视角的 [send, recv]（与发起方相反：k2 发、k1 收）。
    pub(super) fn split(&self) -> (CipherState, CipherState) {
      let (k1, k2) = self.ss.split();
      (k2, k1)
    }
  }

}


#[cfg(feature = "swift-noise-tests")]
mod swift_test_peer {
  use super::{test_responder::TestResponder, CipherState};

  enum Peer {
    Initial(TestResponder),
    WaitingForFinal(TestResponder),
    Transport { send: CipherState, receive: CipherState },
    Closed,
  }

  #[no_mangle]
  extern "C" fn diehao_noise_test_new() -> *mut Peer {
    Box::into_raw(Box::new(Peer::Initial(TestResponder::new([0x33; 32], [0x44; 32]))))
  }

  #[no_mangle]
  unsafe extern "C" fn diehao_noise_test_step(
    peer: *mut Peer, bytes: *const u8, length: usize,
  ) -> *mut Vec<u8> {
    if bytes.is_null() || !(16..=65_535).contains(&length) { return std::ptr::null_mut() }
    let Some(peer) = peer.as_mut() else { return std::ptr::null_mut() };
    let frame = std::slice::from_raw_parts(bytes, length);
    let output = match std::mem::replace(peer, Peer::Closed) {
      Peer::Initial(mut responder) if frame.len() == 66 => {
        let Ok(freshness) = responder.read_message1(frame) else { return std::ptr::null_mut() };
        if freshness.len() != 34 || !freshness.starts_with(&[0x0a, 0x20]) { return std::ptr::null_mut() }
        let Ok(output) = responder.write_message2(b"Rust responder") else { return std::ptr::null_mut() };
        *peer = Peer::WaitingForFinal(responder);
        output
      }
      Peer::WaitingForFinal(mut responder) if frame.len() == 64 => {
        let Ok(payload) = responder.read_message3(frame) else { return std::ptr::null_mut() };
        if !payload.is_empty() { return std::ptr::null_mut() }
        let (send, receive) = responder.split();
        *peer = Peer::Transport { send, receive };
        responder.ss.handshake_hash().to_vec()
      }
      Peer::Transport { mut send, mut receive } => {
        let Ok(plaintext) = receive.decrypt_with_ad(&[], frame) else { return std::ptr::null_mut() };
        let Ok(output) = send.encrypt_with_ad(&[], &plaintext) else { return std::ptr::null_mut() };
        *peer = Peer::Transport { send, receive };
        output
      }
      _ => return std::ptr::null_mut(),
    };
    Box::into_raw(Box::new(output))
  }

  #[no_mangle]
  unsafe extern "C" fn diehao_noise_test_free(peer: *mut Peer) {
    if !peer.is_null() { drop(Box::from_raw(peer)) }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use super::test_responder::TestResponder;

  fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
  }

  /// 与 Node 参考实现（BotArk noise.ts 逻辑 + 固定密钥，见 /tmp/noise-vector.mjs）
  /// 逐字节一致的握手向量。任一字节不同都说明移植与 BotArk 算法有偏差。
  #[test]
  fn cross_implementation_vectors() {
    // MSG1 = e_pub(0x11*32 的公钥) || payload1 明文（首条消息尚无 key，直通）。
    const MSG1: &str = "7b4e909bbe7ffe44c465a220037d608ee35897d31ef972f07f74892cb0f73f130a20aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const MSG2: &str = "7b0d47d93427f8311160781c7c733fd89f88970aef490d8aa0ee19a4cb8a1b1476c390387466635d58f0323bdcbe69099601a1a274074852c87a5a56adbc2200a3462475954a71468d1b52d5687d02764389e1e5df3cab06fce7d2bd6520dc246325f026035dde1b5f0c6d0b0947679c";
    const MSG3: &str = "b9cb19690dfcfb9a42e033388870388167c822982ffe1d4ef4c15a4d3e5d9d41ec6f97cf8bf9372771046e4e7565101391f6247bb3880ce9999f63d88baa3d56";
    const K1: &str = "eb2fe0bed643c43335b1354ec654dc80e37916a5b95f315e54435ddd476ef060";
    const K2: &str = "f6363bb26e6e9cb20ac77b14b6ec4ee9c92f3f1d9d1397468bb4ea23bed9d1d8";
    const HH: &str = "db5e769dabe91a9a560ffc1fb1e1891442a8aab2c0ea92ce5c356c02c63bfa38";

    let payload1 = {
      let mut w = crate::protobuf::PbWriter::new();
      w.bytes(1, &[0xaa; 32]);
      w.finish()
    };
    let payload2 = b"attestation-test".to_vec();

    let mut init = NoiseXXInitiator::with_fixed_keys([0x11u8; 32], [0x22u8; 32]);
    let mut resp = TestResponder::new([0x33u8; 32], [0x44u8; 32]);

    let msg1 = init.write_message1(&payload1).unwrap();
    assert_eq!(hex(&msg1), MSG1, "Msg1 与 BotArk 参考实现不一致");
    assert_eq!(resp.read_message1(&msg1).unwrap(), payload1);

    let msg2 = resp.write_message2(&payload2).unwrap();
    assert_eq!(hex(&msg2), MSG2, "Msg2 与 BotArk 参考实现不一致");
    assert_eq!(init.read_message2(&msg2).unwrap(), payload2);

    let msg3 = init.write_message3(&[]).unwrap();
    assert_eq!(hex(&msg3), MSG3, "Msg3 与 BotArk 参考实现不一致");
    assert_eq!(resp.read_message3(&msg3).unwrap(), Vec::<u8>::new());

    assert_eq!(hex(&init.handshake_hash()), HH);
    assert_eq!(hex(&resp.ss.handshake_hash()), HH);

    let (i_send, i_recv) = init.split().unwrap();
    let (r_send, r_recv) = resp.split();
    assert_eq!(hex(i_send.key.as_ref().unwrap()), K1, "发起方 send key");
    assert_eq!(hex(i_recv.key.as_ref().unwrap()), K2, "发起方 recv key");
    assert_eq!(hex(r_send.key.as_ref().unwrap()), K2, "响应方 send key");
    assert_eq!(hex(r_recv.key.as_ref().unwrap()), K1, "响应方 recv key");
  }

  #[test]
  fn cipher_state_round_trip_nonce_and_poison() {
    let mut cs = CipherState::new(Some([7u8; 32]));
    let c1 = cs.encrypt_with_ad(b"ad", b"hello").unwrap();
    let c2 = cs.encrypt_with_ad(b"ad", b"hello").unwrap();
    assert_ne!(c1, c2, "nonce 必须递增，相同明文密文不同");
    assert_eq!(c1.len(), 5 + 16);

    let mut d = CipherState::new(Some([7u8; 32]));
    assert_eq!(d.decrypt_with_ad(b"ad", &c1).unwrap(), b"hello");
    assert_eq!(d.decrypt_with_ad(b"ad", &c2).unwrap(), b"hello");

    // 篡改 → 解密失败且 poison，后续调用一律失败。
    let mut bad = c1.clone();
    bad[0] ^= 1;
    let mut d2 = CipherState::new(Some([7u8; 32]));
    assert!(d2.decrypt_with_ad(b"ad", &bad).is_err());
    assert!(
      d2.decrypt_with_ad(b"ad", &c1).is_err(),
      "poisoned cipher 不可复用"
    );
    assert!(d2.encrypt_with_ad(b"ad", b"x").is_err());

    // 无 key 时直通（握手 Msg1 阶段）。
    let mut plain = CipherState::new(None);
    assert_eq!(plain.encrypt_with_ad(b"ad", b"abc").unwrap(), b"abc");
    assert_eq!(plain.decrypt_with_ad(b"ad", b"abc").unwrap(), b"abc");
  }

  #[test]
  fn hkdf2_is_deterministic_and_splits() {
    let (a1, a2) = hkdf2(&[0u8; 32], &[]);
    let (b1, b2) = hkdf2(&[0u8; 32], &[]);
    assert_eq!((a1, a2), (b1, b2));
    assert_ne!(a1, a2);
    // 换 ikm 则输出全变。
    let (c1, _) = hkdf2(&[0u8; 32], b"x");
    assert_ne!(a1, c1);
  }

  #[test]
  fn full_handshake_fixed_keys_round_trip() {
    let e_priv = [0x11u8; 32];
    let s_priv = [0x22u8; 32];
    let re_priv = [0x33u8; 32];
    let rs_priv = [0x44u8; 32];
    let payload1 = {
      let mut w = crate::protobuf::PbWriter::new();
      w.bytes(1, &[0xaa; 32]);
      w.finish()
    };
    let payload2 = b"attestation-test".to_vec();

    let mut init = NoiseXXInitiator::with_fixed_keys(e_priv, s_priv);
    let mut resp = TestResponder::new(re_priv, rs_priv);

    let msg1 = init.write_message1(&payload1).unwrap();
    assert_eq!(resp.read_message1(&msg1).unwrap(), payload1);

    let msg2 = resp.write_message2(&payload2).unwrap();
    assert_eq!(init.read_message2(&msg2).unwrap(), payload2);
    assert!(init.remote_static().is_some());

    let msg3 = init.write_message3(&[]).unwrap();
    assert_eq!(resp.read_message3(&msg3).unwrap(), Vec::<u8>::new());

    assert_eq!(
      init.handshake_hash(),
      resp.ss.handshake_hash(),
      "握手哈希须一致"
    );

    let (mut i_send, mut i_recv) = init.split().unwrap();
    let (mut r_send, mut r_recv) = resp.split();
    // 发起方 send == 响应方 recv，反之亦然。
    let probe = b"ping-over-noise";
    let enc = i_send.encrypt_with_ad(EMPTY, probe).unwrap();
    assert_eq!(r_recv.decrypt_with_ad(EMPTY, &enc).unwrap(), probe);
    let enc2 = r_send.encrypt_with_ad(EMPTY, probe).unwrap();
    assert_eq!(i_recv.decrypt_with_ad(EMPTY, &enc2).unwrap(), probe);

    // 阶段错用应报错。
    assert!(init.write_message1(&[]).is_err());
    assert!(init.split().is_err());
    let mut fresh = NoiseXXInitiator::new();
    assert!(fresh.read_message2(&msg2).is_err());
    assert!(matches!(
      NoiseXXInitiator::new().read_message2(&[0u8; 10]),
      Err(NoiseError::WrongPhase(_))
    ));
  }

  #[test]
  fn read_message2_rejects_short() {
    let mut init = NoiseXXInitiator::with_fixed_keys([0x11u8; 32], [0x22u8; 32]);
    init.write_message1(&[]).unwrap();
    let err = init.read_message2(&[0u8; 95]).unwrap_err();
    assert!(matches!(err, NoiseError::MessageTooShort(95)));
    let _ = hex(&[0u8; 1]); // 占位：hex 供向量测试复用
  }
}
