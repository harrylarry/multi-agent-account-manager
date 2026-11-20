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
  private static let maxAccountPages = 2
  private static let idleTimeout: TimeInterval = 120

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
