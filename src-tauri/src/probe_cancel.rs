//! 后端取消（#11/#16/#18）。
//!
//! 前端 `Promise.race` 只能让 UI 停止等待，**不会**停止 Rust 侧的任务：
//! 超时之后探针仍可能在发请求、发消息。这里提供进程内的取消令牌，
//! 由命令 `muse_native_cancel_probe(uuid)` 触发，长任务在关键步骤上
//! `select!` 监听它，停止后不再发起新的发送。
//!
//! 归属与并发：
//! - 每个任务注册时带 `kind`（message / fingerprint / proxy / watch…），
//!   **同一账号可以同时存在多个不同任务**（例如边跑指纹边开代理探针），
//!   早期版本按账号单槽存放，后来的任务会顶掉前面的登记，导致「按了停止，
//!   旧任务还在跑」；
//! - 取消按账号广播到该账号的全部任务，返回真正命中的数量；
//! - guard 带唯一 id，只注销自己那一条，不会误删后来者。
//!
//! 语义是「尽快停止发起新请求」：已经发出的字节无法撤回，对应的发送台账
//! 仍按 UNKNOWN 记录，绝不重发。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tokio::sync::watch;

struct TaskEntry {
  id: u64,
  kind: &'static str,
  tx: watch::Sender<bool>,
}

#[derive(Default)]
pub struct ProbeCancel {
  entries: Mutex<HashMap<String, Vec<TaskEntry>>>,
  seq: AtomicU64,
}

pub struct CancelToken {
  rx: watch::Receiver<bool>,
}

impl CancelToken {
  /// 等待取消信号。发送端被丢弃（任务自然结束）也视为取消，避免永久挂起。
  pub async fn cancelled(&mut self) {
    if *self.rx.borrow() {
      return;
    }
    let _ = self.rx.changed().await;
  }

  #[allow(dead_code)] // 只给测试与诊断用
  pub fn is_cancelled(&self) -> bool {
    *self.rx.borrow()
  }

  /// 非阻塞地消费一次取消请求（用于 `select!` 之外的检查点）。
  pub fn check(&mut self) -> bool {
    if *self.rx.borrow() {
      return true;
    }
    if self.rx.has_changed().unwrap_or(false) {
      return *self.rx.borrow_and_update();
    }
    false
  }

  /// 底层 watch 接收端。传进 `send_message_cancellable` / `read_ack` 后，
  /// 取消可以在**写入每一帧之间**和**等 ack**时生效，而不是只在两个阶段
  /// 之间（外层 `select!` 只能砍掉整段 future，拿不到写到第几帧）。
  pub fn receiver(&mut self) -> &mut watch::Receiver<bool> {
    &mut self.rx
  }
}

/// 任务结束时注销的 RAII guard。带唯一 id，避免误删同账号下的其它任务。
pub struct CancelGuard<'a> {
  registry: &'a ProbeCancel,
  account: String,
  id: u64,
}

impl Drop for CancelGuard<'_> {
  fn drop(&mut self) {
    let mut entries = self.registry.entries.lock().unwrap();
    if let Some(list) = entries.get_mut(&self.account) {
      list.retain(|e| e.id != self.id);
      if list.is_empty() {
        entries.remove(&self.account);
      }
    }
  }
}

impl ProbeCancel {
  pub fn new() -> Self {
    Self::default()
  }

  /// 为某账号的某个任务注册取消令牌，返回令牌与自动注销 guard。
  /// 同账号可注册多个不同 `kind` 的任务。
  pub fn register(&self, account: &str, kind: &'static str) -> (CancelToken, CancelGuard<'_>) {
    let (tx, rx) = watch::channel(false);
    let id = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
    let mut entries = self.entries.lock().unwrap();
    let list = entries.entry(account.to_string()).or_default();
    // 同 kind 的旧任务（理论上不该重叠）先置为取消，避免两个同名任务并存。
    for old in list.iter_mut().filter(|e| e.kind == kind) {
      let _ = old.tx.send(true);
    }
    list.push(TaskEntry { id, kind, tx });
    drop(entries);
    (
      CancelToken { rx },
      CancelGuard {
        registry: self,
        account: account.to_string(),
        id,
      },
    )
  }

  /// 取消该账号**所有**正在运行的任务，返回命中的任务数。
  pub fn cancel_account(&self, account: &str) -> usize {
    let entries = self.entries.lock().unwrap();
    match entries.get(account) {
      Some(list) => {
        for e in list {
          let _ = e.tx.send(true);
        }
        list.len()
      }
      None => 0,
    }
  }

  /// 该账号当前在跑的任务种类（诊断/日志用，不含任何内容）。
  #[allow(dead_code)] // 诊断/日志用，暂未接 UI
  pub fn active(&self, account: &str) -> Vec<&'static str> {
    let entries = self.entries.lock().unwrap();
    entries
      .get(account)
      .map(|l| l.iter().map(|e| e.kind).collect())
      .unwrap_or_default()
  }

  /// 是否有任何任务在跑（用于前端提示）。
  #[allow(dead_code)] // 诊断用
  pub fn any_active(&self) -> bool {
    !self.entries.lock().unwrap().is_empty()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn cancel_wakes_waiter() {
    let reg = ProbeCancel::new();
    let (mut token, _guard) = reg.register("u1", "message");
    assert!(!token.is_cancelled());
    let handle = tokio::spawn(async move {
      token.cancelled().await;
      token.is_cancelled()
    });
    tokio::task::yield_now().await;
    assert_eq!(reg.cancel_account("u1"), 1);
    assert!(handle.await.unwrap());
  }

  #[tokio::test]
  async fn guard_drop_removes_entry() {
    let reg = ProbeCancel::new();
    {
      let (_token, _guard) = reg.register("u1", "message");
      assert_eq!(reg.cancel_account("u1"), 1);
    }
    assert_eq!(reg.cancel_account("u1"), 0, "guard drop 后不应命中");
    assert!(!reg.any_active());
  }

  #[tokio::test]
  async fn cancel_unknown_key_is_zero() {
    let reg = ProbeCancel::new();
    assert_eq!(reg.cancel_account("nobody"), 0);
  }

  #[tokio::test]
  async fn waiter_wakes_when_sender_dropped() {
    let reg = ProbeCancel::new();
    let (mut token, guard) = reg.register("u1", "message");
    let handle = tokio::spawn(async move { token.cancelled().await });
    tokio::task::yield_now().await;
    drop(guard);
    handle.await.unwrap();
  }

  /// 同账号的两个不同任务必须都能被取消——这是早期单槽实现丢的语义。
  #[tokio::test]
  async fn concurrent_tasks_on_same_account_are_all_cancelled() {
    let reg = ProbeCancel::new();
    let (mut fp, _g1) = reg.register("u1", "fingerprint");
    let (mut px, _g2) = reg.register("u1", "proxy");
    let (mut other, _g3) = reg.register("u2", "message");
    assert_eq!(reg.cancel_account("u1"), 2);
    assert_eq!(reg.cancel_account("u2"), 1);
    tokio::task::yield_now().await;
    assert!(fp.check());
    assert!(px.check());
    assert!(other.check());
    let mut kinds = reg.active("u1");
    kinds.sort();
    assert_eq!(kinds, vec!["fingerprint", "proxy"]);
  }

  /// 同 kind 重复注册：旧任务立即被取消，不并存两个同名任务。
  #[tokio::test]
  async fn duplicate_kind_supersedes_previous() {
    let reg = ProbeCancel::new();
    let (mut first, _g1) = reg.register("u1", "message");
    let (_second, _g2) = reg.register("u1", "message");
    tokio::task::yield_now().await;
    assert!(first.check(), "同 kind 的旧任务必须收到取消");
    assert_eq!(reg.cancel_account("u1"), 2);
  }
}
