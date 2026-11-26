// RecentAccountPages.swift — iOS port of RecentAccountPages.kt.
//
// Keeps recently used account WKWebViews parked (per provider+uuid) so
// switching back to an account is instant. 0.1.8: no idle expiry any
// more — parked pages stay until the cap or memory pressure evicts them,
// least-recently-active first (the array is park-ordered, oldest first).
// Main-thread only, like the Kotlin Handler(Looper.getMainLooper()) version.
//
// Differences from Android:
// - No low-RAM check: iOS has no ActivityManager equivalent; memory warnings
//   are handled via UIApplication.didReceiveMemoryWarningNotification.
// - No renderer-priority / onPause equivalents on WKWebView.
// - The data-store identity check is subsumed by the (providerId, uuid) key:
//   WKWebsiteDataStore exposes no identifier getter, but a parked view is
//   only ever re-taken for the same uuid it was parked under.

import UIKit
import WebKit

enum RecentAccountPages {
  // 0.1.8 capacity-aware cap. There is no API for the WebContent
  // process jetsam limit (all WKWebViews share one WebContent process),
  // so the cap starts from device RAM and adapts: a process termination
  // (jetsam = we were over budget) lowers it by one, persistently, and
  // it regrows slowly (+1 per launch after 3 stable days) up to the RAM
  // bound. One loaded AI web app costs roughly 80–200 MB in the shared
  // WebContent process.
  private static let capDefaultsKey = "maam.parkedPageCap"
  private static let capDateDefaultsKey = "maam.parkedPageCapDate"

  private static var ramCap: Int {
    let gb = Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824.0
    switch gb {
    case ..<4.5: return 3
    case ..<6.5: return 5
    case ..<8.5: return 6
    default: return 8
    }
  }

  private static var storedCap: Int {
    get {
      let v = UserDefaults.standard.integer(forKey: capDefaultsKey)
      return v > 0 ? v : ramCap
    }
    set {
      UserDefaults.standard.set(newValue, forKey: capDefaultsKey)
    }
  }

  private static var regrowthChecked = false
  private static var maxAccountPages: Int {
    if !regrowthChecked {
      regrowthChecked = true
      if storedCap < ramCap {
        let last = UserDefaults.standard.object(forKey: capDateDefaultsKey) as? Date
        if last == nil || Date().timeIntervalSince(last!) >= 3 * 86400 {
          storedCap = min(ramCap, storedCap + 1)
        }
      }
    }
    return min(ramCap, storedCap)
  }

  /// Call when a web content process is terminated (parked or active):
  /// the shared WebContent process was jetsammed, so lower the cap.
  /// Coalesced to one reduction per 60s — a single jetsam event fires
  /// the terminate callback once per live web view.
  private static var lastTerminationNoted: Date?
  static func noteProcessTermination() {
    if let last = lastTerminationNoted,
      Date().timeIntervalSince(last) < 60
    {
      return
    }
    lastTerminationNoted = Date()
    storedCap = max(2, storedCap - 1)
    UserDefaults.standard.set(Date(), forKey: capDateDefaultsKey)
    trim(maxAccountPages)
  }

  /// Blocks main-frame navigations on a parked view and discards the view
  /// if its web content process dies — mirrors Kotlin's parkedClient.
  private final class ParkedDelegate: NSObject, WKNavigationDelegate {
    func webView(
      _ webView: WKWebView,
      decidePolicyFor navigationAction: WKNavigationAction,
      decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
    ) {
      decisionHandler(
        navigationAction.targetFrame?.isMainFrame == true ? .cancel : .allow
      )
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
      RecentAccountPages.discardView(webView)
      RecentAccountPages.noteProcessTermination()
    }
  }

  private struct Entry {
    let view: WKWebView
    /// Retained because WKWebView.navigationDelegate is weak.
    let delegate: ParkedDelegate
    /// Alias as of park time, for task-completion notifications
    /// (0.1.15). May lag the live alias by one park cycle; refreshed
    /// whenever the account is re-parked.
    let alias: String
    let timezone: String?
    let parkedAtMs: Int64
  }

  // Park-ordered (oldest first = least recently active); eviction takes
  // from the front, so the least-recently-active space goes first.
  private static var pages: [(key: (providerId: String, uuid: String), entry: Entry)] = []
  private static var memoryObserverInstalled = false
  /// Retained: NotificationCenter does not retain block-observer tokens,
  /// and dropping the token would unregister the observation immediately.
  private static var memoryObserver: NSObjectProtocol?

  private static let isolateEgressKey = "diehao.isolate-same-egress"
  private static let quotaMinutesKey = "diehao.quota-sync-minutes"

  static func networkPreferences(isolate: Bool?, quotaMinutes: Int?) -> [String: Any] {
    let preferences = UserDefaults.standard
    if let quotaMinutes, [0, 5, 15, 30].contains(quotaMinutes) { preferences.set(quotaMinutes, forKey: quotaMinutesKey) }
    if let isolate {
      preferences.set(isolate, forKey: isolateEgressKey)
      if isolate {
        // Retain the most recently used page of each known outbound group.
        var retained = Set<EgressGroup>()
        for candidate in pages.reversed() {
          if let group = egress(candidate.key.providerId, candidate.key.uuid), retained.insert(group).inserted { continue }
          release(candidate.key.providerId, candidate.key.uuid)
        }
      }
    }
    return ["isolate":preferences.bool(forKey: isolateEgressKey), "quotaMinutes":preferences.integer(forKey: quotaMinutesKey)]
  }

  private struct EgressGroup: Hashable { let provider: String; let host: String?; let port: UInt16? }

  private static func egress(_ provider: String, _ uuid: String) -> EgressGroup? {
    switch AccountProxyResolver.resolve(uuid: uuid) {
    case .direct: return EgressGroup(provider: provider, host: nil, port: nil)
    case .use(let config): return EgressGroup(provider: provider, host: config.host.lowercased(), port: config.port)
    case .blocked: return nil
    }
  }

  private static func release(_ provider: String, _ uuid: String) {
    guard let index = pages.firstIndex(where: { $0.key.providerId == provider && $0.key.uuid == uuid }) else { return }
    let view = pages.remove(at: index).entry.view
    _ = AccountProxyQuarantine.engage(uuid: uuid)
    destroy(view)
  }

  private static func releasePeers(of spec: IosAccountSessionSpec) {
    guard UserDefaults.standard.bool(forKey: isolateEgressKey), let group = egress(spec.providerId, spec.uuid) else { return }
    let peers = pages.filter { $0.key.uuid != spec.uuid && egress($0.key.providerId, $0.key.uuid) == group }
    for peer in peers { release(peer.key.providerId, peer.key.uuid) }
  }

  private static func ensureMemoryObserver() {
    guard !memoryObserverInstalled else { return }
    memoryObserverInstalled = true
    // 0.1.8: a memory warning used to wipe the whole cache. Halve it
    // instead (LRU first) — pressure usually passes after one or two
    // warnings, and wiping everything on the first warning threw away
    // exactly the cache the user asked to keep.
    memoryObserver = NotificationCenter.default.addObserver(
      forName: UIApplication.didReceiveMemoryWarningNotification,
      object: nil,
      queue: .main
    ) { _ in trim(max(0, pages.count / 2)) }
  }

  private static func isReusable(_ view: WKWebView, spec: IosAccountSessionSpec) -> Bool {
    guard let url = view.url,
      let entryURL = URL(string: spec.entryUrl),
      let entryHost = entryURL.host
    else { return false }
    return (entryURL.path.isEmpty || entryURL.path == "/" || entryURL.path == url.path)
      && AccountSessionCoordinator.hostMatchesAllowed(url, allowedHosts: [entryHost])
      && AccountSessionCoordinator.hostMatchesAllowed(url, allowedHosts: spec.allowedHosts)
  }

  static func take(spec: IosAccountSessionSpec) -> WKWebView? {
    ensureMemoryObserver()
    releasePeers(of: spec)
    trim(maxAccountPages)
    guard let idx = pages.firstIndex(where: { $0.key.providerId == spec.providerId && $0.key.uuid == spec.uuid })
    else { return nil }
    let entry = pages.remove(at: idx).entry
    trim(maxAccountPages - 1)
    if spec.isColdStart || entry.timezone != spec.timezone || !isReusable(entry.view, spec: spec) {
      destroy(entry.view)
      return nil
    }
    return entry.view
  }

  static func park(_ view: WKWebView, spec: IosAccountSessionSpec) {
    ensureMemoryObserver()
    guard isReusable(view, spec: spec) else {
      destroy(view)
      return
    }
    releasePeers(of: spec)
    let delegate = ParkedDelegate()
    view.navigationDelegate = delegate
    view.uiDelegate = nil
    view.isHidden = true
    let key = (providerId: spec.providerId, uuid: spec.uuid)
    if let idx = pages.firstIndex(where: { $0.key == key }) {
      destroy(pages.remove(at: idx).entry.view)
    }
    pages.append((key: key, entry: Entry(view: view, delegate: delegate, alias: spec.alias,
      timezone: spec.timezone, parkedAtMs: Int64(Date().timeIntervalSince1970 * 1000))))
    trim(maxAccountPages)
    if spec.providerId == "muse" {
      view.evaluateJavaScript(TaskWatcher.taskStateScript) { [weak view] result, _ in
        guard let view, pages.contains(where: { $0.key == key && $0.entry.view === view }) else { return }
        TaskWatcher.shared.noteSample(providerId: spec.providerId, uuid: spec.uuid, result: result)
      }
    }
  }

  /// Snapshot of parked accounts (provider, uuid, alias) for the task
  /// watcher sweep (0.1.15). A key listed here may be evicted before
  /// the sweep probes it; probeParked then returns nil, which the
  /// watcher treats as no sample.
  static func parkedKeys() -> [(providerId: String, uuid: String, alias: String)] {
    trim(maxAccountPages)
    return pages.map {
      (providerId: $0.key.providerId, uuid: $0.key.uuid, alias: $0.entry.alias)
    }
  }

  static func discardProfile(_ uuid: String) {
    let doomed = pages.filter { $0.key.uuid == uuid }
    pages.removeAll { $0.key.uuid == uuid }
    for item in doomed {
      destroy(item.entry.view)
    }
    trim(maxAccountPages)
  }

  static func dataStore(providerId: String, uuid: String) -> WKWebsiteDataStore? {
    pages.first { $0.key.providerId == providerId && $0.key.uuid == uuid }?.entry.view.configuration.websiteDataStore
  }

  static func museCatchUpTargets() -> [[String: Any]] {
    pages.reversed().compactMap { item in
      guard item.key.providerId == "muse" else { return nil }
      var target: [String: Any] = ["uuid": item.key.uuid, "parkedAtMs": item.entry.parkedAtMs]
      if let url = item.entry.view.url, url.scheme == "https",
         ["muse.ai", "www.muse.ai"].contains(url.host ?? "") {
        if url.path.isEmpty || url.path == "/" {
          target["primary"] = true
        } else if url.pathComponents.count == 3, url.pathComponents[1] == "thread",
                  let thread = UUID(uuidString: url.pathComponents[2]) {
          target["threadId"] = thread.uuidString.lowercased()
        }
      }
      return target
    }
  }

  /// Drops every parked page. Used when memory is tight or the active web
  /// content process was just terminated: inactive spaces are released
  /// first, before the active page is reloaded (batch-1 eviction order).
  static func clearAll() {
    trim(0)
  }

  /// Runs JavaScript on a parked page without taking it out of the cache.
  /// Used by background quota sync: a parked page already holds the
  /// account's live session, so probing it is free (no reload).
  /// The completion receives the raw evaluateJavaScript result, or nil when
  /// the account has no parked page. Main-thread only. Probing does not
  /// refresh recency — only user activity (take/park) does.
  static func probeParked(
    providerId: String, uuid: String, javascript: String,
    completion: @escaping (Any?) -> Void
  ) {
    trim(maxAccountPages)
    guard
      let entry = pages.first(where: {
        $0.key.providerId == providerId && $0.key.uuid == uuid
      })?.entry
    else {
      completion(nil)
      return
    }
    entry.view.evaluateJavaScript(javascript) { result, _ in
      completion(result)
    }
  }

  private static func discardView(_ view: WKWebView) {
    guard let idx = pages.firstIndex(where: { $0.entry.view === view }) else { return }
    destroy(pages.remove(at: idx).entry.view)
  }

  private static func destroy(_ view: WKWebView) {
    view.stopLoading()
    view.navigationDelegate = nil
    view.uiDelegate = nil
    view.removeFromSuperview()
  }

  /// Enforces the cap, least-recently-active first. No idle expiry:
  /// a parked page stays until the cap or memory pressure evicts it.
  private static func trim(_ limit: Int) {
    while pages.count > limit {
      destroy(pages.removeFirst().entry.view)
    }
  }
}
