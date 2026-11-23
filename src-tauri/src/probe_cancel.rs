//! 后端取消（#11/#16）。
//!
//! 前端 `Promise.race` 只能让 UI 停止等待，**不会**停止 Rust 侧的任务：
//! 超时之后探针仍可能在发请求、发消息。这里提供进程内的取消令牌，
//! 由命令 `muse_native_cancel_probe(uuid)` 触发，长任务在关键步骤上
//! `select!` 监听它，停止后不再发起新的发送。
//!
//! 语义是「尽快停止发起新请求」：已经发出的字节无法撤回，对应的发送台账
//! 仍按 UNKNOWN 记录，绝不重发。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tokio::sync::watch;

#[derive(Default)]
pub struct ProbeCancel {
  entries: Mutex<HashMap<String, (u64, watch::Sender<bool>)>>,
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
}

/// 任务结束时注销的 RAII guard。带唯一 id，避免误删后来注册的同 key 任务。
pub struct CancelGuard<'a> {
  registry: &'a ProbeCancel,
  key: String,
  id: u64,
}

impl Drop for CancelGuard<'_> {
  fn drop(&mut self) {
    let mut entries = self.registry.entries.lock().unwrap();
    if entries.get(&self.key).map(|(id, _)| *id) == Some(self.id) {
      entries.remove(&self.key);
    }
  }
}

impl ProbeCancel {
  pub fn new() -> Self {
    Self::default()
  }

  /// 为某账号注册当前任务，返回取消令牌与自动注销 guard。
  pub fn register(&self, key: &str) -> (CancelToken, CancelGuard<'_>) {
    let (tx, rx) = watch::channel(false);
    let id = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
    self
      .entries
      .lock()
      .unwrap()
      .insert(key.to_string(), (id, tx));
    (
      CancelToken { rx },
      CancelGuard {
        registry: self,
        key: key.to_string(),
        id,
      },
    )
  }

  /// 请求取消某账号的当前任务。返回是否命中一个正在运行的任务。
  pub fn cancel(&self, key: &str) -> bool {
    let entries = self.entries.lock().unwrap();
    match entries.get(key) {
      Some((_, tx)) => {
        let _ = tx.send(true);
        true
      }
      None => false,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn cancel_wakes_waiter() {
    let reg = ProbeCancel::new();
    let (mut token, _guard) = reg.register("u1");
    assert!(!token.is_cancelled());
    let handle = tokio::spawn(async move {
      token.cancelled().await;
      token.is_cancelled()
    });
    tokio::task::yield_now().await;
    assert!(reg.cancel("u1"));
    assert!(handle.await.unwrap());
  }

  #[tokio::test]
  async fn guard_drop_removes_entry() {
    let reg = ProbeCancel::new();
    {
      let (_token, _guard) = reg.register("u1");
      assert!(reg.cancel("u1"));
    }
    assert!(!reg.cancel("u1"), "guard drop 后不应命中");
  }

  #[tokio::test]
  async fn cancel_unknown_key_is_false() {
    let reg = ProbeCancel::new();
    assert!(!reg.cancel("nobody"));
  }

  #[tokio::test]
  async fn waiter_wakes_when_sender_dropped() {
    let reg = ProbeCancel::new();
    let (mut token, guard) = reg.register("u1");
    let handle = tokio::spawn(async move { token.cancelled().await });
    tokio::task::yield_now().await;
    drop(guard);
    handle.await.unwrap();
  }
}
