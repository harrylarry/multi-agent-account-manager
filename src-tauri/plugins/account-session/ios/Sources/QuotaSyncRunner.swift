// QuotaSyncRunner.swift — batch-1: background quota sync for iOS.
//
// The Rust silent-sync path is desktop-only (it evals JS in Tauri warm
// webviews, which iOS doesn't have). This runner is the iOS equivalent:
// for each account NOT currently open, it either probes the account's
// parked WKWebView (cheap — the page is already loaded) or spins up a
// hidden WKWebView with the account's data store, loads the entry URL,
// reads identity and quota, then destroys it. An explicit diagnostic
// uses a new navigation and excludes previously cached quota.
//
// One account at a time, with separate navigation and probe deadlines, so a sync never hammers the
// device. Results go back to Rust in the invoke response; Rust applies
// them (quota / identity / logged-out) and emits one snapshot.
//
// Main-thread only (WKWebView). If a sync is already running, a second
// call resolves immediately with no results — the in-flight sync covers it.

import Foundation
import UIKit
import WebKit

enum QuotaSyncRunner {

  struct Target {
    let providerId: String
    let uuid: String
    let entryUrl: String
    let timezone: String?
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
    let authenticated: Bool
    let observation: String

    var json: [String: Any] {
      var obj: [String: Any] = [
        "providerId": providerId,
        "uuid": uuid,
        "identity": identity,
        "loggedOut": loggedOut,
        "authenticated": authenticated,
        "observation": observation,
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

  static func run(targets: [Target], fresh: Bool, completion: @escaping ([SyncResult]) -> Void) {
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
      func completeTarget(_ observation: String, _ parsed: ProbeData?) {
        results.append(SyncResult(
          providerId: target.providerId, uuid: target.uuid,
          identity: parsed?.email ?? "", avatarUrl: parsed?.avatar.nilIfEmpty,
          quotaText: parsed?.quotaText.nilIfEmpty, quotaPercent: parsed?.quotaPct,
          quotaDetailJson: parsed?.quotaDetailJson.nilIfEmpty,
          loggedOut: parsed?.loggedOut ?? false, authenticated: parsed?.auth ?? false,
          observation: observation
        ))
        step(index + 1)
      }
      // #11/#16（补 W）：出站策略必须先解析。配置损坏 / 该账号还没有可信
      // 配置时直接跳过本次同步——宁可没有额度，也不能绕过代理拿一份
      // 「看起来正常」的数据。
      if AccountProxyResolver.resolve(uuid: target.uuid).blockReason != nil {
        completeTarget("proxy_blocked", nil)
        return
      }
      func loadHidden() {
        let loader = SyncPageLoader(target: target, fresh: fresh) { observation, parsed in
          currentLoader = nil
          completeTarget(observation, parsed)
        }
        currentLoader = loader
        loader.start()
      }
      if fresh {
        loadHidden()
        return
      }
      // Cheap path first: a parked page already holds the live session.
      RecentAccountPages.probeParked(
        providerId: target.providerId, uuid: target.uuid,
        javascript: Scripts.identityProbeScript
      ) { raw in
        if let raw {
          completeTarget("parked", parseProbeOutput(raw))
          return
        }
        loadHidden()
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
    private let fresh: Bool
    private let completion: (String, ProbeData?) -> Void
    private var webView: WKWebView?
    private var renderHost: UIView?
    private var committed = false
    private var timeoutWork: DispatchWorkItem?
    private var finished = false
    private var probeDeadline = Date.distantPast

    init(target: Target, fresh: Bool, completion: @escaping (String, ProbeData?) -> Void) {
      self.target = target
      self.fresh = fresh
      self.completion = completion
    }

    func start() {
      guard let identifier = UUID(uuidString: target.uuid),
        let entryURL = URL(string: target.entryUrl)
      else {
        finish("setup_failed")
        return
      }
      guard let window = UIApplication.shared.connectedScenes
        .compactMap({ $0 as? UIWindowScene })
        .filter({ $0.activationState == .foregroundActive })
        .compactMap({ $0.keyWindow })
        .first
      else {
        finish("no_foreground_window")
        return
      }
      let config = WKWebViewConfiguration()
      let store = WKWebsiteDataStore(forIdentifier: identifier)
      config.websiteDataStore = store
      // #11/#16：后台额度同步也是账号出站流量——必须走与可见会话**同一个**
      // 解析入口。损坏/缺失/缺口令/不支持都放弃本次探测，绝不回退直连。
      if AccountProxyResolver.apply(uuid: target.uuid, to: store) != nil {
        finish("proxy_blocked")
        return
      }
      config.defaultWebpagePreferences.allowsContentJavaScript = true
      config.preferences.javaScriptCanOpenWindowsAutomatically = false
      AccountTimeZone.install(target.timezone, into: config)
      config.userContentController.addUserScript(
        WKUserScript(
          source: "window.__maamRequireFreshQuota = true;\n" + Scripts.quotaHookInitScript,
          injectionTime: .atDocumentStart,
          forMainFrameOnly: true
        )
      )
      // Give WebKit an attached viewport so document rendering and quota
      // bootstrap can run. Clip it to a non-interactive point under app content.
      let host = UIView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
      host.clipsToBounds = true
      host.isUserInteractionEnabled = false
      host.accessibilityElementsHidden = true
      host.alpha = 0.02
      let view = WKWebView(frame: CGRect(origin: .zero, size: window.bounds.size), configuration: config)
      view.navigationDelegate = self
      host.addSubview(view)
      window.insertSubview(host, at: 0)
      webView = view
      renderHost = host

      let timeout = DispatchWorkItem { [weak self] in
        self?.finish("navigation_commit_timeout")
      }
      timeoutWork = timeout
      DispatchQueue.main.asyncAfter(deadline: .now() + QuotaSyncRunner.loadTimeout, execute: timeout)
      view.load(URLRequest(url: entryURL, cachePolicy: fresh ? .reloadIgnoringLocalCacheData : .useProtocolCachePolicy))
    }

    func webView(_ webView: WKWebView, decidePolicyFor navigationResponse: WKNavigationResponse,
                 decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void) {
      if navigationResponse.isForMainFrame, let response = navigationResponse.response as? HTTPURLResponse,
         !(200...299).contains(response.statusCode) {
        decisionHandler(.cancel)
        finish("http_error_\(response.statusCode)")
      } else {
        decisionHandler(.allow)
      }
    }

    func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
      guard !finished, !committed, webView.url?.scheme == "https" else { return }
      committed = true
      timeoutWork?.cancel()
      probeDeadline = Date().addingTimeInterval(12)
      let timeout = DispatchWorkItem { [weak self] in self?.finish("probe_timeout") }
      timeoutWork = timeout
      DispatchQueue.main.asyncAfter(deadline: .now() + 12, execute: timeout)
      // Start after the document commits. Subresources and long-lived page
      // requests need not finish before quota becomes available.
      DispatchQueue.main.asyncAfter(deadline: .now() + QuotaSyncRunner.settleDelay) { [weak self] in
        self?.probeAndFinish()
      }
    }

    func webView(
      _ webView: WKWebView,
      didFail navigation: WKNavigation!,
      withError error: Error
    ) {
      finish(navigationError(error, stage: "committed"))
    }

    func webView(
      _ webView: WKWebView,
      didFailProvisionalNavigation navigation: WKNavigation!,
      withError error: Error
    ) {
      finish(navigationError(error, stage: "provisional"))
    }

    private func navigationError(_ error: Error, stage: String) -> String {
      let cause = error as NSError
      let code = cause.domain == NSURLErrorDomain ? "_url_\(cause.code)" : ""
      return "navigation_\(stage)_failed\(code)"
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
      finish("webview_terminated")
    }

    private func probeAndFinish() {
      guard !finished else { return }
      guard let view = webView else {
        finish("setup_failed")
        return
      }
      let script = "if (document.readyState !== 'loading') {\n" + Scripts.identityProbeScript + "\n}"
      view.evaluateJavaScript(script) { [weak self] result, _ in
        guard let self, !self.finished else { return }
        let parsed = QuotaSyncRunner.parseProbeOutput(result)
        if parsed?.loggedOut == true || parsed?.quotaPct != nil ||
           parsed?.quotaDetailJson.isEmpty == false {
          self.finish("loaded", parsed)
          return
        }
        if Date().addingTimeInterval(1) >= self.probeDeadline {
          self.finish("loaded", parsed)
        } else {
          DispatchQueue.main.asyncAfter(deadline: .now() + 0.75) { [weak self] in self?.probeAndFinish() }
        }
      }
    }

    private func finish(_ observation: String, _ parsed: ProbeData? = nil) {
      guard !finished else { return }
      finished = true
      timeoutWork?.cancel()
      timeoutWork = nil
      teardown()
      completion(observation, parsed)
    }

    private func teardown() {
      webView?.stopLoading()
      webView?.navigationDelegate = nil
      webView?.removeFromSuperview()
      webView = nil
      renderHost?.removeFromSuperview()
      renderHost = nil
    }
  }
}

private extension String {
  var nilIfEmpty: String? { isEmpty ? nil : self }
}
