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

struct IosAccountSessionSpec {
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

  /// Kotlin `copy(autoAlias = false, alias = identity)`.
  func withAlias(_ alias: String) -> IosAccountSessionSpec {
    IosAccountSessionSpec(
      providerId: providerId,
      providerName: providerName,
      uuid: uuid,
      alias: alias,
      autoAlias: false,
      entryUrl: entryUrl,
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

  private init() {}

  func setEventChannel(_ channel: Channel) {
    lock.withLock {
      pluginInitialized = true
      eventChannel = channel
    }
    // 0.1.15: a notification tap that cold-launched the app may have
    // arrived before the channel existed; deliver the stored tap now.
    flushPendingNotificationTap()
  }

  func attachViewController(_ vc: AccountWebViewController) {
    lock.withLock { viewControllerRef = vc }
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

  func emitActivityClosed(switchSeq: Int64, viewInstanceId: Int64) {
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

  // Neither event is tied to the active session — a completion can be
  // about a parked account — so there is no isCurrent guard here.

  func emitTaskReplyDetected(providerId: String, uuid: String) {
    let ch: Channel? = lock.withLock { eventChannel }
    guard let ch else { return }
    ch.send([
      "kind": "taskReplyDetected",
      "providerId": providerId,
      "uuid": uuid,
    ])
  }

  func emitNotificationTapped(providerId: String, uuid: String) {
    let ch: Channel? = lock.withLock { eventChannel }
    guard let ch else { return }
    ch.send([
      "kind": "notificationTapped",
      "providerId": providerId,
      "uuid": uuid,
    ])
    clearPendingNotificationTap()
  }

  // MARK: - Pending notification tap (cold launch)

  private static let pendingTapKey = "maam.pendingNotificationTap"

  func storePendingNotificationTap(providerId: String, uuid: String) {
    UserDefaults.standard.set([providerId, uuid], forKey: Self.pendingTapKey)
  }

  func clearPendingNotificationTap() {
    UserDefaults.standard.removeObject(forKey: Self.pendingTapKey)
  }

  private func flushPendingNotificationTap() {
    guard
      let pair = UserDefaults.standard.array(forKey: Self.pendingTapKey) as? [String],
      pair.count == 2
    else { return }
    emitNotificationTapped(providerId: pair[0], uuid: pair[1])
  }
}
