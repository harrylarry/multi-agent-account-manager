use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::{aead, digest, hmac, rand::{SecureRandom, SystemRandom}};
use std::{net::{Ipv4Addr, SocketAddrV4}, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpSocket, TcpStream}};
use zeroize::Zeroizing;

const PREFIX: &str = "diehao-lan:v1:";
const CONTEXT: &[u8] = b"diehao local account backup v1";
pub const SHARE_SECONDS: u64 = 120;
const MAX_SIZE: usize = 2_800_000 + 16;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Wifi { pub address: Ipv4Addr, pub mask: Ipv4Addr }

impl Wifi {
  fn contains(self, other: Ipv4Addr) -> bool {
    let mask = u32::from(self.mask);
    self.address.is_private() && other.is_private() && mask.count_ones() >= 8
      && (mask | mask.wrapping_sub(1)) == u32::MAX
      && u32::from(self.address) & mask == u32::from(other) & mask
  }
}

pub struct Pairing { endpoint: SocketAddrV4, key: Zeroizing<[u8; 32]> }

impl Pairing {
  pub fn parse(code: &str) -> Result<Self, String> {
    let invalid = || "配对码无效，仅接受叠号局域网备份码".to_string();
    let (addr, encoded) = code.trim().strip_prefix(PREFIX).and_then(|code| code.rsplit_once(':')).ok_or_else(invalid)?;
    let endpoint: SocketAddrV4 = addr.parse().map_err(|_| invalid())?;
    if !endpoint.ip().is_private() || endpoint.port() < 1024 || encoded.len() != 43 { return Err(invalid()); }
    let mut key = Zeroizing::new([0; 32]);
    if URL_SAFE_NO_PAD.decode_slice(encoded, key.as_mut()).map_err(|_| invalid())? != key.len() { return Err(invalid()); }
    Ok(Self { endpoint, key })
  }

  fn cipher(&self) -> Result<aead::LessSafeKey, String> {
    aead::UnboundKey::new(&aead::AES_256_GCM, self.key.as_ref()).map(aead::LessSafeKey::new)
      .map_err(|_| "无法初始化局域网加密".into())
  }
}

// The wire never carries the QR key. A fresh challenge authenticates the receiver
// before the one-use ciphertext is sent. One random key seals exactly one message.
pub async fn prepare<F>(wifi: Wifi, encrypted: String, current_wifi: impl Fn() -> F + Send + 'static)
  -> Result<(String, tokio::task::JoinHandle<()>), String>
where F: std::future::Future<Output = Result<Wifi, String>> + Send {
  if !wifi.contains(wifi.address) || encrypted.len() > MAX_SIZE - 16 { return Err("局域网地址或备份大小无效".into()); }
  let listener = TcpListener::bind(SocketAddrV4::new(wifi.address, 0)).await.map_err(|_| "无法在当前 Wi-Fi 开启分享")?;
  let endpoint = SocketAddrV4::new(wifi.address, listener.local_addr().map_err(|_| "无法读取分享端口")?.port());
  let mut key = Zeroizing::new([0; 32]);
  SystemRandom::new().fill(key.as_mut()).map_err(|_| "无法生成安全配对码")?;
  let pairing = Pairing { endpoint, key };
  let code = format!("{PREFIX}{endpoint}:{}", URL_SAFE_NO_PAD.encode(pairing.key.as_ref()));
  let mut wrapped = encrypted.into_bytes();
  pairing.cipher()?.seal_in_place_append_tag(aead::Nonce::assume_unique_for_key([0; 12]), aead::Aad::from(CONTEXT), &mut wrapped)
    .map_err(|_| "无法封装局域网备份")?;
  let proof = proof_key(&pairing);
  let task = tokio::spawn(async move {
    let _ = tokio::time::timeout(Duration::from_secs(SHARE_SECONDS), async move {
      while let Ok((stream, peer)) = listener.accept().await {
        if let std::net::SocketAddr::V4(peer) = peer {
          if wifi.contains(*peer.ip()) {
            if current_wifi().await != Ok(wifi) || serve_peer(stream, &proof, &wrapped).await { break; }
          }
        }
      }
    }).await;
  });
  Ok((code, task))
}

fn proof_key(pairing: &Pairing) -> hmac::Key {
  let digest = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, pairing.key.as_ref()), CONTEXT);
  hmac::Key::new(hmac::HMAC_SHA256, digest.as_ref())
}

async fn serve_peer(mut stream: TcpStream, proof: &hmac::Key, wrapped: &[u8]) -> bool {
  let authenticated = tokio::time::timeout(Duration::from_secs(3), async {
    let mut challenge = [0; 32];
    SystemRandom::new().fill(&mut challenge).ok()?;
    stream.write_all(&challenge).await.ok()?;
    let mut response = [0; digest::SHA256_OUTPUT_LEN];
    stream.read_exact(&mut response).await.ok()?;
    hmac::verify(proof, &challenge, &response).ok()
  }).await.ok().flatten().is_some();
  if !authenticated { return false; }
  let _ = tokio::time::timeout(TRANSFER_TIMEOUT, async {
    stream.write_u32(wrapped.len() as u32).await?;
    stream.write_all(wrapped).await?;
    stream.shutdown().await
  }).await;
  true
}

pub async fn receive(wifi: Wifi, pairing: Pairing) -> Result<String, String> {
  if !wifi.contains(*pairing.endpoint.ip()) || *pairing.endpoint.ip() == wifi.address {
    return Err("请让两台设备连接到同一 Wi-Fi 子网".into());
  }
  let local = TcpSocket::new_v4().map_err(|_| "无法启动接收")?;
  local.bind(SocketAddrV4::new(wifi.address, 0).into()).map_err(|_| "无法绑定当前 Wi-Fi")?;
  tokio::time::timeout(TRANSFER_TIMEOUT, async {
    let stream = local.connect(pairing.endpoint.into()).await.map_err(|_| "无法连接发送设备，请检查局域网权限和配对有效期")?;
    read_backup(stream, &pairing).await
  }).await.map_err(|_| "局域网接收超时，请检查权限或生成新配对码")?
}

async fn read_backup(mut stream: TcpStream, pairing: &Pairing) -> Result<String, String> {
  let disconnected = "连接已中断，请重新配对";
  let mut challenge = [0; 32];
  stream.read_exact(&mut challenge).await.map_err(|_| disconnected)?;
  let proof = hmac::sign(&proof_key(pairing), &challenge);
  stream.write_all(proof.as_ref()).await.map_err(|_| disconnected)?;
  let size = stream.read_u32().await.map_err(|_| disconnected)? as usize;
  if !(16..=MAX_SIZE).contains(&size) { return Err("局域网备份大小无效".into()); }
  let mut data = vec![0; size];
  stream.read_exact(&mut data).await.map_err(|_| disconnected)?;
  let plain = pairing.cipher()?.open_in_place(aead::Nonce::assume_unique_for_key([0; 12]), aead::Aad::from(CONTEXT), &mut data)
    .map_err(|_| "局域网备份未通过完整性校验")?;
  String::from_utf8(plain.to_vec()).map_err(|_| "局域网备份格式无效".into())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pairing_accepts_only_private_ipv4_and_checks_the_subnet() {
    let token = URL_SAFE_NO_PAD.encode([0; 32]);
    for host in ["127.0.0.1", "169.254.1.2", "1.1.1.1", "example.org", "[::1]"] {
      assert!(Pairing::parse(&format!("{PREFIX}{host}:5555:{token}")).is_err());
    }
    assert!(Pairing::parse(&format!("{PREFIX}192.168.1.7:5555:{token}")).is_ok());
    let wifi = Wifi { address: Ipv4Addr::new(192,168,1,9), mask: Ipv4Addr::new(255,255,255,0) };
    assert!(wifi.contains(Ipv4Addr::new(192,168,1,7)));
    assert!(!wifi.contains(Ipv4Addr::new(192,168,2,7)));
  }

  #[tokio::test]
  async fn receiver_authentication_and_ciphertext_integrity_use_the_pairing_key() {
    let pairing = Pairing { endpoint: "127.0.0.1:5555".parse().unwrap(), key:Zeroizing::new([1; 32]) };
    let bad = Pairing { endpoint: pairing.endpoint, key:Zeroizing::new([2; 32]) };
    let mut encrypted = b"test backup".to_vec();
    pairing.cipher().unwrap().seal_in_place_append_tag(aead::Nonce::assume_unique_for_key([0; 12]), aead::Aad::from(CONTEXT), &mut encrypted).unwrap();
    let proof = proof_key(&pairing);
    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = server.local_addr().unwrap();
    let connect = async { read_backup(TcpStream::connect(address).await.unwrap(), &bad).await };
    let (valid, result) = tokio::join!(async { serve_peer(server.accept().await.unwrap().0, &proof, &encrypted).await }, connect);
    assert!(!valid && result.is_err());
    let connect = async { read_backup(TcpStream::connect(address).await.unwrap(), &pairing).await };
    let (valid, result) = tokio::join!(async { serve_peer(server.accept().await.unwrap().0, &proof, &encrypted).await }, connect);
    assert!(valid);
    assert_eq!(result.unwrap(), "test backup");
    encrypted[0] ^= 1;
    let connect = async { read_backup(TcpStream::connect(address).await.unwrap(), &pairing).await };
    let (valid, result) = tokio::join!(async { serve_peer(server.accept().await.unwrap().0, &proof, &encrypted).await }, connect);
    assert!(valid && result.unwrap_err().contains("完整性"));
  }
}
