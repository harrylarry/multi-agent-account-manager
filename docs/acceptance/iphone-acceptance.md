# iPhone 真机验收

当前 Spike 验收包是 **0.1.25**，Bundle ID 为 `com.ldjx.accountmanager.spike`，最低 iOS 17.0。开发状态与待验收范围见[当前进度](../development/current-progress.md)。

## 下载和安装

1. 从 [GitHub Actions 下载 0.1.25 产物](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45389039398/artifacts/14918402522)并解压。构建提交为 `b8ffb6b8219205a2d98b6e933c1e210a01e1b5e7`。可在[构建记录](https://github.com/harrylarry/multi-agent-account-manager/actions/runs/45389039398)核对。
2. 校验解压后的未签名 IPA，预期 SHA-256 为 `9852970df74f1b6d847fbaa4be29d321b761266576b8416768f9467cd743ac50`。使用现有签名和侧载流程安装「叠号 Spike」。
3. 登录非主力 Muse 测试账号。0.1.24 有重启丢失代理配置的问题，代理验证使用 0.1.25。若旧版已丢失设置，先在新版重新保存。

## Muse 指纹与发送

1. 记下测试账号原会话的当前状态。返回管理页，在该账号的原生诊断区运行指纹探针。
2. 核对探针均出现在新建会话，原会话没有新增探针。验证每条完整回复结束后才发下一条。
3. 记录网络写入、发送确认、回复完成、有效样本和实际 lmfpd 评分。通过条件是 **3 个 ACK、3 条完整回复、3 个有效样本和实际评分**。
4. 若中途失败，记录最后阶段、采集结束原因和后续发出条数。连接中断或停止后，核对后续探针未继续发送。ACK 已确认的发送仍应保持确认状态。

## 原生代理与重启回归

1. 用同一代理地址及认证参数，依次配置 HTTP CONNECT、HTTPS CONNECT、SOCKS5。每种先运行「检测代理连通性」，再运行完整的「代理探针」。分别记录匿名 HTTPS 响应、握手、Ping 状态及失败阶段。
2. 启用代理后强退并重开 Spike，核对配置和凭据可用状态仍在，并复跑一次完整探针。条件允许时，用另一个非主力账号验证设置和诊断结果不会串号。
3. 在代理已启用的情况下，使用错误凭据或断开代理。分别检查原生探针、WebView 及可触发的后台额度同步。预期是请求失败并保持阻断。没有代理侧出口日志或受控网络观测时，记录客户端表现并将「无直连」标为 `UNVERIFIED`。

## 回填结果

在 [PR #16 验收记录](https://github.com/harrylarry/multi-agent-account-manager/pull/16#issuecomment-7130670517)关联的 Issue 或 PR 下记录机型、iOS 版本、构建 SHA、场景、结果、预期差异和脱敏诊断。保留各项的 `DEVICE_VERIFIED`、`FAIL`、`UNVERIFIED` 或 `BLOCKED` 状态。回传前移除 Cookie、Token、账号邮箱及代理认证信息。
