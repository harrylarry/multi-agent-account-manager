# 叠号（Multi-Agent Account Manager）

[![LINUX DO](https://img.shields.io/badge/Community-LINUX%20DO-0064e0?style=flat-square)](https://linux.do/)

基于 Tauri 2 构建的跨平台（macOS / Windows / Android / iOS）AI 助手多账号隔离与快速切换工具，安装后应用名称为 **叠号**。

---

## iOS 移植说明

本仓库是 [ldjx7/multi-agent-account-manager](https://github.com/ldjx7/multi-agent-account-manager) 的 iOS 移植 fork：基于 Tauri 2 + WKWebView，在上游 v0.1.5 基础上新增 iOS 支持。

- **当前版本**：0.1.26（PR #16 已于 2026-11-27 合入 main）。
- **IPA 构建**：由 GitHub Actions（`.github/workflows/ios.yml`）在 `macos-15-intel` runner 上构建，产物为未签名 IPA（`account-manager-ios-unsigned.ipa`），无需 Apple 开发者账号。
- **安装方式**：未签名 IPA（iOS 26.0+），需自行签名后侧载安装，详见「iOS 安装注意事项」。
- **上游更新**：按 iOS 需求逐项评估和适配上游变更，排期见[执行计划 v2.2](docs/development/development-plan-v2.2.md)。

---

## 支持服务与核心特性

| 服务 | 状态 | 说明 |
| --- | --- | --- |
| **Muse · Meta** | 已启用 | 独立会话隔离、邮箱识别、额度明细同步 |
| **Hark** | 已启用 | 独立会话隔离、额度与余额同步 |

- **原生级多账号隔离**：macOS 使用独立 UUID `WKWebsiteDataStore`，Windows 使用独立 WebView2 `UserDataFolder`，Android 使用独立 WebView `MULTI_PROFILE`，iOS 使用独立 UUID `WKWebsiteDataStore`。
- **桌面端快捷切号**：支持 `⌘1~9` / `Ctrl+1~9` 以及 `⌘↑`/`⌘↓`/`⌘[`/`⌘]`（Windows 为 `Ctrl`）快速切换账号。
- **应用内检查更新**：支持在客户端内检查 GitHub Releases 最新版本、带进度条下载、`SHA-256` 校验及覆盖更新（iOS 因系统限制无法应用内覆盖更新，检查到新版后提示手动重新侧载）。

---

## 界面预览

### macOS

<p align="center">
  <img src="docs/screenshots/macos-muse.png" alt="macOS Muse 多账号管理与额度同步" width="49%" />
  <img src="docs/screenshots/macos-hark.png" alt="macOS Hark 账号与双上限额度" width="49%" />
</p>

### Windows

<p align="center">
  <img src="docs/screenshots/windows-muse.png" alt="Windows Muse 多账号管理与额度同步" width="49%" />
  <img src="docs/screenshots/windows-hark.png" alt="Windows Hark 账号与双上限额度" width="49%" />
</p>

### Android

<p align="center">
  <img src="docs/screenshots/android-muse-accounts.png" alt="Android Muse 多账号与额度" width="210" />
  <img src="docs/screenshots/android-hark-accounts.png" alt="Android Hark 账号与双上限额度" width="210" />
  <img src="docs/screenshots/android-muse-chat.png" alt="Android 独立会话隔离界面" width="210" />
  <img src="docs/screenshots/android-update-modal.png" alt="Android 应用内检查更新" width="210" />
</p>

---

## 下载与安装

**本 fork 只发布 iOS 安装包。**macOS / Windows / Android 安装包请前往[上游 Releases](https://github.com/ldjx7/multi-agent-account-manager/releases)下载。

| 平台 | 下载文件 | 架构 / 系统要求 | 发布方 |
| --- | --- | --- | --- |
| macOS | `account-manager-macos-universal.dmg` | Apple Silicon & Intel（macOS 14.0+） | 上游 |
| Windows | `account-manager-windows-x64-setup.exe` | x64（Windows 10 / 11） | 上游 |
| Android | `account-manager-android-universal-release.apk` | 通用 ABI（Android 7.0+） | 上游 |
| iOS | `account-manager-ios-unsigned.ipa` | 未签名 IPA（iOS 26.0+，需自行签名侧载） | 本 fork |

本 fork 的 iOS 安装包见 [GitHub Releases](https://github.com/harrylarry/multi-agent-account-manager/releases)。

> `account-manager-macos-universal.app.tar.gz` 仅用于 macOS 应用内自动更新，首次安装请下载 `.dmg`。

### macOS 安装注意事项

1. **拖入应用程序目录**：打开 `account-manager-macos-universal.dmg` 后，请务必将 `叠号.app` 拖入 `/Applications`（应用程序）文件夹再启动。切勿直接在 DMG 挂载镜像中运行，否则无法写入替换，应用内自动更新将改为打开解压目录提示手动替换。
2. **提示「“叠号”已损坏，无法打开」或「无法验证开发者」**：
   - 因未购买 Apple 付费开发者证书签名与公证（Notarization），macOS Gatekeeper 会对浏览器下载的应用附加隔离属性 `com.apple.quarantine`，从而触发「已损坏」拦截提示。
   - **解决方法一（推荐，终端移除隔离属性）**：

     ```bash
     xattr -rd com.apple.quarantine /Applications/叠号.app
     ```

     若提示权限不足，可使用：

     ```bash
     sudo xattr -rd com.apple.quarantine /Applications/叠号.app
     ```

   - **解决方法二（系统设置放行）**：先点击一次「取消」，随后打开「系统设置 → 隐私与安全性」，在页面底部找到被拦截提示并点击「仍要打开」。
   - 若直接打开 `.dmg` 也提示异常，可先在终端对 `.dmg` 执行一次 `xattr -d com.apple.quarantine ~/Downloads/account-manager-macos-universal.dmg`。
3. **数据保留**：账号配置保存在 `~/Library/Application Support/com.ldjx.accountmanager/`，Web 登录态保存在系统 `WKWebsiteDataStore` 中。后续覆盖安装或应用内更新均不会丢失已登录账号。

### Windows 安装注意事项

1. **SmartScreen 安全拦截（「Windows 已保护你的电脑」）**：
   - 因安装包未使用付费 EV/OV 代码签名证书签名，首次运行 `account-manager-windows-x64-setup.exe` 时 Microsoft Defender SmartScreen 可能弹出拦截窗口。
   - 点击窗口中的 **「更多信息」**，再点击 **「仍要运行」** 即可正常安装。
2. **安全软件误报与校验**：
   - 若杀毒软件误报拦截，可在 Release 页面查看 `latest.json` 中 `platforms.windows.sha256` 的官方哈希，与本地安装包核对一致后放行：

     ```powershell
     Get-FileHash .\account-manager-windows-x64-setup.exe -Algorithm SHA256
     ```

3. **WebView2 运行时依赖**：
   - 依赖系统内置的 Microsoft Edge WebView2 Runtime。Windows 11 默认已自带；部分精简版或老旧 Windows 10 若首次打开空白或无法启动，请前往 [Microsoft Edge WebView2 官方下载页](https://developer.microsoft.com/microsoft-edge/webview2/) 安装「常青独立安装程序（x64）」。
4. **安装权限与数据保留**：
   - 如安装到受系统保护目录需通过 UAC 授权。账号配置与各账号独立 WebView2 数据目录均保存在 `%APPDATA%\com.ldjx.accountmanager\`，覆盖安装或应用内更新均不会清除已有登录状态。

### Android 安装注意事项

1. **允许安装未知来源应用**：首次安装 `.apk` 以及使用应用内更新时，系统会要求授予「允许安装未知来源应用」权限，授权后返回即可调起系统安装器覆盖更新。
2. **WebView 内核要求**：多账号隔离依赖 Android System WebView 的 `MULTI_PROFILE` 能力；如提示不支持，请在应用商店或系统更新中升级 Android System WebView（或 Chrome）。
3. **签名一致说明**：所有 Release APK 均使用固定发布密钥签名，可直接覆盖升级并完整保留多账号登录态。

### iOS 安装注意事项

1. **自行签名侧载**：发布的是未签名 IPA，无法直接安装，需用 AltStore / Sideloadly / SideStore 或 Xcode 以自己的 Apple ID 签名后安装。
2. **免费签名有效期**：免费 Apple ID 签名有效期 7 天、每台设备最多 3 个自签应用，到期后需重新签名安装；付费开发者账号签名有效期 1 年。
3. **应用内更新**：iOS 无法应用内覆盖更新，检查到新版后请从 Releases 下载新版 IPA 重新签名安装；同一 bundle id 覆盖安装可保留已登录账号。

---

## 应用内更新与发版

- **客户端检查更新**：iPhone 在「管理 → 检查更新」查看新版并按提示侧载。Android 使用右上角下载图标，桌面使用左侧栏底部「更新」。启动后会静默检查，发现新版时显示更新标记。
- **维护者发布新版本**：本 fork 只做 `ios-v*` 发版，不做 `v*` 三端发版（`v*` 标签会触发三端发版流程，不要用）。
  1. 同步更新 [package.json](package.json)、[src-tauri/Cargo.toml](src-tauri/Cargo.toml) 与 [src-tauri/tauri.conf.json](src-tauri/tauri.conf.json) 中的版本号。
  2. 合入 `main` 后由 GitHub Actions（`.github/workflows/ios.yml`）构建未签名 IPA。
  3. 手动创建 Release（标签形如 `ios-v0.1.5`），上传 CI 构建的未签名 IPA 与含 `ios` 条目的 `latest.json`。

---

## 本地开发

环境要求：Node.js 22+、Rust 1.85+，Android 构建需额外配置 Android SDK 与 NDK 26+，iOS 构建需 macOS + Xcode 26（见 [.github/workflows/ios.yml](.github/workflows/ios.yml)）。

```bash
npm ci
npm run tauri -- dev
npm run tauri -- build
```

---

## 开发与验收

- [AGENTS.md](AGENTS.md) · 开发协作、测试与安全门禁。
- [产品设计 v4](<docs/叠号 iOS 借鉴 BotArk 优化设计规划 v4.md>) · 产品需求与设计决策。后续裁决见[开发决策记录](docs/开发决策记录.md)。
- [开发计划 v2.1](docs/叠号开发计划-严谨执行版-v2.1-2026-11-23.md) · Phase 0–5 的任务、验收矩阵与 Go/No-Go 门禁。
- [当前进度](docs/development/current-progress.md) · 阶段状态、候选提交、待验收事项与 GitHub 证据。
- [iPhone 真机验收](docs/acceptance/iphone-acceptance.md) · Spike IPA、校验值、测试步骤与结果回填。

---

## 社区与开源协议

本项目已加入并致谢 [LINUX DO](https://linux.do/) 开源社区。

本项目基于 [Apache-2.0](LICENSE) 协议开源。
