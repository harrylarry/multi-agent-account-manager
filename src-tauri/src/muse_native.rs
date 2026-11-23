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
use muse_native_client::fingerprint::{self, FingerprintError, FingerprintResult, Sample};
use muse_native_client::gateway::{GatewayError, MuseGateway};
use muse_native_client::transport::{HttpRequest, Service};
use muse_native_client::watch::{WatchSummary, WatchTracker};
use serde::Serialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
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
use crate::egress::ReqwestFetch;
use crate::probe_cancel::ProbeCancel;
use crate::providers::ProviderId;
use crate::secrets::Secrets;
use crate::storage::ProxySetting;

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
  setting: Option<ProxySetting>,
) -> Result<SpikeDiagnose, String> {
  let app_owned = app.clone();
  let provider_owned = provider_id.to_string();
  let uuid_owned = uuid.to_string();
  let handle = tokio::spawn(async move {
    diagnose_inner(&app_owned, &provider_owned, &uuid_owned, setting.as_ref()).await
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
  setting: Option<&ProxySetting>,
) -> Result<SpikeDiagnose, String> {
  if provider_id != ProviderId::Muse.as_str() {
    return Err("Spike 原生诊断只支持 Muse 账号".to_string());
  }
  #[cfg(target_os = "android")]
  {
    let _ = (app, uuid, setting);
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
    let egress = match crate::egress::resolve_for_app(app, setting) {
      Ok(e) => e,
      Err(e) => {
        cookies.zeroize();
        return Ok(SpikeDiagnose::fail("egress", e));
      }
    };
    let mut connected = match egress.connect(&cookies).await {
      Ok(c) => c,
      Err(e) => {
        cookies.zeroize();
        return Ok(SpikeDiagnose::fail("connect", e.to_string()));
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
  setting: Option<&ProxySetting>,
  secrets: &Secrets,
  cancel: &ProbeCancel,
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
    let _ = (app, node_id, text, setting, secrets, cancel);
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

    // 后端取消：用户停止后不再发出这条消息。
    let (mut cancel_token, _cancel_guard) = cancel.register(uuid);
    if cancel_token.check() {
      cookies.zeroize();
      ledger.fail(&send_id, "用户取消".to_string());
      return SendOutcome {
        ok: false, status: "DEFINITIVELY_FAILED".to_string(), send_id,
        ack: None, session_id: session_id.map(str::to_string),
        error: Some("用户取消（未发送）".to_string()),
      };
    }

    // 账号级出站策略：启用代理时全程走代理，解析失败也不得退回直连。
    let egress = match crate::egress::AccountEgress::resolve(setting, secrets) {
      Ok(e) => e,
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
    let connected = match egress.connect(&cookies).await {
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
    let send_outcome = tokio::select! {
      r = connected.send_message(text, node_id, session_id, Duration::from_secs(30)) => Some(r),
      _ = cancel_token.cancelled() => None,
    };
    match send_outcome {
      None => {
        // 取消发生在发送阶段：可能已发出，按 UNKNOWN 记账，绝不重发。
        ledger.mark_unknown(&send_id, "发送阶段被取消".to_string());
        SendOutcome {
          ok: false, status: "UNKNOWN".to_string(), send_id,
          ack: None, session_id: session_id.map(str::to_string),
          error: Some("发送阶段被用户取消（结果不明，未重发）".to_string()),
        }
      }
      Some(Ok((ack, sid))) => {
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
      Some(Err(e)) => {
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

/// 单条探针的发送/采集状态，用于「整个检测任务」的汇总。
///
/// 与台账一一对应：`send_status` 直接取自该条探针自己的 SendAttempt，
/// 不允许用任务级结果顶替；`text_done` 表示是否真的收到 `message_done`。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintSendRecord {
  pub probe_id: String,
  pub send_status: String,
  pub text_done: bool,
  pub sample_used: bool,
  pub error: Option<String>,
}

/// #18 模型指纹探针结果。verdict/identification 的语义：
/// - `scored`：三条有效样本 + 校准置信度，是**有效识别结果**；
/// - `partial`：部分样本，只给候选排名，不算有效识别；
/// - `indeterminate`：不可判定（样本不足 / 请求或参考库失败），无候选。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintProbe {
  pub ok: bool,
  pub phase: String,
  pub verdict: String,
  pub identification: bool,
  pub result: Option<FingerprintResult>,
  /// 本次实际发出的探针条数（拿到 ack，不代表采集完成）。
  pub probes_sent: usize,
  /// 发送台账 CONFIRMED 的条数（ack + 收到 message_done）。
  pub probes_confirmed: usize,
  /// 本次实际采到并送评分的样本数。
  pub samples_collected: usize,
  /// 任务汇总：每条探针的独立发送/采集状态。
  pub sends: Vec<FingerprintSendRecord>,
  /// 隔离会话 id（非敏感，便于人工核对探针落在测试会话里）。
  pub session_id: Option<String>,
  pub error: Option<String>,
}

impl FingerprintProbe {
  fn fail(phase: &str, verdict: &str, error: impl Into<String>) -> Self {
    Self {
      ok: false,
      phase: phase.to_string(),
      verdict: verdict.to_string(),
      identification: false,
      result: None,
      probes_sent: 0,
      probes_confirmed: 0,
      samples_collected: 0,
      sends: Vec::new(),
      session_id: None,
      error: Some(error.into()),
    }
  }
}

/// #18：在隔离会话里发 3 条无语义计数探针，收齐回答后本地评分。
///
/// 安全约束：
/// - 全程占用单发锁（一次指纹评估视为一笔发送事务），期间不再接受其他发送；
/// - 每条探针"先订后发"，正文只从 chat.subscribe 收，绝不从别处拼；
/// - 拿到明确 ack 才算发出；超时/断线不重发，样本不足就返回"不可判定"；
/// - cookie 只内存传递，用完 zeroize；结果不含正文、不含任何凭据。
pub async fn fingerprint_probe(
  app: &AppHandle,
  ledger: &crate::send_ledger::SendLedger,
  uuid: &str,
  setting: Option<&ProxySetting>,
  secrets: &Secrets,
  cancel: &ProbeCancel,
) -> FingerprintProbe {
  #[cfg(target_os = "android")]
  {
    let _ = (app, ledger, uuid, setting, secrets, cancel);
    return FingerprintProbe::fail("android", "indeterminate", "指纹探针在 Android 上不可用");
  }

  #[cfg(not(target_os = "android"))]
  {
    use crate::send_ledger::SendError;

    // 账号级出站策略：启用代理时必须全程走代理，解析失败也不得退回直连。
    let egress = match crate::egress::AccountEgress::resolve(setting, secrets) {
      Ok(e) => e,
      Err(e) => return FingerprintProbe::fail("egress", "indeterminate", e.to_string()),
    };
    // 后端取消：前端停止/超时后不会再发起新的探针发送。
    let (mut cancel_token, _cancel_guard) = cancel.register(uuid);

    // 一次指纹评估 = 一个检测任务（mission）：独占该账号发送权，期间拒绝手动发送。
    // 任务内每条探针各自落一条 SendAttempt，有独立状态；任务级结果只做汇总。
    // MissionGuard 在任何返回路径上都会释放独占，不会把账号锁死。
    let mission = match ledger.mission(uuid) {
      Ok(m) => m,
      Err(SendError::Busy(existing)) => {
        return FingerprintProbe::fail(
          "busy",
          "indeterminate",
          format!("该账号已有发送或检测任务在进行中，请稍后再试：{existing}"),
        )
      }
      Err(SendError::Unknown(e)) => return FingerprintProbe::fail("ledger", "indeterminate", e),
    };

    // 取 cookie（独立任务 + 竞速，避免同步阻塞打穿运行时）。
    let app_owned = app.clone();
    let uuid_owned = uuid.to_string();
    let cookie_handle =
      tokio::spawn(async move { fetch_session_cookies(&app_owned, &uuid_owned).await });
    let mut cookies = tokio::select! {
      res = cookie_handle => match res {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
          return FingerprintProbe::fail("cookies", "indeterminate", e);
        }
        Err(e) => {
          return FingerprintProbe::fail("cookies", "indeterminate", format!("取 cookie 任务异常: {e}"));
        }
      },
      _ = tokio::time::sleep(COOKIE_TIMEOUT) => {
        return FingerprintProbe::fail("cookies", "indeterminate", "取 muse.ai 会话 cookie 超时（15 秒）");
      }
    };
    if cookies.is_empty() {
      cookies.zeroize();
      return FingerprintProbe::fail("no_cookies", "indeterminate", "该账号还没有 muse.ai 登录态");
    }

    // 生成 3 条真实探针题（lmfpd 生成器，计数 292–332，无语义无隐私）。
    let probes = match fingerprint::generate_probes() {
      Ok(p) => p,
      Err(e) => {
        cookies.zeroize();
        return FingerprintProbe::fail("generate", "indeterminate", e.to_string());
      }
    };

    // 加载参考库（首次联网下载，之后 SDK 缓存）。失败不伪造候选。
    let detector = match fingerprint::FingerprintDetector::load() {
      Ok(d) => d,
      Err(e) => {
        cookies.zeroize();
        return FingerprintProbe::fail("reference_bank", "indeterminate", e.to_string());
      }
    };

    let mut samples: Vec<Sample> = Vec::new();
    let mut session_id: Option<String> = None;
    let mut probes_sent = 0usize;
    let mut probes_confirmed = 0usize;
    let mut sends: Vec<FingerprintSendRecord> = Vec::new();
    let mut first_error: Option<String> = None;

    for probe in &probes {
      // 后端取消检查点：用户停止后不再建立新连接、不再发新探针。
      if cancel_token.check() {
        first_error.get_or_insert("用户取消：停止发起新探针".to_string());
        break;
      }
      // 先订：订阅连接必须在发送之前建立，避免漏掉正文增量头。
      // 连接走账号级出站策略：启用代理时经隧道，失败即记 NOT_SENT，不直连。
      let mut sub = match egress.connect(&cookies).await {
        Ok(gw) => match gw.subscribe().await {
          Ok(s) => s,
          Err(e) => {
            let msg = format!("订阅失败: {e}");
            first_error.get_or_insert(msg.clone());
            sends.push(FingerprintSendRecord {
              probe_id: probe.id.clone(),
              send_status: "NOT_SENT".to_string(),
              text_done: false,
              sample_used: false,
              error: Some(msg),
            });
            continue;
          }
        },
        Err(e) => {
          let msg = format!("订阅连接失败: {e}");
          first_error.get_or_insert(msg.clone());
          sends.push(FingerprintSendRecord {
            probe_id: probe.id.clone(),
            send_status: "NOT_SENT".to_string(),
            text_done: false,
            sample_used: false,
            error: Some(msg),
          });
          continue;
        }
      };

      // 后发：订阅已就绪，再取发送权并落一条独立 SendAttempt。
      let send_id = match mission.begin_probe("fingerprint-isolated", &probe.prompt) {
        Ok(id) => id,
        Err(e) => {
          let msg = match e {
            SendError::Busy(x) => format!("探针 {} 单发锁被占用: {x}", probe.id),
            SendError::Unknown(x) => format!("探针 {} 台账异常: {x}", probe.id),
          };
          first_error.get_or_insert(msg.clone());
          sends.push(FingerprintSendRecord {
            probe_id: probe.id.clone(),
            send_status: "NOT_SENT".to_string(),
            text_done: false,
            sample_used: false,
            error: Some(msg),
          });
          continue;
        }
      };
      ledger.mark_in_flight(&send_id);

      // 同一隔离会话（首条建会话，后续复用其 session_id）。
      let send_gw = match egress.connect(&cookies).await {
        Ok(g) => g,
        Err(e) => {
          let msg = format!("发送连接失败: {e}");
          first_error.get_or_insert(msg.clone());
          ledger.fail(&send_id, msg.clone());
          sends.push(FingerprintSendRecord {
            probe_id: probe.id.clone(),
            send_status: "DEFINITIVELY_FAILED".to_string(),
            text_done: false,
            sample_used: false,
            error: Some(msg),
          });
          continue;
        }
      };
      // 发送前可被后端取消：取消后不再把这条探针发出去。
      let send_outcome = tokio::select! {
        r = send_gw.send_message(&probe.prompt, "", session_id.as_deref(), Duration::from_secs(30)) => Some(r),
        _ = cancel_token.cancelled() => None,
      };
      match send_outcome {
        None => {
          let msg = format!("探针 {} 在发送阶段被取消，不重发", probe.id);
          first_error.get_or_insert(msg.clone());
          ledger.mark_unknown(&send_id, msg.clone());
          sends.push(FingerprintSendRecord {
            probe_id: probe.id.clone(),
            send_status: "UNKNOWN".to_string(),
            text_done: false,
            sample_used: false,
            error: Some(msg),
          });
          break;
        }
        Some(Ok((ack, sid))) => {
          probes_sent += 1;
          if session_id.is_none() {
            session_id = sid.clone();
          }
          let msg_id = ack.get("message_id").and_then(|v| v.as_str()).map(str::to_string);
          let collected = tokio::select! {
            c = sub.collect(
              msg_id.as_deref(),
              Duration::from_secs(90),
              Duration::from_secs(10),
            ) => Some(c),
            _ = cancel_token.cancelled() => None,
          };
          let Some(collected) = collected else {
            let msg = format!("探针 {} 在采集阶段被取消，不重发", probe.id);
            first_error.get_or_insert(msg.clone());
            ledger.mark_unknown(&send_id, msg.clone());
            sends.push(FingerprintSendRecord {
              probe_id: probe.id.clone(),
              send_status: "UNKNOWN".to_string(),
              text_done: false,
              sample_used: false,
              error: Some(msg),
            });
            break;
          };
          // 有效样本必须「真的收完」：以 message_done 为完成信号，不能只看非空。
          if collected.done {
            probes_confirmed += 1;
            ledger.confirm(&send_id);
            let usable = !collected.text.trim().is_empty();
            if usable {
              samples.push(Sample {
                text: collected.text,
                expected_count: probe.expected_count,
              });
            } else {
              first_error.get_or_insert(format!("探针 {} 已回复但正文为空", probe.id));
            }
            sends.push(FingerprintSendRecord {
              probe_id: probe.id.clone(),
              send_status: "CONFIRMED".to_string(),
              text_done: true,
              sample_used: usable,
              error: if usable {
                None
              } else {
                Some("已回复但正文为空".to_string())
              },
            });
          } else {
            // 发出去了但没收到 message_done（超时 / 断流）：结果不明，绝不重发。
            let msg = format!("探针 {} 采集未完成（{}），不重发", probe.id, collected.ended_by);
            first_error.get_or_insert(msg.clone());
            ledger.mark_unknown(&send_id, msg.clone());
            sends.push(FingerprintSendRecord {
              probe_id: probe.id.clone(),
              send_status: "UNKNOWN".to_string(),
              text_done: false,
              sample_used: false,
              error: Some(msg),
            });
          }
        }
        Some(Err(e)) => {
          // 明确 ack 没拿到：不重发，记一条失败，继续下一条探针。
          let msg = format!("发送探针 {} 失败: {e}", probe.id);
          first_error.get_or_insert(msg.clone());
          ledger.fail(&send_id, msg.clone());
          sends.push(FingerprintSendRecord {
            probe_id: probe.id.clone(),
            send_status: "DEFINITIVELY_FAILED".to_string(),
            text_done: false,
            sample_used: false,
            error: Some(msg),
          });
        }
      }
    }

    // cookie 不再需要，立即清零。
    cookies.zeroize();

    let samples_collected = samples.len();
    let result = if samples.is_empty() {
      fingerprint::FingerprintResult::indeterminate(
        first_error
          .unwrap_or_else(|| "三条探针均未采到有效正文".to_string()),
        detector.built_at().to_string(),
      )
    } else {
      match detector.score(&samples) {
        Ok(r) => r,
        Err(FingerprintError::Scoring(e)) => fingerprint::FingerprintResult::indeterminate(
          format!("评分失败: {e}"),
          detector.built_at().to_string(),
        ),
        Err(e) => fingerprint::FingerprintResult::indeterminate(
          e.to_string(),
          detector.built_at().to_string(),
        ),
      }
    };

    let ok = result.identification || result.verdict == fingerprint::Verdict::Partial;
    FingerprintProbe {
      ok,
      phase: "done".to_string(),
      verdict: result.verdict.as_str().to_string(),
      identification: result.identification,
      result: Some(result),
      probes_sent,
      probes_confirmed,
      samples_collected,
      sends,
      session_id,
      error: None,
    }
  }
}

/// #10 存活探针状态：记录一条长连接从建立到断开的时间线。
pub use muse_native_client::liveness::Liveness;

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

fn observation_now_ms() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
    .unwrap_or(0)
}

/// 开一条长连接并持续读事件，直到超时或断开；结果写进共享状态。
/// 用途是实测「App 退到后台后连接还能活多久」，不做保活、不重连。
pub fn liveness_start(
  app: &AppHandle,
  state: &std::sync::Arc<LivenessState>,
  uuid: &str,
  seconds: u64,
  setting: Option<ProxySetting>,
) {
  let seconds = seconds.clamp(30, 3600);
  // Check-and-set under one lock: concurrent starts must not create two probes.
  {
    let mut current = state.inner.lock().unwrap();
    if current.running {
      return;
    }
    *current = Liveness::started(observation_now_ms());
  }

  let app_owned = app.clone();
  let uuid_owned = uuid.to_string();
  let state_owned = state.clone();
  // 用 Tauri 的运行时 spawn：即使命令在非 tokio 上下文被调用也不会 panic。
  tauri::async_runtime::spawn(async move {
    #[cfg(target_os = "android")]
    {
      let _ = (app_owned, uuid_owned, seconds, setting);
      state_owned.set(|l| {
        l.running = false;
        l.ended_ms = Some(observation_now_ms());
        l.error = Some("Android 不可用".to_string());
      });
      return;
    }
    #[cfg(not(target_os = "android"))]
    {
      let cookies = match tokio::time::timeout(
        COOKIE_TIMEOUT,
        fetch_session_cookies(&app_owned, &uuid_owned),
      )
      .await
      {
        Err(_) => {
          state_owned.set(|l| {
            l.running = false;
            l.phase = "cookies-timeout".to_string();
            l.ended_ms = Some(observation_now_ms());
            l.error = Some("取 cookie 超时（15 秒）".to_string());
          });
          return;
        }
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
          state_owned.set(|l| {
            l.running = false;
            l.phase = "cookies-error".to_string();
            l.ended_ms = Some(observation_now_ms());
            l.error = Some(e);
          });
          return;
        }
      };
      state_owned.set(|l| l.phase = "connect".to_string());
      if cookies.is_empty() {
        state_owned.set(|l| {
          l.running = false;
          l.phase = "no-cookies".to_string();
          l.ended_ms = Some(observation_now_ms());
          l.error = Some("无登录态".to_string());
        });
        return;
      }
      let egress = match crate::egress::resolve_for_app(&app_owned, setting.as_ref()) {
        Ok(e) => e,
        Err(e) => {
          state_owned.set(|l| {
            l.running = false;
            l.phase = "egress".to_string();
            l.ended_ms = Some(observation_now_ms());
            l.error = Some(e);
          });
          return;
        }
      };
      let mut cookies = cookies;
      let connected = match tokio::time::timeout(Duration::from_secs(60), egress.connect(&cookies)).await {
        Err(_) => {
          cookies.zeroize();
          state_owned.set(|l| {
            l.running = false;
            l.phase = "connect-timeout".to_string();
            l.ended_ms = Some(observation_now_ms());
            l.error = Some("建连超时（60 秒）".to_string());
          });
          return;
        }
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
          cookies.zeroize();
          state_owned.set(|l| {
            l.running = false;
            l.phase = "connect-error".to_string();
            l.ended_ms = Some(observation_now_ms());
            l.error = Some(e.to_string());
          });
          return;
        }
      };
      cookies.zeroize();
      state_owned.set(|l| l.connected(observation_now_ms()));

      let mut sub = match tokio::time::timeout(Duration::from_secs(15), connected.subscribe()).await {
        Err(_) => {
          state_owned.set(|l| l.finish(observation_now_ms(), "subscribe_timeout", Some("订阅发送超时（15 秒）".into())));
          return;
        }
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
          state_owned.set(|l| {
            l.running = false;
            l.ended_ms = Some(observation_now_ms());
            l.error = Some(e.to_string());
          });
          return;
        }
      };
      state_owned.set(|l| l.reading(observation_now_ms(), seconds));
      let mut end_reason = "window_elapsed";
      let mut error = None;
      loop {
        let now = observation_now_ms();
        state_owned.set(|l| l.observe(now));
        let snapshot = state_owned.snapshot();
        if snapshot.expired(now) {
          break;
        }
        // Poll in short slices to record observation gaps. These are local task
        // polls, not network heartbeats and not evidence of a live remote peer.
        let left_ms = snapshot.deadline_ms.unwrap_or(now).saturating_sub(now);
        let slice = Duration::from_millis(left_ms.min(5000));
        let result = sub.next_line(slice).await;
        let observed = observation_now_ms();
        state_owned.set(|l| l.observe(observed));
        match result {
          Ok(Some(line)) => {
            // A buffered row read on resume was received at an unknown time.
            if state_owned.snapshot().expired(observed) {
              break;
            }
            let is_event = serde_json::from_str::<serde_json::Value>(&line)
              .ok()
              .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(|t| t == "event"))
              .unwrap_or(false);
            state_owned.set(|l| l.record_line(observed, is_event));
          }
          Ok(None) => {
            end_reason = "stream_ended";
            break;
          }
          Err(GatewayError::Timeout(_)) => continue,
          Err(e) => {
            end_reason = "read_error";
            error = Some(e.to_string());
            break;
          }
        }
      }
      state_owned.set(|l| l.finish(observation_now_ms(), end_reason, error));
    }
  });
}

/// #8 VM 租约观测结果（不含任何凭据）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseProbe {
  pub ok: bool,
  pub phase: String,
  /// 网关主机（只保留域名，便于对照不同 VM）。
  pub gateway_host: String,
  pub vm_name: String,
  pub first_expires_at_ms: Option<i64>,
  pub ttl_seconds: Option<i64>,
  /// 上游是否真的返回了到期字段；缺失时一切都只能算「不可观测」。
  pub expiry_field_present: bool,
  /// `not_attempted` / `unobservable` / `unchanged` / `changed`。
  /// 注意：`changed` 只代表**到期字段变化**，不等于自动续租。
  pub renewal: String,
  /// 是否已确认存在自动续租行为。只有长期观测到多次「到期时间随租期前移」
  /// 才能置 true；单次探针永远无法确认，故当前恒为 false。
  pub renewal_confirmed: bool,
  pub second_expires_at_ms: Option<i64>,
  /// 本次探针实际发出的控制面调用（vmType + 结果），用于评估副作用。
  pub calls: Vec<String>,
  /// 副作用说明：探针不调用 wake，但 lease-vm 本身是否会导致 VM 分配/唤醒属于未验证。
  pub side_effects_note: String,
  pub error: Option<String>,
}

/// 两次租约的到期字段对比结论。刻意不让「字段变化」等同于「自动续租」。
pub fn renewal_verdict(first: Option<i64>, second: Option<i64>) -> &'static str {
  match (first, second) {
    // 两次都拿不到到期字段：无法判断，不能报「无变化」。
    (None, None) => "unobservable",
    // 一次有、一次没有：连字段稳定性都不成立，同样算不可观测。
    (Some(_), None) | (None, Some(_)) => "unobservable",
    (Some(a), Some(b)) => {
      if a == b {
        "unchanged"
      } else {
        // 仅「到期字段变化」；是否属于自动续租需长期观测，一次探针不能断言。
        "changed"
      }
    }
  }
}

/// #8：只读控制面执行 `lease-vm`，报告租期字段与「到期字段是否变化」。
/// 明确不做的事：不调用 `wake()`、不建 Noise 连接、不发消息、不打印 token。
/// 未验证的是：`lease-vm` 在服务端是否会造成 VM 分配或唤醒（见 side_effects_note）。
pub async fn lease_probe(
  app: &AppHandle,
  uuid: &str,
  twice: bool,
  setting: Option<&ProxySetting>,
) -> Result<LeaseProbe, String> {
  let cookie = cookie_header_for(app, uuid).await?;
  let cookies = muse_native_client::cookie::SessionCookies::from_header(&cookie);
  // 控制面 HTTP 走账号级出站策略（启用代理时经代理，失败不直连）。
  let egress = crate::egress::resolve_for_app(app, setting)?;
  let fetch = egress.http().map_err(|e| e.to_string())?;
  let mut cp = muse_native_client::control_plane::MuseControlPlane::new(fetch, cookies);
  let note = concat!(
    "探针不调用 wake()；每次 lease_vm 依次尝试 standard 与 hatch_vm",
    "（最多 2 次 POST /api/hatch/lease-vm）。该接口在服务端是否触发 VM",
    "分配/唤醒未经证实，因此不要把本探针当作「无副作用」。"
  )
  .to_string();

  let lease = match cp.lease_vm().await {
    Ok(l) => l,
    Err(e) => {
      return Ok(LeaseProbe {
        ok: false,
        phase: "lease".to_string(),
        gateway_host: String::new(),
        vm_name: String::new(),
        first_expires_at_ms: None,
        ttl_seconds: None,
        expiry_field_present: false,
        renewal: "not_attempted".to_string(),
        renewal_confirmed: false,
        second_expires_at_ms: None,
        calls: cp.lease_calls.clone(),
        side_effects_note: note,
        error: Some(e.to_string()),
      })
    }
  };

  let host = lease
    .gateway_url
    .split("://")
    .nth(1)
    .unwrap_or(&lease.gateway_url)
    .split('/')
    .next()
    .unwrap_or("")
    .to_string();

  let first = lease.expires_at_ms;
  let ttl = lease.ttl_seconds;
  let vm_name = lease.vm_name.clone();

  let (renewal, second) = if !twice {
    ("not_attempted".to_string(), None)
  } else {
    tokio::time::sleep(Duration::from_secs(2)).await;
    match cp.lease_vm().await {
      Ok(l2) => {
        let second = l2.expires_at_ms;
        // 关键：两者都缺失 → 不可观测；相等 → 字段未变化；
        // 不等 → 仅说明字段变了，不能据此断言自动续租。
        (renewal_verdict(first, second).to_string(), second)
      }
      Err(_) => ("unobservable".to_string(), None),
    }
  };

  Ok(LeaseProbe {
    ok: true,
    phase: "leased".to_string(),
    gateway_host: host,
    vm_name,
    first_expires_at_ms: first,
    ttl_seconds: ttl,
    expiry_field_present: first.is_some() || ttl.is_some(),
    renewal,
    renewal_confirmed: false,
    second_expires_at_ms: second,
    calls: cp.lease_calls.clone(),
    side_effects_note: note,
    error: None,
  })
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
  setting: Option<&ProxySetting>,
) -> Result<WatchProbe, String> {
  if provider_id != ProviderId::Muse.as_str() {
    return Err("订阅探针只支持 Muse 账号".to_string());
  }
  #[cfg(target_os = "android")]
  {
    let _ = (app, uuid, seconds, setting);
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
    let egress = crate::egress::resolve_for_app(app, setting)?;
    let connected = match egress.connect(&cookies).await {
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
    let mut sub = match connected.subscribe().await {
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
  setting: Option<&ProxySetting>,
) -> Result<ApprovalsProbe, String> {
  if provider_id != ProviderId::Muse.as_str() {
    return Err("审批探针只支持 Muse 账号".to_string());
  }
  #[cfg(target_os = "android")]
  {
    let _ = (app, uuid, setting);
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
    let egress = crate::egress::resolve_for_app(app, setting)?;
    let mut connected = match egress.connect(&cookies).await {
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

/// #11/#16 代理探针结果。只含阶段、状态码与脱敏代理串，不含 cookie /
/// token / 代理口令。`failClosed` 恒为 true：代理失败只阻断，不直连。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyProbe {
  pub ok: bool,
  pub phase: String,
  pub tunnel_via: Option<String>,
  pub proxy_scheme: Option<String>,
  pub handshake_completed: bool,
  pub ping_status: Option<u16>,
  pub fail_closed: bool,
  pub error: Option<String>,
}

impl ProxyProbe {
  fn fail(phase: &str, tunnel_via: Option<String>, scheme: Option<String>, error: impl Into<String>) -> Self {
    Self {
      ok: false,
      phase: phase.to_string(),
      tunnel_via,
      proxy_scheme: scheme,
      handshake_completed: false,
      ping_status: None,
      fail_closed: true,
      error: Some(error.into()),
    }
  }
}

/// #11/#16：用指定代理跑一次控制面 + WSS + Noise + ping，验证隧道可行。
/// `proxy_uri` 形如 http[s]://[user:pass@]host[:port] 或 socks5://[...]。
/// 任何失败都 fail-closed（阻断并报错），不会改用直连。
#[cfg(not(target_os = "android"))]
pub async fn proxy_probe(app: &AppHandle, uuid: &str, proxy_uri: &str) -> ProxyProbe {
  // 先解析代理 URI：非法则直接失败（不建任何连接）。
  let cfg = match muse_native_client::proxy::ProxyConfig::parse(proxy_uri) {
    Ok(c) => c,
    Err(e) => return ProxyProbe::fail("proxy_uri", None, None, format!("代理配置无效: {e}")),
  };
  let tunnel_via = cfg.display_safe();
  let scheme = match cfg.scheme() {
    muse_native_client::proxy::ProxyScheme::Http => "http",
    muse_native_client::proxy::ProxyScheme::Https => "https",
    muse_native_client::proxy::ProxyScheme::Socks5 => "socks5",
  }
  .to_string();

  let app_owned = app.clone();
  let uuid_owned = uuid.to_string();
  let cookie_handle =
    tokio::spawn(async move { fetch_session_cookies(&app_owned, &uuid_owned).await });
  let mut cookies = tokio::select! {
    res = cookie_handle => match res {
      Ok(r) => match r {
        Ok(c) => c,
        Err(e) => return ProxyProbe::fail("cookies", Some(tunnel_via), Some(scheme), e),
      },
      Err(e) => return ProxyProbe::fail("cookies", Some(tunnel_via), Some(scheme), format!("取 cookie 任务异常: {e}")),
    },
    _ = tokio::time::sleep(COOKIE_TIMEOUT) => {
      return ProxyProbe::fail("cookies", Some(tunnel_via), Some(scheme), "取会话 cookie 超时（15 秒）");
    }
  };
  if cookies.is_empty() {
    return ProxyProbe::fail(
      "cookies",
      Some(tunnel_via),
      Some(scheme),
      "该账号未登录 muse.ai：请先在 Spike App 里登录 Muse 测试账号。",
    );
  }

  // 控制面 HTTP 走同一代理（HTTP 侧），WSS 走代理隧道（WS 侧）。
  let fetch = match ReqwestFetch::new_with_proxy(Some(proxy_uri)) {
    Ok(f) => f,
    Err(e) => return ProxyProbe::fail("proxy_http", Some(tunnel_via), Some(scheme), e),
  };
  let gateway = MuseGateway::new(fetch);
  let mut connected = match gateway.connect_via_proxy(&cookies, &cfg).await {
    Ok(c) => c,
    Err(e) => {
      cookies.zeroize();
      return ProxyProbe::fail("tunnel", Some(tunnel_via), Some(scheme), e.to_string());
    }
  };
  cookies.zeroize();

  match connected.ping().await {
    Ok(status) => {
      connected.close().await;
      ProxyProbe {
        ok: status < 400,
        phase: "ready".to_string(),
        tunnel_via: Some(tunnel_via),
        proxy_scheme: Some(scheme),
        handshake_completed: true,
        ping_status: Some(status),
        fail_closed: true,
        error: if status < 400 {
          None
        } else {
          Some(format!("connection.ping HTTP {status}"))
        },
      }
    }
    Err(e) => {
      connected.close().await;
      ProxyProbe::fail("ping", Some(tunnel_via), Some(scheme), e.to_string())
    }
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn renewal_verdict_cases() {
    // 都缺失 → 不可观测（关键回归：不能当成「无变化」）
    assert_eq!(renewal_verdict(None, None), "unobservable");
    // 相等 → 字段未变化
    assert_eq!(
      renewal_verdict(Some(1_762_100_300_000), Some(1_762_100_300_000)),
      "unchanged"
    );
    // 不等 → 仅字段变化；UI 文案把「≠ 自动续租」讲清楚
    assert_eq!(
      renewal_verdict(Some(1_762_100_300_000), Some(1_762_100_400_000)),
      "changed"
    );
    // 一有一缺 → 不可观测
    assert_eq!(renewal_verdict(Some(1), None), "unobservable");
    assert_eq!(renewal_verdict(None, Some(1)), "unobservable");
    // 反向也要一致（对称性回归）
    assert_eq!(
      renewal_verdict(Some(1_762_100_400_000), Some(1_762_100_300_000)),
      "changed"
    );
  }

  #[test]
  fn renewal_never_claims_confirmation() {
    // 单次探针不可能确认自动续租：以上四种结论里没有「renewed」。
    for (a, b) in [(None, None), (Some(1), None), (Some(1), Some(1)), (Some(1), Some(2))] {
      let v = renewal_verdict(a, b);
      assert_ne!(v, "renewed", "续租必须由长期观测确认，不能由一次探针得出");
      assert_ne!(v, "extended");
    }
  }
}
