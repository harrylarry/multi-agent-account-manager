# 当前开发进度

2026-11-24 接续快照。任务状态和原始证据以 [PR #16 接续记录](https://github.com/harrylarry/multi-agent-account-manager/pull/16#issuecomment-7130670517)、[Issue #13](https://github.com/harrylarry/multi-agent-account-manager/issues/13) 和 [Issue #14](https://github.com/harrylarry/multi-agent-account-manager/issues/14) 为准。范围与验收门禁见[开发计划 v2.1](../叠号开发计划-严谨执行版-v2.1-2026-11-23.md)与[开发决策记录](../开发决策记录.md)。

## Phase 0 技术 Spike

开发分支为 `spike/native-methods`，本快照覆盖的功能提交为 `034415b41250c36bbb5b72a721a2735c07367659`。当前 0.1.26 Spike IPA 由 `034415b41250c36bbb5b72a721a2735c07367659` 构建，安装与校验见 [iPhone 真机验收](../acceptance/iphone-acceptance.md)。

| 切片 | 已核对证据 | 待验收 |
| --- | --- | --- |
| Muse 认证、订阅、审批、额度 | 旧版真机已有 Noise 握手、Ping、订阅、空审批列表与额度证据，详见 PR #16。 | 新包回归。有待审批项的真实样本继续单独记录。 |
| Muse 隔离发送与模型指纹 | 0.1.26 历史恢复路径 `DEVICE_VERIFIED`。3 次发送均收到 ACK，3 条回复经 `history_completed` 确认完成并参与 lmfpd 评分。 | 实时流式回复仍 `UNVERIFIED`。本轮三条均未观测到文本增量。原正常会话无污染需要网页侧对照。 |
| 原生代理与 WebView | 0.1.25 `CI_PASSED`。受控 reqwest 测试验证 SOCKS5 远端解析及代理拒绝时无直连，重启回归覆盖代理持久化。0.1.24 代理重启场景 `FAIL`。 | SOCKS5 真机端到端、账号切换隔离、WebView 与后台额度同步的代理故障阻断 `UNVERIFIED`。 |
| VM 租约与后台 WSS | 旧版真机已取得租约。历史后台观测出现断连，PR #16 保留原始时间线。 | 租约到期与续租、新包锁屏及回前台恢复 `UNVERIFIED`。 |
| Hark 只读检测 | `8eed8ff` 的 REST/SSE/poll 与两路去重 `CI_PASSED`，已包含在 0.1.26 IPA。 | Hark 真机登录、状态对照、断流回退、WebView 审批 `UNVERIFIED`。Hark 指纹按已确认裁决另做 Spike。 |

0.1.25 的 [Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45389037648)、[Xcode 打包](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45389039398)和 [Hark Quick checks](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45390431182)均有对应提交。真机结果需记录对应包和测试环境。

## 下一步

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

2026-11-25（UTC+8）BotArk 隔离会话对照。14:29 开启诊断时，`chat.stream` 返回 200，`chat.subscribe` 在解密后记录到正文块，白名单内的文本增量与结束事件未出现。网页已有回复。14:37 关闭诊断并重启后，同一会话再次出现网页已回复而 BotArk 无显示。切离后重新选中该会话，BotArk 显示了 14:37 的回复，确认历史读取和显示成功。BotArk 本地测试复现了一个恢复缺口：发送获 ACK 且历史已有完整回复，订阅保持连接但无可用事件时，直接聊天一直等到超时且未查询历史。该测试解释了为何客户端可持续空白，线上事件投递或格式仍待取证。

0.1.25 的 Muse 真机指纹采集记录为 `FAIL`。两次各确认发送 1 条，网页可见回复，采集均超时且样本为 0。两次均停止后续发送。

继续用 0.1.26 验证三协议代理、重启后的故障阻断和 Hark 只读检测。实时流式回复、VM 租约和后台 WSS 各自保留门禁。Phase 0 的 Go/No-Go 以验收记录为准。
