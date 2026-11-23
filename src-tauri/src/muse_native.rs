// Spike 原生客户端的 App 接线（Issue #12 #2–#4）：
// - Cookie 桥：iOS 上经 account-session 插件从 Swift 取各账号
//   WKWebsiteDataStore 的 muse.ai 会话 cookie（内存传递，见下）；
// - HTTP 客户端：控制面用的 reqwest（桌面与 iOS 均为 native-tls）；
// - 诊断命令 muse_native_diagnose：跑一次“取凭证 → WSS → Noise 握手 →
//   connection.ping”，返回不含任何密钥的结果；
// - Spike 版启动自检：只在 bundle id 不是日常版时跑一次，日常版绝不自动跑。
//
// 协议实现在 muse-native crate；这里只有平台接线，不含协议逻辑。

use muse_native_client::control_plane::ControlPlaneFetch;
use muse_native_client::cookie::{SessionCookie, SessionCookies};
use muse_native_client::approvals::{parse_pending, ApprovalItem};
use muse_native_client::gateway::{GatewayError, MuseGateway};
use muse_native_client::transport::{HttpRequest, Service};
use muse_native_client::watch::{WatchSummary, WatchTracker};
use serde::Serialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::AppHandle;
use zeroize::Zeroize;

use crate::providers::ProviderId;
use crate::session_manager::SessionManager;
use crate::storage::AuthState;

/// 日常版 bundle id：自检只在 Spike 版（换了 bundle id 的包）里跑。
const DAILY_IDENTIFIER: &str = "com.ldjx.accountmanager";

/// Cookie 桥超时：15 秒。WKWebsiteDataStore 本地读 cookie 应毫秒级完成，
/// 超时说明 Swift 桥没响应（getAllCookies 回调没回来）。
#[cfg(not(target_os = "android"))]
const COOKIE_TIMEOUT: Duration = Duration::from_secs(15);

/// 诊断整体超时：60 秒。控制面（20s×N 请求）+ 握手（10s）+ ping（10s）
/// 再慢也不应超过这个时间。超时返回明确的失败结果，不让前端一直转圈。
const DIAGNOSE_TIMEOUT: Duration = Duration::from_secs(60);

/// 诊断结果——只含阶段与状态，不含 cookie / token / 凭证值。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpikeDiagnose {
  pub ok: bool,
  pub phase: String,
  pub handshake_completed: bool,
  pub ping_status: Option<u16>,
  pub cvm_detected: bool,
  pub error: Option<String>,
}

impl SpikeDiagnose {
  fn fail(phase: &str, error: impl Into<String>) -> Self {
    Self {
      ok: false,
      phase: phase.to_string(),
      handshake_completed: false,
      ping_status: None,
      cvm_detected: false,
      error: Some(error.into()),
    }
  }
}

#[cfg(not(target_os = "android"))]
struct ReqwestFetch {
  client: reqwest::Client,
}

#[cfg(not(target_os = "android"))]
impl ReqwestFetch {
  fn new() -> Result<Self, String> {
    let client = reqwest::Client::builder()
      .timeout(Duration::from_secs(20))
      .build()
      .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    Ok(Self { client })
  }
}

#[cfg(not(target_os = "android"))]
impl ControlPlaneFetch for ReqwestFetch {
  async fn post(
    &self,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
  ) -> Result<(u16, Vec<u8>), String> {
    let mut req = self.client.post(url).body(body.to_vec());
    for (k, v) in headers {
      req = req.header(*k, *v);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    Ok((status, bytes.to_vec()))
  }
}

/// 是否 Spike 版：bundle id 不是日常版即为 Spike 版。
/// 前端据此决定是否显示 Spike 专属 UI（顶栏徽章）。
/// 诊断区不走这个门控（日常版也显示，方便后续调试——用户 2026-11-23 定）。
#[tauri::command]
pub fn is_spike_build(app: AppHandle) -> bool {
  app.config().identifier != DAILY_IDENTIFIER
}

/// 跑一次原生握手诊断：取凭证 → WSS → Noise 握手 → connection.ping。
/// 只做诊断，不发聊天、不做 watcher、不做通知、不改 UI。
/// cookie 只在内存里用，控制面跑完立即 zeroize。
///
/// 超时实现说明（2026-11-23 实测教训）：不能用 tokio::time::timeout 包
/// diagnose_inner——如果内层在某处同步阻塞（如插件调用卡住工作线程），
/// 同一任务里的 timer 永远得不到轮询，超时不会触发（spike4/spike5 实测：
/// 前端 70 秒超时先触发，Rust 60 秒超时没反应）。
/// 改成把 diagnose_inner 派到独立任务，再用 select! 对 sleep 竞速：
/// 计时器在另一个任务里，即使内层任务卡死也能按时返回。
pub async fn diagnose(
  app: &AppHandle,
  provider_id: &str,
  uuid: &str,
) -> Result<SpikeDiagnose, String> {
  let app_owned = app.clone();
  let provider_owned = provider_id.to_string();
  let uuid_owned = uuid.to_string();
  let handle = tokio::spawn(async move {
    diagnose_inner(&app_owned, &provider_owned, &uuid_owned).await
  });
  tokio::select! {
    res = handle => match res {
      Ok(r) => r,
      Err(e) => Err(format!("诊断任务异常结束: {e}")),
    },
    _ = tokio::time::sleep(DIAGNOSE_TIMEOUT) => Ok(SpikeDiagnose::fail(
      "timeout",
      "诊断整体超时（60 秒）：某一步卡住了，请重启 App 再试；如反复出现请反馈。",
    )),
  }
}

async fn diagnose_inner(
  app: &AppHandle,
  provider_id: &str,
  uuid: &str,
) -> Result<SpikeDiagnose, String> {
  if provider_id != ProviderId::Muse.as_str() {
    return Err("Spike 原生诊断只支持 Muse 账号".to_string());
  }
  #[cfg(target_os = "android")]
  {
    let _ = (app, uuid);
    return Err(
      "Spike 原生诊断在 Android 上不可用（Cookie 桥只接了 iOS WKWebsiteDataStore）".to_string(),
    );
  }
  #[cfg(not(target_os = "android"))]
  {
    // Cookie 取用也走独立任务 + select! 竞速（同上：timeout 包不住同步阻塞）。
    let app_owned = app.clone();
    let uuid_owned = uuid.to_string();
    let cookie_handle =
      tokio::spawn(async move { fetch_session_cookies(&app_owned, &uuid_owned).await });
    let mut cookies = tokio::select! {
      res = cookie_handle => match res {
        Ok(r) => r?,
        Err(e) => return Err(format!("取 cookie 任务异常结束: {e}")),
      },
      _ = tokio::time::sleep(COOKIE_TIMEOUT) => {
        return Ok(SpikeDiagnose::fail(
          "timeout",
          "取 muse.ai 会话 cookie 超时（15 秒）：Cookie 桥没响应，请重启 App 再试。",
        ));
      }
    };
    if cookies.is_empty() {
      return Ok(SpikeDiagnose::fail(
        "no_cookies",
        "该账号在 Spike App 里还没有 muse.ai 登录态：请先在 Spike App 里打开 Muse 账号完成登录，再跑诊断。",
      ));
    }
    eprintln!(
      "[muse-native] diagnose: {} cookie(s) from WKWebsiteDataStore, names={:?}",
      cookies.len(),
      cookies.names()
    );
    let fetch = ReqwestFetch::new()?;
    let gateway = MuseGateway::new(fetch);
    let mut connected = match gateway.connect(&cookies).await {
      Ok(c) => c,
      Err(e) => {
        cookies.zeroize();
        return Ok(report_connect_error(e));
      }
    };
    // 控制面已跑完：cookie 不再需要，立即清零。
    cookies.zeroize();
    eprintln!("[muse-native] diagnose: noise handshake completed");
    match connected.ping().await {
      Ok(status) => {
        connected.close().await;
        eprintln!("[muse-native] diagnose: connection.ping status={status}");
        Ok(SpikeDiagnose {
          ok: status < 400,
          phase: "ready".to_string(),
          handshake_completed: true,
          ping_status: Some(status),
          cvm_detected: false,
          error: if status < 400 {
            None
          } else {
            Some(format!("connection.ping HTTP {status}"))
          },
        })
      }
      Err(e) => {
        connected.close().await;
        Ok(SpikeDiagnose {
          ok: false,
          phase: "ping".to_string(),
          handshake_completed: true,
          ping_status: None,
          cvm_detected: false,
          error: Some(e.to_string()),
        })
      }
    }
  }
}

/// B4 发送结果（字段与前端契约一致）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendOutcome {
  pub ok: bool,
  pub status: String,
  pub send_id: String,
  pub ack: Option<serde_json::Value>,
  pub session_id: Option<String>,
  pub error: Option<String>,
}

/// 发送一条消息：申请单发锁 → 建连 → 发一次 → 按结果记账。
/// 超时/断线一律记 UNKNOWN，绝不自动重发。
pub async fn send_probe(
  app: &AppHandle,
  ledger: &crate::send_ledger::SendLedger,
  uuid: &str,
  text: &str,
  node_id: &str,
  session_id: Option<&str>,
) -> SendOutcome {
  use crate::send_ledger::SendError;
  let conversation = session_id.unwrap_or("new").to_string();
  let send_id = match ledger.begin(uuid, &conversation, text) {
    Ok(id) => id,
    Err(SendError::Busy(existing)) => {
      return SendOutcome {
        ok: false,
        status: "BUSY".to_string(),
        send_id: existing,
        ack: None,
        session_id: session_id.map(str::to_string),
        error: Some("该账号已有一条发送在进行中（不排队）".to_string()),
      }
    }
    Err(SendError::Unknown(e)) => {
      return SendOutcome {
        ok: false,
        status: "ERROR".to_string(),
        send_id: String::new(),
        ack: None,
        session_id: None,
        error: Some(e),
      }
    }
  };

  #[cfg(target_os = "android")]
  {
    let _ = (app, node_id, text);
    ledger.fail(&send_id, "Android 不可用".to_string());
    return SendOutcome {
      ok: false, status: "DEFINITIVELY_FAILED".to_string(), send_id,
      ack: None, session_id: session_id.map(str::to_string),
      error: Some("发送探针在 Android 上不可用".to_string()),
    };
  }

  #[cfg(not(target_os = "android"))]
  {
    let app_owned = app.clone();
    let uuid_owned = uuid.to_string();
    let handle = tokio::spawn(async move { fetch_session_cookies(&app_owned, &uuid_owned).await });
    let mut cookies = match tokio::time::timeout(COOKIE_TIMEOUT, handle).await {
      Err(_) => {
        ledger.fail(&send_id, "取 cookie 超时".to_string());
        return SendOutcome {
          ok: false, status: "DEFINITIVELY_FAILED".to_string(), send_id,
          ack: None, session_id: session_id.map(str::to_string),
          error: Some("取 muse.ai 会话 cookie 超时（15 秒）".to_string()),
        };
      }
      Ok(Ok(Ok(c))) => c,
      Ok(Ok(Err(e))) => {
        ledger.fail(&send_id, e.clone());
        return SendOutcome {
          ok: false, status: "DEFINITIVELY_FAILED".to_string(), send_id,
          ack: None, session_id: session_id.map(str::to_string), error: Some(e),
        };
      }
      Ok(Err(e)) => {
        ledger.fail(&send_id, e.to_string());
        return SendOutcome {
          ok: false, status: "DEFINITIVELY_FAILED".to_string(), send_id,
          ack: None, session_id: session_id.map(str::to_string),
          error: Some(format!("取 cookie 任务异常: {e}")),
        };
      }
    };
    if cookies.is_empty() {
      ledger.fail(&send_id, "无登录态".to_string());
      return SendOutcome {
        ok: false, status: "DEFINITIVELY_FAILED".to_string(), send_id,
        ack: None, session_id: session_id.map(str::to_string),
        error: Some("该账号还没有 muse.ai 登录态".to_string()),
      };
    }

    let fetch = match ReqwestFetch::new() {
      Ok(f) => f,
      Err(e) => {
        cookies.zeroize();
        ledger.fail(&send_id, e.clone());
        return SendOutcome {
          ok: false, status: "DEFINITIVELY_FAILED".to_string(), send_id,
          ack: None, session_id: session_id.map(str::to_string), error: Some(e),
        };
      }
    };
    let gateway = MuseGateway::new(fetch);
    let connected = match gateway.connect(&cookies).await {
      Ok(c) => c,
      Err(e) => {
        cookies.zeroize();
        ledger.fail(&send_id, e.to_string());
        return SendOutcome {
          ok: false, status: "DEFINITIVELY_FAILED".to_string(), send_id,
          ack: None, session_id: session_id.map(str::to_string),
          error: Some(e.to_string()),
        };
      }
    };
    cookies.zeroize();
    ledger.mark_in_flight(&send_id);

    // 发送用一次性连接：mark_in_flight 之后所有权交给 send_message。
    match muse_native_client::chat_send::send_message(
      connected,
      text,
      node_id,
      session_id,
      Duration::from_secs(30),
    )
    .await
    {
      Ok((ack, sid)) => {
        ledger.confirm(&send_id);
        SendOutcome {
          ok: true,
          status: "CONFIRMED".to_string(),
          send_id,
          ack: Some(ack),
          session_id: sid,
          error: None,
        }
      }
      Err(e) => {
        ledger.mark_unknown(&send_id, e.to_string());
        SendOutcome {
          ok: false, status: "UNKNOWN".to_string(), send_id,
          ack: None, session_id: session_id.map(str::to_string),
          error: Some(e.to_string()),
        }
      }
    }
  }
}

/// #10 存活探针状态：记录一条长连接从建立到断开的时间线。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Liveness {
  pub running: bool,
  pub started_ms: u128,
  pub connected_ms: Option<u128>,
  pub last_event_ms: Option<u128>,
  pub ended_ms: Option<u128>,
  pub events: u64,
  pub error: Option<String>,
}

#[derive(Default)]
pub struct LivenessState {
  inner: std::sync::Mutex<Liveness>,
}

impl LivenessState {
  pub fn snapshot(&self) -> Liveness {
    self.inner.lock().unwrap().clone()
  }
  fn set<F: FnOnce(&mut Liveness)>(&self, f: F) {
    f(&mut self.inner.lock().unwrap());
  }
}

fn now_ms_u128() -> u128 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_millis())
    .unwrap_or(0)
}

/// 开一条长连接并持续读事件，直到超时或断开；结果写进共享状态。
/// 用途是实测「App 退到后台后连接还能活多久」，不做保活、不重连。
pub fn liveness_start(
  app: &AppHandle,
  state: &std::sync::Arc<LivenessState>,
  uuid: &str,
  seconds: u64,
) {
  if state.snapshot().running {
    return;
  }
  let seconds = seconds.clamp(30, 3600);
  state.set(|l| {
    *l = Liveness {
      running: true,
      started_ms: now_ms_u128(),
      ..Default::default()
    }
  });

  let app_owned = app.clone();
  let uuid_owned = uuid.to_string();
  let state_owned = state.clone();
  tokio::spawn(async move {
    #[cfg(target_os = "android")]
    {
      let _ = (app_owned, uuid_owned, seconds);
      state_owned.set(|l| {
        l.running = false;
        l.ended_ms = Some(now_ms_u128());
        l.error = Some("Android 不可用".to_string());
      });
      return;
    }
    #[cfg(not(target_os = "android"))]
    {
      let cookies = match fetch_session_cookies(&app_owned, &uuid_owned).await {
        Ok(c) => c,
        Err(e) => {
          state_owned.set(|l| {
            l.running = false;
            l.ended_ms = Some(now_ms_u128());
            l.error = Some(e);
          });
          return;
        }
      };
      if cookies.is_empty() {
        state_owned.set(|l| {
          l.running = false;
          l.ended_ms = Some(now_ms_u128());
          l.error = Some("无登录态".to_string());
        });
        return;
      }
      let fetch = match ReqwestFetch::new() {
        Ok(f) => f,
        Err(e) => {
          state_owned.set(|l| {
            l.running = false;
            l.ended_ms = Some(now_ms_u128());
            l.error = Some(e);
          });
          return;
        }
      };
      let gateway = MuseGateway::new(fetch);
      let mut cookies = cookies;
      let connected = match gateway.connect(&cookies).await {
        Ok(c) => c,
        Err(e) => {
          cookies.zeroize();
          state_owned.set(|l| {
            l.running = false;
            l.ended_ms = Some(now_ms_u128());
            l.error = Some(e.to_string());
          });
          return;
        }
      };
      cookies.zeroize();
      state_owned.set(|l| l.connected_ms = Some(now_ms_u128()));

      let body = br#"{"capabilities":["hotel_results","chat_cancel","delta_stream","custom_reactions","custom_reactions_facebook_thumbs_up_v1"]}"#.to_vec();
      let req = HttpRequest {
        service: Service::Daemon,
        verb: "POST".to_string(),
        path: "/chat/subscribe".to_string(),
        headers: vec![
          ("Content-Type".to_string(), "application/json".to_string()),
          ("x-request-id".to_string(), uuid::Uuid::new_v4().to_string()),
          ("x-app-id".to_string(), "hatch-web".to_string()),
        ],
        body,
      };
      let mut sub = match connected.subscribe(req).await {
        Ok(s) => s,
        Err(e) => {
          state_owned.set(|l| {
            l.running = false;
            l.ended_ms = Some(now_ms_u128());
            l.error = Some(e.to_string());
          });
          return;
        }
      };
      let deadline = Instant::now() + Duration::from_secs(seconds);
      let mut error = None;
      loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
          break;
        }
        match sub.next_line(left).await {
          Ok(Some(_line)) => {
            state_owned.set(|l| {
              l.events += 1;
              l.last_event_ms = Some(now_ms_u128());
            });
          }
          Ok(None) => {
            error = Some("订阅结束".to_string());
            break;
          }
          Err(GatewayError::Timeout(_)) => break,
          Err(e) => {
            error = Some(e.to_string());
            break;
          }
        }
      }
      state_owned.set(|l| {
        l.running = false;
        l.ended_ms = Some(now_ms_u128());
        l.error = error;
      });
    }
  });
}

/// 取一次会话 cookie 头（B3 额度用）。调用方用后立即丢弃，不落盘不打印。
pub async fn cookie_header_for(app: &AppHandle, uuid: &str) -> Result<String, String> {
  #[cfg(target_os = "android")]
  {
    let _ = (app, uuid);
    return Err("该功能在 Android 上不可用".to_string());
  }
  #[cfg(not(target_os = "android"))]
  {
    let app_owned = app.clone();
    let uuid_owned = uuid.to_string();
    let handle = tokio::spawn(async move { fetch_session_cookies(&app_owned, &uuid_owned).await });
    let mut cookies = tokio::select! {
      res = handle => match res {
        Ok(r) => r?,
        Err(e) => return Err(format!("取 cookie 任务异常结束: {e}")),
      },
      _ = tokio::time::sleep(COOKIE_TIMEOUT) => {
        return Err("取 muse.ai 会话 cookie 超时（15 秒）".to_string());
      }
    };
    if cookies.is_empty() {
      return Err("该账号还没有 muse.ai 登录态".to_string());
    }
    let header = cookies.cookie_header();
    cookies.zeroize();
    Ok(header)
  }
}

/// B1 `chat.subscribe` 探针（Issue #13 #5）：订阅 N 秒，只统计事件数量与去重结果。
/// 不返回任何事件文本、消息 id 或凭据，只给计数，便于真机核验。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchProbe {
  pub ok: bool,
  pub phase: String,
  pub seconds: u64,
  pub status: Option<u16>,
  pub summary: WatchSummary,
  pub lines: u64,
  pub non_event: u64,
  pub parse_errors: u64,
  pub type_counts: std::collections::BTreeMap<String, u64>,
  pub error: Option<String>,
}

pub async fn watch_probe(
  app: &AppHandle,
  provider_id: &str,
  uuid: &str,
  seconds: u64,
) -> Result<WatchProbe, String> {
  if provider_id != ProviderId::Muse.as_str() {
    return Err("订阅探针只支持 Muse 账号".to_string());
  }
  #[cfg(target_os = "android")]
  {
    let _ = (app, uuid, seconds);
    return Err(
      "订阅探针在 Android 上不可用（Cookie 桥只接了 iOS WKWebsiteDataStore）".to_string(),
    );
  }
  #[cfg(not(target_os = "android"))]
  {
    let seconds = seconds.clamp(5, 60);
    let app_owned = app.clone();
    let uuid_owned = uuid.to_string();
    let cookie_handle =
      tokio::spawn(async move { fetch_session_cookies(&app_owned, &uuid_owned).await });
    let mut cookies = tokio::select! {
      res = cookie_handle => match res {
        Ok(r) => r?,
        Err(e) => return Err(format!("取 cookie 任务异常结束: {e}")),
      },
      _ = tokio::time::sleep(COOKIE_TIMEOUT) => {
        return Err("取 muse.ai 会话 cookie 超时（15 秒）".to_string());
      }
    };
    if cookies.is_empty() {
      return Ok(WatchProbe {
        ok: false,
        phase: "no_cookies".to_string(),
        seconds,
        status: None,
        summary: WatchSummary::default(),
        lines: 0,
        non_event: 0,
        parse_errors: 0,
        type_counts: Default::default(),
        error: Some("该账号在 Spike App 里还没有 muse.ai 登录态".to_string()),
      });
    }
    let fetch = ReqwestFetch::new()?;
    let gateway = MuseGateway::new(fetch);
    let mut connected = match gateway.connect(&cookies).await {
      Ok(c) => c,
      Err(e) => {
        cookies.zeroize();
        return Ok(WatchProbe {
          ok: false,
          phase: "connect".to_string(),
          seconds,
          status: None,
          summary: WatchSummary::default(),
          lines: 0,
          non_event: 0,
          parse_errors: 0,
          type_counts: Default::default(),
          error: Some(e.to_string()),
        });
      }
    };
    cookies.zeroize();

    // chat.subscribe 入参：网页端会广告这些能力，其中 delta_stream 决定是否推送文本增量。
    let body = br#"{"capabilities":["hotel_results","chat_cancel","delta_stream","custom_reactions","custom_reactions_facebook_thumbs_up_v1"]}"#.to_vec();
    let req = HttpRequest {
      service: Service::Daemon,
      verb: "POST".to_string(),
      path: "/chat/subscribe".to_string(),
      headers: vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("x-request-id".to_string(), uuid::Uuid::new_v4().to_string()),
        ("x-app-id".to_string(), "hatch-web".to_string()),
        ("Accept-Language".to_string(), "en-US".to_string()),
      ],
      body,
    };
    let mut sub = match connected.subscribe(req).await {
      Ok(s) => s,
      Err(e) => {
        return Ok(WatchProbe {
          ok: false,
          phase: "subscribe".to_string(),
          seconds,
          status: None,
          summary: WatchSummary::default(),
          lines: 0,
          non_event: 0,
          parse_errors: 0,
          type_counts: Default::default(),
          error: Some(e.to_string()),
        });
      }
    };

    let mut tracker = WatchTracker::new();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut error = None;
    let mut lines: u64 = 0;
    let mut non_event: u64 = 0;
    let mut parse_errors: u64 = 0;
    let mut type_counts: std::collections::BTreeMap<String, u64> = Default::default();
    loop {
      let left = deadline.saturating_duration_since(Instant::now());
      if left.is_zero() {
        break;
      }
      match sub.next_line(left).await {
        Ok(Some(line)) => {
          lines += 1;
          match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(v) => {
              let ty = v
                .get("type")
                .and_then(|x| x.as_str())
                .unwrap_or("<none>")
                .to_string();
              *type_counts.entry(ty.clone()).or_insert(0) += 1;
              if ty == "event" {
                let _ = tracker.observe_line(&line);
              } else {
                non_event += 1;
              }
            }
            Err(_) => parse_errors += 1,
          }
        }
        Ok(None) => {
          error = Some("订阅已结束".to_string());
          break;
        }
        // 探针窗口到期：长连接空闲时本来就不会有下一行，属正常结束。
        Err(GatewayError::Timeout(_)) => break,
        Err(e) => {
          error = Some(e.to_string());
          break;
        }
      }
    }
    Ok(WatchProbe {
      ok: error.is_none(),
      phase: "subscribed".to_string(),
      seconds,
      status: sub.status(),
      summary: tracker.summary().clone(),
      lines,
      non_event,
      parse_errors,
      type_counts,
      error,
    })
  }
}

/// B2：只读拉取一次 `egress.approvals`，返回待审批数量与最新一条的脱敏摘要。
/// 不做任何 approve/deny，也不返回审批正文。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalsProbe {
  pub ok: bool,
  pub phase: String,
  pub status: Option<u16>,
  pub count: usize,
  pub first: Option<ApprovalItem>,
  pub error: Option<String>,
}

pub async fn approvals_probe(
  app: &AppHandle,
  provider_id: &str,
  uuid: &str,
) -> Result<ApprovalsProbe, String> {
  if provider_id != ProviderId::Muse.as_str() {
    return Err("审批探针只支持 Muse 账号".to_string());
  }
  #[cfg(target_os = "android")]
  {
    let _ = (app, uuid);
    return Err("审批探针在 Android 上不可用".to_string());
  }
  #[cfg(not(target_os = "android"))]
  {
    let app_owned = app.clone();
    let uuid_owned = uuid.to_string();
    let cookie_handle =
      tokio::spawn(async move { fetch_session_cookies(&app_owned, &uuid_owned).await });
    let mut cookies = tokio::select! {
      res = cookie_handle => match res {
        Ok(r) => r?,
        Err(e) => return Err(format!("取 cookie 任务异常结束: {e}")),
      },
      _ = tokio::time::sleep(COOKIE_TIMEOUT) => {
        return Err("取 muse.ai 会话 cookie 超时（15 秒）".to_string());
      }
    };
    if cookies.is_empty() {
      return Ok(ApprovalsProbe {
        ok: false, phase: "no_cookies".to_string(), status: None, count: 0,
        first: None, error: Some("该账号还没有 muse.ai 登录态".to_string()),
      });
    }
    let fetch = ReqwestFetch::new()?;
    let gateway = MuseGateway::new(fetch);
    let mut connected = match gateway.connect(&cookies).await {
      Ok(c) => c,
      Err(e) => {
        cookies.zeroize();
        return Ok(ApprovalsProbe {
          ok: false, phase: "connect".to_string(), status: None, count: 0,
          first: None, error: Some(e.to_string()),
        });
      }
    };
    cookies.zeroize();

    let req = HttpRequest {
      service: Service::Sentinel,
      verb: "GET".to_string(),
      path: "/approvals".to_string(),
      headers: vec![
        ("x-request-id".to_string(), uuid::Uuid::new_v4().to_string()),
        ("x-app-id".to_string(), "hatch-web".to_string()),
        ("Accept-Language".to_string(), "en-US".to_string()),
      ],
      body: Vec::new(),
    };
    let (status, body) = match connected.request(req).await {
      Ok(r) => r,
      Err(e) => {
        return Ok(ApprovalsProbe {
          ok: false, phase: "request".to_string(), status: None, count: 0,
          first: None, error: Some(e.to_string()),
        });
      }
    };
    let json: serde_json::Value = match serde_json::from_slice(&body) {
      Ok(v) => v,
      Err(e) => {
        return Ok(ApprovalsProbe {
          ok: false, phase: "parse".to_string(), status: Some(status), count: 0,
          first: None, error: Some(format!("响应不是 JSON: {e}")),
        });
      }
    };
    let list = parse_pending(&json);
    Ok(ApprovalsProbe {
      ok: status < 400,
      phase: "ready".to_string(),
      status: Some(status),
      count: list.len(),
      first: list.into_iter().next(),
      error: if status < 400 { None } else { Some(format!("egress.approvals HTTP {status}")) },
    })
  }
}

#[cfg(not(target_os = "android"))]
fn report_connect_error(e: GatewayError) -> SpikeDiagnose {
  match e {
    GatewayError::SignedOut => SpikeDiagnose::fail(
      "signed_out",
      "控制面 401/403：账号未登录或登录已过期，请在 Spike App 里重新登录 Muse 测试账号。",
    ),
    GatewayError::CvmRequired => SpikeDiagnose {
      ok: false,
      phase: "cvm_required".to_string(),
      handshake_completed: false,
      ping_status: None,
      cvm_detected: true,
      error: Some(
        "该 VM 是保密 VM（Owner RV challenge）：请在浏览器里打开 muse.ai 完成注册，或换标准 VM。"
          .to_string(),
      ),
    },
    GatewayError::ControlPlane(_) => SpikeDiagnose::fail("credentials", e.to_string()),
    GatewayError::Timeout(what) => SpikeDiagnose::fail("handshake", format!("{what} timed out")),
    other => SpikeDiagnose::fail("handshake", other.to_string()),
  }
}

#[cfg(target_os = "ios")]
async fn fetch_session_cookies(app: &AppHandle, uuid: &str) -> Result<SessionCookies, String> {
  let entries = crate::platform::ios::get_session_cookies(app, uuid).await?;
  Ok(SessionCookies {
    cookies: entries
      .into_iter()
      .map(|e| SessionCookie {
        name: e.name,
        value: e.value,
      })
      .collect(),
  })
}

#[cfg(all(not(target_os = "android"), not(target_os = "ios")))]
async fn fetch_session_cookies(_app: &AppHandle, _uuid: &str) -> Result<SessionCookies, String> {
  Err("Spike 原生诊断只在 iOS 可用（Cookie 桥只接了 WKWebsiteDataStore）".to_string())
}

/// Spike 版启动自检：bundle id 不是日常版时，对第一个已登录的 Muse 账号
/// 跑一次握手诊断，结果只打到诊断日志（不含密钥）。日常版直接返回。
pub async fn maybe_auto_diagnose(app: &AppHandle, mgr: &Arc<SessionManager>) {
  if app.config().identifier == DAILY_IDENTIFIER {
    return;
  }
  // 等启动任务就绪（插件桥、账号快照）。
  tokio::time::sleep(Duration::from_secs(3)).await;
  let snap = mgr.get_snapshot().await;
  let Some(acc) = snap
    .accounts
    .iter()
    .find(|a| a.provider_id == ProviderId::Muse && a.auth_state == AuthState::Authenticated)
  else {
    eprintln!("[muse-native] spike auto-diagnose: no authenticated Muse account, skip");
    return;
  };
  let uuid = acc.uuid.clone();
  match diagnose(app, ProviderId::Muse.as_str(), &uuid).await {
    Ok(r) => eprintln!(
      "[muse-native] spike auto-diagnose: ok={} phase={} handshake={} ping={:?} cvm={} error={:?}",
      r.ok, r.phase, r.handshake_completed, r.ping_status, r.cvm_detected, r.error
    ),
    Err(e) => eprintln!("[muse-native] spike auto-diagnose failed: {e}"),
  }
}
