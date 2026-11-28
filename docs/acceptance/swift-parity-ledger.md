# 叠号 Swift 重构验收台账

Swift 各阶段在这里与 0.1.26 日常版逐项对照。基线是 main `0bd4341537516826ee957cf50bf69710e03572bc`。0.1.26 日常 IPA 的 SHA-256 为 `40fe5b53aefc7322955da067b552a523bcf71225efdddc7e33e2345bcb69e11e`。B1 订阅、B2 审批、B4 发送、指纹与三协议代理是交接中已真机验证的回归锚点。[基线记录](../development/current-progress.md) · [T0 决策](https://github.com/harrylarry/multi-agent-account-manager/issues/17)

每项通过后记录源码 SHA、构建、设备与系统版本、测试步骤和观察结果。有意采用降级路径时同时记录触发条件。模拟器结果提供 E2 证据。真机未验的项目保持 `UNVERIFIED`。本表全过前保留旧 Tauri App。

| 阶段 | 模块 | 真机关闭条件 | 当前证据 |
|---|---|---|---|
| T1 | Swift 骨架、注册表 | iPhone 启动原生 @main，经注册表建取删测试 DataStore。A 组双向隔离，重启后各自保留 Cookie 和 localStorage。代理注册配置归属与 WebView 重建在自检中验证。 | [T1 Issue](https://github.com/harrylarry/multi-agent-account-manager/issues/18) · [验收步骤](swift-t1-iphone.md) · 真机待验 |
| T2 | 隔离、代理、Cookie | 隔离 A–C。双代理真实出口不串；同邮箱双槽登录态隔离。HTTP/HTTPS/SOCKS5 端到端、失败阻断、移除只影响目标。Muse 与 Meta 双域导出 20/20、导入验证、覆盖和恢复按 UUID 管理。记录 CHIPS/DBSC，WebAuthn 等有开关的真机包再对照。 | [Issue #20](https://github.com/harrylarry/multi-agent-account-manager/issues/20) · [真机说明](swift-t2-iphone.md) |
| T3 | 总览、账号列表、Compose | 正确聚合未读、待审批、进行中并显示空态。前台轮询 10/20/30 秒可换，默认 20 秒。Compose 可手动选择账号、输入 1–16,000 字并调用发送接口。 | [Issue #22](https://github.com/harrylarry/multi-agent-account-manager/issues/22) · [模拟器 3/3](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/46147399099) · 真机统一验收 |
| T4 | 额度 | 进 App N 个账号额度 N/N，带更新时间、来源和 stale。L1 失败仅经相应条件转 L2 一次，进入账号触发 L3。30 分钟内重进不重复刷新，0% 与未观测分开。 | T0 S1、S2 已明确原生额度路径及取消被动层 |
| T5 | 订阅、未读、红点（B1） | 两项出入账号的未读场景、断线重连与回填无丢重、看到最新消息后清除，诊断能区分订阅、轮询与回填。 | [Issue #26](https://github.com/harrylarry/multi-agent-account-manager/issues/26) · 真机统一验收 |
| T6 | 审批（B2） | 总览和红点准确。事件和轮询备援、四类硬拒绝、过期与重启未决队列通过。按 T0 裁决用工具与 entity/domain 匹配长期授权，分类自动放行默认关闭。 | T0 S4 已跳过摘要验收 |
| T7 | 发送（B4） | HTTP/HTTPS/SOCKS5 账号代理下发送新对话。四态明确，UNKNOWN 走 T0 S3 的 26 秒对账，重启后可续核对，发送不重复。 | T0 S3 已验证降级路径 |
| T8 | 加密备份与恢复 | 三模式真机往返，坏包零改动，覆盖/合并可预测并能回滚，独立恢复码有效，快照保留可调。Cookie 用逐条备份。 | T0 S5 已确定逐条方案 |
| T9 | 模型指纹 | 手动运行，结果和置信度正确显示，按账号留历史，进 App 或切换账号不触发。 | 待实现与验收 |
| T10 | Noise | Noise XX AESGCM 向量、Rust 互通和真机发送/订阅回归均通过。未过则继续用 Rust 桥接并记录未完成。 | 待实现与验收 |

最终切换安装标识前，另用同容器升级验证 0.1.26 的 UUID profile 和更早可识别的旧默认 profile 都按引用续用。独立验收包用于隔离和构建验证，其容器无法读取日常版资料。
