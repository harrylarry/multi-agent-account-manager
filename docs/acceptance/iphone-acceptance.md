# iPhone 真机验收

当前 Spike 验收包是 **0.1.26**，Bundle ID 为 `com.ldjx.accountmanager.spike`，最低 iOS 17.0。开发状态与待验收范围见[当前进度](../development/current-progress.md)。

## 下载和安装

1. 从 [GitHub Actions 下载 0.1.26 产物](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45394618651/artifacts/14921695519)并解压。构建提交为 `034415b41250c36bbb5b72a721a2735c07367659`。可在[构建记录](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45394618651)核对。
2. 校验解压后的未签名 IPA，预期 SHA-256 为 `d666a196436d81a3da4bf4e9235e1c381862437dd987439565b0b288b4b99279`。使用现有签名和侧载流程安装「叠号 Spike」。
3. 登录非主力 Muse 测试账号。0.1.24 有重启丢失代理配置的问题，代理验证使用 0.1.26。若旧版已丢失设置，先在新版重新保存。

## Muse 指纹与发送

### 实时增量专项复测

当前实时取证包为 [叠号 Spike 0.1.26 `7a8dd54`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45567860405/artifacts/15000649381)，构建 SHA 为 `7a8dd54698f76a4fade1aa5536cfa750d353bf0b`。解压后未签名 IPA 的 SHA-256 为 `701827c022e50ab9e5de89f9c19565c2f9605615ee08229c4cd47b66635cdae9`。从 Muse 顶栏点「诊断」，用同一测试账号和代理在前台运行一次「指纹探针 3 条」。结果出来后直接点诊断页顶部「复制脱敏流诊断」并回传。滚动、顶部复制和实时回复均待 iPhone 复测。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45567818630)

上一取证包 [0.1.26 `5d8bf46`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45563228807/artifacts/14998504572) 解压后的 IPA SHA-256 为 `0ac460d3e88d798d8bd1965c2b5eff68ecf8c846d549cc264751c7aef911d679`。真机三条均走 `history_completed`，增量为零，旧诊断区无法滚到底，未取得独立帧日志。

使用 [0.1.26 Spike 构建 `8f50c91`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45417370699/artifacts/14932589152)。解压后未签名 IPA 的 SHA-256 为 `bcf5aa552c37c31fef7feed864ed5f08a9127917d18a6437081e51d7c31e66e8`。两个验收包显示同一版本号，以下载链接和校验值区分。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45407570093)

在前台运行「指纹探针 3 条」，回传每条的 ACK、文本增量数和结束原因。`文本增量 > 0` 表示采集器收到增量，`done` 表示从流判定结束。`history_completed` 表示由历史恢复确认。此包在结束后汇总增量计数，无法用汇总数核对首个增量的到达时间。

专项包 `8f50c91` 的真机回执耗时约 5 分 48 秒。三条均确认发送，均经 `history_completed` 恢复并参加评分，解析数为 317/317、295/295、330/330。关联增量均为 0，事件行数为 7、1、1，未关联事件数为 2、0、0。此次实时增量与流内结束验收均为 `FAIL`，指纹历史恢复为 `DEVICE_VERIFIED`。首位候选为 `qwen3.8-max-0902`，闭集校准置信度 48.3%。

下一轮使用 [0.1.26 诊断构建 `d983d52`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45431942405/artifacts/14939822602)。解压后 IPA SHA-256 为 `faeab56be537556e1feb5c13dd0f69c3090d4068f6d763c23590d239c937073c`。保持同一测试账号、代理与前台环境，回传每条的「发送流」「订阅流」和上方的结束原因。两路文本增量计数均为零表示观察窗口内未收到可识别的增量事件。原始增量计数大于零且关联增量为零时，可进一步核对关联规则。首个关联增量时间以开始采集为基准，包含缓冲事件被处理的情况。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45431938839)

### 通用验收

1. 记下测试账号原会话的当前状态。返回管理页，在该账号的原生诊断区运行指纹探针。
2. 核对探针均出现在新建会话，原会话没有新增探针。验证每条完整回复结束后才发下一条。
3. 记录网络写入、发送确认、回复完成、有效样本和实际 lmfpd 评分。通过条件是 **3 个 ACK、3 条完整回复、3 个有效样本和实际评分**。
4. 若诊断显示 `history_completed`，该轮由只读历史确认完成。若中途失败，记录最后阶段、采集结束原因和后续发出条数。连接中断或停止后，核对后续探针未继续发送。ACK 已确认的发送仍应保持确认状态。

## 原生代理与重启回归

1. 用同一代理地址及认证参数，依次配置 HTTP CONNECT、HTTPS CONNECT、SOCKS5。每种先运行「检测代理连通性」，再运行完整的「代理探针」。分别记录匿名 HTTPS 响应、握手、Ping 状态及失败阶段。
2. 启用代理后强退并重开 Spike，核对配置和凭据可用状态仍在，并复跑一次完整探针。条件允许时，用另一个非主力账号验证设置和诊断结果不会串号。
3. 在代理已启用的情况下，使用错误凭据或断开代理。分别检查原生探针、WebView 及可触发的后台额度同步。预期是请求失败并保持阻断。没有代理侧出口日志或受控网络观测时，记录客户端表现并将「无直连」标为 `UNVERIFIED`。

## 回填结果

### 0.1.26 Muse 指纹真机回执

2026-11-24 回执。历史恢复路径为 `DEVICE_VERIFIED`，本轮从确认到返回约 5 分 29 秒。3 条探针均为 `CONFIRMED` 和 `history_completed`，有效样本解析为 330/330、326/326、305/305，3 条全部参与实际 lmfpd 评分。参考库首位候选为 `deepseek-v4-flash-0731`，闭集校准置信度 21.0%。三条流式文本增量均为 0，实时采集仍为 `UNVERIFIED`。机型、iOS 版本及原会话网页侧对照未随本次回执提供。

在 [PR #16 验收记录](https://github.com/harrylarry/multi-agent-account-manager/pull/16#issuecomment-7130670517)关联的 Issue 或 PR 下记录机型、iOS 版本、构建 SHA、场景、结果、预期差异和脱敏诊断。保留各项的 `DEVICE_VERIFIED`、`FAIL`、`UNVERIFIED` 或 `BLOCKED` 状态。回传前移除 Cookie、Token、账号邮箱及代理认证信息。
