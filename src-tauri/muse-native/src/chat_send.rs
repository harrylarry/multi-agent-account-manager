//! B4：`chat.stream` 原生发送（Issue #13 / i005 #15）。
//!
//! Wire 形状（BotArk `chat.ts` streamChat 实测注释）：
//! - `chat.stream` 是**订阅**而非 unary：`gw.subscribe('chat.stream', params)`；
//! - params: `{items:[{type:"text",text}], node_id, capabilities:[...delta_stream]}`，
//!   可选 `session_id`；
//! - 首个非 event 行是 ack（`{message_id, session_id, channel…}`），随后事件在
//!   `chat.subscribe` 上继续。
//!
//! 安全：本模块只负责「发一次并拿到 ack」，其余判断（单发锁、不重发、UNKNOWN）
//! 由 `send_ledger` 决策。
//!
//! ## 三条硬约束
//!
//! 1. **`Err` 不等于「没发出去」。** 一条请求会被 Noise 切成多帧，`subscribe`
//!    的写入循环可能在写到第 2 帧时失败，此时第 1 帧已经在 TCP 缓冲区里，
//!    对端可能已经收到。所以结果里必须带 `wrote_frames`，只有 0 帧才能说
//!    「这次发送没有开始」；其余一律 UNKNOWN，绝不自动重发。
//! 2. **任何非 event JSON 都不算 ack。** 服务端的错误体也是 JSON 对象，
//!    直接当成功会把「被拒」记成「已送达」。ack 必须通过
//!    [`validate_ack`]：2xx 状态、无 error 体、有非空 `message_id`。
//! 3. **取消要能中断写入和等 ack。** 只在外面 `select!` 拦一层是不够的：
//!    写入阶段要逐帧检查，等 ack 阶段要能被唤醒，采集阶段同理。

use crate::gateway::{ConnectedGateway, GatewayError, Subscription};
use crate::transport::{HttpRequest, Service};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::watch;

/// 网页端广告的能力，`delta_stream` 决定服务端是否推文本增量。
pub const CHAT_CAPABILITIES: &[&str] = &[
  "hotel_results",
  "chat_cancel",
  "delta_stream",
  "custom_reactions",
  "custom_reactions_facebook_thumbs_up_v1",
];

pub fn build_params(text: &str, node_id: &str, session_id: Option<&str>) -> Value {
  let mut p = json!({
    "items": [{ "type": "text", "text": text }],
    "node_id": node_id,
    "capabilities": CHAT_CAPABILITIES,
  });
  if let Some(sid) = session_id {
    if !sid.is_empty() {
      p["session_id"] = json!(sid);
    }
  }
  p
}

/// 一条合法的发送 ack。
#[derive(Debug, Clone, PartialEq)]
pub struct Ack {
  /// 服务端为这条消息分配的 id。空值/缺失一律不算 ack。
  pub message_id: String,
  pub session_id: Option<String>,
  /// 原始对象，交给上层展示（不含正文）。
  pub raw: Value,
}

/// 拒绝原因。区分「这行根本不是 ack」和「这是一条明确的错误响应」：
/// - 前者继续等（可能是心跳/无关对象）；
/// - 后者立刻失败——请求已经到服务端了，不能再假装成功。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AckReject {
  NotAck,
  Error(String),
}

fn non_empty_str(v: Option<&Value>) -> Option<String> {
  v.and_then(Value::as_str)
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_string)
}

/// 从错误体里尽量取一句人话，截断到 120 字符（不记正文）。
fn error_hint(v: &Value) -> String {
  let raw = v
    .get("error")
    .or_else(|| v.get("message"))
    .or_else(|| v.get("detail"))
    .map(|e| match e {
      Value::String(s) => s.clone(),
      other => other.to_string(),
    })
    .unwrap_or_default();
  raw.chars().take(120).collect()
}

/// 严格校验一行响应是否是「成功的发送 ack」。
///
/// 规则（任一条不满足即不是成功）：
/// 1. HTTP 状态必须是 2xx（`None` = 还没收到响应头，只按 body 判断）；
/// 2. 必须是 JSON 对象；
/// 3. 不能是错误响应：`type == "error"`、非空 `error`、`code` 非 0/200、
///    或 `status` 明确是失败态；
/// 4. 必须有非空 `message_id`。
///
/// 第 3 条命中 → [`AckReject::Error`]；只缺 `message_id` → [`AckReject::NotAck`]。
pub fn validate_ack(v: &Value, http_status: Option<u16>) -> Result<Ack, AckReject> {
  // 只拒绝**明确的错误码**（3xx/4xx/5xx）。0 表示响应帧里没有状态字段
  //（Noise 帧里 status 是可选的），不能据此判定失败，否则正常 ack 会被
  // 一律判成 UNKNOWN。
  if let Some(status) = http_status {
    if (300..600).contains(&status) {
      let hint = error_hint(v);
      return Err(AckReject::Error(if hint.is_empty() {
        format!("HTTP {status}（无错误体）")
      } else {
        format!("HTTP {status}: {hint}")
      }));
    }
  }
  let Some(obj) = v.as_object() else {
    return Err(AckReject::NotAck);
  };

  if obj.get("type").and_then(Value::as_str) == Some("error") {
    let hint = error_hint(v);
    return Err(AckReject::Error(if hint.is_empty() {
      "服务端返回 error 事件".to_string()
    } else {
      hint
    }));
  }
  match obj.get("error") {
    None | Some(Value::Null) => {}
    // `"error": ""` / `"error": {}` 视为没有错误，避免把可选字段误判成失败。
    Some(Value::String(s)) if s.trim().is_empty() => {}
    Some(Value::Object(m)) if m.is_empty() => {}
    Some(Value::Array(a)) if a.is_empty() => {}
    Some(_) => return Err(AckReject::Error(error_hint(v))),
  }
  if let Some(code) = obj.get("code") {
    let bad = match code {
      Value::Number(n) => !matches!(n.as_i64(), Some(0) | Some(200)),
      Value::String(s) => {
        let t = s.trim();
        !(t.is_empty() || t == "0" || t == "200" || t.eq_ignore_ascii_case("ok"))
      }
      _ => false,
    };
    if bad {
      return Err(AckReject::Error(format!(
        "code={} {}",
        code,
        error_hint(v)
      )));
    }
  }
  if let Some(status) = obj.get("status") {
    let failed = match status {
      Value::Number(n) => !matches!(n.as_i64(), Some(s) if (200..300).contains(&s)),
      Value::String(s) => matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "error" | "failed" | "failure" | "denied" | "rejected" | "cancelled" | "canceled"
      ),
      _ => false,
    };
    if failed {
      return Err(AckReject::Error(format!(
        "status={} {}",
        status,
        error_hint(v)
      )));
    }
  }

  match non_empty_str(obj.get("message_id")) {
    Some(message_id) => Ok(Ack {
      message_id,
      session_id: non_empty_str(obj.get("session_id")),
      raw: v.clone(),
    }),
    None => Err(AckReject::NotAck),
  }
}

/// 可按行读取的响应源。抽出来是为了让 ack 判定、取消、超时能被单测覆盖，
/// 不必真的起一条 WSS + Noise 连接。
#[allow(async_fn_in_trait)] // 只在本 crate 内使用，不做 dyn，Send 约束由调用方的 select! 保证
pub trait LineSource {
  fn http_status(&self) -> Option<u16>;
  async fn next_line(&mut self, wait: Duration) -> Result<Option<String>, GatewayError>;
}

impl<S> LineSource for Subscription<S>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  fn http_status(&self) -> Option<u16> {
    self.status()
  }

  async fn next_line(&mut self, wait: Duration) -> Result<Option<String>, GatewayError> {
    Subscription::next_line(self, wait).await
  }
}

/// 等一条合法 ack，直到超时 / 断流 / 取消。
///
/// 非 event 行按行校验：明确的错误响应立即失败；既不是 ack 也不是错误的
/// 对象（心跳、无关通知）跳过继续等，绝不把它们当成功。
pub async fn read_ack<L: LineSource>(
  src: &mut L,
  timeout: Duration,
  mut cancel: Option<&mut watch::Receiver<bool>>,
) -> Result<Ack, GatewayError> {
  let deadline = tokio::time::Instant::now() + timeout;
  loop {
    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
    if left.is_zero() {
      return Err(GatewayError::Timeout("chat.stream ack"));
    }
    let next = match cancel.as_deref_mut() {
      Some(rx) => tokio::select! {
        r = src.next_line(left) => r,
        _ = rx.changed() => return Err(GatewayError::Cancelled),
      },
      None => src.next_line(left).await,
    };
    let line = match next {
      Ok(Some(l)) => l,
      Ok(None) => {
        return Err(GatewayError::Closed(
          "chat.stream ended without ack".to_string(),
        ))
      }
      Err(e) => return Err(e),
    };
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
      // 非 JSON 行（保活/半截帧）：继续等。
      continue;
    };
    if v.get("type").and_then(Value::as_str) == Some("event") {
      continue;
    }
    match validate_ack(&v, src.http_status()) {
      Ok(ack) => return Ok(ack),
      Err(AckReject::NotAck) => continue,
      Err(AckReject::Error(m)) => return Err(GatewayError::Protocol(m)),
    }
  }
}

/// 一次发送的结果，**区分「请求有没有真正写到线上」**。
///
/// 这决定台账怎么记账，不能混：
/// - `on_wire == false`：一帧都没写出去（加密/序列化/首帧前失败），
///   账号侧可以据此判断「这次发送没有开始」；
/// - `on_wire == true`：至少一帧已经写进 WS，之后的任何失败（断流、超时、
///   ack 丢失、写第 2 帧失败）都意味着**上游可能已经收到**，只能记
///   UNKNOWN，绝不重发。
#[derive(Debug)]
pub struct SendAttemptResult {
  pub on_wire: bool,
  /// 已经写进 WS 的帧数（0 表示确定没开始）。
  pub wrote_frames: usize,
  pub outcome: Result<Ack, GatewayError>,
}

/// 发送一次并等待 ack。返回 ack。
/// 泛型于底层 WS 流：直连与代理连接都可复用同一发送逻辑。
pub async fn send_message<S>(
  gw: ConnectedGateway<S>,
  text: &str,
  node_id: &str,
  session_id: Option<&str>,
  timeout: Duration,
) -> Result<Ack, GatewayError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  send_message_tracked(gw, text, node_id, session_id, timeout, None)
    .await
    .outcome
}

/// 与 [`send_message`] 相同，但额外报告「请求是否已经写到线上」，并允许在
/// 写入阶段与等 ack 阶段被 `cancel` 中断。
pub async fn send_message_tracked<S>(
  gw: ConnectedGateway<S>,
  text: &str,
  node_id: &str,
  session_id: Option<&str>,
  timeout: Duration,
  mut cancel: Option<&mut watch::Receiver<bool>>,
) -> SendAttemptResult
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  let params = build_params(text, node_id, session_id);
  let req = HttpRequest {
    service: Service::Daemon,
    verb: "POST".to_string(),
    path: "/chat/stream".to_string(),
    headers: vec![
      ("Content-Type".to_string(), "application/json".to_string()),
      ("x-request-id".to_string(), uuid::Uuid::new_v4().to_string()),
      ("x-app-id".to_string(), "hatch-web".to_string()),
      ("Accept-Language".to_string(), "en-US".to_string()),
    ],
    body: serde_json::to_vec(&params).unwrap_or_default(),
  };
  let attempt = gw
    .subscribe_tracked(req, cancel.as_deref_mut())
    .await;
  let wrote_frames = attempt.wrote_frames;
  let mut sub = match attempt.result {
    Ok(s) => s,
    // `wrote_frames == 0` 才是「这次发送没有开始」；写过帧的一律结果不明。
    Err(e) => {
      return SendAttemptResult {
        on_wire: wrote_frames > 0,
        wrote_frames,
        outcome: Err(e),
      }
    }
  };
  let outcome = read_ack(&mut sub, timeout, cancel).await;
  SendAttemptResult {
    // 订阅成功说明整条请求已经在线上。
    on_wire: true,
    wrote_frames,
    outcome,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::VecDeque;
  use std::sync::atomic::AtomicBool;
  use std::sync::Arc;

  #[test]
  fn params_include_text_item_and_capabilities() {
    let p = build_params("hello", "node-1", None);
    assert_eq!(p["items"][0]["type"], "text");
    assert_eq!(p["items"][0]["text"], "hello");
    assert_eq!(p["node_id"], "node-1");
    assert!(p["capabilities"].as_array().unwrap().iter().any(|c| c == "delta_stream"));
    assert!(p.get("session_id").is_none());
  }

  #[test]
  fn params_include_session_when_given() {
    let p = build_params("x", "n", Some("sess-9"));
    assert_eq!(p["session_id"], "sess-9");
    let p2 = build_params("x", "n", Some(""));
    assert!(p2.get("session_id").is_none());
  }

  // ---- ack 校验：任何非 event JSON 都不能被当成成功 ----------------

  #[test]
  fn ack_with_message_id_is_accepted() {
    let v = json!({"message_id": "m-1", "session_id": "s-1", "channel": "web"});
    let ack = validate_ack(&v, Some(200)).expect("合法 ack");
    assert_eq!(ack.message_id, "m-1");
    assert_eq!(ack.session_id.as_deref(), Some("s-1"));
  }

  #[test]
  fn error_body_is_never_an_ack() {
    let cases = [
      json!({"error": "invalid session"}),
      json!({"error": {"code": "bad_request", "message": "nope"}}),
      json!({"type": "error", "message": "denied"}),
      json!({"code": 401, "message": "unauthorized"}),
      json!({"code": "403"}),
      json!({"status": "failed"}),
      json!({"status": 500}),
    ];
    for v in cases {
      match validate_ack(&v, Some(200)) {
        Err(AckReject::Error(_)) => {}
        other => panic!("错误体必须被拒绝，实际: {other:?} for {v}"),
      }
    }
  }

  #[test]
  fn non_2xx_status_wins_over_body() {
    // 即便 body 里恰好有 message_id，HTTP 非 2xx 也不是成功。
    let v = json!({"message_id": "m-1"});
    match validate_ack(&v, Some(502)) {
      Err(AckReject::Error(m)) => assert!(m.contains("502")),
      other => panic!("5xx 必须被拒绝，实际: {other:?}"),
    }
  }

  #[test]
  fn missing_status_field_does_not_poison_a_good_ack() {
    // 响应帧里 status 缺失时解出来是 0：不能因此把合法 ack 判成失败。
    let ack = validate_ack(&json!({"message_id": "m-1"}), Some(0)).expect("缺状态码不应误判");
    assert_eq!(ack.message_id, "m-1");
  }

  #[test]
  fn plain_object_without_message_id_is_not_ack() {
    assert_eq!(validate_ack(&json!({}), None), Err(AckReject::NotAck));
    assert_eq!(
      validate_ack(&json!({"message_id": "   "}), None),
      Err(AckReject::NotAck)
    );
    assert_eq!(validate_ack(&json!("ok"), None), Err(AckReject::NotAck));
    // 空 error 字段是可选的「没有错误」，不能当作错误体。
    assert!(matches!(
      validate_ack(&json!({"error": "", "message_id": "m"}), None),
      Ok(_)
    ));
  }

  // ---- 等 ack：跳过心跳、错误体立即失败、可取消 ----------------

  struct MockSource {
    status: Option<u16>,
    lines: VecDeque<Result<Option<String>, GatewayError>>,
    /// 每次 next_line 之前先等这个（模拟迟迟不来的 ack）。
    stall: Option<Arc<tokio::sync::Notify>>,
    canceled: Arc<AtomicBool>,
  }

  impl MockSource {
    fn new(lines: Vec<&str>) -> Self {
      Self {
        status: Some(200),
        lines: lines
          .into_iter()
          .map(|l| Ok(Some(l.to_string())))
          .collect(),
        stall: None,
        canceled: Arc::new(AtomicBool::new(false)),
      }
    }
  }

  impl LineSource for MockSource {
    fn http_status(&self) -> Option<u16> {
      self.status
    }

    async fn next_line(&mut self, _wait: Duration) -> Result<Option<String>, GatewayError> {
      if let Some(n) = &self.stall {
        n.notified().await;
        return Err(GatewayError::Cancelled);
      }
      let _ = &self.canceled;
      self.lines.pop_front().unwrap_or(Ok(None))
    }
  }

  #[tokio::test]
  async fn read_ack_skips_heartbeat_and_events() {
    let mut src = MockSource::new(vec![
      "",
      "not-json",
      r#"{"type":"event","event":"delta.text_append","payload":{"text":"hi"}}"#,
      r#"{}"#,
      r#"{"message_id":"m-7","session_id":"s-2"}"#,
    ]);
    let ack = read_ack(&mut src, Duration::from_secs(1), None)
      .await
      .expect("应该跳过噪声拿到 ack");
    assert_eq!(ack.message_id, "m-7");
    assert_eq!(ack.session_id.as_deref(), Some("s-2"));
  }

  #[tokio::test]
  async fn read_ack_fails_fast_on_error_body() {
    let mut src = MockSource::new(vec![r#"{"error":"quota exhausted"}"#]);
    match read_ack(&mut src, Duration::from_secs(1), None).await {
      Err(GatewayError::Protocol(m)) => assert!(m.contains("quota exhausted")),
      other => panic!("错误体必须立即失败，实际: {other:?}"),
    }
  }

  #[tokio::test]
  async fn read_ack_closed_stream_is_error() {
    let mut src = MockSource::new(vec![]);
    assert!(matches!(
      read_ack(&mut src, Duration::from_secs(1), None).await,
      Err(GatewayError::Closed(_))
    ));
  }

  #[tokio::test]
  async fn read_ack_honours_cancel_while_waiting() {
    let mut src = MockSource::new(vec![]);
    src.stall = Some(Arc::new(tokio::sync::Notify::new()));
    let (tx, rx) = watch::channel(false);
    let mut rx = rx;
    let handle = tokio::spawn(async move {
      // 模拟「等 ack 期间用户按了停止」。
      tokio::time::sleep(Duration::from_millis(10)).await;
      let _ = tx.send(true);
    });
    let res = read_ack(&mut src, Duration::from_secs(30), Some(&mut rx)).await;
    handle.await.unwrap();
    assert!(matches!(res, Err(GatewayError::Cancelled)));
  }
}
