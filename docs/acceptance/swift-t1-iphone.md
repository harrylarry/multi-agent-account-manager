# 叠号 Swift T1 真机验收

## 安装

在 [T1 Issue](https://github.com/harrylarry/multi-agent-account-manager/issues/18) 对应的 Actions 运行中下载 `diehao-swift-t1-ipa`。压缩包内有未签名 IPA 和 SHA-256 文件。用日常验包的签名方式安装「叠号 T1」。构建配置为版本 0.2.0、最低 iOS 26.0、独立 Bundle ID `com.ldjx.accountmanager.swift-t1`。签名时让它和旧叠号并存。

## A 组隔离与建取删

1. 打开「叠号 T1」。确认首页显示「骨架已启动」。自检使用临时 UUID 和本地合成网页。
2. 点「写入并检查 A / B」。等出现「双向隔离通过」。它验证每账号缓存独立 store、测试代理应用与重建、Cookie 和 localStorage 双向隔离。
3. 在 App 切换器划掉「叠号 T1」并重新打开。点「验证重启后隔离」。等出现「A 组通过」。该操作在原进程内会明确要求重启。
4. 点「清理测试资料」。等出现「A 组通过，测试资料已清理」。程序会核对两个 UUID 已从 WebKit 列表移除。

回执给出机型、iOS 版本和三个操作结果。失败时提供失败步骤及界面文字。A 组在这个版本使用合成 Cookie 做对照。真实代理出口与同邮箱双槽的服务端登录态随 T2，用非主力账号核验。

## 迁移边界

真正的同容器升级才看得到旧资料。T1 并行 IPA 使用独立容器，所以此包可验证新注册表和隔离逻辑。同容器的 0.1.26 账号已使用 UUID，注册表直接引用原 identifier。确认是旧单槽数据并能识别 Muse 的默认 store 时，旧首槽引用默认 store。运行时不搬 Cookie。对于已标记需代理的旧账号，代理解析接入前阻断创建 WebView。
