// AccountSessionCoordinator.swift — iOS port of AccountSessionCoordinator.kt.
//
// Owns the event Channel back to Rust, the latest switchSeq guard, and the
// active/committed session specs. Mirrors the Kotlin object 1:1 except:
// - `recoverSpecOnActivityRecreate` is dropped: iOS has no Activity recreate;
//   if the app process dies, the Rust state dies with it, so there is nothing
//   to recover against.
// - `emitUpdateProgress` is kept for contract parity (the Rust IosSessionEvent
//   enum has the variant), but nothing on iOS sends it — there is no in-app
//   install flow; updates are manual re-sideloads.

import Foundation
import Tauri
import UIKit
import WebKit

struct IosAccountSessionSpec {
  let providerId: String
  let providerName: String
  let uuid: String
  let alias: String
  let autoAlias: Bool
  let entryUrl: String
  let timezone: String?
  let language: String?
  let allowedHosts: [String]
  let switchSeq: Int64
  let viewInstanceId: Int64
  let isColdStart: Bool

  /// Kotlin `copy(autoAlias = false, alias = identity)`.
  func withAlias(_ alias: String) -> IosAccountSessionSpec {
    IosAccountSessionSpec(
      providerId: providerId,
      providerName: providerName,
      uuid: uuid,
      alias: alias,
      autoAlias: false,
      entryUrl: entryUrl,
      timezone: timezone,
      language: language,
      allowedHosts: allowedHosts,
      switchSeq: switchSeq,
      viewInstanceId: viewInstanceId,
      isColdStart: isColdStart
    )
  }
}

/// Tracks which WKWebsiteDataStore identifiers have actually been created.
///
/// WKWebsiteDataStore offers no API to enumerate existing stores, but Rust
/// needs `profileNames` for its cold-start check ("is the committed account's
/// data store still there?"). So the set of created identifiers is persisted
/// in UserDefaults: marked on first creation, removed on deleteProfile.
enum DataStoreRegistry {
  private static let defaultsKey = "maam.knownDataStoreIds"

  static func contains(_ uuid: String) -> Bool {
    allIds().contains(uuid)
  }

  static func markCreated(_ uuid: String) {
    var ids = Set(allIds())
    guard !ids.contains(uuid) else { return }
    ids.insert(uuid)
    UserDefaults.standard.set(Array(ids), forKey: defaultsKey)
  }

  static func remove(_ uuid: String) {
    var ids = Set(allIds())
    guard ids.contains(uuid) else { return }
    ids.remove(uuid)
    UserDefaults.standard.set(Array(ids), forKey: defaultsKey)
  }

  static func allIds() -> [String] {
    (UserDefaults.standard.array(forKey: defaultsKey) as? [String] ?? []).sorted()
  }
}

final class AccountSessionCoordinator {
  static let shared = AccountSessionCoordinator()

  // NSRecursiveLock: emit* methods lock and then call isCurrentLocked-style
  // checks that also lock — exactly like Kotlin's reentrant `synchronized`.
  private let lock = NSRecursiveLock()

  private var pluginInitialized = false
  private var eventChannel: Channel?
  private var latestSwitchSeq: Int64 = 0
  private var activeSpec: IosAccountSessionSpec?
  private var committedSpec: IosAccountSessionSpec?
  private weak var viewControllerRef: AccountWebViewController?
  // 0.1.16: aggregate unread state (any parked account with a new
  // reply) — drives the "← 账号" button dot.
  private var hasUnreadBadge = false

  // 0.1.17: return to the account list when the app comes back after
  // a long absence (5 min) instead of resuming the stale page.
  private static let backgroundReturnThreshold: TimeInterval = 300
  private var backgroundedAt: Date?
  private var backgroundObserver: NSObjectProtocol?
  private var foregroundObserver: NSObjectProtocol?

  private init() {
    backgroundObserver = NotificationCenter.default.addObserver(
      forName: UIApplication.didEnterBackgroundNotification,
      object: nil,
      queue: .main
    ) { [weak self] _ in self?.backgroundedAt = Date() }
    foregroundObserver = NotificationCenter.default.addObserver(
      forName: UIApplication.willEnterForegroundNotification,
      object: nil,
      queue: .main
    ) { [weak self] _ in self?.returnToListIfAwayLong() }
  }

  func setEventChannel(_ channel: Channel) {
    lock.withLock {
      pluginInitialized = true
      eventChannel = channel
    }
  }

  func attachViewController(_ vc: AccountWebViewController) {
    lock.withLock { viewControllerRef = vc }
  }

  /// 0.1.16: aggregate unread dot on the session "← 账号" button.
  /// Rust owns the flags and pushes the aggregate whenever one flips.
  /// Stored here so a newly opened session applies it in viewDidLoad.
  func setUnreadBadge(_ hasUnread: Bool) {
    lock.withLock { hasUnreadBadge = hasUnread }
    currentViewController()?.setUnreadDotVisible(hasUnread)
  }

  func currentUnreadBadge() -> Bool {
    lock.withLock { hasUnreadBadge }
  }

  func detachViewController(_ vc: AccountWebViewController) {
    lock.withLock {
      if viewControllerRef === vc {
        viewControllerRef = nil
      }
    }
  }

  func currentViewController() -> AccountWebViewController? {
    lock.withLock { viewControllerRef }
  }

  /// Returns the currently presented view controller (if any) for reuse.
  func beginSession(_ spec: IosAccountSessionSpec) -> AccountWebViewController? {
    lock.withLock {
      if spec.switchSeq < latestSwitchSeq { return nil }
      latestSwitchSeq = spec.switchSeq
      activeSpec = spec
      return viewControllerRef
    }
  }

  func currentActiveSpec() -> IosAccountSessionSpec? {
    lock.withLock { activeSpec }
  }

  func isCurrent(switchSeq: Int64, viewInstanceId: Int64) -> Bool {
    lock.withLock {
      guard let cur = activeSpec else { return false }
      return latestSwitchSeq == switchSeq
        && cur.switchSeq == switchSeq
        && cur.viewInstanceId == viewInstanceId
    }
  }

  func markCommittedAndReveal(switchSeq: Int64, title: String) {
    var target: AccountWebViewController?
    var snapshot: IosAccountSessionSpec?
    lock.withLock {
      guard let cur = activeSpec,
        latestSwitchSeq == switchSeq, cur.switchSeq == switchSeq
      else { return }
      committedSpec = cur
      snapshot = cur
      target = viewControllerRef
    }
    if let snapshot {
      target?.revealCommittedSession(
        switchSeq: snapshot.switchSeq,
        viewInstanceId: snapshot.viewInstanceId,
        title: title
      )
    }
  }

  func invalidateAndClose(nextSeq: Int64) -> AccountWebViewController? {
    lock.withLock {
      if nextSeq < latestSwitchSeq { return nil }
      latestSwitchSeq = nextSeq
      activeSpec = nil
      return viewControllerRef
    }
  }

  func isUuidActive(_ uuid: String) -> Bool {
    lock.withLock { activeSpec?.uuid == uuid }
  }

  // MARK: - #11/#16（补 W）：每账号 WKWebView 代理

  /// 写入/清除某账号的 WebView 代理，并丢弃该账号缓存的页面，
  /// 让下次打开按新配置重建 WebView（WKWebsiteDataStore 的
  /// proxyConfigurations 只在 WebView 创建时读取一次，改已有的 store 不保证生效）。
  ///
  /// **正在打开的会话会被销毁并隔离**：换出口时旧 WebView 仍在按旧配置
  /// （可能是直连）发请求，只 stopLoading / 隐藏都是不够的——已建立的
  /// XHR/WebSocket 还在跑。这里先由 Rust 侧 quarantine 把 data store 的
  /// 出口指向不可达端点，再把 WebView 从视图层级移除并释放引用，让 WebKit
  /// 回收其内容进程。**这一链路依赖 WebKit 的回收时机，须真机验证**：
  /// 网络层兜底（不可达代理 + allowFailover=false）不依赖回收。
  ///
  /// 返回 (是否已落配置, 该账号会话是否曾被阻断)。
  func setAccountProxy(uuid: String, config: AccountProxyConfig?) -> (applied: Bool, sessionOpen: Bool) {
    AccountProxyStore.set(uuid: uuid, config: config)
    RecentAccountPages.discardProfile(uuid)
    TaskWatcher.shared.forget(uuid: uuid)
    // 关闭代理时要把 store 上可能残留的隔离/旧代理清掉；开启时由下发方
    // （plugin.setAccountProxy）在同一个 store 上应用新配置。
    if config?.enabled != true {
      if #available(iOS 17.0, *), let identifier = UUID(uuidString: uuid) {
        AccountProxyApplier.clear(store: WKWebsiteDataStore(forIdentifier: identifier))
      }
    }

    var liveVC: AccountWebViewController?
    var sessionOpen = false
    lock.withLock {
      if activeSpec?.uuid == uuid {
        sessionOpen = true
        liveVC = viewControllerRef
      }
    }
    if let liveVC {
      let reason = config?.enabled == true
        ? "代理已变更，当前会话已停止出站（请重新打开该账号）"
        : "该账号代理已切换为直连，当前会话已停止出站（请重新打开该账号）"
      liveVC.blockEgress(reason: reason)
    }
    return (true, sessionOpen)
  }

  /// 先隔离再改配置：关闭 Rust 保存与 Swift 下发之间的时间窗口。
  ///
  /// 调用顺序（Rust `muse_native_set_account_proxy`）：
  /// 1. `quarantine` —— 立刻把该账号 data store 的出口换成不可达端点，
  ///    并销毁正在打开的 WebView；此后旧配置（可能是直连）再也发不出去；
  /// 2. 落 Rust 侧配置；
  /// 3. `push` 新配置 —— 成功即替换隔离，失败则保持隔离（fail-closed）。
  ///
  /// 隔离只作用于 DataStore 与内存中的视图，**不写** UserDefaults：
  /// 若第 2 步失败，持久化配置仍是旧值，下次打开按旧配置重建，
  /// 不会出现「内存说没代理、实际有代理」的分裂。
  func quarantine(uuid: String, reason: String) -> (engaged: Bool, sessionOpen: Bool) {
    let engaged = AccountProxyQuarantine.engage(uuid: uuid)
    RecentAccountPages.discardProfile(uuid)
    TaskWatcher.shared.forget(uuid: uuid)

    var liveVC: AccountWebViewController?
    var sessionOpen = false
    lock.withLock {
      if activeSpec?.uuid == uuid {
        sessionOpen = true
        liveVC = viewControllerRef
      }
    }
    liveVC?.blockEgress(reason: reason)
    return (engaged, sessionOpen)
  }

  /// 该账号当前的持久化出口配置（不含口令）；`nil` 表示没有可信条目。
  /// **只给诊断/界面用**，出站决策必须走 `AccountProxyResolver`。
  func accountProxyConfig(uuid: String) -> AccountProxyConfig? {
    AccountProxyStore.rawMap()?[uuid]
  }

  static func hostMatchesAllowed(_ url: URL?, allowedHosts: [String]) -> Bool {
    guard let url, url.scheme?.lowercased() == "https",
      let host = url.host?.lowercased()
    else { return false }
    return allowedHosts.contains { allowed in
      let clean = allowed.lowercased()
      return host == clean || host.hasSuffix(".\(clean)")
    }
  }

  static func isGoogleOAuthHost(_ url: URL?) -> Bool {
    guard let host = url?.host?.lowercased() else { return false }
    return host == "accounts.google.com" || host.hasSuffix(".accounts.google.com")
  }

  // MARK: - Event emission (kind strings must match Rust IosSessionEvent)

  func emitPageCommitVisible(spec: IosAccountSessionSpec, url: String) {
    let ch: Channel? = lock.withLock {
      if !isCurrent(switchSeq: spec.switchSeq, viewInstanceId: spec.viewInstanceId) {
        return nil
      }
      return eventChannel
    }
    guard let ch else { return }
    ch.send([
      "kind": "pageCommitVisible",
      "providerId": spec.providerId,
      "uuid": spec.uuid,
      "switchSeq": spec.switchSeq,
      "viewInstanceId": spec.viewInstanceId,
      "url": url,
    ])
  }

  func emitIdentityDetected(
    spec: IosAccountSessionSpec,
    identity: String,
    avatarUrl: String?,
    quotaText: String?,
    quotaPercent: Int?,
    quotaDetailJson: String?
  ) {
    let ch: Channel? = lock.withLock {
      if !isCurrent(switchSeq: spec.switchSeq, viewInstanceId: spec.viewInstanceId) {
        return nil
      }
      // Same side effect as Kotlin: first detected identity becomes the
      // alias in place, and autoAlias is switched off afterwards.
      if !identity.isEmpty, activeSpec?.autoAlias == true {
        activeSpec = activeSpec?.withAlias(identity)
        committedSpec = committedSpec?.withAlias(identity)
      }
      return eventChannel
    }
    guard let ch else { return }
    var obj: JsonObject = [
      "kind": "identityDetected",
      "providerId": spec.providerId,
      "uuid": spec.uuid,
      "switchSeq": spec.switchSeq,
      "viewInstanceId": spec.viewInstanceId,
      "identity": identity,
    ]
    if let avatarUrl, !avatarUrl.isEmpty { obj["avatarUrl"] = avatarUrl }
    if let quotaText, !quotaText.isEmpty { obj["quotaText"] = quotaText }
    if let quotaPercent { obj["quotaPercent"] = quotaPercent }
    if let quotaDetailJson, !quotaDetailJson.isEmpty {
      obj["quotaDetailJson"] = quotaDetailJson
    }
    ch.send(obj)
  }

  func emitSessionFailed(switchSeq: Int64, viewInstanceId: Int64, reason: String) {
    let ch: Channel? = lock.withLock {
      if activeSpec?.switchSeq == switchSeq
        && activeSpec?.viewInstanceId == viewInstanceId
      {
        activeSpec = nil
      }
      return eventChannel
    }
    guard let ch else { return }
    ch.send([
      "kind": "sessionFailed",
      "switchSeq": switchSeq,
      "viewInstanceId": viewInstanceId,
      "reason": reason,
    ])
  }

  /// 0.1.17: if the app was away for 5+ minutes, close the open
  /// session back to the account list (same path as "← 账号") instead
  /// of resuming the stale page. No-op when no session is open or the
  /// absence was short; the VC itself skips the return while an OAuth
  /// popup is open.
  private func returnToListIfAwayLong() {
    guard let at = backgroundedAt,
      Date().timeIntervalSince(at) >= Self.backgroundReturnThreshold
    else { return }
    backgroundedAt = nil
    currentViewController()?.returnToListAfterLongAbsence()
  }

  func emitActivityClosed(switchSeq: Int64, viewInstanceId: Int64) {
    let ch: Channel? = lock.withLock {
      if activeSpec?.switchSeq == switchSeq
        && activeSpec?.viewInstanceId == viewInstanceId
      {
        activeSpec = nil
      }
      return eventChannel
    }
    // 0.1.17: returning to the list refreshes the dots immediately —
    // sweep the parked pages once instead of waiting for the next 20s
    // tick. Runs after activeSpec is cleared above, so the just-closed
    // account is treated as parked, not skipped.
    TaskWatcher.shared.sweepParkedPages()
    guard let ch else { return }
    ch.send([
      "kind": "activityClosed",
      "switchSeq": switchSeq,
      "viewInstanceId": viewInstanceId,
    ])
  }

  func emitUpdateProgress(
    stage: String,
    downloaded: Int64,
    total: Int64?,
    percent: Int?,
    message: String
  ) {
    let ch: Channel? = lock.withLock { eventChannel }
    guard let ch else { return }
    var obj: JsonObject = [
      "kind": "updateProgress",
      "stage": stage,
      "downloaded": downloaded,
      "message": message,
    ]
    if let total { obj["total"] = total }
    if let percent { obj["percent"] = percent }
    ch.send(obj)
  }

  func emitSessionLoggedOut(spec: IosAccountSessionSpec) {
    let ch: Channel? = lock.withLock {
      if !isCurrent(switchSeq: spec.switchSeq, viewInstanceId: spec.viewInstanceId) {
        return nil
      }
      return eventChannel
    }
    guard let ch else { return }
    ch.send([
      "kind": "sessionLoggedOut",
      "providerId": spec.providerId,
      "uuid": spec.uuid,
      "switchSeq": spec.switchSeq,
      "viewInstanceId": spec.viewInstanceId,
    ])
  }

  // MARK: - 0.1.15 task notifications

  // 0.1.16: only PARKED accounts get flagged — the active account's
  // completions are Muse web's own job (hamburger dot etc.), so the
  // app never marks them. On the account list there is no active spec,
  // so every completion flags normally.

  func emitTaskReplyDetected(providerId: String, uuid: String) {
    if let cur = currentActiveSpec(),
      cur.providerId == providerId && cur.uuid == uuid
    {
      return
    }
    let ch: Channel? = lock.withLock { eventChannel }
    guard let ch else { return }
    let obj: JsonObject = [
      "kind": "taskReplyDetected",
      "providerId": providerId,
      "uuid": uuid,
    ]
    ch.send(obj)
  }

  func emitForegroundTaskCatchUp(targets: [[String: Any]], resumedAtMs: Int64) {
    let channel: Channel? = lock.withLock { eventChannel }
    channel?.send(["kind": "foregroundTaskCatchUp", "targets": targets, "resumedAtMs": resumedAtMs])
  }
}
