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
import Tauri
import UIKit
import WebKit

// MARK: - Command arguments (field names must match the Rust JSON keys)

private struct SubscribeEventsArgs: Decodable {
  let channel: Channel
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

class AccountSessionPlugin: Plugin {

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
        let mine = cookies.filter { $0.domain == "muse.ai" || $0.domain.hasSuffix(".muse.ai") }
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
