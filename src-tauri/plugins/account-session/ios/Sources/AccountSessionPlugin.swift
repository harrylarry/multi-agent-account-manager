// AccountSessionPlugin.swift — iOS port of AccountSessionPlugin.kt.
//
// Exposes the same 8 commands as the Android plugin (subscribeEvents,
// checkCapabilities, openAccountSession, updateAccountTitle,
// closeActiveSession, deleteProfile, openExternalUrl, fetchText), plus
// syncAccountQuotas (batch-1 iOS background quota sync — no Android
// equivalent, the Android sync runs through its own warm webviews) and
// setUnreadBadge (0.1.16 aggregate unread dot on the session "← 账号"
// button — no Android equivalent).
// downloadAndInstallApk is intentionally NOT ported: iOS updates are
// manual re-sideloads (see Rust updater.rs, status "manual_install").
//
// Command methods are dispatched by name via the ObjC selector, exactly
// like tauri-plugin-notification (e.g. Rust run_mobile_plugin("show", …)
// -> @objc func show(_ invoke: Invoke)). Argument parsing uses
// invoke.parseArgs(Decodable.self), which is also how the Rust Channel
// reaches Swift (see Channel.swift: parseArgs injects sendChannelData
// into the decoder's userInfo).

import Foundation
import AVFoundation
import Network
import Tauri
import UIKit
import UserNotifications
import WebKit

// MARK: - Command arguments (field names must match the Rust JSON keys)

private struct SubscribeEventsArgs: Decodable {
  let channel: Channel
}

private struct FingerprintForegroundArgs: Decodable {
  let taskId: String
  let interrupted: Channel?
}

private struct OpenSessionArgs: Decodable {
  let providerId: String
  let providerName: String
  let uuid: String
  let alias: String
  let autoAlias: Bool
  let entryUrl: String
  let timezone: String?
  let allowedHosts: [String]
  let switchSeq: Int64
  let viewInstanceId: Int64
  let isColdStart: Bool
}

private struct UpdateTitleArgs: Decodable {
  let switchSeq: Int64
  let title: String
}

private struct CloseSessionArgs: Decodable {
  let switchSeq: Int64
}

private struct DeleteProfileArgs: Decodable {
  let uuid: String
}

private struct TransferProfileCookiesArgs: Decodable {
  struct Entry: Decodable {
    let name: String
    let value: String
    let domain: String
    let path: String
    let secure: Bool
    let httpOnly: Bool
    let expirationDate: Double?
    let sameSite: String?
  }

  let uuid: String
  let provider: String
  let cookies: [Entry]?
  let inspect: Bool?
  let clipboardText: String?
  let clipboardCount: Int?
}

private struct TaskNotificationArgs: Decodable {
  let identifier: String
  let kind: String
  let approval: ApprovalNotice?
}

private struct ApprovalNotice: Decodable {
  let uuid: String
  let approvalId: String
  let sessionScopeId: String
  let expiresAtMs: Int64
  let allow: Bool
  let deny: Bool
}

private final class ApprovalNotificationDelegate: NSObject, UNUserNotificationCenterDelegate {
  static let shared = ApprovalNotificationDelegate()
  static let queueKey = "diehao.local-approval-actions"
  static let allow = "diehao.approval.allow-once"
  static let deny = "diehao.approval.deny"
  static func configure() {
    let center = UNUserNotificationCenter.current()
    center.delegate = shared
    let allow = UNNotificationAction(identifier: Self.allow, title: "允许一次", options: [.foreground, .authenticationRequired])
    let deny = UNNotificationAction(identifier: Self.deny, title: "拒绝", options: [.foreground, .authenticationRequired, .destructive])
    center.setNotificationCategories([
      UNNotificationCategory(identifier: "diehao.approval.both", actions: [allow, deny], intentIdentifiers: []),
      UNNotificationCategory(identifier: "diehao.approval.allow", actions: [allow], intentIdentifiers: []),
      UNNotificationCategory(identifier: "diehao.approval.deny", actions: [deny], intentIdentifiers: []),
    ])
  }

  func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                              withCompletionHandler completionHandler: @escaping () -> Void) {
    let info = response.notification.request.content.userInfo
    let decision = response.actionIdentifier == Self.allow ? "allow_once" : response.actionIdentifier == Self.deny ? "deny" : ""
    guard !decision.isEmpty, let uuid = info["uuid"] as? String, let approvalId = info["approvalId"] as? String,
          let sessionScopeId = info["sessionScopeId"] as? String, let expiry = info["expiresAtMs"] as? NSNumber else {
      completionHandler()
      return
    }
    DispatchQueue.main.async {
      let store = UserDefaults.standard
      var queue = store.array(forKey: Self.queueKey) as? [[String: Any]] ?? []
      if !queue.contains(where: { $0["uuid"] as? String == uuid && $0["approvalId"] as? String == approvalId }) {
        queue.append(["actionId":UUID().uuidString, "uuid":uuid, "approvalId":approvalId, "sessionScopeId":sessionScopeId,
                      "expiresAtMs":expiry, "decision":decision])
        store.set(Array(queue.suffix(16)), forKey: Self.queueKey)
      }
      completionHandler()
    }
  }

  func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                              withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
    completionHandler(notification.request.content.categoryIdentifier.hasPrefix("diehao.approval.") ? [.banner, .list, .sound] : [])
  }
}

private struct ShareAccountBackupArgs: Decodable {
  let contents: String
}

private struct GetSessionCookiesArgs: Decodable {
  let uuid: String
  let provider: String?
}

private struct SetUnreadBadgeArgs: Decodable {
  let hasUnread: Bool
}

private struct OpenExternalUrlArgs: Decodable {
  let url: String
}

private struct FetchTextArgs: Decodable {
  let url: String
  let userAgent: String
  let accept: String?
}

// Batch-1: background quota sync targets (Rust filters out the active
// account — the foreground probe already covers it).
private struct SyncAccountTarget: Decodable {
  let providerId: String
  let uuid: String
  let entryUrl: String
  let timezone: String?
}

private struct SyncAccountQuotasArgs: Decodable {
  let accounts: [SyncAccountTarget]
  let fresh: Bool?
}

// #11/#16（补 W）：每账号 WebView 代理。口令不经这里传递——Swift 侧按
// `proxy-<uuid>` 直接读 Keychain（service 与 Rust secrets.rs 一致）。
private struct SetAccountProxyArgs: Decodable {
  let uuid: String
  let scheme: String
  let host: String
  let port: UInt16
  let username: String?
  let enabled: Bool
}

// #11/#16（补 W）：在「Rust 保存」与「Swift 下发」之间先隔离旧出口。
private struct QuarantineProxyArgs: Decodable {
  let uuid: String
  let reason: String?
}

class AccountSessionPlugin: Plugin {
  private var fingerprintTasks: [String: (Channel, DispatchWorkItem)] = [:]
  private var fingerprintPreviousIdleTimer: Bool?
  private var fingerprintBackgroundObserver: NSObjectProtocol?

  @objc public func setFingerprintForeground(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(FingerprintForegroundArgs.self)
    DispatchQueue.main.async {
      guard let interrupted = args.interrupted else {
        self.finishFingerprintForeground(args.taskId)
        invoke.resolve()
        return
      }
      guard UIApplication.shared.applicationState == .active else {
        invoke.reject("指纹探针需要在前台运行")
        return
      }
      if self.fingerprintTasks[args.taskId] != nil {
        invoke.reject("该指纹任务已在运行")
        return
      }
      if self.fingerprintTasks.isEmpty {
        self.fingerprintPreviousIdleTimer = UIApplication.shared.isIdleTimerDisabled
        UIApplication.shared.isIdleTimerDisabled = true
        self.fingerprintBackgroundObserver = NotificationCenter.default.addObserver(
          forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main
        ) { [weak self] _ in
          MainActor.assumeIsolated {
            guard let self else { return }
            for id in Array(self.fingerprintTasks.keys) {
              self.finishFingerprintForeground(id, interruption: "background")
            }
          }
        }
      }
      let taskId = args.taskId
      let expiry = DispatchWorkItem { [weak self] in
        MainActor.assumeIsolated {
          self?.finishFingerprintForeground(taskId, interruption: "deadline")
        }
      }
      self.fingerprintTasks[taskId] = (interrupted, expiry)
      DispatchQueue.main.asyncAfter(deadline: .now() + 570, execute: expiry)
      invoke.resolve()
    }
  }

  @MainActor
  private func finishFingerprintForeground(_ taskId: String, interruption: String? = nil) {
    guard let (channel, expiry) = fingerprintTasks.removeValue(forKey: taskId) else { return }
    expiry.cancel()
    if fingerprintTasks.isEmpty {
      if let previous = fingerprintPreviousIdleTimer {
        UIApplication.shared.isIdleTimerDisabled = previous
      }
      fingerprintPreviousIdleTimer = nil
      if let observer = fingerprintBackgroundObserver {
        NotificationCenter.default.removeObserver(observer)
        fingerprintBackgroundObserver = nil
      }
    }
    if let interruption {
      do {
        try channel.send(["reason": interruption])
      } catch {
        NSLog("Fingerprint interruption could not be delivered")
      }
    }
  }

  @objc public func subscribeEvents(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(SubscribeEventsArgs.self)
    AccountSessionCoordinator.shared.setEventChannel(args.channel)
    invoke.resolve()
  }

  @objc public func checkCapabilities(_ invoke: Invoke) {
    // Multi-profile is always supported on iOS 17+ (WKWebsiteDataStore
    // identifiers); the profile list comes from DataStoreRegistry because
    // WebKit offers no API to enumerate existing stores.
    DispatchQueue.main.async {
      invoke.resolve([
        "multiProfileSupported": true,
        "profileNames": DataStoreRegistry.allIds(),
      ])
    }
  }

  @objc public func openAccountSession(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(OpenSessionArgs.self)
    DispatchQueue.main.async {
      if args.isColdStart && !DataStoreRegistry.contains(args.uuid) {
        invoke.reject("已提交账号的 Profile 不存在")
        return
      }

      let spec = IosAccountSessionSpec(
        providerId: args.providerId,
        providerName: args.providerName,
        uuid: args.uuid,
        alias: args.alias,
        autoAlias: args.autoAlias,
        entryUrl: args.entryUrl,
        timezone: args.timezone,
        allowedHosts: args.allowedHosts,
        switchSeq: args.switchSeq,
        viewInstanceId: args.viewInstanceId,
        isColdStart: args.isColdStart
      )

      let coordinator = AccountSessionCoordinator.shared
      let existing = coordinator.beginSession(spec)
      if !coordinator.isCurrent(
        switchSeq: spec.switchSeq, viewInstanceId: spec.viewInstanceId)
      {
        invoke.reject("切换请求已过期")
        return
      }
      if let existing, !existing.isBeingDismissed {
        existing.bindSessionSpec(spec)
      } else {
        let vc = AccountWebViewController()
        vc.bindSessionSpec(spec)
        guard let presenter = self.manager.viewController else {
          invoke.reject("无法呈现账号界面")
          return
        }
        presenter.present(vc, animated: true)
      }
      invoke.resolve()
    }
  }

  @objc public func updateAccountTitle(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(UpdateTitleArgs.self)
    DispatchQueue.main.async {
      AccountSessionCoordinator.shared.markCommittedAndReveal(
        switchSeq: args.switchSeq, title: args.title)
      invoke.resolve()
    }
  }

  @objc public func closeActiveSession(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(CloseSessionArgs.self)
    DispatchQueue.main.async {
      let coordinator = AccountSessionCoordinator.shared
      let previous = coordinator.currentActiveSpec()
      let vc = coordinator.invalidateAndClose(nextSeq: args.switchSeq)
      vc?.closeAndFinishFromManager(
        keepPage: previous != nil && args.switchSeq > previous!.switchSeq)
      invoke.resolve()
    }
  }

  @objc public func deleteProfile(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(DeleteProfileArgs.self)
    DispatchQueue.main.async {
      let coordinator = AccountSessionCoordinator.shared
      if coordinator.isUuidActive(args.uuid) {
        invoke.reject("目标 Profile 视图尚未销毁")
        return
      }
      RecentAccountPages.discardProfile(args.uuid)
      TaskWatcher.shared.forget(uuid: args.uuid)
      // 账号被删除：出口条目也一并删掉（保留会让 uuid 复用者继承旧策略）。
      AccountProxyStore.remove(uuid: args.uuid)
      guard DataStoreRegistry.contains(args.uuid) else {
        invoke.resolve()
        return
      }
      guard let identifier = UUID(uuidString: args.uuid) else {
        invoke.reject("无效的会话标识")
        return
      }
      let store = WKWebsiteDataStore(forIdentifier: identifier)
      store.removeData(
        ofTypes: WKWebsiteDataStore.allWebsiteDataTypes(),
        modifiedSince: Date.distantPast
      ) {
        DataStoreRegistry.remove(args.uuid)
        invoke.resolve()
      }
    }
  }

  /// Spike 0.1.22 (#2 Cookie 桥)：从指定账号的 WKWebsiteDataStore 取
  /// muse.ai 会话 cookie 传给 Rust（内存传递）。
  /// 只回传 muse.ai 域的 cookie；值不记日志、不落盘，Rust 侧用后 zeroize。
  @objc public func getSessionCookies(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(GetSessionCookiesArgs.self)
    DispatchQueue.main.async {
      guard let identifier = UUID(uuidString: args.uuid) else {
        invoke.reject("无效的会话标识")
        return
      }
      let store = RecentAccountPages.dataStore(providerId: args.provider ?? "muse", uuid: args.uuid)
        ?? WKWebsiteDataStore(forIdentifier: identifier)
      store.httpCookieStore.getAllCookies { [store] cookies in
        defer { withExtendedLifetime(store) {} }
        let mine: [HTTPCookie]
        switch args.provider {
        case nil, "muse":
          mine = cookies.filter { $0.domain == "muse.ai" || $0.domain.hasSuffix(".muse.ai") }
        case "hark":
          mine = cookies.filter { cookie in
            let domain = cookie.domain.lowercased()
            let validDomain = domain == "app.hark.com" || domain == ".app.hark.com" || domain == ".hark.com"
            let validPath = cookie.path == "/" || cookie.path == "/api" || cookie.path == "/api/"
            return validDomain && validPath && (cookie.expiresDate.map { $0 > Date() } ?? true)
          }
        default:
          invoke.reject("不支持的登录态服务")
          return
        }
        invoke.resolve(["cookies": mine.map { ["name": $0.name, "value": $0.value] }])
      }
    }
  }

  @objc public func isAutomaticTaskPollingAvailable(_ invoke: Invoke) {
    DispatchQueue.main.async {
      let active = UIApplication.shared.applicationState == .active
      invoke.resolve(["available": active && !ProcessInfo.processInfo.isLowPowerModeEnabled])
    }
  }

  @objc public func requestTaskNotificationPermission(_ invoke: Invoke) {
    UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { granted, _ in
      invoke.resolve(["granted": granted])
    }
  }

  @objc public func postTaskNotification(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(TaskNotificationArgs.self)
    DispatchQueue.main.async {
      guard UIApplication.shared.applicationState != .active || args.approval != nil else {
        invoke.resolve()
        return
      }
      let content = UNMutableNotificationContent()
      content.title = "叠号"
      content.body = args.kind == "approval" ? "有待处理的审批。" : "检测到任务回复。"
      content.sound = .default
      if let approval = args.approval {
        content.categoryIdentifier = approval.allow && approval.deny ? "diehao.approval.both"
          : approval.allow ? "diehao.approval.allow" : "diehao.approval.deny"
        content.userInfo = ["uuid":approval.uuid, "approvalId":approval.approvalId,
                            "sessionScopeId":approval.sessionScopeId, "expiresAtMs":approval.expiresAtMs]
      }
      let request = UNNotificationRequest(identifier: args.identifier, content: content, trigger: nil)
      UNUserNotificationCenter.current().add(request) { _ in invoke.resolve() }
    }
  }

  @objc public func readApprovalNotificationActions(_ invoke: Invoke) {
    DispatchQueue.main.async {
      guard UIApplication.shared.applicationState == .active else {
        invoke.resolve(["actions": []])
        return
      }
      let store = UserDefaults.standard
      var actions = store.array(forKey: ApprovalNotificationDelegate.queueKey) as? [[String: Any]] ?? []
      let legacy = actions.indices.filter { actions[$0]["actionId"] == nil }
      for index in legacy { actions[index]["actionId"] = UUID().uuidString }
      if !legacy.isEmpty { store.set(actions, forKey: ApprovalNotificationDelegate.queueKey) }
      invoke.resolve(["actions": actions])
    }
  }

  @objc public func finishApprovalNotificationAction(_ invoke: Invoke) throws {
    struct Args: Decodable { let actionId: String }
    let args = try invoke.parseArgs(Args.self)
    DispatchQueue.main.async {
      let store = UserDefaults.standard
      let queue = store.array(forKey: ApprovalNotificationDelegate.queueKey) as? [[String: Any]] ?? []
      store.set(queue.filter { $0["actionId"] as? String != args.actionId }, forKey: ApprovalNotificationDelegate.queueKey)
      invoke.resolve()
    }
  }

  @objc public func enforceLocalDataProtection(_ invoke: Invoke) throws {
    struct Args: Decodable { let directory: String }
    let args = try invoke.parseArgs(Args.self)
    let manager = FileManager.default
    let base = URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/Application Support", isDirectory: true)
      .resolvingSymlinksInPath().path
    let directory = URL(fileURLWithPath: args.directory, isDirectory: true).resolvingSymlinksInPath()
    guard directory.deletingLastPathComponent().path == base,
          directory.lastPathComponent == Bundle.main.bundleIdentifier else {
      invoke.reject("本机资料目录归属无法核验")
      return
    }
    let protection = FileProtectionType.completeUntilFirstUserAuthentication
    var report: [[String: String]] = []
    do {
      try manager.createDirectory(at: directory, withIntermediateDirectories: true)
      for name in ["目录", "accounts.json", "rooms-v1.json", "send-ledger.json", "backup-restore-v1.json"] {
        let target = name == "目录" ? directory : directory.appendingPathComponent(name)
        if name != "目录" && !manager.fileExists(atPath: target.path) {
          report.append(["file":name, "status":"尚未创建"])
          continue
        }
        let metadata = try target.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .isDirectoryKey])
        guard metadata.isSymbolicLink != true,
              name == "目录" ? metadata.isDirectory == true : metadata.isRegularFile == true else {
          report.append(["file":name, "status":"无法核验文件类型"])
          continue
        }
        let mode = name == "目录" ? 0o700 : 0o600
        try manager.setAttributes([.protectionKey: protection, .posixPermissions: NSNumber(value: mode)], ofItemAtPath: target.path)
        let attributes = try manager.attributesOfItem(atPath: target.path)
        let observed = (attributes[.protectionKey] as? FileProtectionType)?.rawValue
          ?? attributes[.protectionKey] as? String
        let permissions = (attributes[.posixPermissions] as? NSNumber)?.intValue
        report.append(["file":name, "status":observed == protection.rawValue && permissions == mode ? "已核验" : "保护属性无法核验"])
      }
      invoke.resolve(["files": report])
    } catch {
      invoke.reject("本机资料保护设置或读取失败")
    }
  }

  @objc public func readParkedTaskState(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(GetSessionCookiesArgs.self)
    DispatchQueue.main.async {
      guard let provider = args.provider, provider == "muse",
            UIApplication.shared.applicationState == .active else {
        invoke.resolve(["activity": NSNull()])
        return
      }
      RecentAccountPages.probeParked(providerId: provider, uuid: args.uuid,
                                      javascript: TaskWatcher.taskStateScript) { raw in
        guard let text = raw as? String, let data = text.data(using: .utf8),
              let payload = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              payload["usable"] as? Bool == true,
              let working = payload["working"] as? Bool else {
          invoke.resolve(["activity": NSNull()])
          return
        }
        invoke.resolve(["activity": working ? "working" : "idle"])
      }
    }
  }

  @objc public func lanBackupUi(_ invoke: Invoke) throws {
    struct Args: Decodable { let action: String; let code: String? }
    let args = try invoke.parseArgs(Args.self)
    DispatchQueue.main.async {
      switch args.action {
      case "wifi":
        if let network = LanBackupUI.wifi() { invoke.resolve(network) }
        else { invoke.reject("请打开叠号并连接支持局域网传输的 Wi-Fi") }
      case "render":
        if let code = args.code, code.utf8.count <= 100, let png = LanBackupUI.png(code: code) { invoke.resolve(["png":png]) }
        else { invoke.reject("无法生成配对二维码") }
      case "scan":
        AVCaptureDevice.requestAccess(for: .video) { allowed in
          DispatchQueue.main.async {
            guard allowed else { invoke.reject("相机权限不可用，可以手动填写配对码"); return }
            guard let root = UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene })
              .first(where: { $0.activationState == .foregroundActive })?.keyWindow?.rootViewController else {
              invoke.reject("无法打开扫码页面"); return
            }
            var presenter = root
            while let next = presenter.presentedViewController { presenter = next }
            let scanner = LanBackupScanner()
            scanner.modalPresentationStyle = .fullScreen
            scanner.result = { code in invoke.resolve(["code":code ?? ""]) }
            presenter.present(scanner, animated: true)
          }
        }
      default: invoke.reject("局域网备份操作无效")
      }
    }
  }

  @objc public func iosNetworkPreferences(_ invoke: Invoke) throws {
    struct Args: Decodable { let isolate: Bool?; let quotaMinutes: Int? }
    let args = try invoke.parseArgs(Args.self)
    if let minutes = args.quotaMinutes, ![0, 5, 15, 30].contains(minutes) {
      invoke.reject("额度同步间隔无效"); return
    }
    DispatchQueue.main.async {
      invoke.resolve(RecentAccountPages.networkPreferences(isolate: args.isolate, quotaMinutes: args.quotaMinutes))
    }
  }

  @objc public func shareAccountBackup(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(ShareAccountBackupArgs.self)
    DispatchQueue.main.async {
      guard args.contents.utf8.count <= 2_800_000,
            let decoded = Data(base64Encoded: args.contents),
            decoded.starts(with: Data("MAAMBAK1".utf8)),
            let window = UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene })
              .first(where: { $0.activationState == .foregroundActive })?.keyWindow,
            let root = window.rootViewController else {
        invoke.reject("无法打开备份分享")
        return
      }
      var presenter = root
      while let next = presenter.presentedViewController { presenter = next }
      let file = FileManager.default.temporaryDirectory.appendingPathComponent(
        "diehao-" + String(UUID().uuidString.prefix(8)) + ".maambackup"
      )
      do {
        try Data(args.contents.utf8).write(to: file, options: [.atomic, .completeFileProtection])
      } catch {
        invoke.reject("无法创建加密备份文件")
        return
      }
      let controller = UIActivityViewController(activityItems: [file], applicationActivities: nil)
      if let popover = controller.popoverPresentationController {
        popover.sourceView = presenter.view
        popover.sourceRect = CGRect(x: presenter.view.bounds.midX, y: presenter.view.bounds.midY, width: 1, height: 1)
      }
      controller.completionWithItemsHandler = { _, shared, _, _ in
        try? FileManager.default.removeItem(at: file)
        invoke.resolve(["shared": shared])
      }
      presenter.present(controller, animated: true)
    }
  }

  @objc public func shareRoomArtifact(_ invoke: Invoke) throws {
    struct Args: Decodable { let name: String; let contents: String }
    let args = try invoke.parseArgs(Args.self)
    DispatchQueue.main.async {
      guard !args.name.isEmpty, args.name.utf8.count <= 400,
            !args.name.contains("/"), !args.name.contains("\\"), !args.name.contains(":"),
            args.name != ".", args.name != "..", args.contents.utf8.count <= 350_000,
            let decoded = Data(base64Encoded: args.contents), decoded.count <= 262_144, !decoded.isEmpty,
            let root = UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene })
              .first(where: { $0.activationState == .foregroundActive })?.keyWindow?.rootViewController else {
        invoke.reject("无法打开房间附件分享")
        return
      }
      let folder = FileManager.default.temporaryDirectory.appendingPathComponent("diehao-" + UUID().uuidString, isDirectory: true)
      let file = folder.appendingPathComponent(args.name, isDirectory: false)
      do {
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false)
        try decoded.write(to: file, options: [.atomic, .completeFileProtection])
      } catch {
        try? FileManager.default.removeItem(at: folder)
        invoke.reject("无法创建分享附件")
        return
      }
      var presenter = root
      while let next = presenter.presentedViewController { presenter = next }
      let controller = UIActivityViewController(activityItems: [file], applicationActivities: nil)
      if let popover = controller.popoverPresentationController {
        popover.sourceView = presenter.view
        popover.sourceRect = CGRect(x: presenter.view.bounds.midX, y: presenter.view.bounds.midY, width: 1, height: 1)
      }
      controller.completionWithItemsHandler = { _, shared, _, _ in
        try? FileManager.default.removeItem(at: folder)
        invoke.resolve(["shared": shared])
      }
      presenter.present(controller, animated: true)
    }
  }

  @objc public func transferProfileCookies(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(TransferProfileCookiesArgs.self)
    Task { @MainActor in
      let domains: [String]
      switch args.provider {
      case "muse": domains = ["muse.ai", "meta.com"]
      case "hark": domains = ["hark.com"]
      default:
        invoke.reject("不支持的登录态服务")
        return
      }
      guard let identifier = UUID(uuidString: args.uuid),
            !AccountSessionCoordinator.shared.isUuidActive(args.uuid) else {
        invoke.reject("请关闭该账号会话后重试")
        return
      }
      if let text = args.clipboardText, let count = args.clipboardCount {
        UIPasteboard.general.setItems(
          [["public.utf8-plain-text": text]],
          options: [.localOnly: true, .expirationDate: Date().addingTimeInterval(60)]
        )
        invoke.resolve(["count": count])
        return
      }
      func allowed(_ domain: String) -> Bool {
        let host = domain.trimmingCharacters(in: CharacterSet(charactersIn: ".")).lowercased()
        return domains.contains { host == $0 || host.hasSuffix("." + $0) }
      }
      func key(_ cookie: HTTPCookie) -> String {
        let host = cookie.domain.trimmingCharacters(in: CharacterSet(charactersIn: ".")).lowercased()
        return [cookie.name, host, cookie.path].joined(separator: "\u{0}")
      }
      let dataStore = RecentAccountPages.dataStore(providerId: args.provider, uuid: args.uuid)
        ?? WKWebsiteDataStore(forIdentifier: identifier)
      defer { withExtendedLifetime(dataStore) {} }
      let store = dataStore.httpCookieStore
      func allCookies() async -> [HTTPCookie] {
        await withCheckedContinuation { continuation in
          store.getAllCookies { cookies in continuation.resume(returning: cookies) }
        }
      }
      func write(_ cookie: HTTPCookie) async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
          store.setCookie(cookie) { continuation.resume() }
        }
      }
      func remove(_ cookie: HTTPCookie) async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
          store.delete(cookie) { continuation.resume() }
        }
      }
      func matches(_ actual: HTTPCookie, _ expected: HTTPCookie) -> Bool {
        let expiryMatches: Bool
        switch (actual.expiresDate, expected.expiresDate) {
        case (nil, nil): expiryMatches = true
        case (let a?, let b?): expiryMatches = abs(a.timeIntervalSince(b)) <= 1
        default: expiryMatches = false
        }
        return key(actual) == key(expected) && actual.value == expected.value &&
          actual.isSecure == expected.isSecure && actual.isHTTPOnly == expected.isHTTPOnly &&
          expiryMatches
      }
      if let entries = args.cookies {
        var imported: [HTTPCookie] = []
        var keys = Set<String>()
        for entry in entries {
          var properties: [HTTPCookiePropertyKey: Any] = [
            .name: entry.name, .value: entry.value, .domain: entry.domain, .path: entry.path,
          ]
          if entry.secure { properties[.secure] = "TRUE" }
          if entry.httpOnly { properties[HTTPCookiePropertyKey("HttpOnly")] = "TRUE" }
          if let expiry = entry.expirationDate {
            properties[.expires] = Date(timeIntervalSince1970: expiry)
          } else {
            properties[.discard] = "TRUE"
          }
          if let sameSite = entry.sameSite { properties[HTTPCookiePropertyKey("SameSite")] = sameSite }
          guard allowed(entry.domain), let cookie = HTTPCookie(properties: properties),
                cookie.name == entry.name, cookie.value == entry.value,
                cookie.isSecure == entry.secure, cookie.isHTTPOnly == entry.httpOnly,
                cookie.path == entry.path, keys.insert(key(cookie)).inserted else {
            invoke.reject("系统无法保留 Cookie 属性，未写入")
            return
          }
          imported.append(cookie)
        }
        RecentAccountPages.discardProfile(args.uuid)
        TaskWatcher.shared.forget(uuid: args.uuid)
        let previous = await allCookies().filter { keys.contains(key($0)) }
        for cookie in imported { await write(cookie) }
        let actual = await allCookies().filter { keys.contains(key($0)) }
        if imported.allSatisfy({ expected in
          actual.contains { matches($0, expected) }
        }) {
          DataStoreRegistry.markCreated(args.uuid)
          invoke.resolve(["count": imported.count])
        } else {
          for cookie in actual { await remove(cookie) }
          for cookie in previous { await write(cookie) }
          let restored = await allCookies().filter { keys.contains(key($0)) }
          let rollbackOK = restored.count == previous.count && previous.allSatisfy { expected in
            restored.contains { matches($0, expected) }
          }
          invoke.reject(rollbackOK ? "Cookie 写入失败，已恢复原会话" : "Cookie 恢复未能确认，请在该账号重新登录")
        }
        return
      }

      let cookies = await allCookies().filter {
        allowed($0.domain) && ($0.expiresDate.map { $0 > Date() } ?? true)
      }
      if args.inspect == true {
        let expiry = cookies.compactMap { $0.expiresDate?.timeIntervalSince1970 }.min()
        var summary: [String: Any] = ["count": cookies.count]
        if let expiry { summary["nextExpiry"] = expiry }
        invoke.resolve(summary)
        return
      }
      guard !cookies.isEmpty else {
        invoke.reject("该账号没有可导出的 Cookie")
        return
      }
      let export: [[String: Any]] = cookies.map { cookie in
        var item: [String: Any] = [
          "name": cookie.name, "value": cookie.value, "domain": cookie.domain, "path": cookie.path,
          "secure": cookie.isSecure, "httpOnly": cookie.isHTTPOnly,
        ]
        if let expires = cookie.expiresDate { item["expirationDate"] = expires.timeIntervalSince1970 }
        if let sameSite = cookie.properties?[HTTPCookiePropertyKey("SameSite")] as? String {
          item["sameSite"] = sameSite
        }
        return item
      }
      guard let data = try? JSONSerialization.data(withJSONObject: export, options: [.sortedKeys]),
            let text = String(data: data, encoding: .utf8) else {
        invoke.reject("Cookie 导出失败")
        return
      }
      // The trusted Rust bridge validates the document against the import parser.
      invoke.resolve(["count": cookies.count, "cookieExportJson": text])
    }
  }

  /// 0.1.16: aggregate unread dot on the session "← 账号" button.
  /// Rust pushes the aggregate (any account with an unread reply)
  /// whenever a flag flips; the coordinator stores it and applies it
  /// to the current session view (or the next one opened).
  @objc public func setUnreadBadge(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(SetUnreadBadgeArgs.self)
    DispatchQueue.main.async {
      AccountSessionCoordinator.shared.setUnreadBadge(args.hasUnread)
      invoke.resolve()
    }
  }

  /// #11/#16（补 W）：把每账号代理写入 WKWebView 配置。
  ///
  /// iOS 17+ 的 `WKWebsiteDataStore.proxyConfigurations` 是 WKWebView 唯一的
  /// 代理入口：Rust 侧的 reqwest / 网关隧道都影响不到 WebView 内部请求，
  /// 所以必须单独下发一次。口令不在这里传（Swift 按 `proxy-<uuid>` 读 Keychain）。
  ///
  /// 只落配置 + 丢弃该账号缓存页；返回 applied / sessionOpen 交给上层提示。
  @objc public func setAccountProxy(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(SetAccountProxyArgs.self)
    DispatchQueue.main.async {
      // 关闭代理：删配置（此时 scheme/host/port 无意义，不校验），
      // 同时把 store 上可能残留的隔离/旧代理清掉，让直连真正可用。
      if !args.enabled {
        let result = AccountSessionCoordinator.shared.setAccountProxy(
          uuid: args.uuid, config: nil)
        invoke.resolve([
          "applied": true,
          "sessionOpen": result.sessionOpen,
          "quarantined": false,
        ])
        return
      }

      let scheme = args.scheme.lowercased()
      guard ["http", "https", "socks5", "socks5h"].contains(scheme) else {
        invoke.reject("代理协议不支持（仅 http/https/socks5/socks5h）")
        return
      }
      let host = args.host.trimmingCharacters(in: .whitespacesAndNewlines)
      guard !host.isEmpty, !host.contains("/"), !host.contains("://") else {
        invoke.reject("代理主机无效")
        return
      }
      guard let port = NWEndpoint.Port(rawValue: args.port) else {
        invoke.reject("代理端口无效")
        return
      }

      let username = args.username?.trimmingCharacters(in: .whitespacesAndNewlines)
      let cfg = AccountProxyConfig(
        scheme: scheme,
        host: host,
        port: port.rawValue,
        username: (username?.isEmpty ?? true) ? nil : username,
        enabled: true
      )

      guard let identifier = UUID(uuidString: args.uuid) else {
        invoke.reject("无效的会话标识")
        return
      }
      // 先用临时 store 试装一次，确认这份配置在本机能真正生效
      //（iOS 17+ 才有 proxyConfigurations）。不 markCreated——它只用于验证。
      let probeStore = WKWebsiteDataStore(forIdentifier: identifier)
      let result = AccountProxyApplier.apply(cfg: cfg, uuid: args.uuid, to: probeStore)
      let applied = result.isApplied

      // 试装结果不理想也照常落配置：落下去之后，该账号的 WebView 在创建时会
      // 再走一次同样的检查，失败即阻断（fail-closed），不会静默走直连。
      // 若本地配置此前损坏，这次写入同时把它覆盖修复。
      let stored = AccountSessionCoordinator.shared.setAccountProxy(
        uuid: args.uuid, config: cfg)

      var payload: [String: Any] = [
        "applied": applied,
        "sessionOpen": stored.sessionOpen,
        "quarantined": false,
      ]
      if let reason = result.blockReason {
        // 试装失败：store 上没有可信的新代理，保持隔离态（上面 apply 已把它
        // 换成新配置或维持原样，两种都不会回退直连），并把原因交给上层提示。
        payload["reason"] = reason
        payload["quarantined"] = true
      }
      invoke.resolve(payload)
    }
  }

  /// #11/#16（补 W）：隔离该账号的 WebView 出口。
  ///
  /// Rust 在**保存新代理之前**调用：先把该账号 data store 的出口指向不可达
  /// 端点并销毁正在打开的 WebView，再落配置、再下发。这样「保存成功但下发
  /// 失败」「App 重启两端未同步」「已打开会话」都不会在窗口期继续走旧出口。
  @objc public func quarantineAccountProxy(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(QuarantineProxyArgs.self)
    DispatchQueue.main.async {
      let result = AccountSessionCoordinator.shared.quarantine(
        uuid: args.uuid,
        reason: args.reason ?? "代理配置正在变更，当前会话已停止出站（请重新打开该账号）"
      )
      var payload: [String: Any] = [
        "engaged": result.engaged,
        "sessionOpen": result.sessionOpen,
      ]
      if !result.engaged {
        payload["reason"] = "该设备无法隔离 WebView 出口（需要 iOS 17+）"
      }
      invoke.resolve(payload)
    }
  }

  @objc public func openExternalUrl(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(OpenExternalUrlArgs.self)
    DispatchQueue.main.async {
      guard let url = URL(string: args.url) else {
        invoke.reject("无效的链接地址")
        return
      }
      UIApplication.shared.open(url, options: [:]) { success in
        if success {
          invoke.resolve()
        } else {
          invoke.reject("无法调用系统浏览器打开链接")
        }
      }
    }
  }

  /// Batch-1: background quota sync. Probes each listed account's quota
  /// (parked page first, else a hidden one-shot webview) and resolves
  /// with the per-account results for Rust to apply.
  @objc public func syncAccountQuotas(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(SyncAccountQuotasArgs.self)
    DispatchQueue.main.async {
      let targets = args.accounts.map {
        QuotaSyncRunner.Target(
          providerId: $0.providerId, uuid: $0.uuid, entryUrl: $0.entryUrl, timezone: $0.timezone)
      }
      QuotaSyncRunner.run(targets: targets, fresh: args.fresh ?? false) { results in
        invoke.resolve(["results": results.map { $0.json }])
      }
    }
  }

  @objc public func fetchText(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(FetchTextArgs.self)
    // Same contract as Kotlin's HttpURLConnection loop: HTTPS only, manual
    // redirect following (max 10 hops), 20s timeout, 2MB body cap.
    Task {
      do {
        let (status, body) = try await Self.fetchText(
          url: args.url, userAgent: args.userAgent, accept: args.accept)
        invoke.resolve(["status": status, "body": body])
      } catch let error as FetchError {
        invoke.reject(error.message)
      } catch {
        invoke.reject("网络请求失败")
      }
    }
  }

  // MARK: - fetchText implementation

  private struct FetchError: Error {
    let message: String
  }

  /// Disables URLSession's automatic redirect following so the loop below
  /// can enforce the HTTPS-only rule on every hop, like Android.
  private final class NoRedirectDelegate: NSObject, URLSessionTaskDelegate {
    func urlSession(
      _ session: URLSession,
      task: URLSessionTask,
      willPerformHTTPRedirection response: HTTPURLResponse,
      newRequest request: URLRequest,
      completionHandler: @escaping (URLRequest?) -> Void
    ) {
      completionHandler(nil)
    }
  }

  private static func fetchText(url: String, userAgent: String, accept: String?) async throws
    -> (status: Int, body: String)
  {
    guard var currentURL = URL(string: url),
      currentURL.scheme?.lowercased() == "https"
    else {
      throw FetchError(message: "仅允许 HTTPS 更新地址")
    }

    let config = URLSessionConfiguration.default
    config.timeoutIntervalForRequest = 20
    config.timeoutIntervalForResource = 20
    // URLSession retains its delegate for the session's lifetime.
    let session = URLSession(
      configuration: config, delegate: NoRedirectDelegate(), delegateQueue: nil)

    for _ in 0..<10 {
      var request = URLRequest(url: currentURL)
      if !userAgent.isEmpty {
        request.setValue(userAgent, forHTTPHeaderField: "User-Agent")
      }
      if let accept, !accept.isEmpty {
        request.setValue(accept, forHTTPHeaderField: "Accept")
      }
      let (stream, response) = try await session.bytes(for: request)
      guard let http = response as? HTTPURLResponse else {
        throw FetchError(message: "网络请求失败")
      }
      switch http.statusCode {
      case 301, 302, 303, 307, 308:
        guard let location = http.value(forHTTPHeaderField: "Location"),
          !location.isEmpty
        else {
          throw FetchError(message: "重定向缺少 Location 头")
        }
        guard let next = URL(string: location, relativeTo: currentURL)?.absoluteURL,
          next.scheme?.lowercased() == "https"
        else {
          throw FetchError(message: "重定向到非 HTTPS 地址已被拒绝")
        }
        currentURL = next
        continue
      default:
        var data = Data()
        for try await byte in stream {
          data.append(byte)
          if data.count > 2 * 1024 * 1024 {
            throw FetchError(message: "响应内容过大")
          }
        }
        return (http.statusCode, String(data: data, encoding: .utf8) ?? "")
      }
    }
    throw FetchError(message: "网络重定向次数过多")
  }
}

@_cdecl("init_plugin_account_session")
public func initPlugin() -> Plugin {
  // 0.1.15: start the task watcher at plugin init so the parked-page
  // sweep is running from the first session on.
  TaskWatcher.shared.start()
  ApprovalNotificationDelegate.configure()
  return AccountSessionPlugin()
}
