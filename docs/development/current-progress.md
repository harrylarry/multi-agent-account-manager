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

下一轮取证使用同一账号、同一代理和各自新建的隔离会话。两端在解密并识别传输帧后、分流前记录 `MUSE_WIRE_TRACE`。字段依次是本地时间戳（Unix 毫秒）、连接序号、记录序号、RPC 方法、连接内流 ID、帧类型、分流匹配、正文长度、正文是否封尾、HTTP 状态或 reset 码、白名单事件名。未知事件只显示 `other`，超过采集上限有明确标记。先收 BotArk 日志，叠号待诊断 IPA 后在「原生诊断 → 解密后流诊断」复制脱敏结果。收到两端记录前，实时流式回复和根因均为 `UNVERIFIED`。

2026-11-25（UTC+8）BotArk 隔离会话对照。14:29 开启诊断时，`chat.stream` 返回 200，`chat.subscribe` 在解密后记录到正文块，白名单内的文本增量与结束事件未出现。网页已有回复。14:37 关闭诊断并重启后，同一会话再次出现网页已回复而 BotArk 无显示。切离后重新选中该会话，BotArk 显示了 14:37 的回复，确认历史读取和显示成功。BotArk 本地测试复现了一个恢复缺口：发送获 ACK 且历史已有完整回复，订阅保持连接但无可用事件时，直接聊天一直等到超时且未查询历史。该测试解释了为何客户端可持续空白，线上事件投递或格式仍待取证。

0.1.25 的 Muse 真机指纹采集记录为 `FAIL`。两次各确认发送 1 条，网页可见回复，采集均超时且样本为 0。两次均停止后续发送。

继续用 0.1.26 验证三协议代理、重启后的故障阻断和 Hark 只读检测。实时流式回复、VM 租约和后台 WSS 各自保留门禁。Phase 0 的 Go/No-Go 以验收记录为准。
