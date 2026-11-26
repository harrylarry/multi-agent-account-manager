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

## VM 租约、后台连接与账号组织

使用 [叠号 Spike 0.1.26 `d32511e`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45620072467/artifacts/15028035192)。解压后未签名 IPA 的 SHA-256 为 `d07a4af43bed96e51de0b1c2a9b9eab470f486cbf94ee0c057481ee11f2b846c`。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45619968807) 与 [Xcode 打包第 2 次尝试](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45620072467/attempts/2) 均通过。分项真机结果见下方回执。

1. **VM 租约**。在已登录的 Muse 测试账号上打开「诊断 → 连接与消息测试 → 租约探针」。保存调用轨迹、到期时间、TTL 和复查结论。若显示「已确认 VM 分配」且无到期字段，记录分配可见、租期不可观测。若两次对应不同 VM，分开记录，勿算作续期。用同一账号分别记录代理和直连条件下的结果。
2. **后台连接**。运行「存活 5 分钟」，等到「订阅就绪」有时间再退后台。分别记录前台、锁屏、短后台、长后台、低电量的结果与最大观测间隔。强制结束进程单独记为测试中止。另开一轮点击「停止」，确认结束原因为 `cancelled`。再用不同账号重复，核对停止与原探针绑定。此测试不会依靠静音音频维持连接。
3. **Hark 检测回归**。若手边有已登录的 Hark 测试账号，在 Hark 页运行「Hark 原生检测」，用同一会话的网页记录对照；重复一次包含断流、恢复和重叠样本的观测。有审批项时单独对照页面状态，先做只读验收。
4. **账号组织**。在两个测试账号上设置不同分组，搜索备注及分组名。按分组过滤，再清空分组，确认账号仍在且可打开。切换 Muse/Hark 检查过滤独立。无过滤时长按拖动，重启后核对备注、分组和排序均保留。

## Muse 账号审批与回前台补查

使用 [叠号 Spike 0.1.26 `bda84e7`](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45631028431/artifacts/15032276410)，解压后的 IPA SHA-256 为 `ca283f9ac32c2de16200a6076458e5e276295a9394695d08839e0ef12ee52ca7`。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45631020421) 和 [iOS 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45631028431) 均通过，待真机验收。

1. 在 iPhone 的 Muse 账号列表点一个已登录测试账号所在行的「审批」，核对账号名称。有待审批项时与会话页面对照条目名称、类型、请求时间和到期时间；若无待审批项，记录本轮只观察到空列表。用「打开账号」回到同一个账号。若查询失败，记录界面中显示的状态，保留服务返回为空与结果未知的区别。
2. 在 Muse 测试账号中发起用户自己的任务，看到生成后返回账号管理，让会话留在停驻缓存中。锁屏或切到后台，等待网页对应回复完成后回前台。观察约 12 秒内该账号是否出现完成红点；另一个账号的红点不得因此改变。复测账号会话已被关闭或进程被杀的情况，单独记录是否有可观察页面。
3. 本轮验收前台补查及现有红点。系统挂起期间的实时检测、系统通知和原生审批写操作各自保留验收状态。

## v2.2 A–C 联合验收

叠号 Spike 0.1.26 构建提交为 `815cffab61309f12df1411f22386250643486e12`。解压后的 `account-manager-ios-unsigned.ipa` SHA-256 为 `5527b11240430815c99458b0429f494d64fddc7bb06fea71e43f4d10bd88a9a0`。已验证包体完整、Spike Bundle ID 和 iOS 17.0 最低版本。[下载 IPA](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45646166968/artifacts/15036864191) · [Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45646195238) · [Xcode 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45646166968)

下载 GitHub Actions 产物并解压。按现有方式给未签名 IPA 签名，再在同一个 iPhone 上覆盖安装 Spike。准备两个自己的测试账号。回执只需机型、iOS 版本及各项的操作结果和脱敏时间。

1. **Cookie 与登录恢复**。在目标账号打开「登录恢复」，分别预览 JSON、Netscape 和请求头。试一个外域或过期输入，确认拒绝后旧会话仍能打开。导入有效数据后点「打开并验证」，切换另一个账号核对身份隔离，重启确认带有效期的 Cookie 保留。断网时检查登录状态没有误判为登出。另测只在本机可用的导出剪贴板是否在 60 秒后过期。
2. **任务、审批及回前台**。在测试 Muse 账号发起一个任务，生成中回账号管理并把 App 切到后台，任务结束后回前台，记录同一账号的状态、最近一次成功时间和红点。手动刷新时用间隔至少 5 秒的两次一致结果确认状态。复测关闭通知系统权限、重启、无网络恢复。用真实待审批样本对照账号、时限和列表，优先测试「拒绝」或「仅本次允许」与回读。永久允许只用明确可撤销、限定范围的测试项。结果未知时检查网页结果，保留同一审批 ID 的首次回执。
3. **代理池与额度**。在「代理池」粘贴导入或添加 HTTPS 订阅，手动分配给目标账号，比较测速、端到端探针与任务页的额度来源、时间和重置字段。保存错误的代理密码测试失败阻断，再恢复正确密码。刷新一个失效的订阅，确认已有端点与其他账号代理保持可用。
4. **会话与分组**。在两个 Muse 账号分别打开「会话」，点开非当前线程，对照同一账号的网页记录。断网后刷新并核对最近打开入口还在。设置不同的账号分组、排序和时区，重启核对配置与会话绑定，比较额度时间。Hark 用已登录测试账号检查任务刷新、断流恢复及「打开会话查看审批」。
5. **加密备份**。分别选轻量、全量和自定义，保存到「文件」。先用错口令、损坏副本和过期备份检查预览失败，再核对正确口令的新增、冲突、代理池计数。用测试资料验证合并与覆盖。恢复完成后检查分组和代理资料，账号凭据通过登录恢复配置。

通知和回前台补查分别记录。设备锁屏、系统挂起及强退期间只记录实际观察到的结果。回执中移除 Cookie、Token、代理认证信息和会话正文。

## v2.2 真机回归修复复测

首轮 `815cffa` 回执（2026-11-26，UTC+8）。账号「任务」触发闪退，崩溃程序 UUID 与构建匹配，主线程故障指令访问已落入 Stack Guard 的栈指针。Muse「会话」返回格式错误。登录恢复显示的本机 Cookie 数量在打开账号前后不一致，预览重绘导致输入区闪烁。退后台再回前台未出现叠号账号红点，进网页可见网页自己的提示点。代理池手工添加和使用通过，审批列表空态可见，备份导出可用。订阅格式和恢复留待复测。

修复候选为 0.1.26 Spike，提交 `9a7c229ae266efb8ea4ed22e2f677754d53fa01a`。下载后解压并签名覆盖安装。解压后的 IPA SHA-256 为 `ebd12b4f40b7b60ce0e5cb4f41f9897f8d70ef8c1d8dec6751d4d50314e7ab5d`。[下载 IPA](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45657309502/artifacts/15043432757) · [Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45657308835) · [Xcode 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45657309502)

1. **任务是否闪退**。在 Muse 账号列表，点某个已登录账号的编辑图标 →「任务」。保持页面打开等结果，最多约 35 秒。记录状态或错误，点「刷新」再测一次。账号保持可打开即可继续下项。
2. **回前台红点**。先打开一个已有的 Muse 对话，发一个要生成十几秒以上的请求。回复生成中，先点左上「← 账号」，看到叠号账号列表后再回 iPhone 主屏。等回复完成，重新进入叠号，在列表停留 20 秒。看**该账号头像旁**是否出现红点，进网页前先记下结果。网页菜单里的点单独记录。再做一轮停在账号列表、不回主屏的对照。
3. **Cookie**。账号行点「登录恢复」，不填内容先点「预览」，应提示先粘贴。将 `Cookie: maam_preview=ok` 粘贴到下方输入框，点「预览」应显示 1 条。再粘贴 `[{"name":"maam_preview","value":"ok","domain":"example.com","path":"/","secure":true}]`，应提示外域。用该行 JSON 把域名改为 `muse.ai`，并添加 `"expirationDate":1` 验证过期提示。示例都只点预览。之后关闭并正常打开原账号，返回列表再进「登录恢复」，连续关闭打开两次，记录 Cookie 数是否仍从有变零。需要验证自己的导入时，在已登录测试账号点「导出到剪贴板」→「复制」，60 秒内粘到同一「登录恢复」输入框预览。确认目标是该测试账号后才选择「导入」→「打开并验证」。分开重新导出一次，等 60 秒后尝试粘贴验证到期。
4. **会话与分组**。Muse 账号行直接点「会话」，等列表后点一个能在网页识别的旧会话，核对打开的是同一标题。返回列表，点编辑图标 →「分组」，输入「测试组」并保存。用列表的分组筛选选「测试组」，重启 App 后再次选该组，确认测试账号仍在。审批列表显示「当前没有待审批项」可记空态通过，写操作留到有真实待审批项。
5. **代理池与备份**。编辑图标 →「代理池」。普通代理地址点「导入代理」。点「添加订阅」前粘贴服务商提供的单个 HTTPS 下载链接，响应须是每行一个完整代理 URI 的纯文本，例 `socks5://user:pass@proxy.example:1080`。该示例只说明格式。备份可先在「备份」选刚导出的文件，输入口令点「预览恢复」，看到预览后直接关闭，这一步无需退出账号。

## 回填结果

### `9a7c229` 复测回执（2026-11-26，UTC+8）

| 场景 | 状态与证据 |
| --- | --- |
| 任务入口、原生额度 | `DEVICE_VERIFIED`。任务页正常打开并返回原生检测，原生额度显示更新。 |
| 会话与分组 | `DEVICE_VERIFIED`。用户确认会话能打开及分组复测正常。 |
| 回前台红点 | `FAIL`。生成中先回账号列表，再切到后台。回前台等 20 秒，账号头像旁无红点，打开同账号后网页提示可见。 |
| Cookie 导出再预览 | `FAIL`。同账号导出的内容粘回输入框后显示格式或属性无效。 |
| 代理订阅 | `BLOCKED`。本轮没有可用测试订阅地址。手动代理保留先前通过记录。 |

### Cookie 回贴与 Muse 回前台红点专项

候选为 0.1.26 Spike，提交 `7ed2ab599159c9746954ef976879e32b20f9bec5`。解压后的 IPA SHA-256 为 `11d8bc911fe6bfe2db2b102bdcac5fecf0e1a3c130a3607fa5617f70bb1b4535`。CI 和 iOS 打包通过，待真机。[下载 IPA](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45663105641/artifacts/15046817245) · [Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45663102947) · [iOS 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45663105641)

1. **Cookie 回贴**。Muse 账号行 →「登录恢复」→「导出到剪贴板」→「复制」。立即粘回同页输入框，再点「预览」。预览应显示有效条数，与刚才显示的复制条数一致。若导出提示部分记录未复制，记下条数。只测回贴时到预览即止。
2. **红点**。沿用上一轮相同操作。生成中点「← 账号」回叠号列表，再回 iPhone 主屏。回复完成后打开叠号，在列表等 20 秒。先看头像红点，再从编辑图标打开「任务」，记下「回前台补查」一行。测试同一账号时先记录红点与补查情况，再进入网页。

### Muse 协议补查与城市时区

`7ed2ab5` iPhone 回执。账号 Cookie 导出后回贴可正常预览，用户确认恢复后能正常登录。导出报告 20 条，恢复后报告 19 条。缺少各操作阶段的条数，差值原因 `UNVERIFIED`。回前台等 20 秒未见头像红点，打开网页可见通知点。同一次验收中，Cookie 恢复账号曾出现临时红点，触发动作未观察。

0.1.26 Spike 候选 `258e4283b94c8764c25f9393173ff2e69daf8f71` 已通过 CI 和 Xcode。解压后的 IPA SHA-256 为 `b737852dea6ea45861b0ed8e1bcb05d8964853037dfd6aaa544dac812065c1b5`。[下载 IPA](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45673505743/artifacts/15050354887) · [Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45673504573) · [Xcode 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45673505743)

1. 在一个已有 Muse 会话发起较长的回复，生成中点「← 账号」回列表，再回 iPhone 主屏。等回复完成后打开叠号并留在账号列表，先看头像旁红点，再打开该账号编辑菜单 →「任务」，记下「回前台补查」一行。新路径优先核对同一停驻会话的完成回复，能识别时显示「会话历史确认有新回复」。
2. 红点出现后，可先切换管理页服务标签再回来，核对红点保留。等成功打开同一账号页面，再退出看红点消失。该操作不会调用 Muse 的标记已读写接口。
3. 点「编辑账号」→「账号时区」，选「东京」或「纽约」并保存，重开编辑页核对。再选「自定义」，填 `Europe/Paris` 并保存。用另一个账号核对其仍跟随系统。额度时间可作时区对照。

`258e428` 用户补充回执。按新版步骤复测，红点仍未能第一时间显示。精确延迟与「回前台补查」状态未随回执提供。即时显示验收保留未通过。

### 新增账号 Cookie 与 Hark 回复补同步

0.1.26 Spike 联合候选 `fcd12926377359093b3fc3561f431bab1e052df8`。CI 和 iOS 构建通过。解压后 IPA SHA-256 为 `200965325380d3d3a2c59d4cf5faff8f9e978ec1181bdf301b7e8b22e57882fd`。[下载 IPA](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45690395600/artifacts/15058807796) · [Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45690394689) · [iOS 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45690395600)

1. **新增账号 Cookie**。Muse 或 Hark 账号页点「新增」，填不同备注，先粘贴一个错误格式，点「预览 Cookie」应提示错误，列表保持原样。再粘贴自己的测试账号 Cookie，点「预览 Cookie」，确认条数及域名，再点「创建并打开」。看到正确账号登录后返回列表，验证原账号仍能打开，重启后复查新账号。留空时仍可「创建并打开」走常规登录。
2. **Hark 回复补同步**。用已登录 Hark 测试账号，在主会话发一条可识别的请求。回复生成中返回叠号账号列表。如需观察断网场景，可临时断开 iPhone 网络并恢复。打开该账号编辑图标 →「任务」→「补同步回复」，等待轮询结果，核对最新回复的内容和完成状态。点「重新读取」，同一回复只出现一次。完全断网时应显示失败或保留此前已读取的内容。恢复后重试。原会话中没有新增发送。
3. **卡片展开**。用一个主会话里已有的报告、总结、表格或代码卡片，或让测试账号生成一个。补同步后打开对应回复，再点「展开卡片内容」核对文本。如果协议只给普通回复文本或属于其他卡片类型，从「在 Hark 打开」核对原卡片，记录该类型未覆盖。

### Muse 红点分段计时与房间草稿

0.1.26 Spike 候选 `886d615bf437d5b73ee0dacf97277c1c22e4fcc6`。解压后 IPA SHA-256 为 `1271caa47cd5ccbe2c6c12fe50a4e92f9de51f3c2265fe6151516bdcdd6f7f4e`。CI、Xcode 与产物完整性已核对。[下载 IPA](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45708221165/artifacts/15067261041) · [Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45708219295) · [iOS 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45708221165)

1. **红点取证**。在已存在的 Muse 会话发长回复，生成中点「← 账号」回列表，再退出到主屏。等回复应已完成后回叠号，在列表停 20 秒，记录头像是否亮、约第几秒亮。先别打开会话，点同账号「编辑 → 任务」，回传完整的一行「红点计时 · …」，含「尚未收到回前台采样」的情况。该行只有时间、阶段和红点状态。
2. **房间**。Muse 顶部点「房间 → 新增房间」，用两个已有 Muse 账号选「评审」模板。A 勾指挥和审核，B 勾执行和上传，填写目标保存后点「校验并准备」。重启核对房间、目标、轮次和状态。可试建第二个房间，验证第一个未停止时第二个无法准备。另给执行者同时勾审核，确认保存会拒绝。此验收覆盖本机流程，真实模型派发继续下批。

### Muse 单账号房间和指纹档案

联合候选 Spike 0.1.26（iOS 17.0+），代码提交 `4884cee0adf797cb15cbf2f1df3ee44cb5b87b63`。iOS target、宿主检查及 Xcode 打包均通过，房间真实发收和按账号指纹档案待 iPhone 验收。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45711886220) · [iOS 构建](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45711886482) · [SPIKE IPA 下载](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45711886482/artifacts/15068048520)

下载 ZIP 后取出 `account-manager-ios-unsigned.ipa`。IPA SHA-256 为 `37956d838372289ebb0bedf6d5c05d5329069b8e6ddcc5f2b620518e592b14a4`。外层 ZIP SHA-256 为 `c3785cf6f374b2a6ec4748b0e817b05aabf5a4b869c8f986a825da9cf82c545f`。

1. **单账号真实派发**。用已登录的 Muse 测试账号点「房间 → 新增房间」，选「单账号任务」，同一账号勾「指挥、执行、上传」。轮次至少设为 2。目标可用「把早会、设计评审、测试三项安排写成三条中文清单」。保存 →「校验并准备」→「真实派发一轮」。确认仅发一条，出现「待确认」后展开「文字交付」，核对正文、来源、时间和 SHA-256。从「打开原隔离会话」核对该次请求与回复，再回房间。普通会话保持隔离。回传「待确认」和「已定稿」两种状态、轮次与回复来源，省略会话标识及正文。
2. **用户定稿与返工**。可先输入一条具体的返工要求，点「要求返工」，确认收藏的上次交付还在，下一轮由自己再次明确点击「真实派发一轮」。回收完成后点「确认文字定稿」，重启核对定稿、轮次与来源。不点击确认时应始终显示「待确认」。
3. **中断**。另建一个房间，点「真实派发一轮」，发出后点「停止本轮」。若已见有效 ACK，可从「只读回收回复」恢复，核对原隔离会话的用户消息数仍为 1。发送结果不明且没有可核对 ACK 时，可打开隔离会话人工核对，或点「结束房间」。强退重启后也按该规则检查。
4. **指纹按账号存档**。Muse 账号行点「编辑 → 任务 → 手动检测模型」，核对提示里的测试账号并确认。保持叠号前台，等任务结束后在同页展开「模型指纹」。核对时间、样本数、结果及参考库标识。切到另一账号和重启各检查一次。只有三条有效样本与校准分数都满足时才显示置信度，部分样本只留候选线索。

### Muse 多账号接力与独立审核

联合候选 Spike 0.1.26（iOS 17.0+），代码提交 `9c5ed9c1e8b6bf97cd2557b600d2ecfd2bb331ba`。`CI_PASSED`，Xcode 打包通过，真机待验。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45720907901) · [iOS 构建](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45720909560) · [Spike IPA 下载](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45720909560/artifacts/15072715946)

解压得到 `account-manager-ios-unsigned.ipa`。IPA SHA-256 为 `ab76d237bb58227b34eeafbb066eb9bd8898dcad25cc0ff36078726c233d4e4f`。外层 ZIP SHA-256 为 `8744be4a48acd73f643b82f19a3271ed20758635c38308deba269c08d5288c3c`。

1. **两账号审核和返工**。在「Muse → 房间 → 新增房间」选「评审」，让 A 担任执行，B 担任指挥、审核、上传。轮次设为 8。用纯文字目标，例如「起草一份三项测试清单。审核者独立核对是否包含断网恢复和重启验证，缺项时打回并写明补法」。点「校验并准备 → 开始接力」，保持前台。展开各步核对账号、来源和原隔离会话。正常顺序为执行 → 独立审核 → 整理交付 → 待确认。打回后应回到执行。待确认时输入一条返工要求，点「要求返工 → 继续准备 → 开始接力」。确认旧记录保留、新要求进入新轮次、计数继续累计，最后手动「确认文字定稿」。
2. **成员与固定角色确认**。准备后点「暂停 → 提议调整成员或固定角色」。把上传者从 B 改为 A，点「预览变更」，确认实际成员栏仍显示旧分工。点「取消变更」应保留原分工。再次提议并点「确认变更成员与角色」，实际成员栏才应变更，房间仍停在暂停。
3. **先达到的上限**。另建评审房间，把轮次设为 2。执行和审核入账后应显示「等待裁决」，没有第 3 次派发。再用 8 轮测审核打回，可让审核者只在收到用户未提供的独立验收材料时通过。若实际发生 3 次打回，第 6 次完整请求后应停在「等待裁决」，没有第 4 次执行或审核。若审核者没有打回，记录真实决定。
4. **三账号接力**。选「头脑风暴」，A、B、C 都勾讨论，A 兼指挥、C 兼上传。8 轮预算内应依次出现 3 条讨论、指挥汇总和整理交付，共 5 轮，再等待定稿。「群发对比」可用两账号核查，两次独立作答收到原题与用户补充，最后的上传者请求才附两份回答。
5. **中途补充与中断**。本轮进行中输入补充要求，点「本轮结束后暂停并补充」，看本轮完成入账后自动暂停，手动继续才发下一轮。另测「停止本轮」，核对后续接力未发生。有 ACK 时先只读回收，再决定是否继续。远端收到停止请求的应答与实际停生成分别记录。

回传模板、参与账号数、每轮的角色和来源、实际轮次与打回次数、末态即可。省略目标原文、完整会话 ID、凭据和账号地址。

### Muse 自动分派、简报与指纹重测

1. **分派依据**。用至少三个已登录的 Muse 测试账号，在「Muse → 房间 → 新增房间」点「刷新 Muse 额度」。选「评审」和「自动分派与换人」，不勾角色并保存。展开房间，记录两个分工、额度窗口与剩余比例。可另按「纳入手选成员」选择两个账号并让角色留空，确认只能在这两个账号内分工。额度缺失或窗口不同时应有明确提示。
2. **自动换人和固定角色**。在自动房间保存后、发送前，让执行者的代理使用错误的代理密码。开始接力后，若本轮确认没有发出且另一个符合条件的账号可用，应在「自动换人」中显示选择并继续。恢复正确代理密码。另建一个自动房间，创建时固定执行者的角色再复测，同一失败应停下，只有经过「预览变更 → 确认变更成员与角色」才改变固定分工。发送状态不明时应停下核对原会话。
3. **简报与复用**。完成一间文字评审并确认定稿，打开「运行简报」核对审核决定、待办或会话摘录和来源。另建房间，填相似目标，在「选择要引用的旧房间」查看相关提示。确认推荐没有自动选中，勾选后才保存。展开「已引用定稿」，核对节选、来源摘要和「查看源房间」。在草稿或暂停时取消勾选并点「确认选定引用」，重启核对。删除源房间需要单独勾选确认，已主动引用的节选保留，可在引用房间撤销。
4. **指纹手动修正和自动重测**。在某个已登录、未做过指纹的 Muse 测试账号点「编辑 → 任务 → 模型设置」。手动指定一个可辨认的模型名，将自动间隔设为 1 天并保存。关闭任务面板，停在叠号前台，确保当前没有活跃房间、额度可用。到期的探针应自动运行一次，结果仍记在这个账号。重开模型设置核对手填值、下次到期与档案，切换账号检查隔离，重启确认当天不会重复发。间隔改为 0 后关闭自动重测，手动检测入口仍可用。

回传分派依据和实际换人、引用是否经确认、自动任务是否只运行一次及重启后的日期。省略账号地址、代理密码和房间正文。

### `d32511e` 真机回执（UTC+8）

| 场景 | 实测 | 结论 |
| --- | --- | --- |
| Muse 原生连接、订阅与单发 | Noise 握手通过，Ping 200。15 秒订阅返回 HTTP 200，4 个事件含 1 次空闲。单发收到有效 ACK，台账为 `CONFIRMED`。 | 本轮回归 `DEVICE_VERIFIED`。订阅中的生成及完成事件去重未在这段实测覆盖。 |
| VM 租约 | 两次 `lease-vm(standard)` 均返回 `assigned`。到期与 TTL 均未见。 | 租约获取 `DEVICE_VERIFIED`。到期和续租不可观测，`UNVERIFIED`。 |
| 后台观测窗口 | 21:43:10 订阅就绪，最后事件同秒。21:51:15 记录 `window_elapsed`。目标窗口 300 秒，墙钟经过 486 秒，最大本地观测空窗 475 秒。 | 已观测到本地任务存在长空窗。空窗内的 WSS 状态和连接恢复 `UNVERIFIED`。 |
| 手动停止存活探针 | 21:52:16 订阅就绪，21:52:17 结束，结果为 `cancelled`。 | 手动停止 `DEVICE_VERIFIED`。 |
| CVM | 界面显示「未检测到」，诊断当前没有实际 CVM 能力查询。 | `UNVERIFIED`。 |

账号分组的重启持久化、Hark 和有待审批样本的只读查询仍待真机回执。

### 0.1.26 Muse 指纹真机回执

2026-11-24 回执。历史恢复路径为 `DEVICE_VERIFIED`，本轮从确认到返回约 5 分 29 秒。3 条探针均为 `CONFIRMED` 和 `history_completed`，有效样本解析为 330/330、326/326、305/305，3 条全部参与实际 lmfpd 评分。参考库首位候选为 `deepseek-v4-flash-0731`，闭集校准置信度 21.0%。三条流式文本增量均为 0，实时采集仍为 `UNVERIFIED`。机型、iOS 版本及原会话网页侧对照未随本次回执提供。

在 [PR #16 验收记录](https://github.com/harrylarry/multi-agent-account-manager/pull/16#issuecomment-7130670517)关联的 Issue 或 PR 下记录机型、iOS 版本、构建 SHA、场景、结果、预期差异和脱敏诊断。保留各项的 `DEVICE_VERIFIED`、`FAIL`、`UNVERIFIED` 或 `BLOCKED` 状态。回传前移除 Cookie、Token、账号邮箱及代理认证信息。
