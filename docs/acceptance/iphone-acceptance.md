# iPhone 真机验收

当前 Spike 验收包是 **0.1.26**，Bundle ID 为 `com.ldjx.accountmanager.spike`，最低 iOS 17.0。开发状态与待验收范围见[当前进度](../development/current-progress.md)。

## 下载和安装

1. 从 [GitHub Actions 下载 0.1.26 产物](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45394618651/artifacts/14921695519)并解压。构建提交为 `034415b41250c36bbb5b72a721a2735c07367659`。可在[构建记录](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45394618651)核对。
2. 校验解压后的未签名 IPA，预期 SHA-256 为 `d666a196436d81a3da4bf4e9235e1c381862437dd987439565b0b288b4b99279`。使用现有签名和侧载流程安装「叠号 Spike」。
3. 登录非主力 Muse 测试账号。0.1.24 有重启丢失代理配置的问题，代理验证使用 0.1.26。若旧版已丢失设置，先在新版重新保存。

## Muse 指纹与发送

### 实时增量专项复测

**`DEVICE_VERIFIED`**。2026-11-25 18:30–18:31（UTC+8），`8b7eb0a` 在 iPhone 前台完成三条 Muse 隔离探针。三条均收到 ACK，并由订阅流确认 `done`，全部入样评分。回传的摘要和脱敏帧记录相符。

| 探针 | 关联文本增量 | 首末增量间隔（帧记录） | 首个增量至消息结束（帧记录） | 结束 |
| --- | ---: | ---: | ---: | --- |
| 1 | 72 | 2,438 ms | 2,834 ms | `done` |
| 2 | 79 | 2,677 ms | 3,072 ms | `done` |
| 3 | 80 | 2,913 ms | 3,213 ms | `done` |

三条的解析数分别为 295/295、323/323、326/326，均超过有效样本阈值。lmfpd 完成 3/3 校准评分，闭集参考首位为 `deepseek-v4-pro`（60.3%）。本轮未单独验收退后台取消或自动锁屏设置恢复。

当前前台采集复测包为 [叠号 Spike 0.1.26 `8b7eb0a`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45592282143/artifacts/15012002699)，构建 SHA 为 `8b7eb0aec761c30d606c1f2883822453cd87e95d`。解压后未签名 IPA 的 SHA-256 为 `2e18af7d6a03ba816193860c4f17bba24bdcc70ed28db34df9c0fc2902a06771`。用同一账号和代理，在叠号前台运行一次「指纹探针 3 条」。生成时观察实时增量提示，逐条核对关联文本增量大于 0、结束原因为 `done`，并确认 3/3 入样评分。结束后回传顶部的结果摘要和脱敏流诊断。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45592279913)

当前流关联复测包为 [叠号 Spike 0.1.26 `07cf1fa`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45587399034/artifacts/15010245635)，构建 SHA 为 `07cf1fa66981c108cdf10bf6e8b0eea12941a66f`。解压后未签名 IPA 的 SHA-256 为 `4ea3d42ab05e7d13c5cfba4f597220bf7e70c0793216cec7071faf5bc8dceebd`。同一账号和代理，保持前台亮屏并运行一次「指纹探针 3 条」。观察运行时是否出现增量提示。结束后回传顶部的结果摘要和脱敏流诊断，逐条核对关联文本增量大于 0、结束原因为 `done`。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45587301157)

[叠号 Spike 0.1.26 `11c6993`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45584363920/artifacts/15008332630) 真机三次 ACK、三条 `done`、3/3 样本及评分均通过。三轮各收到 80、81、75 个订阅增量帧，首末间隔分别为 2,336、2,162、2,037 ms。增量关联均为 0。该包的 IPA SHA-256 为 `b1648092ef0683d5ab41533f3c6d9ca597f3493afe7afaab3bf3c84433bdd317`。

[叠号 Spike 0.1.26 `2066ccb`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45578059347/artifacts/15005551703) 真机的推送能力注册、隔离订阅、两次发送 ACK 均通过。第二条读取 147 个增量帧，关联数为 0，用户手动停止，第三条未发送。上一包的 IPA SHA-256 为 `a4e11a6f6417315d3311ed8f44743a6dd355934720e5a350cffd2b6c5f1b5615`。

注册诊断包 [叠号 Spike 0.1.26 `775a49b`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45575340271/artifacts/15005065668) 在设备节点注册时返回 `HTTP 400 · invalid_params`，没有写出任何探针消息。解压后的 IPA SHA-256 为 `fc55c7b75e73d78540edebb01e97f75adc4b5f119076d946a5d834b202842080`。

隔离路由复测包 [叠号 Spike 0.1.26 `7ae68db`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45572061671/artifacts/15003088501) 解压后的 IPA SHA-256 为 `612bac62bc52807b90dd436d8a0a36b8108ccbd62e53e111b7e968fdc8c923eb`。真机在节点注册阶段收到 HTTP 400，发送 0 条。注册和订阅成功后，生成期间观察「已收到实时文本增量，等待流内结束」。结束后核对逐条结果的「文本增量 > 0」与结束原因为 `done`，回传结果摘要和脱敏流诊断。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45572018340)

界面诊断包 [叠号 Spike 0.1.26 `7a8dd54`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45567860405/artifacts/15000649381) 的 IPA SHA-256 为 `701827c022e50ab9e5de89f9c19565c2f9605615ee08229c4cd47b66635cdae9`。顶部复制真机通过，三条均走历史恢复，实时文本增量与流内结束未通过。

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

代理专项复测使用 [叠号 Spike 0.1.26 `7588dca`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45600497159/artifacts/15015772654)。解压后未签名 IPA 的 SHA-256 为 `150e955efa70bed2dbab3b0f521867bed3d545f51397b85d87166df365a9829f`。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45600495276)

`8b7eb0a` 真机中，三协议的连通性均为 200，原生均在 `/api/hatch/lease-vm` 返回 `Invalid vmType`，未进入 WS 握手。

2026-11-25，`7588dca` 的三协议原生端到端验证为 `DEVICE_VERIFIED`。每次均在同一账号测试中切换代理协议，验证返回 `ready`、WebSocket 握手完成、Ping HTTP 200。

| 协议 | 通过时间（UTC+8） |
| --- | --- |
| SOCKS5 | 19:18:35 |
| HTTP CONNECT | 19:22:26 |
| HTTPS CONNECT | 19:23:07 |

该轮未记录租约命中路径。随后同一构建的 SOCKS5 在重启后于 19:31:49 再次到达 `ready`、握手完成、Ping 200，代理与凭据持久化为 `DEVICE_VERIFIED`。

错误代理密码注入后，19:29:48 连通性检测报 `connect / socks_connect`，无 HTTP 响应。19:30:42 完整原生探针达到 30 秒连接总时限，未完成握手、无 Ping 结果。另一次在确认框取消，未纳入验收。同一 IPA 后续真机对照中，保存错误 SOCKS5 密码后 Muse WebView 无法加载，恢复正确密码后页面重新载入。原生和前台 WebView 的故障表现已真机观察，实际出口路径和后台额度同步仍为 `UNVERIFIED`。该构建将外层连接总超时误标为 `http_client`，无法据此确定底层子阶段。

1. 用同一代理地址及认证参数，依次配置 HTTP CONNECT、HTTPS CONNECT、SOCKS5。每种先运行「检测代理连通性」，再运行完整的「代理探针」。分别记录匿名 HTTPS 响应、握手、Ping 状态及失败阶段。
2. 启用代理后强退并重开 Spike，核对配置和凭据可用状态仍在，并复跑一次完整探针。条件允许时，用另一个非主力账号验证设置和诊断结果不会串号。
3. 在代理已启用的情况下，使用错误凭据或断开代理。分别检查原生探针、WebView 及可触发的后台额度同步。预期是请求失败并保持阻断。没有代理侧出口日志或受控网络观测时，记录客户端表现并将「无直连」标为 `UNVERIFIED`。

## iOS 后台额度同步

当前使用 [叠号 Spike 0.1.26 `ab6f6fa`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45613523633/artifacts/15020828326)。解压后未签名 IPA 的 SHA-256 为 `4f5fbe8aaf3f5e0fd933041e3efbf5a6f6a673cac61b0bec95bcf6e9d82ad9ff`。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45613514999) 与 [Xcode 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45613523633) 均通过。

先明确清除代理，再运行一次「测试后台额度同步」，确认主文档提交及额度读取。然后启用已知正确的 SOCKS5 密码并保存，复测后台同步；若「查询额度」仍失败，附上原生的脱敏错误分类。两条基线定位后，再按下方做错误及恢复正确密码的对照。隐藏 WebView 从主文档提交后开始探测，新包中「主文档已提交」表示导航已到该阶段，额度结果另行核对。

上一轮使用 [叠号 Spike 0.1.26 后台额度复测包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45608434585/artifacts/15019407801)。构建提交为 `dda5127fbbb38ae71efc98570af04296f051e36f`，解压后未签名 IPA 的 SHA-256 为 `5fb9a4f0bcea507490af8240f12c8e84435986e7c44f004c5146515e7f4c33cc`。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45608432316) [Xcode 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45608434585) 均为 `CI_PASSED`，真机失败见下方回执。

保持叠号在前台的 Muse 账号管理页。测试按钮会为未打开会话的账号运行后台同步，其中隐藏 WebView 重新导航，并排除旧的本地额度缓存。每一行用平台与账号 UUID 末八位标识，分别显示代理、联网和额度结果。

1. 用已登录的同一个测试账号，保存正确的 SOCKS5 代理密码。打开 Muse 顶栏「诊断」→「额度与审批」→「测试后台额度同步」。该账号应显示「代理 已下发」「主文档已提交」，额度为「已更新并保存」或「已取得，数据未变」。
2. 只改该账号的代理密码并保存，保留代理地址、协议和用户名。再次运行同一测试。若代理参数下发成功但认证失败，预期显示「代理 已下发」，随后显示联网失败、超时或 HTTP 失败，额度未更新。若代理本身下发失败或应用失败，预期显示对应代理错误和「联网 未发起」。其他账号不应随这一项变更而失败。
3. 恢复并保存正确的代理密码，第三次运行，确认该账号恢复。每轮用「复制脱敏同步结果」保存对应行。记录 iOS 版本和三轮结果，保持代理凭据只留在设备上。如果提示已有后台同步在运行，等待一轮后重试。

「页面加载成功」只表示主页面已加载，额度结果另行核对。若正确密码下仍显示「未取得可更新的额度」，或错误密码下报告更新，需要保留该轮诊断，继续排查。实际出口路径需要独立网络证据。

### `ab6f6fa` 真机回执

同一 Muse 账号 ···6102e36b。

| 场景 | 观测结果 | 验收状态 |
| --- | --- | --- |
| 清除代理后的原生查询 | Muse 免费版已用 8%，来源 action | `DEVICE_VERIFIED` |
| 清除代理后的后台同步 | 代理未启用，主文档已提交，额度已取得、数据未变 | `DEVICE_VERIFIED` |
| 首轮标为有代理的后台同步 | 代理未启用，主文档已提交，未取得可更新的额度 | 未纳入代理对照 |
| 错误 SOCKS5 密码的后台同步 | 代理已下发，主文档提交前失败，URL 错误 −1013，未取得可更新的额度 | 客户端故障阻断表现 `DEVICE_VERIFIED` |
| 恢复正确 SOCKS5 密码的后台同步 | 代理已下发，主文档已提交，额度已取得、数据未变 | `DEVICE_VERIFIED` |
| 正确 SOCKS5 密码下的原生「查询额度」 | 用户补充确认该按钮已成功读取额度 | `DEVICE_VERIFIED` |

本轮通过 Muse 后台额度直连、错误 SOCKS5 密码阻断及恢复正确密码的对照。正确密码复测取得的额度与已保存的数据一致，未观察到数值变化触发写入。独立出口观测仍为 `UNVERIFIED`。

### `dda5127` 真机回执

SOCKS5 启用时，后台同步显示「代理 已下发」「联网超时」「未取得可更新的额度」。明确清除代理后，同一账号仍显示后台联网超时和未取得额度。本包的超时从启动导航计到 `didFinish`，不能确定是未收到主文档还是未完成全页加载。该构建的后台同步正向验收为 `FAIL`。未执行保存错误密码和恢复正确密码的对照。

原生「查询额度」走独立路径。代理启用时，匿名 HEAD 连通性返回 HTTP 200，带会话的冷发现 GET 返回传输错误。清除代理后，原生读取 Muse 免费版额度成功。原报告没有底层错误类别，不能确定代理失败原因。

## 回填结果

### 0.1.26 Muse 指纹真机回执

2026-11-24 回执。历史恢复路径为 `DEVICE_VERIFIED`，本轮从确认到返回约 5 分 29 秒。3 条探针均为 `CONFIRMED` 和 `history_completed`，有效样本解析为 330/330、326/326、305/305，3 条全部参与实际 lmfpd 评分。参考库首位候选为 `deepseek-v4-flash-0731`，闭集校准置信度 21.0%。三条流式文本增量均为 0，实时采集仍为 `UNVERIFIED`。机型、iOS 版本及原会话网页侧对照未随本次回执提供。

在 [PR #16 验收记录](https://github.com/harrylarry/multi-agent-account-manager/pull/16#issuecomment-7130670517)关联的 Issue 或 PR 下记录机型、iOS 版本、构建 SHA、场景、结果、预期差异和脱敏诊断。保留各项的 `DEVICE_VERIFIED`、`FAIL`、`UNVERIFIED` 或 `BLOCKED` 状态。回传前移除 Cookie、Token、账号邮箱及代理认证信息。
