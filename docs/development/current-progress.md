# 当前开发进度

## 当前优先级 · 多账号稳定性

已清理 iPhone 界面候选中的入口重复。账号详情分别提供任务与通知、额度、模型指纹与会话功能。共享代理池集中维护，单账号的选择、手动配置、停用与检测都在账号详情。Muse 诊断保留协议测试，Hark 诊断保留只读链路测试。真机重点复核子页返回、账号上下文与代理隔离。[验收指南](../acceptance/iphone-acceptance.md#iphone-界面与切号)

iPhone 界面候选 `1e8445f`（Spike 0.1.26）已完成账号首页、账号详情、添加账号和管理页重构。切号与账号代理沿用原生路径，管理页的共享代理池与账号详情内的代理应用分开。前端构建、13 项嵌入脚本测试、Quick checks、macOS iOS 构建及 IPA 校验通过。带触控模拟的桌面 WebKit 在 320×568 深色及 390×844 浅色下通过快速切号、按账号应用代理、列表返回位置、Cookie 预览与导入、审批和备份回归。真实会话和代理隔离仍为 `UNVERIFIED`。[验证步骤与下载](../acceptance/iphone-acceptance.md#iphone-界面与切号)

用户已取消协作房间。先前房间设计与验收记录仅作历史。新版移除入口、派发、模板、附件分享及备份扩展，在升级启动时清理应用内的相关文件和旧台账条目。账号、网页登录态、代理、分组、时区和普通发送台账继续兼容。旧加密备份仍能恢复账号和代理资料。

iOS 打开会话和自动额度同步用同一代理写锁读取并下发最新配置；额度回写会重查账号归属、代理与时区。并发的自动额度触发合并。当前回归优先核对 Muse/Hark 切换、代理故障阻断和重启后的登录隔离。[真机步骤](../acceptance/iphone-acceptance.md#多账号稳定性回归)

`c70c8ba` 已通过 81 项主 crate、110 项 Muse 协议测试、前端构建、嵌入脚本和两个移动 WebKit 视口的模拟回归。iOS CI、Xcode 和 IPA 下载校验通过。真机覆盖升级、清理和并发切换保持 `UNVERIFIED`。[iOS 构建](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45853722650)

2026-11-25 接续快照。任务状态和原始证据以 [PR #16 接续记录](https://github.com/harrylarry/multi-agent-account-manager/pull/16#issuecomment-7130670517)、[Issue #13](https://github.com/harrylarry/multi-agent-account-manager/issues/13) 和 [Issue #14](https://github.com/harrylarry/multi-agent-account-manager/issues/14) 为准。范围与验收门禁见[开发计划 v2.1](../叠号开发计划-严谨执行版-v2.1-2026-11-23.md)与[开发决策记录](../开发决策记录.md)。

## Phase 0 技术 Spike

开发分支为 `spike/native-methods`。v2.2 A–C 联合验收候选 0.1.26 的代码提交为 `815cffab61309f12df1411f22386250643486e12`，交付在 PR #16。[候选和步骤](../acceptance/iphone-acceptance.md#v22-ac-联合验收)

| 切片 | 已核对证据 | 待验收 |
| --- | --- | --- |
| Muse 认证、订阅、审批、额度 | 旧版真机已有 Noise 握手、Ping、订阅、空审批列表与额度证据，详见 PR #16。 | 新包回归。有待审批项的真实样本继续单独记录。 |
| Muse 隔离发送、实时接收与模型指纹 | `8b7eb0a` 前台 `DEVICE_VERIFIED`。3 次 ACK，三条分别关联 72、79、80 个文本增量，均经流确认 `done` 并参加 lmfpd 校准评分。历史恢复路径已有独立真机证据。 | 退后台取消、锁屏设置恢复、原正常会话无污染的网页侧对照。 |
| 原生代理与 WebView | `7588dca` 的三协议原生端到端与 SOCKS5 重启持久化 `DEVICE_VERIFIED`。`ab6f6fa` 的 Muse 后台额度直连、SOCKS5 正误密码对照及经 SOCKS5 的原生额度读取 `DEVICE_VERIFIED`。错误密码下两种原生探针均未连通，Muse WebView 无法加载，恢复正确密码后重新载入。受控 reqwest 测试覆盖 SOCKS5 远端解析与代理拒绝时无直连。 | 故障时真机出口观测、账号切换隔离、后台额度数值变化写入 `UNVERIFIED`。 |
| VM 租约与后台 WSS | `d32511e` 真机两次取得租约，订阅就绪与手动停止通过，实测最大任务观测空窗 475 秒。 | 租约到期字段与续租不可观测，后台空窗内的 WSS 状态、回前台连接恢复和 CVM `UNVERIFIED`。 |
| Hark 只读检测 | `8eed8ff` 的 REST/SSE/poll 与两路去重 `CI_PASSED`，已包含在 0.1.26 IPA。 | Hark 真机登录、状态对照、断流回退、WebView 审批 `UNVERIFIED`。Hark 指纹按已确认裁决另做 Spike。 |

0.1.25 的 [Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45389037648)、[Xcode 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45389039398)和 [Hark Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45390431182)均有对应提交。真机结果需记录对应包和测试环境。

## v2.2 A–C 联合交付

| 切片 | 代码与本地证据 | 新候选 iPhone 门禁 |
| --- | --- | --- |
| A · Cookie 与登录恢复 | `529564c` 完成三格式解析、账号绑定、导入预览、写入核验和回滚、本机限时导出及重新打开验证。UI 和存储回归包含分组、搜索和排序。 | Cookie 过期或拒绝后的原会话、跨账号、重启与剪贴板到期 `UNVERIFIED`。 |
| B · 任务、审批与代理池 | `757fccd`、`4662396`、`815cffa` 完成任务检测与前台补查、失败保持未知、两次稳定采样、完成和待审批去重、审批提交前复查和台账、账号代理池与测速。 | 真实待审批项及写操作、通知系统权限、回前台红点、订阅失败与代理交叉隔离 `UNVERIFIED`。iOS 挂起和强退期间的实时提醒继续受系统执行条件限制。 |
| C · 会话、Hark 与数据 | `aabd2e5` 完成 Muse 会话列表和账户内打开。后续提交接通最近会话绑定、网页及原生额度时效、账号时区、Hark 任务状态和 WebView 审批入口、AES-256-GCM 备份预览与恢复。 | Muse 跨账号会话、Hark 登录/断流/审批、加密文件分享和恢复 `UNVERIFIED`。需要可用的 Hark 账号及真实审批样本。 |

Windows 主 crate 67 项、muse-native 110 项、嵌入脚本 13 项与构建检查通过。Edge 和 WebKit 在 320×568、390×844 测试登录预览、会话选择、分组、代理池、审批确认与备份流程。另验证时区覆盖和 5 秒任务采样间隔。macOS CI 已通过 iOS Rust target、Swift 插件编译与宿主测试。`CI_PASSED`。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45646195238)

同一 SHA 的 Xcode IPA 构建通过，ZIP 与 GitHub SHA-256 一致。已核验内部 IPA、Spike Bundle ID 和 iOS 17.0 最低版本。真机按联合清单保留逐项 `UNVERIFIED`。[iOS 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45646166968) · [验收包及校验值](../acceptance/iphone-acceptance.md#v22-ac-联合验收)

## 历史批次与验收记录

房间按 2026-11-26 新裁决支持先创建长期房间，再在房间发消息。创建选择 Muse 成员和一名指挥，名称可选。发送先写入房间记录，指挥按当前消息直接回应或提交绑定本次派发的结构化分工。App 自动校验并依次派活，给后续成员传共享发言与当前审核必需材料。任务级确认、会话隔离、丢 ACK 停止、每任务轮次门禁及既有模板房间继续运行。真机尚未验证。[房间复测](../acceptance/iphone-acceptance.md#长期房间与指挥自动派发)

`e205d61` 为 `CI_PASSED`。96 项 Rust 测试通过，覆盖普通消息、自动派发、执行审核分离和多任务定稿记录。两个移动 WebKit 视口用模拟 IPC 验证建房和发送。已下载校验 IPA。真机先重新验收一条普通消息，再验多人任务。[iOS 构建](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45834074291)

`e205d61` 真机已建两账号房间，观察到指挥分派、成员回复与两次审核打回。房间每次派发均创建新 Muse 会话，前两次历史回收仅取得进度文字，第三份长回复后停在等待裁决 6/8，原因未从回执单独确认。整改使长期房间每个成员保留一个专用隔离会话；旧版分散会话继续作为历史来源。仅完成一条助手消息的历史记录不能推进房间，必须有与当轮相关的结束证据；无证据时停止接力并保留核对入口。已延长房间实时回收窗口。新行为待 iPhone 验收。

整改包 `750e12a` 已通过 96 项主 crate、111 项 Muse 协议测试、CI 及 Xcode。测试覆盖同成员会话复用、不同成员各自建会话，以及没有匹配结束事件时拒绝把进度文字作完整回复。IPA 已下载校验，真机连续性和长任务仍为 `UNVERIFIED`。[下载和复测](../acceptance/iphone-acceptance.md#房间成员会话连续性)

R1 单账号房间的 iPhone 首轮在打开房间面板时阻断。用户截图只有面板标题、说明和关闭。代码核对发现 IPC 直接返回房间归档结构，派生模板目录被序列化跳过。前端收到数据后访问缺失的模板并在渲染时出错。改为在列表、修改和真实派发的 IPC 返回中提供完整模板目录，保持归档结构；首次读取失败时显示错误及重试。R1 尚未进行派发、回复回收与定稿。[R1 验收](../acceptance/iphone-acceptance.md#muse-单账号房间和指纹档案)

修复候选 `36a172e` 为 `CI_PASSED`。95 项本地 Rust 测试通过。320×568 与 390×844 的 WebKit 模拟 IPC 验证旧归档缺字段会显示错误，重试可新建房间，并测试从模拟派发到手动定稿。实际 iPhone 房间入口与真实发收仍为 `UNVERIFIED`。已下载并核验新版 IPA。[下载及 R1 步骤](../acceptance/iphone-acceptance.md#muse-单账号房间和指纹档案)

`e598f3c` 补齐全量备份的一次合并或覆盖。先核对账号及房间两份预览，再一并恢复账号、代理元数据、房间、附件和模板。房间写入后以账号持久化标记提交，进程中断后保留已提交结果或回滚房间。另补齐 v2.2 房间新话题入口。新话题使用旧模板版本，保留手选范围及用户指定的固定角色，自动模式重新计算其余分派。用户可主动选入旧定稿，新话题从独立会话和发送台账开始。主 crate 91 项、Muse native 110 项、嵌入脚本 13 项、前端构建和两种移动视口 WebKit 回归通过。真机为 `UNVERIFIED`。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45774086898) · [iPhone 复测](../acceptance/iphone-acceptance.md#全量恢复与房间新话题)

同一 SHA 的 iOS 构建成功，本机下载后核验 ZIP 和 IPA 完整性、Spike Bundle ID、iOS 最低版本及 SHA-256。新包仍待真机执行联合验收。[iOS 构建](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45774090087) · [下载与验收步骤](../acceptance/iphone-acceptance.md#全量恢复与房间新话题)

本批补齐 v4 房间模板的执行、讨论席位及可选阶段。手选、自动分派、换人和备份共用模板校验。指挥与上传各一人，审核启用时仍须一名独立审核者。编辑模板回填旧值并创建新版本，已有房间固定原版本。房间正文按派发顺序显示角色、成员与时间，保留可展开的来源和摘要。iOS 通知审批逐项读取并在处理返回后出队，重启仍由审批台账阻止重复写入。[联合复测](../acceptance/iphone-acceptance.md#自定义角色模板房间消息与通知审批)

联合候选提交 `f43cf9c` 为 `CI_PASSED`。92 项主 crate、110 项 Muse native、13 项嵌入脚本、前端构建及两个移动视口 WebKit 流程通过。iOS Rust target、Swift 和 Xcode 构建通过。下载后校验了 ZIP、IPA、Bundle ID 和最低 iOS 版本，真机为 `UNVERIFIED`。[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45801370345) · [iOS 构建](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45801405405)

Muse 红点的设备回执有延迟或不亮症状，暂无可定位的完整分段计时。移动 WebKit 用模拟快照验证了收到状态后渲染头像以及计时关联，不能说明设备上游投递和补查的耗时。此次保持监测行为。VM 续租、CVM、长后台实时通知仍缺服务端或系统可观测证据，前台补查已实现。Hark 需要同账号的真实隔离回执解锁指纹，再验证三个有效样本。私有提交核验和裸 Bearer 登录兑换取决于授权或服务契约，临时分享的口令分通道交互仍有产品裁决。Phase 5 可选项另行排期。

上游 `v0.2.0` 对比 `v0.1.10` 新增跨设备加密同步、二维码、局域网与 Cloudflare 中转，以及可配置代理保活。同步载荷新增账号 Cookie，涉及既有备份范围、登录隔离与中转信任边界，列入 v2.2 的独立评估。上游保活参数不提供本分支在 iOS 挂起期间运行的验证证据。[上游差异](https://github.com/ldjx7/multi-agent-account-manager/compare/v0.1.10...v0.2.0)

### 上游 v0.2.0 的适配范围

| 切片 | 处理与边界 |
| --- | --- |
| 同一 Wi-Fi 与二维码 | 独立配对协议绑定两台 iPhone 当前的私有 IPv4 Wi-Fi 子网。两分钟内一次接收，支持相机扫码与手动填写。配对码包含临时地址和独立随机密钥，备份口令另行传达。挑战应答认证接收方，传输层密文经认证加密后再走原加密备份的口令、预览和显式恢复。传送账号资料、分组、时区及去凭据代理端点。需要代理凭据的新增账号先保持阻断。 |
| 同出口与额度同步 | 本机可选同平台同出口仅保留最近页面，按明确直连或代理主机和端口分组，配置未知仍按 fail-closed 处理。仅释放旧页面，重开按原 UUID 和凭据配置加载。前台自动额度间隔可选 5、15、30 分钟或沿用前台事件，进入后台后依赖系统生命周期，手动同步仍可用。 |
| 云中转 | 上游以 Cloudflare Worker 和 Durable Object 暂存密文，通过领取、删除凭证和到期任务释放。纳入产品前需确定中转归属、服务可观测的元数据、到期和删除核验、二维码转发风险及离线替代路径。保留独立议题。 |
| Cookie 跨设备 | 上游同步载荷带账号 Cookie。纳入产品前需分别验证用户授权、域与账号归属、导入失败回滚、过期/会话撤销及目标设备代理的绑定和阻断。当前局域网使用现有账号资料备份 schema，Keychain 凭据和 WebKit 登录态留在原设备。 |

局域网、摄像头权限、实际两机接收与出口释放为真机 `UNVERIFIED`。[iPhone 复测](../acceptance/iphone-acceptance.md#局域网加密备份和同出口保留策略) · [Apple 局域网权限](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)

`bc85566` 已通过 94 项主 crate 测试、前端及嵌入脚本测试。TCP 回环覆盖错误配对密钥拒绝和篡改密文拒绝。320×568、390×844 的 WebKit 用模拟 IPC 核对扫码、显式接收、未自动恢复及本机代理偏好。Quick checks 和 Xcode 打包皆通过。已下载并核对归档哈希、IPA、Bundle ID 和权限声明。[CI](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45810614186) · [iOS 构建](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45810616659) · [下载和步骤](../acceptance/iphone-acceptance.md#局域网加密备份和同出口保留策略)

本轮收尾范围包括代理池账号持久引用与一对一占用、订阅变更提醒和元数据恢复；账号资料的 iOS 目录及文件保护检查；Muse 的前台 Live/Watch/Idle 监听窗口、掉线退避和系统暂停；以及 Hark 手动和按账号启用的自动指纹检测。Hark 只有在同账号的隔离发送探针取得完整、同会话和同回执的回复并持久落账后才解锁指纹发送。探针严格串行，结果不明、取消、未完成或无效样本均停止后续发送。新增带服务、真实 Cookie 名称和来源域名的会话 Token 导入，走同一预览和写入验证链。

`c285088` 为联合验收候选，`CI_PASSED`。主 crate 89 项、嵌入脚本 13 项、前端和移动 WebKit 回归通过。Hark HTTP 回环核对一发、主会话不发、串会话回执不确认为成功，以及完整关联回复解锁。订阅认证参数变化保留原账号配置，备份保存非敏感绑定元数据。iOS 构建通过，已核验 IPA、ZIP 与最低版本。设备能力和服务端实际行为仍为 `UNVERIFIED`。[CI](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45770836204) · [iOS 构建](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45770837066)

仍需要外部证据或产品裁决的事项为 Hark 真实隔离回执及三样本评分、Muse 红点分段真机回执、VM 到期与续租字段、实测 CVM 和锁屏/长后台能力、代理实际出口、审批通知真机样本和备份恢复。裸 Bearer Token 到网页登录的转换缺少服务商支持的兑换契约。临时分享备份的口令分通道交互和 Phase 5 可选项目仍按已定范围裁决。私有代码提交的联机验证需要授权的访问机制，现有公开提交核验与本机文件摘要可用。[验收步骤](../acceptance/iphone-acceptance.md#收尾验证代理引用文件保护与-hark-指纹)

本批加入文件与可核验公开提交凭证、HTTPS 链接记录、长正文 Markdown 附件和 Muse 原生同请求附件派发。自定义模板版本冻结已有房间，可修改阶段顺序及适用角色人数，审核门保留。已有会话经各成员账号原生列表核验后绑定为快捷入口。Hark Spike 新建并核对隔离项目，以持久台账防重，回执仅在证据关联后确认。Muse 通知提供服务允许的「允许一次」「拒绝」，打开 App 后复用审批复查与台账。全量和自定义备份可附房间、模板与附件，账号/代理及房间分别原子恢复；恢复的启用代理缺凭据时阻断。本批还需真机核对协议附件、Hark 回执、通知动作和恢复行为。Hark 三样本评分继续依赖隔离发送真机证据。[联合步骤](../acceptance/iphone-acceptance.md#交付凭证hark-隔离发送与备份)

`38b8286` 联合候选 `CI_PASSED`。本地 87 项主 crate、110 项 muse-native、13 项嵌入脚本及前端构建通过。移动 WebKit 两种视口验证了文件选择、分享命令与绑定会话的账号参数。Hark HTTP 回环覆盖错会话禁止发送和串会话 200 回执进入 UNKNOWN；代理回归覆盖端点协议保留、同端点凭据保留、不同端点待重新配置。已下载核验 IPA、ZIP 和 Spike 包信息，真机为 `UNVERIFIED`。[CI](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45763568829) · [构建](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45763569820)

P4-D 接入同一额度窗口的自动分派与发送前失败换人，手选可在选定成员内自动分配空缺角色，固定角色保持原分工。自动扩展先考虑房间外符合条件的账号，允许不影响后续步骤与独立审核的房间内轮转。额度未知、过期或用尽不会列为可用；额度排序相同时，手动模型或近期可信度较高的指纹结果提供辅助信息。任何发送结果不明仍按原会话核对。本机记录增加结构化简报、主动选择和推荐确认的定稿引用、原始记录可查和删除。Muse 指纹设置支持手动模型及账号级前台自动重测，关闭后取消该账号正在运行的自动探针。下次到期以最近一次记录或已登记自动尝试计时，自动探针提前登记，重启不会重复触发。多账号实际代理、可比较额度、自动换人的真机验证独立记录。[联合验收步骤](../acceptance/iphone-acceptance.md#muse-自动分派简报与指纹重测)

`ce89f14` 联合候选 `CI_PASSED`。Windows 主 crate 83 项、嵌入脚本 13 项通过。两种移动端 WebKit 视口通过分派依据、推荐引用需勾选、指定账号设置保存和到期只触发一次的模拟验证。已核对同提交的 Xcode 包、ZIP/IPA 校验值、Spike Bundle ID 与最低 iOS 版本。真实自动换人、定稿复用及自动探针仍待 iPhone 验收。[CI](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45727104322) · [候选和记录](../acceptance/iphone-acceptance.md#muse-自动分派简报与指纹重测)

P4-C 多账号按 v4 接通代码开发、头脑风暴、评审、群发对比的真实顺序接力。独立审核使用当前请求绑定的结构化决定；只有已通过审核的产出进入有审核模板的整理阶段。审核打回回流执行，已用轮次不重置。用户补充要求在已发请求入账后暂停。手选成员或固定角色变更使用两步提议、确认，并保留原始分工。轮次或三次打回先到即停止，保留可核对记录。交付范围是可溯源文字，P4-D 自动选人和自动扩展继续独立推进。[多账号联合真机步骤](../acceptance/iphone-acceptance.md#muse-多账号接力与独立审核)

`9c5ed9c` 联合候选 `CI_PASSED`。Windows 主 crate 80 项、muse-native 110 项、嵌入脚本 13 项通过，Noise 回环核对停止请求仅指向本轮隔离会话。两个手机尺寸的 WebKit 仿真通过多人评审和确认分工、定稿，并回归单账号房间与指定账号指纹档案。macOS iOS target 与 Xcode 通过，已核验 ZIP、IPA、Spike Bundle ID 和最低 iOS 版本。多账号真实派发、审核及返工、端到端停止均为 `UNVERIFIED`。停止请求应答只表示远端收到请求。单账号房间、指纹档案和红点仍等待用户回传。[CI](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45720907901) · [候选包和验收](../acceptance/iphone-acceptance.md#muse-多账号接力与独立审核)

`4884cee` 交付 P4-B 单账号文字闭环与 P4-E 的 Muse 手动检测及账号指纹档案。语义沿用自有 v4 §3.1–3.11、v2.1 和已裁决的固定角色、轮次及定稿规则。新增「单账号任务」入口，四个正式场景继续按其角色规则工作。使用同一账号真实建立隔离会话、订阅就绪后发送一次、独立落账 ACK、优先流式回收，必要时按同会话和 ACK 只读回查。只有用户按钮推进定稿。会话、发送和回收来源可核对。当前交付类型是文字，多账号接力、独立审核、真实外部产物验证、自动指纹及模型选择属于后续工作。[联合真机步骤](../acceptance/iphone-acceptance.md#muse-单账号房间和指纹档案)

联合候选 `CI_PASSED`。两种手机尺寸的本机 WebKit 仿真验证了派发入口、确认定稿和指定账号指纹档案；macOS Quick checks、iOS Rust target、Swift 编译及 Xcode IPA 构建通过。已核验 ZIP、IPA、Spike Bundle ID 和最低 iOS 版本。真机仍为 `UNVERIFIED`，等待单账号隔离发收、定稿及重启后的档案回执。Muse 红点仍等待「红点计时」真机回执。[CI](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45711886220) · [iOS 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45711886482)

### 协作房间之外的剩余范围

| 状态 | 剩余项 |
| --- | --- |
| 代码已落地、待实际验收 | Muse 模型修正和自动重测、Hark 隔离发送与指纹门禁、Muse 通知快捷操作、代理池账号引用与订阅变更、账号资料文件保护、房间和代理加密备份。 |
| 需协议或产品裁决 | 裸 Bearer Token 导入网页登录、临时分享的密钥通道与到期交互、私有仓库提交核验。 |
| 实际故障待定位 | Muse 回前台头像红点不够及时。分开度量 Swift 前台进入、Rust 启动、网关准备、协议请求、持久化发布、前端接收及 DOM 更新。 |
| 已有代码，待真机或故障注入 | Hark 主会话断流、轮询去重、卡片、网页审批及会话失效；新增账号 Cookie、跨账号隔离、写入失败回滚、重启和剪贴板到期；真实 Muse 审批（允许一次、拒绝、限定范围持久授权、到期、错账号、结果未知）；本地通知与权限拒绝；代理订阅、换网/IPv6 出口故障对照、账号隔离、额度写回及跨额度窗口、跨账号会话和备份实际恢复。 |
| 技术门禁待观察 | Muse VM 到期与续租、CVM 能力范围；发送丢 ACK、强退恢复、取消与占用门禁；普通会话订阅断流和重连；锁屏低电量等当下实测。长期挂起推送需要已验证的系统投递路径。静音音频实验维持独立，不用于判定产品可持续通知。 |
| 可选或未排期 | Cron、Goals、Memories、Skills 只读入口；Phase 5 分享扩展、Shortcuts、原生只读消息预览、Hark 参与房间、多房间并行和跨设备房间备份。 |

历史候选 `886d615` 加入 Muse 红点分段计时与协作房间本地建房、模板、角色验证、状态记录和暂停/继续/停止。该候选只覆盖本机房间流程。

`886d615` 通过 Quick checks 与 Xcode 打包，已下载校验 IPA。本地房间测试覆盖角色冲突、活跃房间互斥、轮次/打回门禁、重复事件与持久化恢复。移动 WebKit 两个视口验证建房准备和红点计时关联。延迟根因等设备中的分段数据。[下载、校验值和真机步骤](../acceptance/iphone-acceptance.md#muse-红点分段计时与房间草稿)

`258e428` 延迟红点真机复测仍未达到预期。用户确认先交付新增账号 Cookie 与 Hark 断流补同步。候选 `fcd1292` 在 iOS 新增账号内预览并核验写入独立资料库，写入未核对时不创建账号，持久化清理记录用于中断恢复。已登录 Hark 账号从任务页手动启动同一主会话的 REST/SSE 与只读轮询补查，共用回复 ID 去重，协议内的总结、文档、代码、文本模板及表格可展开文本。运行中无消息发送。客户端回环验证断流后补回完整回复、拒绝错会话、保持较新的 SSE 正文。移动 WebKit 验证 Cookie 失败可重试、Hark 正文与卡片展开。仍待真机。[联合验收步骤](../acceptance/iphone-acceptance.md#新增账号-cookie-与-hark-回复补同步)

`fcd1292` 的 macOS CI、iOS Swift/Rust 编译和 Xcode 打包通过，已核对下载产物完整性及解压 IPA SHA-256。主 crate 本地 71 项通过。新增账号真机写入、Hark 实际断流及卡片返回字段仍待验收。[下载与校验值](../acceptance/iphone-acceptance.md#新增账号-cookie-与-hark-回复补同步)

`7ed2ab5` 的 Cookie 回贴预览与登录恢复通过，导出 20 条、恢复后 19 条的阶段差异未定位。回前台红点复测仍失败，另见一次无法复现的短暂红点。现有实现进入账号即清除本地红点，且上轮会话列表只看可选 `unread_count`，没有对该次回复的确定证据。

候选 `258e428` 在 iOS 回前台时复用此前真机可用的 Muse Noise 历史读取。要求停驻会话有可核对的标识，且协议历史出现晚于停驻时间的同会话已完成助手消息。按账号和回复 ID 去重，未读计数仍可作补充。红点清除延后到账号页面成功提交。网页通知与本次原生历史信号的实际一致性需真机验证。已接入上游城市时区选择。[真机步骤](../acceptance/iphone-acceptance.md#muse-协议补查与城市时区) · [上游取舍](development-plan-v2.2.md#上游-v0110-的二次核对)

同一 SHA 的 Quick checks、Xcode 和 IPA 完整性核验通过。主 crate 71 项测试通过，含协议历史解析的错会话、旧回复、未完成和非公开反例。两种移动 WebKit 视口验证预设和自定义时区保存后重开。协议补查和红点显示待 iPhone 复测。[下载候选](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45673505743/artifacts/15050354887)

`9a7c229` 的后续真机回执确认任务入口、原生额度更新、会话和分组通过。生成中回账号列表后切后台，回前台 20 秒仍无账号红点，网页提示可见。App 自身导出的 Cookie 回贴后预览失败。订阅因无测试地址待验。候选 `7ed2ab5` 用同一导入解析器筛选并复查剪贴板导出内容，显示未复制条数。iOS 回前台事件触发近期停驻 Muse 账号的已知服务端未读读取，保持账号代理、读取结果去重和 DOM 补查。任务页显示本次补查结果，实际返回字段与头像红点仍需复测。[专项复测](../acceptance/iphone-acceptance.md#cookie-回贴与-muse-回前台红点专项)

`7ed2ab5` 已通过 macOS CI 与 Xcode 打包。主 crate 70 项本地测试包括 Cookie 导出回贴契约及未读去重。移动端 WebKit 验证跳过条数、账号红点及补查状态显示。已下载并核验 IPA，未读字段和 Cookie 实际写入继续按真机验收。[验收包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45663105641/artifacts/15046817245)

2026-11-26 首轮 A–C iPhone 回执。`815cffa` 的任务入口闪退和 Muse 会话列表为 `FAIL`。本机 Cookie 枚举结果在打开会话前后不一致，预览输入区闪烁。回前台未看到叠号红点，Muse 网页点可见。手动代理添加与使用、审批空态、备份导出为本次 `DEVICE_VERIFIED`。订阅导入、实际审批和备份恢复继续待验。

修复候选 `9a7c229`。核对真机日志和原 IPA Mach-O 的 UUID 与故障指令，确认任务入口触及主线程栈保护页，已将原生网络 future 移至 Tokio worker 并拆分装箱。会话列表改为按账号出站策略进行的 Noise daemon 请求。Cookie 摘要复用停驻页的 WebKit 数据存储并保持其生命周期，前端原位更新预览。Muse 回前台补查增加停驻基线及同会话助手消息变化判断。Quick checks 和 Xcode 均成功，IPA 已下载核验。两个移动端 WebKit 视口验证 Cookie 预览保留输入框节点。新包仍需真机比对。[下载和逐步复测](../acceptance/iphone-acceptance.md#v22-真机回归修复复测)

按联合验收清单收集 A–C 的真机证据。有授权的 Hark 账号及真实待审批项时同步验收。房间与指纹产品化按 [执行计划 v2.2](development-plan-v2.2.md)进入后续批次。以下保留历史验证时间线。

实时流式专项包 `8f50c91` 的 CI 与 Xcode 打包通过。本次 iPhone 专项实测中，实时增量与流内结束验收均未通过。三条均为零关联文本增量，最终由历史恢复完成。下一轮按发送流和订阅流分别记录匹配前的事件类型计数，以区分未观测到文本事件与关联失败。[下载与步骤](../acceptance/iphone-acceptance.md#实时增量专项复测)

诊断构建 `d983d52` 的 CI 与 Xcode 打包均通过。iPhone 回报三条均经 `history_completed` 入样，发送流和订阅流均未报告文本增量或结束事件。BotArk 桌面端 02:03 的手测可见文字逐步出现，当时的日志过滤未保留足够帧证据，尚无法确认两端收到的事件差异。[构建](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45431942405)

`MUSE_WIRE_TRACE` 在解密并识别传输帧后、分流前记录。字段依次是本地时间戳（Unix 毫秒）、连接序号、记录序号、RPC 方法、连接内流 ID、帧类型、分流匹配、正文长度、正文是否封尾、HTTP 状态或 reset 码、白名单事件名。未知事件显示 `other`，超过采集上限有明确标记。

当前实时取证只需要在叠号完成一次真机复测。`5d8bf46` 的 Spike IPA 已通过 Xcode 构建，诊断代码 `9e73c5e` 的 Quick checks 已通过。等待叠号前台探针的脱敏帧记录，实时接收仍为 `UNVERIFIED`。[下载与步骤](../acceptance/iphone-acceptance.md#实时增量专项复测)

`5d8bf46` 的 iPhone 回报三条均为 `history_completed`，关联文本增量为 0。界面出现「解密后流诊断」标题，长结果无法滚到底部，未取得独立帧日志。新版 `7a8dd54` 改从 Muse 顶栏进入独立诊断页，复制摘要和流日志固定在顶栏，其他测试按用途折叠。WebKit 与 Edge 在 320×568、390×844 两个视口通过了 1,500 行日志滚动和一键复制检查，Edge 另验证触控拖动。Quick checks 和 Xcode 打包通过，长内容滚动、复制和实时接收待 iPhone 验证。[下载与步骤](../acceptance/iphone-acceptance.md#实时增量专项复测)

`7a8dd54` 真机已通过顶部复制，收到完整 41 条脱敏帧记录。`chat.subscribe` 流 4 返回 200，收到 `agent.status` 和归类为 `other` 的正文帧。三次 `chat.stream`（流 5、6、7）均返回 200，ACK 后封尾。解密后、分流前未见标准正文增量或结束事件，也未见未路由帧。3 条均走历史恢复并有效入样，实时接收本轮为 `FAIL`。`other` 已脱敏，单凭旧日志无法还原事件名或证明服务端完全没有发送正文。叠号现有指纹链路发送空 `node_id` 且未注册连接能力，也未按已创建的会话订阅。对照客户端记录了隔离会话推送需要注册节点、注册同标识的连接能力，以及按会话订阅；该客户端主聊天曾验证增量，首条隔离回复仍曾丢失。叠号需在不改变发送台账和隔离要求的前提下单独验证这条路由假设。现有包无需复跑。[对照源码记录](https://github.com/manishkumar1601/muse-ai-api/blob/5935492/docs/wire-protocol.md)

已获授权，仅在叠号隔离指纹探针使用每次运行新生成的临时标识。同一条原生连接先注册节点和推送能力，再订阅已验证的新隔离会话，等订阅就绪后才用同一标识发送。注册或订阅出错时停止，保持发送台账、代理策略和逐条采样规则。加密回环测试验证了请求顺序、标识和会话一致、注册被拒绝时未订阅或发送。界面在首次收到关联增量时报告进度。`7ae68db` 已通过 Quick checks 与 Xcode 打包，服务端实际投递和流内完成仍需 iPhone 验证。[下载与步骤](../acceptance/iphone-acceptance.md#实时增量专项复测)

`7ae68db` 的 16:14 iPhone 复测在 `node_registration` 收到 HTTP 400，确认登记失败时停止发送（网络写入 0/3）。该包只显示 HTTP 状态，未保留拒绝字段。公开对照实现的节点注册请求同时存在平台和命令清单差异，且未检查该请求是否返回成功，不能据此确定出错字段。下一包从拒绝体提取已知错误码、字段名和原因，不输出原文、具体参数值或凭据。修正请求前需取得真机拒绝原因，实时接收仍未验证。[对照请求](https://github.com/VKirill/muse-ai-connector/blob/2fb28fdd744ac12597726960f13bf1ac753b5283/bridge/protocol/api.mjs#L149)

`775a49b` 的 16:33 iPhone 复测在设备节点注册阶段返回 `HTTP 400 · invalid_params`，未指出具体字段，探针发送 0 条。另一对照客户端的实测流式路径仅要求 `/client/register-capabilities` 成功，不依赖 `/api/nodes/register` 的响应。下一候选在叠号隔离探针只发起推送能力注册，沿用同一临时标识、既有会话隔离和订阅就绪门禁。能力注册或订阅失败仍在发送前停止。隔离会话是否能获得实时增量和结束事件仍待 iPhone 确认。[对照实现](https://github.com/vip0014/muse2api/blob/0a2cfffb1fa6dc706398781fb3a75621f8494750/internal/muse/protocol/hatch/chat.go#L212)

候选 `2066ccb` 的 Quick checks 与 Xcode 打包均已通过，等待一轮真机验证。[下载与步骤](../acceptance/iphone-acceptance.md#实时增量专项复测)

`2066ccb` 真机通过推送能力注册、隔离订阅和两次 ACK。第二轮读取到两个增量批次。72 个增量在第二轮 ACK 和匹配用户事件之前到达，75 个在匹配用户事件之后集中到达，后批从助手开始到消息结束约 6 ms。采集器报告 0 个关联增量和 149 个未关联事件，用户手动停止，第三条未发送。帧记录没有关联标识，不能确认每个批次的归属，也不能凭同批到达证明生成中逐段投递。修正使用隔离会话订阅和本轮 ACK 对应的用户事件识别后续稀疏的助手帧，排除本轮前已出现的助手标识和显式冲突。规范的空闲信号可早于消息结束，仍需完整结束和稳定窗口。新增未关联原因与关联增量至结束的时差，运行恢复时也检查实际经过的采集预算。

候选 `11c6993` 的 Quick checks 与 Xcode 打包已通过，待 iPhone 验证当前轮关联、流内结束和生成期间的投递时序。[下载与步骤](../acceptance/iphone-acceptance.md#实时增量专项复测)

`11c6993` 真机确认三次 ACK、三条 `done`、3/3 有效样本和校准评分，流内完成通过。三轮分别收到 80、81、75 个 `delta.text_append`，每轮首末增量间隔为 2,336、2,162、2,037 ms，且结束帧在末个增量之后，确认分时投递到叠号。236 个增量均被采集器的标识冲突规则拒绝，应用层关联与实时进度仍未通过。现有日志未记录具体冲突字段。下一候选对已由本轮绑定的隔离助手消息允许子任务标识不同，继续阻断明确的异会话或异用户指向。按传输序号去除重复增量，并分别报告会话、回复指向、轮次或任务及父消息冲突。

候选 `07cf1fa` 的 Quick checks 与 Xcode 打包均通过，等真机验证已绑定助手消息的关联增量。[下载与步骤](../acceptance/iphone-acceptance.md#实时增量专项复测)

`07cf1fa` 真机拿到 3/3 有效样本并完成校准评分。第二、三条分别关联 73、76 个增量，首个关联增量至消息结束为 2,392、3,418 ms，均由流确认 `done`。第一条 `history_completed`。从第一条 ACK 到第二条发送间隔约 7 分 28 秒，期间第一条显示 0 次读超时，后续在第二条 ACK 之前集中读取 72 个被排除的增量。“帧何时被读取”有证据，iOS 是否在这段时间锁屏或挂起没有证据。三条全量实时验收仍未通过。下一候选仅在 iOS 指纹探针任务内保持亮屏，结束、取消或退到后台时恢复原自动锁屏设置，后台中断和总时限通过任务专属令牌停止后续发送。

`f1e90ec` 的 CI 和打包均因 Swift 通知通道缺少抛错处理失败。修复提交 `8b7eb0a` 的 CI 和 iOS 打包均通过，已下载并校验 Spike IPA。三条实时增量和流内结束仍待同一账号和代理的 iPhone 复测。[下载与步骤](../acceptance/iphone-acceptance.md#实时增量专项复测)

`8b7eb0a` 已完成 iPhone 前台专项验收。三条订阅增量分时到达，均为 `done`，无解析错误和未关联事件，3/3 有效样本完成校准评分。[真机验收证据](../acceptance/iphone-acceptance.md#实时增量专项复测)

2026-11-25（UTC+8）BotArk 隔离会话对照。14:29 开启诊断时，`chat.stream` 返回 200，`chat.subscribe` 在解密后记录到正文块，白名单内的文本增量与结束事件未出现。网页已有回复。14:37 关闭诊断并重启后，同一会话再次出现网页已回复而 BotArk 无显示。切离后重新选中该会话，BotArk 显示了 14:37 的回复，确认历史读取和显示成功。BotArk 本地测试复现了一个恢复缺口：发送获 ACK 且历史已有完整回复，订阅保持连接但无可用事件时，直接聊天一直等到超时且未查询历史。该测试解释了为何客户端可持续空白，线上事件投递或格式仍待取证。

0.1.25 的 Muse 真机指纹采集记录为 `FAIL`。两次各确认发送 1 条，网页可见回复，采集均超时且样本为 0。两次均停止后续发送。

`8b7eb0a` 的 18:52–18:56 真机对照中，HTTP CONNECT、HTTPS CONNECT、SOCKS5 的连通性均返回 200，原生验证均在控制面 `lease-vm` 返回 HTTP 400 `Invalid vmType`，尚未进入 WS 握手。旧实现最终只显示最后一次租约尝试的错误，第一种 VM 类型的具体响应未捕获。修复版本在该拒绝后沿用同一出站客户端查询账号已分配的 VM，继续使用现有 token 与 Noise 流程。

`7588dca` 于 19:18–19:23（UTC+8）通过三种原生代理真机端到端，19:31 完成 SOCKS5 重启持久化复测。错误代理密码下原生探针无法连接、Muse WebView 无法加载，恢复正确密码后网页重新载入。后台额度同步的下一步是补充按账号脱敏的下发、联网和应用结果，供故障对照验收。已修正后台同步忽略代理下发结果的问题，只有下发成功的账号才交给 Swift 同步。该改动尚无真机证据。VM 新分配与续期仍待验收。[真机验收](../acceptance/iphone-acceptance.md#原生代理与重启回归)

`dda5127` 的后台同步诊断按账号分别报告代理下发、隐藏 WebView 主页面联网和额度应用结果。测试入口重载页面并排除旧额度缓存，代理下发或应用失败时跳过该账号联网。Windows 主 crate 61 项测试、13 项嵌入脚本测试、[Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45608432316) 和 [iOS Xcode 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45608434585) 均通过，IPA 已下载并校验。下一步用同一 iPhone 测试账号做正确、错误、恢复正确三轮代理密码对照，回填账号尾号及三段状态。[下载与步骤](../acceptance/iphone-acceptance.md#ios-后台额度同步)

`dda5127` iPhone 真机用同一账号测试启用及明确清除 SOCKS5。隐藏 WebView 两轮均超时，后台额度正向验收为 `FAIL`。原生额度经代理冷发现 GET 失败，匿名 HEAD 为 200，清除代理后原生额度成功。已在候选修复中让隐藏 WebView 使用前台窗口中的不可交互渲染视口，从主文档提交开始独立探测额度。原生路径补充脱敏的请求或响应体错误分类。后续需验证主文档是否提交、额度是否更新，再做错误密码及恢复正确密码的对照。[真机回执](../acceptance/iphone-acceptance.md#dda5127-真机回执)

`ab6f6fa` 的 [Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45613514999) 和 [Xcode 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45613523633) 均通过，已下载校验新 IPA。下一次真机先确认清除代理时后台额度路径能到达主文档提交及实际额度，再用正确 SOCKS5 重跑同一入口；若原生查询仍失败，回填新诊断。新构建实际结果 `UNVERIFIED`。[复测包和步骤](../acceptance/iphone-acceptance.md#ios-后台额度同步)

`ab6f6fa` 的 Muse 后台额度同步已完成同一账号的直连、错误 SOCKS5 密码和恢复正确密码对照。错误密码下主文档提交前失败且未取得额度。恢复正确密码后代理已下发，主文档已提交，额度已取得且数据未变。用户补充确认正确 SOCKS5 密码下的原生「查询额度」也成功。独立网络出口、跨账号隔离与数值变化写入继续保留待验收。[真机回执](../acceptance/iphone-acceptance.md#ab6f6fa-真机回执)

本批转入 VM 租约、后台连接与账号组织。租约探针区分已有 VM 分配和明确租约，去掉原始 VM 名称和网关标识，复查不同 VM 时不推断为续期。订阅就绪后才开始计算存活窗口，租约、存活、订阅和单发沿用启动账号与任务 ID 的停止操作，发送仍按实际网络写入保留未知状态。Muse/Hark 账号支持分组与搜索，旧档默认为未分组。Phase 1 的通知和 watcher 产品能力、Phase 2 的审批及代理池、后续备份和协作房间仍按执行计划保留。[集中复测入口](../acceptance/iphone-acceptance.md#vm-租约后台连接与账号组织)

`d32511e` 的 [Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45619968807) 已通过。Windows 测试为主 crate 62 项、muse-native 109 项及前端脚本 13 项。WebKit 和 Edge 在 320×568、390×844 验证了账号搜索、分组筛选、编辑、清空及搜索焦点。WebKit 另模拟切换展示账号后停止存活探针，核对取消请求仍使用启动时的账号和任务 ID。设备端的真实租约、连接存活、操作取消与账号组织回归仍需 iPhone 核对。

`d32511e` 的 [Xcode 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45620072467/attempts/2) 重新运行后通过。首轮 runner 解析 GitHub 域名失败，SwiftPM 无法下载依赖。第二轮沿用同一功能 SHA，已下载并校验 IPA。GitHub 确认 [PR #15](https://github.com/harrylarry/multi-agent-account-manager/pull/15) 已合入，PR #16 继续承载当前 Spike。下一次真机集中核对租约到期字段、锁屏/后台观测间隔、取消及分组持久化。有 Hark 测试账号或真实待审批样本时，可在同一包核对已有只读路径。

`d32511e` 获得 iPhone 回执。Noise、Ping、订阅和单次发送 ACK 回归通过。两次 standard 租约都返回 assigned，到期与 TTL 缺失。后台窗口结束时有 475 秒观测空窗，不能据此确认远端连接持续或断开。另开一轮确认手动停止为 cancelled。账号分组持久化、Hark 及真实待审批样本仍待验收。[真机回执](../acceptance/iphone-acceptance.md#d32511e-真机回执utc8)

该轮后台实时监测验收为 `FAIL`。475 秒实测空窗确认该场景不能依赖常驻 WSS 提供实时提醒。后续产品路径采用回前台补查和现有红点。本批在 iOS 恢复 active 时立即与延迟复查停驻 Muse 页面，丢弃生命周期切换前发起的旧检查。iOS 账号列表加入 Muse 只读审批，服务端空列表与无法判定分别显示。原生审批决策继续等可核对的写协议及实际待审批样本。

`bda84e7` 已通过 [Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45631020421) 和 [iOS Xcode 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45631028431)，IPA 已下载校验。主 crate 62 项、muse-native 109 项和嵌入 JS 13 项本地测试通过。WebKit 两个移动端视口用模拟响应验证了审批详情、过期、账号隔离、空列表和未知响应。两项产品功能仍需 iPhone 复测。

进入 v2.2 批次 A。iOS 账号列表加入 Cookie 三格式预览、范围校验、写入核验与失败回滚、本机限时剪贴板导出，以及写入后打开原账号验证登录。审批、回前台补查及分组持久化纳入同一候选包。会话、代理池、任务与加密备份沿 v2.2 继续。

`529564c` 登录恢复首包已通过 [Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45635918943) 和 [Xcode](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45635923405)，已下载校验。WebKit 使用模拟 IPC 验证了预览失效、导出确认及指定账号绑定。`aabd2e5` 新增 Muse 会话列表和经该账号 WebView 打开指定线程，本地 64 项 Rust 测试、前端构建和移动视口模拟交互通过，CI 与联合包待完成。代理池、产品级任务事件与通知、持久会话绑定、Hark 与加密备份仍在 v2.2 开发范围内。[验收步骤](../acceptance/iphone-acceptance.md#ios-账号登录恢复联合验收)
