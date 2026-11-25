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
import Network
import Tauri
import UIKit
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
}

private struct SyncAccountQuotasArgs: Decodable {
  let accounts: [SyncAccountTarget]
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
      channel.send(["reason": interruption])
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
      let store = WKWebsiteDataStore(forIdentifier: identifier)
      store.httpCookieStore.getAllCookies { cookies in
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
          providerId: $0.providerId, uuid: $0.uuid, entryUrl: $0.entryUrl)
      }
      QuotaSyncRunner.run(targets: targets) { results in
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
  return AccountSessionPlugin()
}
