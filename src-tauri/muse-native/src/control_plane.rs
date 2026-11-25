// muse.ai HTTPS 控制面：wake / lease / token / notary。
// 移植自 BotArk v0.12.0 src/main/muse-native/controlPlane.ts（MIT）。
//
// 与 BotArk 一致的要点：
// - 账号分区的 cookie 经调用方以 Cookie 头传入（本模块不读、不存、
//   不记 cookie/token 值；cookie 持有者见 cookie.rs，用后 zeroize）；
// - 401/403 即 signed_out（不重试）；429/502/503/504 最多重试 3 次，
//   间隔 300ms × 尝试次数；
// - notary token 可选（失败忽略）；`delegation.*` 开头的 notary token
//   是 CVM 路径的委托凭证，本 Spike 不处理，直接丢弃；
// - lease 按顺序试 `standard` 再 `hatch_vm`。

use crate::cookie::SessionCookies;
use serde_json::Value;
use std::fmt;
use std::time::Duration;
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const ORIGIN: &str = "https://muse.ai";
const MUSE_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlPlaneCode {
  SignedOut,
  VmUnavailable,
  CvmRequired,
  Http,
}

#[derive(Debug, Clone)]
pub struct ControlPlaneError {
  pub message: String,
  pub status: Option<u16>,
  pub code: Option<ControlPlaneCode>,
}

impl ControlPlaneError {
  fn new(message: impl Into<String>, status: Option<u16>, code: Option<ControlPlaneCode>) -> Self {
    Self {
      message: message.into(),
      status,
      code,
    }
  }
}

impl fmt::Display for ControlPlaneError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.message)
  }
}

impl std::error::Error for ControlPlaneError {}

/// 调用方注入的 HTTPS 客户端（BotArk 的 `session.fetch` 对应物）。
/// 只传 URL、请求头、请求体，返回 (HTTP 状态, 响应体)；响应体可能是非 JSON。
/// 实现者不得记录请求头里的 Cookie 值或响应体里的 token 值。
/// （async fn in trait：有意为之——调用方总是 .await，不做 dyn；见 gateway。）
#[allow(async_fn_in_trait)]
pub trait ControlPlaneFetch: Send + Sync {
  async fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, Vec<u8>), String>;

  async fn post(
    &self,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
  ) -> Result<(u16, Vec<u8>), String>;
}

/// Noise 连接所需的网关凭证（内存持有，用后 zeroize）。
#[derive(Debug, Clone, Zeroize, ZeroizeOnDrop)]
pub struct GatewayCreds {
  pub vm_id: String,
  pub vm_name: String,
  pub gateway_url: String,
  pub auth_token: String,
  pub notary_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct VmLease {
  pub vm_id: String,
  pub vm_name: String,
  pub gateway_url: String,
  /// 上游若返回租约到期时间（毫秒归一后），保留用于 #8 观测。
  pub expires_at_ms: Option<i64>,
  /// 上游若返回剩余租期（秒）。
  pub ttl_seconds: Option<i64>,
}

pub struct MuseControlPlane<F: ControlPlaneFetch> {
  fetch: F,
  cookies: SessionCookies,
  lease: Option<VmLease>,
  wake_retry: u32,
  /// 每次 `lease-vm` 尝试的轨迹（vmType + 结果），只读诊断用。
  pub lease_calls: Vec<String>,
}

/// 从任意数字/字符串字段读时间戳并归一到毫秒（>1e12 视为已是毫秒）。
fn read_ts(v: Option<&serde_json::Value>) -> Option<i64> {
  let raw = v?;
  let n = raw
    .as_i64()
    .or_else(|| raw.as_f64().map(|f| f as i64))
    .or_else(|| raw.as_str().and_then(|s| s.trim().parse::<i64>().ok()))?;
  if n <= 0 {
    return None;
  }
  Some(if n < 1_000_000_000_000 { n * 1000 } else { n })
}

fn read_secs(v: Option<&serde_json::Value>) -> Option<i64> {
  let raw = v?;
  raw
    .as_i64()
    .or_else(|| raw.as_f64().map(|f| f as i64))
    .or_else(|| raw.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
    .filter(|n| *n > 0)
}

impl<F: ControlPlaneFetch> MuseControlPlane<F> {
  pub fn new(fetch: F, cookies: SessionCookies) -> Self {
    Self {
      fetch,
      cookies,
      lease: None,
      wake_retry: 0,
      lease_calls: Vec::new(),
    }
  }

  fn headers<'a>(&'a self, cookie: &'a str) -> Vec<(&'a str, &'a str)> {
    let mut h: Vec<(&str, &str)> = vec![
      ("Content-Type", "application/json"),
      ("Accept", "application/json"),
      ("Accept-Language", "en-US,en;q=0.9,zh-CN;q=0.8"),
      ("Origin", ORIGIN),
      ("Referer", "https://muse.ai/"),
      ("User-Agent", MUSE_UA),
      (
        "Sec-Ch-Ua",
        "\"Chromium\";v=\"136\", \"Google Chrome\";v=\"136\", \"Not.A/Brand\";v=\"99\"",
      ),
      ("Sec-Ch-Ua-Mobile", "?0"),
      ("Sec-Ch-Ua-Platform", "\"macOS\""),
      ("Sec-Fetch-Site", "same-origin"),
      ("Sec-Fetch-Mode", "cors"),
      ("Sec-Fetch-Dest", "empty"),
    ];
    if !cookie.is_empty() {
      h.push(("Cookie", cookie));
    }
    h
  }

  async fn post(&self, path: &str, body: Value) -> Result<Value, ControlPlaneError> {
    let url = format!("{ORIGIN}{path}");
    let body_bytes = serde_json::to_vec(&body).unwrap_or_default();
    let cookie = self.cookies.cookie_header();
    let headers = self.headers(&cookie);
    let mut last_err: Option<ControlPlaneError> = None;
    for attempt in 0..3 {
      if attempt > 0 {
        tokio::time::sleep(Duration::from_millis(300 * attempt as u64)).await;
      }
      match self.fetch.post(&url, &headers, &body_bytes).await {
        Ok((status, resp_body)) => {
          let json: Value =
            serde_json::from_slice(&resp_body).unwrap_or(Value::Object(Default::default()));
          if status == 401 || status == 403 {
            return Err(ControlPlaneError::new(
              format!("{path}: not signed in ({status})"),
              Some(status),
              Some(ControlPlaneCode::SignedOut),
            ));
          }
          if (status == 429 || status == 502 || status == 503 || status == 504) && attempt < 2 {
            last_err = Some(ControlPlaneError::new(
              format!("{path}: HTTP {status}"),
              Some(status),
              Some(ControlPlaneCode::Http),
            ));
            continue;
          }
          if !(200..300).contains(&status) {
            let err = json
              .get("error")
              .and_then(|e| e.as_str())
              .map(|s| s.chars().take(120).collect::<String>())
              .unwrap_or_default();
            return Err(ControlPlaneError::new(
              format!(
                "{path}: HTTP {status}{}",
                if err.is_empty() {
                  String::new()
                } else {
                  format!(" {err}")
                }
              ),
              Some(status),
              Some(ControlPlaneCode::Http),
            ));
          }
          return Ok(json);
        }
        Err(e) => {
          last_err = Some(ControlPlaneError::new(
            format!("{path}: {e}"),
            None,
            Some(ControlPlaneCode::Http),
          ));
        }
      }
    }
    Err(last_err.unwrap_or_else(|| {
      ControlPlaneError::new(
        format!("{path}: failed"),
        None,
        Some(ControlPlaneCode::Http),
      )
    }))
  }

  pub async fn auth_check(&self) -> Result<(bool, String), ControlPlaneError> {
    let d = self
      .post("/api/auth/check", Value::Object(Default::default()))
      .await?;
    Ok((
      d.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
      d.get("viewer_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string(),
    ))
  }

  /// Best effort：失败不抛错（BotArk 同款）。
  pub async fn wake(&mut self) {
    let vm_id = self
      .lease
      .as_ref()
      .map(|l| Value::String(l.vm_id.clone()))
      .unwrap_or(Value::Null);
    let retry_count = self.wake_retry;
    self.wake_retry += 1;
    let _ = self
      .post(
        "/api/hatch/vm/wake",
        serde_json::json!({ "vm_id": vm_id, "retry_count": retry_count }),
      )
      .await;
  }

  async fn existing_vm(&mut self) -> Result<VmLease, ControlPlaneError> {
    const PATH: &str = "/api/session?vm_state=live";
    let cookie = self.cookies.cookie_header();
    let (status, body) = self.fetch.get(&format!("{ORIGIN}{PATH}"), &self.headers(&cookie))
      .await.map_err(|error| ControlPlaneError::new(format!("{PATH}: {error}"), None, Some(ControlPlaneCode::Http)))?;
    self.lease_calls.push(format!("session(vm_state=live) → HTTP {status}"));
    if !(200..300).contains(&status) {
      let code = if matches!(status, 401 | 403) { ControlPlaneCode::SignedOut } else { ControlPlaneCode::Http };
      return Err(ControlPlaneError::new(format!("{PATH}: HTTP {status}"), Some(status), Some(code)));
    }
    let unavailable = || ControlPlaneError::new(
      format!("{PATH}: no usable assigned VM"), Some(status), Some(ControlPlaneCode::VmUnavailable),
    );
    let value: Value = serde_json::from_slice(&body).map_err(|_| unavailable())?;
    let vm_id = value["vm_id"].as_str().filter(|id| {
      !id.is_empty() && id.len() <= 128 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    }).ok_or_else(unavailable)?;
    let gateway_url = value["endpoint_url"].as_str().filter(|endpoint| {
      endpoint.parse::<http::Uri>().is_ok_and(|uri| {
        matches!(uri.scheme_str(), Some("https" | "wss"))
          && uri.authority().is_some_and(|a| !a.as_str().contains('@'))
          && uri.port_u16().is_none_or(|port| port == 443)
          && uri.query().is_none()
          && uri.host().is_some_and(|host| {
            host.strip_suffix(".metaaivm.com").is_some_and(|label| {
              !label.is_empty() && label.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            })
          })
      })
    }).ok_or_else(unavailable)?;
    let lease = VmLease {
      vm_id: vm_id.to_string(), vm_name: vm_id.to_string(), gateway_url: gateway_url.to_string(),
      expires_at_ms: None, ttl_seconds: None,
    };
    self.lease = Some(lease.clone());
    if !value["vm_state"].as_str().is_some_and(|state| state.eq_ignore_ascii_case("running")) {
      self.wake().await;
    }
    Ok(lease)
  }

  pub async fn lease_vm(&mut self) -> Result<VmLease, ControlPlaneError> {
    let mut last: Option<ControlPlaneError> = None;
    for vm_type in ["standard", "hatch_vm"] {
      match self
        .post(
          "/api/hatch/lease-vm",
          serde_json::json!({ "vmType": vm_type }),
        )
        .await
      {
        Ok(d) => {
          self.lease_calls.push(format!(
            "lease-vm({vm_type}) → {}",
            d.get("status").and_then(|v| v.as_str()).unwrap_or("unknown")
          ));
          let gateway_url = d
            .get("gatewayUrl")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
          let vm_name = d
            .get("vmName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
          let vm_id = d
            .get("vmId")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| vm_id_from_gateway(&gateway_url))
            .or_else(|| {
              if vm_name.is_empty() {
                None
              } else {
                Some(vm_name.clone())
              }
            })
            .unwrap_or_default();
          if d.get("status").and_then(|v| v.as_str()) == Some("assigned")
            && !gateway_url.is_empty()
            && !vm_id.is_empty()
          {
            let vm_name = if vm_name.is_empty() {
              vm_id.clone()
            } else {
              vm_name
            };
            let lease = VmLease {
              vm_id,
              vm_name,
              gateway_url,
              expires_at_ms: read_ts(
                d.get("expiresAt")
                  .or_else(|| d.get("leaseExpiresAt"))
                  .or_else(|| d.get("expires_at"))
                  .or_else(|| d.get("lease_expires_at")),
              ),
              ttl_seconds: read_secs(
                d.get("ttl")
                  .or_else(|| d.get("ttlSeconds"))
                  .or_else(|| d.get("ttl_seconds"))
                  .or_else(|| d.get("leaseTtlSeconds")),
              ),
            };
            self.lease = Some(lease.clone());
            return Ok(lease);
          }
          last = Some(ControlPlaneError::new(
            format!(
              "lease-vm: status {}",
              d.get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
            ),
            None,
            Some(ControlPlaneCode::VmUnavailable),
          ));
        }
        Err(e) => {
          self.lease_calls.push(match e.status {
            Some(status) => format!("lease-vm({vm_type}) → HTTP {status}"),
            None => format!("lease-vm({vm_type}) → 失败"),
          });
          if e.code == Some(ControlPlaneCode::SignedOut) {
            return Err(e);
          }
          last = Some(e);
        }
      }
    }
    Err(last.unwrap_or_else(|| {
      ControlPlaneError::new(
        "lease-vm failed",
        None,
        Some(ControlPlaneCode::VmUnavailable),
      )
    }))
  }

  /// 取一次 Noise 连接所需的全部凭证（wake → lease → token → notary）。
  pub async fn credentials(
    &mut self,
    force_refresh: bool,
  ) -> Result<GatewayCreds, ControlPlaneError> {
    self.wake().await;
    let lease = match (&self.lease, force_refresh) {
      (Some(l), false) => l.clone(),
      _ => match self.lease_vm().await {
        Err(error) if error.status == Some(400) && error.message.ends_with(": HTTP 400 Invalid vmType") => {
          self.existing_vm().await.map_err(|fallback| ControlPlaneError::new(
            format!("{error} · {fallback}"), fallback.status, fallback.code,
          ))?
        }
        result => result?,
      },
    };
    let mut body = serde_json::json!({ "vmAddress": lease.gateway_url, "vmName": lease.vm_name });
    if force_refresh {
      body["force_refresh"] = Value::Bool(true);
    }
    let t = self.post("/api/hatch/token", body).await?;
    let auth_token = t
      .get("token")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();
    if auth_token.is_empty() {
      return Err(ControlPlaneError::new(
        "hatch/token: no token",
        None,
        Some(ControlPlaneCode::Http),
      ));
    }
    let mut notary_token = t
      .get("notary_token")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string());
    if notary_token
      .as_deref()
      .map(|s| s.is_empty())
      .unwrap_or(true)
    {
      if let Ok(n) = self
        .post(
          "/api/hatch/noise-notary-token",
          serde_json::json!({ "vmName": lease.vm_name }),
        )
        .await
      {
        let v = n
          .get("notaryToken")
          .or_else(|| n.get("notary_token"))
          .and_then(|v| v.as_str());
        if let Some(v) = v.filter(|s| !s.trim().is_empty()) {
          notary_token = Some(v.trim().to_string());
        }
      }
    }
    // delegation.* notary token 需要客户端背书（CVM 路径），本 Spike 跳过。
    if notary_token
      .as_deref()
      .map(|t| t.starts_with("delegation."))
      .unwrap_or(false)
    {
      notary_token = None;
    }
    Ok(GatewayCreds {
      vm_id: lease.vm_id,
      vm_name: lease.vm_name,
      gateway_url: lease.gateway_url,
      auth_token,
      notary_token,
    })
  }
}

/// 从 `wss://<vm-id>.metaaivm.com/` 提取 vm id（BotArk vmIdFromGateway）。
pub fn vm_id_from_gateway(url: &str) -> Option<String> {
  let lower = url.trim().to_ascii_lowercase();
  let rest = lower.strip_prefix("wss://")?;
  let host = rest.split('/').next()?;
  let vm = host.strip_suffix(".metaaivm.com")?;
  if (8..=64).contains(&vm.len())
    && vm
      .chars()
      .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
  {
    Some(vm.to_string())
  } else {
    None
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::VecDeque;
  use std::sync::Mutex;

  struct MockFetch {
    /// 按调用顺序返回的 (status, body)；耗尽后返回 500。
    responses: Mutex<VecDeque<(u16, String)>>,
    pub seen_urls: Mutex<Vec<String>>,
    pub seen_cookie: Mutex<Vec<Option<String>>>,
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
        seen_urls: Mutex::new(Vec::new()),
        seen_cookie: Mutex::new(Vec::new()),
      }
    }
  }

  impl ControlPlaneFetch for MockFetch {
    async fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, Vec<u8>), String> {
      assert!(url.ends_with("/api/session?vm_state=live"));
      self.post(url, headers, &[]).await
    }

    async fn post(
      &self,
      url: &str,
      headers: &[(&str, &str)],
      _body: &[u8],
    ) -> Result<(u16, Vec<u8>), String> {
      self.seen_urls.lock().unwrap().push(url.to_string());
      self.seen_cookie.lock().unwrap().push(
        headers
          .iter()
          .find(|(k, _)| k.eq_ignore_ascii_case("Cookie"))
          .map(|(_, v)| v.to_string()),
      );
      let (s, b) = self
        .responses
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or((500, "{}".to_string()));
      Ok((s, b.into_bytes()))
    }
  }

  fn cookies() -> SessionCookies {
    SessionCookies {
      cookies: vec![crate::cookie::SessionCookie {
        name: "hatch_sess".to_string(),
        value: "s3cr3t".to_string(),
      }],
    }
  }

  #[tokio::test]
  async fn auth_check_ok_and_cookie_header() {
    let fetch = MockFetch::new(vec![(200, r#"{"ok": true, "viewer_id": "v123"}"#)]);
    let cp = MuseControlPlane::new(fetch, cookies());
    let (ok, viewer) = cp.auth_check().await.unwrap();
    assert!(ok);
    assert_eq!(viewer, "v123");
    assert_eq!(
      cp.fetch.seen_cookie.lock().unwrap()[0].as_deref(),
      Some("hatch_sess=s3cr3t")
    );
    assert!(cp.fetch.seen_urls.lock().unwrap()[0].ends_with("/api/auth/check"));
  }

  #[tokio::test]
  async fn signed_out_on_401_no_retry() {
    let fetch = MockFetch::new(vec![(401, "{}"), (200, r#"{"ok": true}"#)]);
    let cp = MuseControlPlane::new(fetch, cookies());
    let err = cp.auth_check().await.unwrap_err();
    assert_eq!(err.code, Some(ControlPlaneCode::SignedOut));
    assert_eq!(err.status, Some(401));
    assert_eq!(cp.fetch.seen_urls.lock().unwrap().len(), 1, "401 不重试");
  }

  #[tokio::test]
  async fn retry_on_503_then_success() {
    let fetch = MockFetch::new(vec![
      (503, "{}"),
      (200, r#"{"ok": true, "viewer_id": "v"}"#),
    ]);
    let cp = MuseControlPlane::new(fetch, cookies());
    let (ok, _) = cp.auth_check().await.unwrap();
    assert!(ok);
    assert_eq!(cp.fetch.seen_urls.lock().unwrap().len(), 2);
  }

  #[tokio::test]
  async fn lease_falls_back_to_hatch_vm() {
    let fetch = MockFetch::new(vec![
      (200, r#"{"status": "pending"}"#),
      (
        200,
        r#"{"status": "assigned", "gatewayUrl": "wss://abc-def-123.metaaivm.com/", "vmName": "vm-1"}"#,
      ),
    ]);
    let mut cp = MuseControlPlane::new(fetch, cookies());
    let lease = cp.lease_vm().await.unwrap();
    assert_eq!(lease.vm_id, "abc-def-123");
    assert_eq!(lease.vm_name, "vm-1");
  }

  #[tokio::test]
  async fn invalid_vm_type_uses_only_a_valid_assigned_session_endpoint() {
    for (endpoint, usable) in [("https://vm-123.metaaivm.com/", true), ("https://vm-123.metaaivm.com.example/", false)] {
      let session = format!(r#"{{"vm_id":"vm-123","endpoint_url":"{endpoint}","vm_state":"RUNNING"}}"#);
      let fetch = MockFetch::new(vec![
        (200, "{}"),
        (200, r#"{"status":"pending"}"#),
        (400, r#"{"error":"Invalid vmType"}"#),
        (200, &session),
        (200, r#"{"token":"test-token","notary_token":"test-notary"}"#),
      ]);
      let mut cp = MuseControlPlane::new(fetch, cookies());
      let result = cp.credentials(false).await;
      if usable {
        let creds = result.unwrap();
        assert_eq!(creds.vm_id, "vm-123");
        assert_eq!(creds.gateway_url, endpoint);
        assert_eq!(cp.fetch.seen_cookie.lock().unwrap()[3].as_deref(), Some("hatch_sess=s3cr3t"));
      } else {
        assert_eq!(result.unwrap_err().code, Some(ControlPlaneCode::VmUnavailable));
        assert_eq!(cp.fetch.seen_urls.lock().unwrap().len(), 4);
      }
    }
  }

  #[tokio::test]
  async fn credentials_full_flow_and_delegation_notary_dropped() {
    let fetch = MockFetch::new(vec![
      (200, "{}"), // wake
      (
        200,
        r#"{"status": "assigned", "vmId": "vm-9", "gatewayUrl": "wss://vm-9.metaaivm.com/", "vmName": "vm-9"}"#,
      ),
      (
        200,
        r#"{"token": "s0:abc", "notary_token": "delegation.xyz"}"#,
      ),
    ]);
    let mut cp = MuseControlPlane::new(fetch, cookies());
    let creds = cp.credentials(false).await.unwrap();
    assert_eq!(creds.vm_id, "vm-9");
    assert_eq!(creds.auth_token, "s0:abc");
    assert_eq!(creds.notary_token, None, "delegation.* notary 应丢弃");
  }

  #[tokio::test]
  async fn credentials_fetches_notary_when_token_lacks_it() {
    let fetch = MockFetch::new(vec![
      (200, "{}"),
      (
        200,
        r#"{"status": "assigned", "vmId": "vm-9", "gatewayUrl": "wss://vm-9.metaaivm.com/", "vmName": "vm-9"}"#,
      ),
      (200, r#"{"token": "s0:abc"}"#),
      (200, r#"{"notaryToken": "endorsement.v1.abc"}"#),
    ]);
    let mut cp = MuseControlPlane::new(fetch, cookies());
    let creds = cp.credentials(false).await.unwrap();
    assert_eq!(creds.notary_token.as_deref(), Some("endorsement.v1.abc"));
  }

  #[tokio::test]
  async fn credentials_errors_without_token() {
    let fetch = MockFetch::new(vec![
      (200, "{}"),
      (
        200,
        r#"{"status": "assigned", "vmId": "vm-9", "gatewayUrl": "wss://vm-9.metaaivm.com/", "vmName": "vm-9"}"#,
      ),
      (200, r#"{}"#),
    ]);
    let mut cp = MuseControlPlane::new(fetch, cookies());
    assert!(cp.credentials(false).await.is_err());
  }

  #[test]
  fn vm_id_parsing() {
    assert_eq!(
      vm_id_from_gateway("wss://abc-def-123.metaaivm.com/").as_deref(),
      Some("abc-def-123")
    );
    assert_eq!(
      vm_id_from_gateway("wss://abc-def-123.metaaivm.com").as_deref(),
      Some("abc-def-123")
    );
    assert_eq!(
      vm_id_from_gateway("WSS://ABC-DEF-123.METAAIVM.COM/").as_deref(),
      Some("abc-def-123")
    );
    assert_eq!(
      vm_id_from_gateway("https://abc-def-123.metaaivm.com/"),
      None
    );
    assert_eq!(
      vm_id_from_gateway("wss://short.metaaivm.com/"),
      None,
      "少于 8 字符"
    );
    assert_eq!(vm_id_from_gateway("wss://abc.metaaivm.com.evil.com/"), None);
    assert_eq!(vm_id_from_gateway("wss://bad_host.metaaivm.com/"), None);
  }

  #[test]
  fn lease_expiry_parsing() {
    use serde_json::json;
    // 秒 → 毫秒
    assert_eq!(read_ts(Some(&json!(1762100300))), Some(1_762_100_300_000));
    // 已是毫秒则保持
    assert_eq!(read_ts(Some(&json!(1762100300000i64))), Some(1_762_100_300_000));
    // 字符串数字
    assert_eq!(read_ts(Some(&json!("1762100300"))), Some(1_762_100_300_000));
    // 非法/缺失
    assert_eq!(read_ts(None), None);
    assert_eq!(read_ts(Some(&json!(0))), None);
    assert_eq!(read_secs(Some(&json!(3600))), Some(3600));
    assert_eq!(read_secs(Some(&json!(-5))), None);
  }
}
