// QuotaSyncRunner.swift — batch-1: background quota sync for iOS.
//
// The Rust silent-sync path is desktop-only (it evals JS in Tauri warm
// webviews, which iOS doesn't have). This runner is the iOS equivalent:
// for each account NOT currently open, it either probes the account's
// parked WKWebView (cheap — the page is already loaded) or spins up a
// hidden WKWebView with the account's data store, loads the entry URL,
// runs the identity probe once, and destroys it.
//
// One account at a time, each capped at ~20s, so a sync never hammers the
// device. Results go back to Rust in the invoke response; Rust applies
// them (quota / identity / logged-out) and emits one snapshot.
//
// Main-thread only (WKWebView). If a sync is already running, a second
// call resolves immediately with no results — the in-flight sync covers it.

import Foundation
import WebKit

enum QuotaSyncRunner {

  struct Target {
    let providerId: String
    let uuid: String
    let entryUrl: String
  }

  struct SyncResult {
    let providerId: String
    let uuid: String
    let identity: String
    let avatarUrl: String?
    let quotaText: String?
    let quotaPercent: Int?
    let quotaDetailJson: String?
    let loggedOut: Bool

    var json: [String: Any] {
      var obj: [String: Any] = [
        "providerId": providerId,
        "uuid": uuid,
        "identity": identity,
        "loggedOut": loggedOut,
      ]
      if let avatarUrl, !avatarUrl.isEmpty { obj["avatarUrl"] = avatarUrl }
      if let quotaText, !quotaText.isEmpty { obj["quotaText"] = quotaText }
      if let quotaPercent { obj["quotaPercent"] = quotaPercent }
      if let quotaDetailJson, !quotaDetailJson.isEmpty {
        obj["quotaDetailJson"] = quotaDetailJson
      }
      return obj
    }
  }

  private static let loadTimeout: TimeInterval = 15
  private static let settleDelay: TimeInterval = 2.5

  private static var running = false
  // Retained for the lifetime of one hidden load (WKWebView's delegates
  // are weak, so nothing else would keep the loader alive).
  private static var currentLoader: SyncPageLoader?

  static func run(targets: [Target], completion: @escaping ([SyncResult]) -> Void) {
    guard !running else {
      completion([])
      return
    }
    guard !targets.isEmpty else {
      completion([])
      return
    }
    running = true
    var results: [SyncResult] = []
    results.reserveCapacity(targets.count)

    func step(_ index: Int) {
      guard index < targets.count else {
        running = false
        currentLoader = nil
        completion(results)
        return
      }
      let target = targets[index]
      // Cheap path first: a parked page already holds the live session.
      RecentAccountPages.probeParked(
        providerId: target.providerId, uuid: target.uuid,
        javascript: Scripts.identityProbeScript
      ) { raw in
        if let raw {
          if let parsed = parseProbeOutput(raw) {
            results.append(
              SyncResult(
                providerId: target.providerId, uuid: target.uuid,
                identity: parsed.email, avatarUrl: parsed.avatar.nilIfEmpty,
                quotaText: parsed.quotaText.nilIfEmpty,
                quotaPercent: parsed.quotaPct,
                quotaDetailJson: parsed.quotaDetailJson.nilIfEmpty,
                loggedOut: parsed.loggedOut
              ))
          }
          step(index + 1)
          return
        }
        // No parked page: hidden load.
        let loader = SyncPageLoader(target: target) { parsed in
          currentLoader = nil
          if let parsed {
            results.append(
              SyncResult(
                providerId: target.providerId, uuid: target.uuid,
                identity: parsed.email, avatarUrl: parsed.avatar.nilIfEmpty,
                quotaText: parsed.quotaText.nilIfEmpty,
                quotaPercent: parsed.quotaPct,
                quotaDetailJson: parsed.quotaDetailJson.nilIfEmpty,
                loggedOut: parsed.loggedOut
              ))
          }
          step(index + 1)
        }
        currentLoader = loader
        loader.start()
      }
    }

    step(0)
  }

  // MARK: - Probe parsing (same JSON shape as AccountWebViewController)

  private struct ProbeData {
    let auth: Bool
    let loggedOut: Bool
    let email: String
    let avatar: String
    let quotaText: String
    let quotaPct: Int?
    let quotaDetailJson: String
  }

  private static func parseProbeOutput(_ result: Any?) -> ProbeData? {
    guard let jsonString = result as? String,
      let data = jsonString.data(using: .utf8),
      let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
    else { return nil }
    let pctRaw = obj["quotaPct"] as? Int ?? -1
    return ProbeData(
      auth: obj["auth"] as? Bool ?? false,
      loggedOut: obj["loggedOut"] as? Bool ?? false,
      email: obj["email"] as? String ?? "",
      avatar: obj["avatar"] as? String ?? "",
      quotaText: obj["quotaText"] as? String ?? "",
      quotaPct: (0...100).contains(pctRaw) ? pctRaw : nil,
      quotaDetailJson: obj["quotaDetail"] as? String ?? ""
    )
  }

  // MARK: - Hidden one-shot page loader

  private final class SyncPageLoader: NSObject, WKNavigationDelegate {
    private let target: Target
    private let completion: (ProbeData?) -> Void
    private var webView: WKWebView?
    private var timeoutWork: DispatchWorkItem?
    private var finished = false

    init(target: Target, completion: @escaping (ProbeData?) -> Void) {
      self.target = target
      self.completion = completion
    }

    func start() {
      guard let identifier = UUID(uuidString: target.uuid),
        let entryURL = URL(string: target.entryUrl)
      else {
        finish(nil)
        return
      }
      let config = WKWebViewConfiguration()
      let store = WKWebsiteDataStore(forIdentifier: identifier)
      config.websiteDataStore = store
      // #11/#16：后台额度同步也是账号出站流量——启用代理的账号必须走代理；
      // 代理不可用（不支持 / 缺口令 / 配置损坏）则放弃本次探测，
      // 绝不回退直连拿一份「看起来正常」的额度。
      if let proxy = AccountProxyStore.config(for: target.uuid), proxy.enabled {
        let result = AccountProxyApplier.apply(cfg: proxy, uuid: target.uuid, to: store)
        if result.blockReason != nil {
          finish(nil)
          return
        }
      }
      config.defaultWebpagePreferences.allowsContentJavaScript = true
      config.preferences.javaScriptCanOpenWindowsAutomatically = false
      config.userContentController.addUserScript(
        WKUserScript(
          source: Scripts.quotaHookInitScript,
          injectionTime: .atDocumentStart,
          forMainFrameOnly: true
        )
      )
      let view = WKWebView(frame: .zero, configuration: config)
      view.navigationDelegate = self
      webView = view

      let timeout = DispatchWorkItem { [weak self] in
        // Load stuck: probe anyway — cached quota in localStorage may
        // still be readable even when the page never finished.
        self?.probeAndFinish()
      }
      timeoutWork = timeout
      DispatchQueue.main.asyncAfter(deadline: .now() + QuotaSyncRunner.loadTimeout, execute: timeout)
      view.load(URLRequest(url: entryURL))
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
      // Let the page's own API traffic run through the quota hooks for a
      // moment before reading the captured values.
      DispatchQueue.main.asyncAfter(deadline: .now() + QuotaSyncRunner.settleDelay) { [weak self] in
        self?.probeAndFinish()
      }
    }

    func webView(
      _ webView: WKWebView,
      didFail navigation: WKNavigation!,
      withError error: Error
    ) {
      probeAndFinish()
    }

    func webView(
      _ webView: WKWebView,
      didFailProvisionalNavigation navigation: WKNavigation!,
      withError error: Error
    ) {
      probeAndFinish()
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
      finish(nil)
    }

    private func probeAndFinish() {
      guard !finished else { return }
      guard let view = webView else {
        finish(nil)
        return
      }
      // The probe callback is the normal path, but never let a stuck
      // webview hang the whole sync: a 10s fallback finishes with nil.
      timeoutWork?.cancel()
      let fallback = DispatchWorkItem { [weak self] in
        self?.finish(nil)
      }
      timeoutWork = fallback
      DispatchQueue.main.asyncAfter(deadline: .now() + 10, execute: fallback)
      view.evaluateJavaScript(Scripts.identityProbeScript) { [weak self] result, _ in
        guard let self, !self.finished else { return }
        let parsed = QuotaSyncRunner.parseProbeOutput(result)
        self.finish(parsed)
      }
    }

    private func finish(_ parsed: ProbeData?) {
      guard !finished else { return }
      finished = true
      timeoutWork?.cancel()
      timeoutWork = nil
      teardown()
      completion(parsed)
    }

    private func teardown() {
      webView?.stopLoading()
      webView?.navigationDelegate = nil
      webView?.removeFromSuperview()
      webView = nil
    }
  }
}

private extension String {
  var nilIfEmpty: String? { isEmpty ? nil : self }
}
