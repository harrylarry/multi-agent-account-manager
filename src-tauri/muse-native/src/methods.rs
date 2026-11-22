// 方法表（BotArk v0.12.0 src/main/muse-native/methods.ts 的 Spike 子集）。
//
// 完整方法表（387 项）是 Task B（i005，Issue #13）的范围；Spike #4 只需
// 一个探针方法验证握手后的 RPC 通路：connection.ping（POST /api/ping，
// service=daemon，noiseOnly）。其余方法不要在这里手写占位——随 i005 移植。

use crate::transport::Service;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodSpec {
  pub http_method: &'static str,
  pub path: &'static str,
  pub service: Service,
}

/// 握手探针：POST /api/ping（BotArk gateway.ts 心跳用的就是它）。
pub fn connection_ping() -> MethodSpec {
  MethodSpec {
    http_method: "POST",
    path: "/api/ping",
    service: Service::Daemon,
  }
}
