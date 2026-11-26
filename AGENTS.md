# AGENTS.md — 叠号项目开发约定

> 本文件约束**开发这个仓库的 AI Agent**，不改变 App 内协作房间的指挥者、执行者、审核者等产品角色。
> 产品需求与设计决策基线：[《叠号 iOS 借鉴 BotArk 优化设计规划 v4》](<docs/叠号 iOS 借鉴 BotArk 优化设计规划 v4.md>)。
> 开发执行与验收基线：[《叠号开发计划·严谨执行版 v2.1》](docs/叠号开发计划-严谨执行版-v2.1-2026-11-23.md)。
> 当前开发批次与上游适配：[执行计划 v2.2](docs/development/development-plan-v2.2.md)。
> 实际开发进度与验收状态以最新 GitHub Issue、PR、提交、Actions 和真机验收记录为准。两份基线保留原始日期与内容。历史进度需要复核，已确认的功能需求继续适用。实质性需求冲突提交用户裁决，其他切片继续推进。
> 已确认的差异裁决见[开发决策记录](docs/开发决策记录.md)。
> 接续工作先看[当前进度](docs/development/current-progress.md)。iPhone 验包和结果回填参照[真机验收指南](docs/acceptance/iphone-acceptance.md)。

## 一、优先一个 Agent 连续开发

- 尽量由同一个 Agent 连续推进整个项目或阶段，包括核查需求、设计、编码、测试、自检、更新 PR 和收尾。**不按 Agent 名字预设规划、开发、审核、集成等固定分工**。
- 当前接手的 Agent 对当前任务完整负责。即使 Issue、PR 或代码由另一个 Agent 创建，也应在授权范围内继续，不得以“不是我的任务”“我只负责审核”为由推诿。
- 同一时刻只推进一个主开发任务；已有 Issue/PR 必须优先沿用，不得因更换 Agent 新开重复任务或平行实现。
- 改变用户已决定的产品功能、降低安全限制或越过审批，必须先取得用户明确同意。

## 二、GitHub 是可接续的项目记录

- **版本化需求与 ADR**：长期产品约束、重要技术取舍与决策变更。
- **GitHub Issue**：单个任务的范围、验收项、依赖、状态、阻断、下一动作。
- **PR、commit、SHA**：实际代码、修改原因和差异；**GitHub Actions**：与 SHA 关联的测试结果；**Release/tag**：真正发布的交付。
- [当前进度](docs/development/current-progress.md)保存阶段接续快照和证据链接。候选包、主要阻断或下一步变化时更新快照，逐项结果记录在关联 Issue 或 PR。
- `.muse-graph/`、邮件、聊天可以作为辅助线索，但不得成为唯一来源，也不得与 Issue/PR 的事实冲突。图谱更新不再是每个任务的必需交接环节；需要时按其协议同步，且必须引用 Issue/PR/SHA。
- 每次接续先读 Issue、关联 PR、最近提交与 Checks，再核对仓库代码；有分支/PR 就在原上下文继续。
- 只有发生 Agent 切换或长时间阻塞时才写简短接续评论：当前 Issue/PR、main 与 HEAD SHA、已完成和未完成项、测试/真机结果、阻断、下一动作。能安全推送的 WIP 留在原分支，禁止将未验收的 WIP 直接合入主线。
- 如果前任的本地修改未提交/未推送，必须说明无法从 GitHub 恢复的部分，从最后可见 SHA 继续。不得声称已接收到不可见的上下文。

## 三、状态与证据

- 任务状态：`NOT_STARTED` / `IN_PROGRESS` / `CODE_READY` / `CI_PASSED` / `DEVICE_VERIFIED` / `MERGED` / `BLOCKED` / `FALLBACK_ACCEPTED`。
- 证据等级：E0 推测、E1 文档/静态源码、E2 自动化测试与 CI、E3 真机可复现步骤与日志、E4 发布包真实使用。
- 代码写完不等于 CI 通过、真机验收、合入或发布；每个事实都用对应 SHA、Actions Run、构建和设备证据支持。未验证记 `UNVERIFIED`，遇阻记 `BLOCKED`。

## 四、本仓库的验证入口

| 范围 | 命令 / 位置 |
|---|---|
| 前端类型与打包 | `npm ci && npm run build`（`tsc && vite build`） |
| 工作流语法 | `.github/workflows/checks.yml` 的 actionlint job；本地可用 `actionlint` |
| Rust（muse-native） | `cargo test --manifest-path src-tauri/muse-native/Cargo.toml`；`cargo clippy --manifest-path src-tauri/muse-native/Cargo.toml --all-targets` |
| Rust（主 crate） | 由 iOS 构建验证（本机无完整 SDK 时不得伪报通过） |
| iOS IPA | `Actions → Build unsigned iOS IPA`（手动 dispatch），产物 `ios-unsigned-ipa` |
| 快速检查 | push 到开发分支自动触发 `Quick checks`（前端 / actionlint / Rust） |

Rust 的 `cargo check` 需要在 macOS + Xcode 环境；若在 CI 中执行，注意 `IPHONEOS_DEPLOYMENT_TARGET=17.0`（Swift 插件最低版本）。

## 五、测试、安全和权限

- 认证、发送、审批、代理 fail-closed、备份恢复、房间编排等高风险改动必须遵守开发计划中的失败路径测试、真机验证和回滚门禁。当前 Agent 负责自检，必要时按风险另请复核，**但复核不是某个固定 Agent 的独占职责**。
- 不得向公开仓库、Issue、PR、Actions 日志或构建产物泄漏 Cookie、token、代理密码及真实账号隐私；不要随意 force-push 已公开提交。
- 保留既定产品范围：iOS 独立 App、Tauri/Rust/Swift、Muse 与 Hark、WebView 聊天、不依赖自建服务器。按最新用户裁决移除协作房间，优先保障多账号隔离、切换、代理和额度同步。
- `main` 的版本号、Bundle ID（`com.ldjx.accountmanager`）、签名与发布流程不随开发分支改动；Spike 版独立的 bundle id 只在 Spike 分支使用。

## 六、每次开始的动作

1. `git fetch`，确认 `main`、当前 Issue、关联 PR、最近 Checks 与工作树状态。
2. 读 Issue 的验收清单与最近一条进度评论；有分支/PR 就在原上下文续做。
3. 只推进一个主任务，按 `核对 → 实现 → 测试 → 更新 Issue/PR → 必要真机验收 → 合入 → 下一项` 闭环。
4. 阻塞时记录原因、证据和可行方案，继续处理不受阻的部分，不擅自简化目标。
