// muse-native — Muse.ai 原生协议客户端（Spike 0.1.22，Issue #12，intent i004）。
//
// 移植来源：BotArk qybaihe/botark v0.12.0（MIT），
//   src/main/muse-native/{noise,transport,controlPlane,gateway,methods}.ts
//   + protobuf.ts，协议文档 docs/museai/{PROTOCOL_FULL,NATIVE_CLIENT}.md。
// 文件头保留来源声明；本实现为自写 Rust 移植，未用 BotArk 代码之外的第三方
// Noise 库（Noise_XX 按 BotArk noise.ts 手工移植，见 noise.rs）。
//
// 安全规则（与 BotArk 一致，Issue #12 硬规则）：
// - cookie / token 只在内存里传递，永不落盘、永不进日志；
// - 密钥持有者（cookie.rs / control_plane.rs 的 GatewayCreds）用后 zeroize；
// - 日志只记阶段与状态码，不记任何密钥、cookie、token 值。
//
// 模块：
// - protobuf: 最小 proto3 wire 编解码（BotArk protobuf.ts）。
// - noise: Noise_XX_25519_AESGCM_SHA256 发起方（BotArk noise.ts）。
// - transport: HTTP-over-Noise 帧编解码与分片重组（BotArk transport.ts）。
// - control_plane: muse.ai HTTPS 控制面 wake / lease / token / notary
//   （BotArk controlPlane.ts），HTTP 客户端由调用方注入（trait）。
// - methods: 方法表（Spike 只需 connection.ping；完整表随 i005）。
// - gateway: WSS + Noise 握手 + 单次 unary RPC（BotArk gateway.ts 的 Spike 子集，
//   无心跳、无自动重连——那是 Phase 1 的事）。
// - cookie: 会话 cookie 的内存持有者（Swift WKWebsiteDataStore 桥接）。
// - approvals: egress.approvals 只读解析（B2，Issue #13 #6）。
// - watch: chat.subscribe 事件归一化与去重（B1，Issue #13 #5）。

pub mod approvals;
#[cfg(not(target_os = "android"))]
pub mod chat_send;
#[cfg(not(target_os = "android"))]
pub mod collect;
pub mod control_plane;
// #18 模型指纹评分编排层（lmfpd 本地评分；不含联网发送）。
pub mod fingerprint;
pub mod cookie;
#[cfg(not(target_os = "android"))]
pub mod gateway;
pub mod liveness;
pub mod methods;
pub mod noise;
pub mod protobuf;
pub mod transport;
pub mod watch;
