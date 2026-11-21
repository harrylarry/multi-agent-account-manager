// RecentAccountPages.swift — iOS port of RecentAccountPages.kt.
//
// Keeps up to 2 recently used account WKWebViews parked (per provider+uuid)
// so switching back to an account is instant. Entries expire after 120s idle.
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
  // Batch-1 optimization (2026-11-21): 2 pages / 120s was too small — a
  // third account always reloaded and the cache expired after a short hop
  // to another app. 4 pages / 300s, still guarded by the memory-warning
  // trim below.
  private static let maxAccountPages = 4
  private static let idleTimeout: TimeInterval = 300

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
    }
  }

  private struct Entry {
    let view: WKWebView
    /// Retained because WKWebView.navigationDelegate is weak.
    let delegate: ParkedDelegate
    let expiresAt: Date
  }

  // Insertion-ordered (oldest first); max 2 entries so a plain array is fine.
  private static var pages: [(key: (providerId: String, uuid: String), entry: Entry)] = []
  private static var expireTimer: Timer?
  private static var memoryObserverInstalled = false
  /// Retained: NotificationCenter does not retain block-observer tokens,
  /// and dropping the token would unregister the observation immediately.
  private static var memoryObserver: NSObjectProtocol?

  private static func ensureMemoryObserver() {
    guard !memoryObserverInstalled else { return }
    memoryObserverInstalled = true
    memoryObserver = NotificationCenter.default.addObserver(
      forName: UIApplication.didReceiveMemoryWarningNotification,
      object: nil,
      queue: .main
    ) { _ in trim(0) }
  }

  private static func isReusable(_ view: WKWebView, spec: IosAccountSessionSpec) -> Bool {
    guard let url = view.url,
      let entryHost = URL(string: spec.entryUrl)?.host
    else { return false }
    return AccountSessionCoordinator.hostMatchesAllowed(url, allowedHosts: [entryHost])
      && AccountSessionCoordinator.hostMatchesAllowed(url, allowedHosts: spec.allowedHosts)
  }

  static func take(spec: IosAccountSessionSpec) -> WKWebView? {
    ensureMemoryObserver()
    trim(maxAccountPages)
    guard let idx = pages.firstIndex(where: { $0.key.providerId == spec.providerId && $0.key.uuid == spec.uuid })
    else { return nil }
    let entry = pages.remove(at: idx).entry
    trim(maxAccountPages - 1)
    if spec.isColdStart || !isReusable(entry.view, spec: spec) {
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
    let delegate = ParkedDelegate()
    view.navigationDelegate = delegate
    view.uiDelegate = nil
    view.isHidden = true
    let key = (providerId: spec.providerId, uuid: spec.uuid)
    if let idx = pages.firstIndex(where: { $0.key == key }) {
      destroy(pages.remove(at: idx).entry.view)
    }
    pages.append((
      key: key,
      entry: Entry(
        view: view,
        delegate: delegate,
        expiresAt: Date().addingTimeInterval(idleTimeout)
      )
    ))
    trim(maxAccountPages)
  }

  static func discardProfile(_ uuid: String) {
    let doomed = pages.filter { $0.key.uuid == uuid }
    pages.removeAll { $0.key.uuid == uuid }
    for item in doomed {
      destroy(item.entry.view)
    }
    trim(maxAccountPages)
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
  /// the account has no parked page. Main-thread only.
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

  private static func trim(_ limit: Int) {
    expireTimer?.invalidate()
    expireTimer = nil
    while let first = pages.first {
      let remaining = first.entry.expiresAt.timeIntervalSinceNow
      if pages.count <= limit && remaining > 0 {
        expireTimer = Timer.scheduledTimer(
          withTimeInterval: remaining,
          repeats: false
        ) { _ in trim(maxAccountPages) }
        return
      }
      destroy(pages.removeFirst().entry.view)
    }
  }
}
