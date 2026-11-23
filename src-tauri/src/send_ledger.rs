//! B4：原生发送安全层（Issue #13 / i005 #15）。
//!
//! 目标不是「能发出去」，而是**发不重、发不清不楚的时候不装明白**：
//! - 每次发送先落一条 `SendAttempt`（PREPARED），再发请求；
//! - 同一账号同一时刻只允许一条 IN_FLIGHT（单发锁），再次触发直接拒绝、不排队；
//! - 请求发出后无论超时还是断线，一律记 `UNKNOWN`，**绝不自动重发**；
//! - 只有拿到明确 ack 才记 CONFIRMED，明确失败才记 DEFINITIVELY_FAILED。
//!
//! 台账只存发送元数据（时间、账号 uuid、目标会话、内容摘要），不存正文。

use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SendStatus {
  Prepared,
  InFlight,
  Confirmed,
  DefinitivelyFailed,
  Unknown,
}

/// 台账条目类型：手动发送，还是指纹检测任务里的探针发送。
///
/// 两者都必须有**独立、准确**的状态；检测任务本身另有一条 mission 记录汇总，
/// 不能拿「任务跑完了」去顶替「每条探针发出去了」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SendKind {
  Message,
  FingerprintProbe,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendAttempt {
  pub send_id: String,
  pub account_uuid: String,
  pub conversation_id: String,
  /// 正文摘要（前 40 字 + 长度），不含完整正文。
  pub digest: String,
  pub started_at_ms: u128,
  pub status: SendStatus,
  pub error: Option<String>,
  pub kind: SendKind,
  /// 指纹探针所属的检测任务 id；手动发送为 None。
  pub mission_id: Option<String>,
}

/// 台账的全部可变状态放在**同一把锁**里。
///
/// 之前 attempts / in_flight / missions 各一把锁，`mission()` 与 `begin()` 会
/// 各自「先查后插」，两步之间可以插进对方的检查，导致检测任务与手动发送
/// 同时通过互斥（真实竞态）。单一 Mutex 让「查 + 插」成为一次原子操作。
#[derive(Default)]
struct LedgerState {
  attempts: Vec<SendAttempt>,
  /// 每个账号当前的 IN_FLIGHT send_id；有则拒绝新发送。
  in_flight: HashMap<String, String>,
  /// 每个账号当前进行中的检测任务 mission_id；有则拒绝任何新发送。
  missions: HashMap<String, String>,
}

#[derive(Default)]
pub struct SendLedger {
  state: Mutex<LedgerState>,
}

fn now_ms() -> u128 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_millis())
    .unwrap_or(0)
}

/// 正文摘要：去空白后取前 40 个字符 + 总长度，避免正文进日志/台账。
pub fn digest_of(text: &str) -> String {
  let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
  let head: String = flat.chars().take(40).collect();
  format!("{head}…({} 字)", flat.chars().count())
}

#[derive(Debug)]
pub enum SendError {
  /// 该账号已有发送在进行中（不排队）。
  Busy(String),
  Unknown(String),
}

impl SendLedger {
  pub fn new() -> Self {
    Self::default()
  }

  /// 申请发送权并落 PREPARED。返回 send_id；已有 IN_FLIGHT 或检测任务时返回 Busy。
  pub fn begin(
    &self,
    account_uuid: &str,
    conversation_id: &str,
    text: &str,
  ) -> Result<String, SendError> {
    let mut st = self.state.lock().unwrap();
    // 单锁内「查 mission + 查 in_flight + 插入」，与 mission() 互斥。
    if let Some(mission) = st.missions.get(account_uuid) {
      return Err(SendError::Busy(mission.clone()));
    }
    Self::start_attempt_locked(&mut st, account_uuid, conversation_id, text, SendKind::Message, None)
  }

  /// 开启一次检测任务（mission）。mission 期间该账号拒绝所有手动发送；
  /// 返回的 guard 在 drop 时自动释放 mission，保证异常路径不锁死账号。
  pub fn mission(&self, account_uuid: &str) -> Result<MissionGuard<'_>, SendError> {
    let mut st = self.state.lock().unwrap();
    if let Some(existing) = st.missions.get(account_uuid) {
      return Err(SendError::Busy(existing.clone()));
    }
    if let Some(flight) = st.in_flight.get(account_uuid) {
      return Err(SendError::Busy(flight.clone()));
    }
    let mission_id = uuid::Uuid::new_v4().to_string();
    st.missions
      .insert(account_uuid.to_string(), mission_id.clone());
    drop(st);
    Ok(MissionGuard {
      ledger: self,
      account_uuid: account_uuid.to_string(),
      mission_id,
    })
  }

  /// 任务内的子发送（指纹探针）。绕过 mission 独占拒绝，但仍遵守单发锁。
  pub fn begin_probe(
    &self,
    mission_id: &str,
    account_uuid: &str,
    conversation_id: &str,
    text: &str,
  ) -> Result<String, SendError> {
    let mut st = self.state.lock().unwrap();
    let belongs = st
      .missions
      .get(account_uuid)
      .map(|m| m == mission_id)
      .unwrap_or(false);
    if !belongs {
      return Err(SendError::Unknown("检测任务已结束或不匹配".to_string()));
    }
    Self::start_attempt_locked(
      &mut st,
      account_uuid,
      conversation_id,
      text,
      SendKind::FingerprintProbe,
      Some(mission_id.to_string()),
    )
  }

  /// 调用方必须持有 state 锁：查 in_flight 与写入是同一个临界区。
  fn start_attempt_locked(
    st: &mut LedgerState,
    account_uuid: &str,
    conversation_id: &str,
    text: &str,
    kind: SendKind,
    mission_id: Option<String>,
  ) -> Result<String, SendError> {
    if let Some(existing) = st.in_flight.get(account_uuid) {
      return Err(SendError::Busy(existing.clone()));
    }
    let send_id = uuid::Uuid::new_v4().to_string();
    st.in_flight
      .insert(account_uuid.to_string(), send_id.clone());
    st.attempts.push(SendAttempt {
      send_id: send_id.clone(),
      account_uuid: account_uuid.to_string(),
      conversation_id: conversation_id.to_string(),
      digest: digest_of(text),
      started_at_ms: now_ms(),
      status: SendStatus::Prepared,
      error: None,
      kind,
      mission_id,
    });
    Ok(send_id)
  }

  fn finish_mission(&self, mission_id: &str) {
    let mut st = self.state.lock().unwrap();
    st.missions.retain(|_, v| v != mission_id);
  }

  fn update(&self, send_id: &str, status: SendStatus, error: Option<String>) {
    let mut st = self.state.lock().unwrap();
    if let Some(a) = st.attempts.iter_mut().find(|a| a.send_id == send_id) {
      a.status = status;
      a.error = error;
    }
  }

  pub fn mark_in_flight(&self, send_id: &str) {
    self.update(send_id, SendStatus::InFlight, None);
  }

  /// 收到明确 ack：释放锁并记 CONFIRMED。
  pub fn confirm(&self, send_id: &str) {
    self.update(send_id, SendStatus::Confirmed, None);
    self.release(send_id);
  }

  /// 明确失败（服务端拒绝）：释放锁并记 DEFINITIVELY_FAILED。
  pub fn fail(&self, send_id: &str, err: String) {
    self.update(send_id, SendStatus::DefinitivelyFailed, Some(err));
    self.release(send_id);
  }

  /// 结果不明（超时/断线）：记 UNKNOWN，**仍然释放锁**（否则该账号永远发不出去），
  /// 但状态保持 UNKNOWN，等待用户人工核对，绝不自动重发。
  pub fn mark_unknown(&self, send_id: &str, err: String) {
    self.update(send_id, SendStatus::Unknown, Some(err));
    self.release(send_id);
  }

  fn release(&self, send_id: &str) {
    let mut st = self.state.lock().unwrap();
    st.in_flight.retain(|_, v| v != send_id);
  }

  #[allow(dead_code)] // 供测试与后续台账 UI
  pub fn attempts(&self) -> Vec<SendAttempt> {
    self.state.lock().unwrap().attempts.clone()
  }

  #[allow(dead_code)] // 供测试/诊断
  pub fn busy_account(&self, account_uuid: &str) -> Option<String> {
    self
      .state
      .lock()
      .unwrap()
      .in_flight
      .get(account_uuid)
      .cloned()
  }

  /// 该账号当前是否有进行中的检测任务。
  #[allow(dead_code)] // 供测试/诊断
  pub fn active_mission(&self, account_uuid: &str) -> Option<String> {
    self
      .state
      .lock()
      .unwrap()
      .missions
      .get(account_uuid)
      .cloned()
  }
}

/// 检测任务的 RAII 租约：`drop` 时释放该账号的 mission 独占。
///
/// 用 guard 而不是手动 `finish`，是为了让取 cookie 失败、探针异常、提前返回
/// 等所有路径都不会把账号的发送权锁死。
pub struct MissionGuard<'a> {
  ledger: &'a SendLedger,
  account_uuid: String,
  mission_id: String,
}

impl<'a> MissionGuard<'a> {
  #[allow(dead_code)] // 调用方目前只用 begin_probe，保留 id 读取供诊断
  pub fn mission_id(&self) -> &str {
    &self.mission_id
  }

  /// 任务内发一条探针；遵守单发锁，但不被自己的 mission 独占拒绝。
  pub fn begin_probe(&self, conversation_id: &str, text: &str) -> Result<String, SendError> {
    self
      .ledger
      .begin_probe(&self.mission_id, &self.account_uuid, conversation_id, text)
  }
}

impl Drop for MissionGuard<'_> {
  fn drop(&mut self) {
    self.ledger.finish_mission(&self.mission_id);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn single_flight_lock_rejects_second_send() {
    let l = SendLedger::new();
    let a = l.begin("u1", "c1", "hello").expect("first");
    match l.begin("u1", "c1", "again") {
      Err(SendError::Busy(id)) => assert_eq!(id, a),
      _ => panic!("second send must be rejected"),
    }
    l.confirm(&a);
    assert!(l.begin("u1", "c1", "third").is_ok(), "lock released after confirm");
  }

  #[test]
  fn unknown_does_not_retry_but_releases_lock() {
    let l = SendLedger::new();
    let id = l.begin("u1", "c1", "x").unwrap();
    l.mark_unknown(&id, "timeout".to_string());
    let list = l.attempts();
    assert_eq!(list[0].status, SendStatus::Unknown);
    assert!(l.busy_account("u1").is_none());
    // 不自动重发：台账里仍然只有一条
    assert_eq!(list.len(), 1);
  }

  #[test]
  fn different_accounts_are_independent() {
    let l = SendLedger::new();
    assert!(l.begin("u1", "c1", "a").is_ok());
    assert!(l.begin("u2", "c1", "b").is_ok());
  }

  #[test]
  fn digest_hides_body() {
    let d = digest_of("  这是一段很长的正文  ".repeat(10).as_str());
    assert!(d.ends_with("字)"));
    assert!(d.chars().count() < 60);
  }

  #[test]
  fn definite_failure_releases_lock() {
    let l = SendLedger::new();
    let id = l.begin("u1", "c1", "x").unwrap();
    l.fail(&id, "HTTP 403".to_string());
    assert_eq!(l.attempts()[0].status, SendStatus::DefinitivelyFailed);
    assert!(l.busy_account("u1").is_none());
  }

  #[test]
  fn mission_blocks_manual_send_and_releases_on_drop() {
    let l = SendLedger::new();
    {
      let m = l.mission("u1").expect("mission");
      let pid = m.begin_probe("fp", "[fingerprint] p1").unwrap();
      l.confirm(&pid);
      // mission 期间，手动发送必须被拒（不能插进隔离会话）。
      assert!(matches!(l.begin("u1", "c1", "hi"), Err(SendError::Busy(_))));
      assert!(l.active_mission("u1").is_some());
    }
    // guard drop 后自动释放，即使没显式 finish。
    assert!(l.active_mission("u1").is_none());
    assert!(l.begin("u1", "c1", "hi").is_ok());
  }

  #[test]
  fn mission_second_start_is_rejected() {
    let l = SendLedger::new();
    let _m = l.mission("u1").unwrap();
    assert!(matches!(l.mission("u1"), Err(SendError::Busy(_))));
    // 其他账号不受影响。
    assert!(l.mission("u2").is_ok());
  }

  #[test]
  fn each_probe_has_its_own_accurate_status() {
    let l = SendLedger::new();
    let m = l.mission("u1").unwrap();
    let a = m.begin_probe("fp", "p1").unwrap();
    l.mark_in_flight(&a);
    l.confirm(&a);
    let b = m.begin_probe("fp", "p2").unwrap();
    l.mark_in_flight(&b);
    l.fail(&b, "HTTP 500".to_string());
    let c = m.begin_probe("fp", "p3").unwrap();
    l.mark_in_flight(&c);
    l.mark_unknown(&c, "采集超时".to_string());

    let list = l.attempts();
    assert_eq!(list.len(), 3, "每条探针各占一条记录");
    assert_eq!(list[0].status, SendStatus::Confirmed);
    assert_eq!(list[1].status, SendStatus::DefinitivelyFailed);
    assert_eq!(list[2].status, SendStatus::Unknown);
    assert!(list.iter().all(|x| x.kind == SendKind::FingerprintProbe));
    assert!(list.iter().all(|x| x.mission_id.as_deref() == Some(m.mission_id())));
    // 失败与 UNKNOWN 都不能被当作成功。
    assert_eq!(
      list.iter().filter(|x| x.status == SendStatus::Confirmed).count(),
      1
    );
  }

  /// 互斥回归：mission 在**另一个线程**持有期间，手动发送必须被拒。
  ///
  /// 旧实现里 missions / in_flight 各一把锁，「先查后插」之间可以互相插入，
  /// 检测任务与手动发送会同时放行。现在两类状态在同一把锁里检查并写入，
  /// 这条约束不再依赖调用顺序。用 channel 同步而不是 sleep 竞速，避免测试
  /// 在高负载 CI 上抖动。
  #[test]
  fn mission_held_from_another_thread_blocks_manual_send() {
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    let ledger = Arc::new(SendLedger::new());
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let held = Arc::clone(&ledger);
    let handle = std::thread::spawn(move || {
      let guard = held.mission("u1").expect("mission 应成功");
      acquired_tx.send(()).unwrap();
      std::thread::sleep(Duration::from_millis(20));
      drop(guard);
    });

    acquired_rx.recv().unwrap();
    // 此刻 mission 确实被另一个线程持有：手动发送必须被拒。
    match ledger.begin("u1", "c1", "hi") {
      Err(SendError::Busy(id)) => assert!(!id.is_empty()),
      other => panic!("mission 持有期间必须拒绝手动发送，实际: {other:?}"),
    }
    handle.join().unwrap();
    // mission 释放后恢复可发送（说明 guard 的 Drop 没有把账号锁死）。
    assert!(ledger.begin("u1", "c1", "hi").is_ok());
  }

  #[test]
  fn probe_requires_live_mission() {
    let l = SendLedger::new();
    let m = l.mission("u1").unwrap();
    let mid = m.mission_id().to_string();
    drop(m);
    assert!(matches!(
      l.begin_probe(&mid, "u1", "fp", "late"),
      Err(SendError::Unknown(_))
    ));
  }
}
