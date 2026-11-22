// AccountWebViewController.swift — iOS port of AccountWebActivity.kt.
//
// Presents one account's web session full-screen: top bar ("← 账号" + title),
// a WKWebView bound to a per-account WKWebsiteDataStore (identifier = uuid),
// an auth popup overlay for window.open flows, and the identity probe loop.
//
// Mapping notes (Android -> iOS):
// - Activity lifecycle -> UIViewController: onResume/onPause become
//   viewDidAppear/viewWillDisappear (probe loop start/stop); onDestroy's
//   activityClosed emission happens in viewDidDisappear when isBeingDismissed,
//   which also covers the swipe-to-dismiss gesture.
// - WebViewClient.onPageCommitVisible has no WKWebView equivalent;
//   WKNavigationDelegate.webView(_:didCommit:) is the closest analog
//   (content started arriving for the main frame) and is used in its place.
// - File chooser: WKWebView presents the system document picker natively,
//   so there is no onShowFileChooser equivalent to port.
// - Document-start script: WKUserScript cannot be scoped by URL patterns
//   (Android scopes to muse.ai / hark.com); the script itself gates its
//   quota capture on those hosts (isPrimaryHost), so it is injected
//   forMainFrameOnly at document start for all hosts.
// - There is no Activity-recreate path on iOS (see Coordinator).

import SafariServices
import UIKit
import WebKit

final class AccountWebViewController: UIViewController, WKNavigationDelegate, WKUIDelegate, SFSafariViewControllerDelegate {

  // MARK: - UI

  private var headerTitleLabel: UILabel!
  private var mainWebContainer: UIView!
  private var loadingIndicator: UIActivityIndicatorView!
  private var popupOverlayRoot: UIView!
  private var popupDomainLabel: UILabel!
  private var popupWebContainer: UIView!
  // Batch-1: thin load-progress bar under the top bar.
  private var progressView: UIProgressView!
  // 0.1.16: aggregate unread dot on the "← 账号" button.
  private var unreadDotView: UIView?
  private var progressObservation: NSKeyValueObservation?

  // MARK: - Session state

  private var currentSpec: IosAccountSessionSpec?
  private var mainWebView: WKWebView?
  private var committedAuthority: String?
  private var mainLoadFailed = false
  private var popupWebView: WKWebView?
  // 0.1.11: set when the popup navigates to a Google OAuth host; the
  // popup auto-closes (and the session reloads) once it lands back on
  // an allowlisted host — the OAuth callback.
  private var popupDidOAuth = false
  // 0.1.12: set when the OAuth callback navigation has been reloaded
  // programmatically once; keeps the cancel+reload intercept from
  // looping on its own reload.
  private var popupCallbackReloaded = false
  // 0.1.13: set once the OAuth callback page has loaded in the popup;
  // the popup only closes after the flow moves on (a post-callback
  // page, window.close, or the timeout below) — never on the callback
  // load itself, which killed the completion in 0.1.12.
  private var popupCallbackLoaded = false
  // 0.1.13: closes a callback popup that never navigates on nor closes
  // itself (8s after the callback page loads).
  private var popupOAuthTimeout: Timer?
  // 0.1.13: reloads the session a short while after an OAuth popup
  // closes, so any in-flight completion on the main page can settle.
  private var postOAuthReloadTimer: Timer?
  private var mainInputEnabled = false
  private var popupInputEnabled = false
  private var closingFromManager = false
  private var capturedIdentityHint: String?
  private var probeTimer: Timer?
  private var probeGeneration = 0
  private var consecutiveLoggedOut = 0
  private var loggedOutReported = false
  // Batch-1 probe backoff: while identity + quota are stable, the probe
  // interval relaxes from 4.5s to 30s; any change snaps it back to 4.5s.
  private var probeInterval: TimeInterval = 4.5
  private var lastProbeSignature: String?
  private var stableProbeCount = 0
  // Batch-1 termination recovery: reload in place on the first kill, and
  // only give up (back to the manager) if the process dies twice within
  // 60 seconds.
  private var contentProcessCrashCount = 0
  private var lastContentProcessCrash: Date?

  private struct ProbeData {
    let auth: Bool
    let loggedOut: Bool
    let typed: String
    let email: String
    let avatar: String
    let quotaText: String
    let quotaPct: Int?
    let quotaDetailJson: String
  }

  init() {
    super.init(nibName: nil, bundle: nil)
    modalPresentationStyle = .fullScreen
    AccountSessionCoordinator.shared.attachViewController(self)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) {
    fatalError("init(coder:) has not been implemented")
  }

  deinit {
    AccountSessionCoordinator.shared.detachViewController(self)
  }

  // MARK: - View hierarchy (mirrors Android's buildNativeHierarchy)

  override func viewDidLoad() {
    super.viewDidLoad()
    view.backgroundColor = UIColor(hex: 0xF7F7F7)

    let topBar = UIView()
    topBar.backgroundColor = UIColor(hex: 0xEDEDED)
    topBar.translatesAutoresizingMaskIntoConstraints = false

    let backButton = UIButton(type: .system)
    backButton.setTitle("← 账号", for: .normal)
    backButton.titleLabel?.font = .systemFont(ofSize: 13)
    backButton.translatesAutoresizingMaskIntoConstraints = false
    backButton.addTarget(self, action: #selector(backTapped), for: .touchUpInside)

    // 0.1.16: aggregate unread dot on the "← 账号" button — shown when
    // any parked account has an unread reply. Initial state comes from
    // the coordinator (Rust pushes updates via setUnreadBadge).
    let unreadDot = UIView()
    unreadDot.backgroundColor = UIColor(hex: 0xEF4444)
    unreadDot.layer.cornerRadius = 4
    unreadDot.translatesAutoresizingMaskIntoConstraints = false
    unreadDot.isHidden = !AccountSessionCoordinator.shared.currentUnreadBadge()
    backButton.addSubview(unreadDot)
    unreadDotView = unreadDot
    NSLayoutConstraint.activate([
      unreadDot.widthAnchor.constraint(equalToConstant: 8),
      unreadDot.heightAnchor.constraint(equalToConstant: 8),
      unreadDot.centerXAnchor.constraint(equalTo: backButton.trailingAnchor, constant: -2),
      unreadDot.centerYAnchor.constraint(equalTo: backButton.topAnchor, constant: 2),
    ])

    headerTitleLabel = UILabel()
    headerTitleLabel.text = "正在核验会话…"
    headerTitleLabel.textColor = UIColor(hex: 0x191919)
    headerTitleLabel.font = .systemFont(ofSize: 15)
    headerTitleLabel.lineBreakMode = .byTruncatingTail
    headerTitleLabel.translatesAutoresizingMaskIntoConstraints = false

    topBar.addSubview(backButton)
    topBar.addSubview(headerTitleLabel)

    let contentFrame = UIView()
    contentFrame.translatesAutoresizingMaskIntoConstraints = false

    loadingIndicator = UIActivityIndicatorView(style: .large)
    loadingIndicator.translatesAutoresizingMaskIntoConstraints = false

    // Batch-1: thin progress bar overlaying the top edge of the content
    // area (Safari-style); hidden when no load is in progress.
    progressView = UIProgressView(progressViewStyle: .bar)
    progressView.translatesAutoresizingMaskIntoConstraints = false
    progressView.progressTintColor = .systemBlue
    progressView.trackTintColor = .clear
    progressView.alpha = 0

    mainWebContainer = UIView()
    mainWebContainer.translatesAutoresizingMaskIntoConstraints = false

    contentFrame.addSubview(loadingIndicator)
    contentFrame.addSubview(mainWebContainer)

    popupOverlayRoot = UIView()
    popupOverlayRoot.backgroundColor = .white
    popupOverlayRoot.isHidden = true
    popupOverlayRoot.translatesAutoresizingMaskIntoConstraints = false

    let popupHeader = UIView()
    popupHeader.backgroundColor = UIColor(hex: 0xF2F5FA)
    popupHeader.translatesAutoresizingMaskIntoConstraints = false

    popupDomainLabel = UILabel()
    popupDomainLabel.text = ""
    popupDomainLabel.textColor = UIColor(hex: 0x111827)
    popupDomainLabel.font = .systemFont(ofSize: 13)
    popupDomainLabel.lineBreakMode = .byTruncatingTail
    popupDomainLabel.translatesAutoresizingMaskIntoConstraints = false

    let closePopupButton = UIButton(type: .system)
    closePopupButton.setTitle("关闭认证", for: .normal)
    closePopupButton.titleLabel?.font = .systemFont(ofSize: 12)
    closePopupButton.translatesAutoresizingMaskIntoConstraints = false
    closePopupButton.addTarget(
      self, action: #selector(closePopupTapped), for: .touchUpInside)

    popupHeader.addSubview(popupDomainLabel)
    popupHeader.addSubview(closePopupButton)

    popupWebContainer = UIView()
    popupWebContainer.translatesAutoresizingMaskIntoConstraints = false

    popupOverlayRoot.addSubview(popupHeader)
    popupOverlayRoot.addSubview(popupWebContainer)

    view.addSubview(topBar)
    view.addSubview(contentFrame)
    view.addSubview(popupOverlayRoot)
    view.addSubview(progressView)

    let safe = view.safeAreaLayoutGuide
    NSLayoutConstraint.activate([
      topBar.topAnchor.constraint(equalTo: safe.topAnchor),
      topBar.leadingAnchor.constraint(equalTo: view.leadingAnchor),
      topBar.trailingAnchor.constraint(equalTo: view.trailingAnchor),
      topBar.heightAnchor.constraint(equalToConstant: 52),

      progressView.topAnchor.constraint(equalTo: topBar.bottomAnchor),
      progressView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
      progressView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
      progressView.heightAnchor.constraint(equalToConstant: 2),

      backButton.leadingAnchor.constraint(equalTo: topBar.leadingAnchor, constant: 10),
      backButton.centerYAnchor.constraint(equalTo: topBar.centerYAnchor),
      backButton.topAnchor.constraint(greaterThanOrEqualTo: topBar.topAnchor, constant: 6),
      backButton.bottomAnchor.constraint(lessThanOrEqualTo: topBar.bottomAnchor, constant: -6),

      headerTitleLabel.leadingAnchor.constraint(equalTo: backButton.trailingAnchor, constant: 10),
      headerTitleLabel.trailingAnchor.constraint(equalTo: topBar.trailingAnchor, constant: -10),
      headerTitleLabel.centerYAnchor.constraint(equalTo: topBar.centerYAnchor),

      contentFrame.topAnchor.constraint(equalTo: topBar.bottomAnchor),
      contentFrame.leadingAnchor.constraint(equalTo: view.leadingAnchor),
      contentFrame.trailingAnchor.constraint(equalTo: view.trailingAnchor),
      contentFrame.bottomAnchor.constraint(equalTo: view.bottomAnchor),

      loadingIndicator.centerXAnchor.constraint(equalTo: contentFrame.centerXAnchor),
      loadingIndicator.centerYAnchor.constraint(equalTo: contentFrame.centerYAnchor),

      mainWebContainer.topAnchor.constraint(equalTo: contentFrame.topAnchor),
      mainWebContainer.leadingAnchor.constraint(equalTo: contentFrame.leadingAnchor),
      mainWebContainer.trailingAnchor.constraint(equalTo: contentFrame.trailingAnchor),
      mainWebContainer.bottomAnchor.constraint(equalTo: contentFrame.bottomAnchor),

      popupOverlayRoot.topAnchor.constraint(equalTo: view.topAnchor),
      popupOverlayRoot.leadingAnchor.constraint(equalTo: view.leadingAnchor),
      popupOverlayRoot.trailingAnchor.constraint(equalTo: view.trailingAnchor),
      popupOverlayRoot.bottomAnchor.constraint(equalTo: view.bottomAnchor),

      popupHeader.topAnchor.constraint(equalTo: safe.topAnchor),
      popupHeader.leadingAnchor.constraint(equalTo: popupOverlayRoot.leadingAnchor),
      popupHeader.trailingAnchor.constraint(equalTo: popupOverlayRoot.trailingAnchor),
      popupHeader.heightAnchor.constraint(equalToConstant: 48),

      popupDomainLabel.leadingAnchor.constraint(equalTo: popupHeader.leadingAnchor, constant: 12),
      popupDomainLabel.centerYAnchor.constraint(equalTo: popupHeader.centerYAnchor),

      closePopupButton.leadingAnchor.constraint(
        equalTo: popupDomainLabel.trailingAnchor, constant: 8),
      closePopupButton.trailingAnchor.constraint(
        equalTo: popupHeader.trailingAnchor, constant: -10),
      closePopupButton.centerYAnchor.constraint(equalTo: popupHeader.centerYAnchor),

      popupWebContainer.topAnchor.constraint(equalTo: popupHeader.bottomAnchor),
      popupWebContainer.leadingAnchor.constraint(equalTo: popupOverlayRoot.leadingAnchor),
      popupWebContainer.trailingAnchor.constraint(equalTo: popupOverlayRoot.trailingAnchor),
      popupWebContainer.bottomAnchor.constraint(equalTo: popupOverlayRoot.bottomAnchor),
    ])
  }

  override func viewDidAppear(_ animated: Bool) {
    super.viewDidAppear(animated)
    if mainInputEnabled, let spec = currentSpec {
      startIdentityProbeLoop(spec: spec)
    }
  }

  override func viewWillDisappear(_ animated: Bool) {
    stopIdentityProbeLoop()
    super.viewWillDisappear(animated)
  }

  override func viewDidDisappear(_ animated: Bool) {
    super.viewDidDisappear(animated)
    // Covers swipe-to-dismiss as well as programmatic dismiss. The managed
    // close paths set closingFromManager and already released the views, so
    // this is a no-op for them (currentSpec is nil by then).
    guard isBeingDismissed else { return }
    let spec = currentSpec
    releaseSessionViews()
    AccountSessionCoordinator.shared.detachViewController(self)
    if !closingFromManager, let spec {
      AccountSessionCoordinator.shared.emitActivityClosed(
        switchSeq: spec.switchSeq, viewInstanceId: spec.viewInstanceId)
    }
  }

  @objc private func backTapped() {
    if popupWebView != nil {
      destroyPopupWebView()
    } else {
      returnToAccountList()
    }
  }

  @objc private func closePopupTapped() {
    destroyPopupWebView()
  }

  /// 0.1.16: shows/hides the aggregate unread dot on the "← 账号"
  /// button. No-op before the view loads (viewDidLoad applies the
  /// stored state when the dot is created).
  func setUnreadDotVisible(_ visible: Bool) {
    unreadDotView?.isHidden = !visible
  }

  // MARK: - Session binding

  func bindSessionSpec(_ spec: IosAccountSessionSpec) {
    // The plugin binds a fresh controller before presenting it; force the
    // view to load so the outlets below are initialized.
    _ = view
    releaseSessionViews(keepPage: true)

    currentSpec = spec
    mainInputEnabled = false
    mainLoadFailed = false
    headerTitleLabel.text = "正在核验会话…"
    loadingIndicator.startAnimating()

    // No MULTI_PROFILE capability check on iOS: WKWebsiteDataStore identifiers
    // are available on every supported device (iOS 17+).
    let cached = RecentAccountPages.take(spec: spec)
    let webView: WKWebView
    if let cached {
      webView = cached
    } else {
      let config = WKWebViewConfiguration()
      // Per-account isolated data store; identifier == account uuid.
      // WKWebsiteDataStore(forIdentifier:) requires iOS 17+ and takes a UUID,
      // so parse the (Rust-normalized) uuid string instead of passing it raw.
      guard let identifier = UUID(uuidString: spec.uuid) else {
        mainLoadFailed = true
        headerTitleLabel.text = "会话标识无效"
        loadingIndicator.stopAnimating()
        return
      }
      config.websiteDataStore = WKWebsiteDataStore(forIdentifier: identifier)
      DataStoreRegistry.markCreated(spec.uuid)
      config.defaultWebpagePreferences.allowsContentJavaScript = true
      config.preferences.javaScriptCanOpenWindowsAutomatically = false
      // Batch-1: on iPhone WKWebView plays every <video> fullscreen by
      // default, which breaks avatar videos (muted autoplay) and leaves
      // no working close control. Inline playback keeps video in-page.
      config.allowsInlineMediaPlayback = true
      config.userContentController.addUserScript(
        WKUserScript(
          source: Scripts.quotaHookInitScript,
          injectionTime: .atDocumentStart,
          forMainFrameOnly: true
        )
      )
      webView = WKWebView(frame: .zero, configuration: config)
    }

    webView.isHidden = true
    webView.isUserInteractionEnabled = false
    webView.navigationDelegate = self
    webView.uiDelegate = self

    mainWebView = webView
    mainWebContainer.addSubview(webView)
    webView.translatesAutoresizingMaskIntoConstraints = false
    NSLayoutConstraint.activate([
      webView.topAnchor.constraint(equalTo: mainWebContainer.topAnchor),
      webView.leadingAnchor.constraint(equalTo: mainWebContainer.leadingAnchor),
      webView.trailingAnchor.constraint(equalTo: mainWebContainer.trailingAnchor),
      webView.bottomAnchor.constraint(equalTo: mainWebContainer.bottomAnchor),
    ])

    // Batch-1: drive the progress bar from estimatedProgress.
    progressObservation?.invalidate()
    progressView.progress = 0
    progressView.alpha = cached == nil ? 1 : 0
    progressObservation = webView.observe(
      \.estimatedProgress, options: [.new]
    ) { [weak self] observed, _ in
      guard let self else { return }
      let update = {
        let p = Float(observed.estimatedProgress)
        self.progressView.setProgress(p, animated: true)
        self.progressView.alpha = p >= 1.0 ? 0 : 1
      }
      if Thread.isMainThread {
        update()
      } else {
        DispatchQueue.main.async(execute: update)
      }
    }

    if let restoredUrl = cached?.url {
      committedAuthority = authority(of: restoredUrl)
      AccountSessionCoordinator.shared.emitPageCommitVisible(spec: spec, url: restoredUrl.absoluteString)
    } else {
      guard let entryURL = URL(string: spec.entryUrl) else {
        AccountSessionCoordinator.shared.emitSessionFailed(
          switchSeq: spec.switchSeq,
          viewInstanceId: spec.viewInstanceId,
          reason: "账号入口地址无效"
        )
        closeAndFinishFromManager()
        return
      }
      webView.load(URLRequest(url: entryURL))
    }
  }

  private func ownsMainView(_ webView: WKWebView, spec: IosAccountSessionSpec) -> Bool {
    webView === mainWebView && currentSpec?.switchSeq == spec.switchSeq
      && currentSpec?.viewInstanceId == spec.viewInstanceId
      && AccountSessionCoordinator.shared.isCurrent(
        switchSeq: spec.switchSeq, viewInstanceId: spec.viewInstanceId)
  }

  func revealCommittedSession(switchSeq: Int64, viewInstanceId: Int64, title: String) {
    guard let spec = currentSpec,
      spec.switchSeq == switchSeq, spec.viewInstanceId == viewInstanceId,
      let webView = mainWebView
    else { return }
    headerTitleLabel.text = title
    loadingIndicator.stopAnimating()
    webView.isHidden = false
    webView.isUserInteractionEnabled = true
    mainInputEnabled = true
    if view.window != nil {
      startIdentityProbeLoop(spec: spec)
    }
  }

  // MARK: - WKNavigationDelegate

  func webView(
    _ webView: WKWebView,
    decidePolicyFor navigationAction: WKNavigationAction,
    decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
  ) {
    guard let spec = currentSpec,
      navigationAction.targetFrame?.isMainFrame == true
    else {
      decisionHandler(.allow)
      return
    }
    let url = navigationAction.request.url

    if webView === popupWebView {
      if url?.absoluteString == "about:blank" {
        decisionHandler(.allow)
        return
      }
      guard AccountSessionCoordinator.shared.isCurrent(
        switchSeq: spec.switchSeq, viewInstanceId: spec.viewInstanceId)
      else {
        decisionHandler(.cancel)
        return
      }
      if let url, AccountSessionCoordinator.isGoogleOAuthHost(url) {
        // 0.1.11: same-store tab model — let Google load in the popup
        // (it shares the session's data store, so Muse's OAuth state
        // validates and the callback lands in the same jar). didFinish
        // waits for the flow to complete before closing the tab (0.1.13).
        popupDidOAuth = true
        popupCallbackReloaded = false
        decisionHandler(.allow)
        return
      }
      if !AccountSessionCoordinator.hostMatchesAllowed(url, allowedHosts: spec.allowedHosts) {
        destroyPopupWebView()
        decisionHandler(.cancel)
        return
      }
      if popupDidOAuth, let url {
        // 0.1.12: the OAuth callback is a universal link (muse.ai):
        // left alone, iOS launches the Muse app instead of delivering
        // the callback to the web page (0.1.11 user test — the password
        // submit's user gesture carries through the redirect). Cancel
        // it and reload programmatically: a programmatic load carries
        // no user gesture, so it stays in the popup and the GET still
        // reaches Muse's server. The flag keeps the intercept from
        // looping on its own reload.
        if !popupCallbackReloaded {
          popupCallbackReloaded = true
          decisionHandler(.cancel)
          webView.load(URLRequest(url: url))
          return
        }
      }
      decisionHandler(.allow)
      return
    }

    // Main web view.
    guard ownsMainView(webView, spec: spec) else {
      decisionHandler(.cancel)
      return
    }
    if let url, AccountSessionCoordinator.isGoogleOAuthHost(url) {
      // 0.1.11: same-store tab model — open Google in a popup tab
      // sharing the session's data store instead of navigating the
      // session page away. (The usual path is window.open, which goes
      // through createWebViewWith; this covers same-frame navigation.)
      if popupWebView == nil,
        let popup = createBoundAuthPopup(spec: spec, configuration: webView.configuration)
      {
        popup.load(URLRequest(url: url))
      } else {
        // A popup is already open; fall back to the Safari sheet.
        presentGoogleOAuth(url: url)
      }
      decisionHandler(.cancel)
      return
    }
    if AccountSessionCoordinator.hostMatchesAllowed(url, allowedHosts: spec.allowedHosts) {
      decisionHandler(.allow)
      return
    }
    // Batch-1: a tapped link outside the allowlist used to be cancelled
    // silently. Offer Safari instead — but only for real link taps; other
    // main-frame navigations (redirects, form posts) keep the silent
    // cancel so auth flows are not interrupted by a prompt.
    if navigationAction.navigationType == .linkActivated,
      let url, url.scheme?.lowercased() == "https" || url.scheme?.lowercased() == "http"
    {
      decisionHandler(.cancel)
      presentExternalLinkAlert(url: url)
      return
    }
    decisionHandler(.cancel)
  }

  /// Batch-1: asks before opening an off-allowlist link in Safari (the
  /// isolated session does not follow — Safari has its own cookies).
  private func presentExternalLinkAlert(url: URL) {
    guard presentedViewController == nil else { return }
    let host = url.host ?? url.absoluteString
    let alert = UIAlertController(
      title: "在 Safari 中打开？",
      message: "即将离开 \(host)，该页面将在系统浏览器中打开，不使用当前账号的独立会话。",
      preferredStyle: .alert
    )
    alert.addAction(
      UIAlertAction(title: "取消", style: .cancel, handler: nil))
    alert.addAction(
      UIAlertAction(title: "在 Safari 中打开", style: .default) { _ in
        UIApplication.shared.open(url, options: [:], completionHandler: nil)
      })
    present(alert, animated: true, completion: nil)
  }

  // 0.1.10: Google OAuth ran in an in-app Safari sheet. It failed in
  // practice (2026-11-21 user test): the sheet shares Safari's cookie
  // jar, so the Muse page inside it is a different session — the user
  // didn't log into Muse there, closed the sheet, and the app's page
  // showed no connection. 0.1.11 opens Google OAuth in a same-store
  // popup tab instead; the sheet remains only as a fallback (main-frame
  // Google navigation while a popup is already open).
  private func presentGoogleOAuth(url: URL) {
    guard presentedViewController == nil else { return }
    let safari = SFSafariViewController(url: url)
    safari.delegate = self
    present(safari, animated: true, completion: nil)
  }

  func safariViewControllerDidFinish(_ controller: SFSafariViewController) {
    // The user closed the sheet (Done or swipe). If the OAuth completed,
    // the Muse page now shows the connector as connected; reload so the
    // session reflects it. Harmless if they cancelled mid-flow.
    mainWebView?.reload()
  }

  func webView(
    _ webView: WKWebView,
    decidePolicyFor navigationResponse: WKNavigationResponse,
    decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void
  ) {
    // Android's onReceivedHttpError marks the load failed; WKWebView does not
    // fail navigations on HTTP errors, so check the status code here.
    if webView === mainWebView,
      let http = navigationResponse.response as? HTTPURLResponse,
      navigationResponse.isForMainFrame,
      !(200...299).contains(http.statusCode)
    {
      mainLoadFailed = true
      committedAuthority = nil
    }
    decisionHandler(.allow)
  }

  func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
    if webView === mainWebView {
      mainLoadFailed = false
      committedAuthority = nil
    }
  }

  func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
    guard let spec = currentSpec else { return }
    if webView === popupWebView {
      guard AccountSessionCoordinator.shared.isCurrent(
        switchSeq: spec.switchSeq, viewInstanceId: spec.viewInstanceId),
        let url = webView.url, url.absoluteString != "about:blank"
      else { return }
      // 0.1.11: reveal the popup for Google OAuth hosts too — the login
      // page loads in the same-store tab; the allowlist alone would
      // destroy the popup the moment the Google page commits.
      guard AccountSessionCoordinator.hostMatchesAllowed(url, allowedHosts: spec.allowedHosts)
        || AccountSessionCoordinator.isGoogleOAuthHost(url)
      else {
        destroyPopupWebView()
        return
      }
      popupDomainLabel.text = "认证域名 · \(url.host ?? "")"
      popupOverlayRoot.isHidden = false
      webView.isHidden = false
      webView.isUserInteractionEnabled = true
      popupInputEnabled = true
      return
    }

    guard ownsMainView(webView, spec: spec), let url = webView.url else { return }
    guard AccountSessionCoordinator.hostMatchesAllowed(url, allowedHosts: spec.allowedHosts)
    else {
      AccountSessionCoordinator.shared.emitSessionFailed(
        switchSeq: spec.switchSeq,
        viewInstanceId: spec.viewInstanceId,
        reason: "顶层页面来源或资料归属核验未通过，已回退管理页"
      )
      closeAndFinishFromManager()
      return
    }
    committedAuthority = mainLoadFailed ? nil : authority(of: url)
    // Batch-1: a successful commit clears the termination-recovery count.
    contentProcessCrashCount = 0
    lastContentProcessCrash = nil
    AccountSessionCoordinator.shared.emitPageCommitVisible(spec: spec, url: url.absoluteString)
  }

  func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
    // 0.1.13: the OAuth callback page has loaded in the popup — but do
    // NOT close it yet. 0.1.12 closed the popup and reloaded the session
    // the moment the callback loaded, which killed the completion: the
    // callback page (or the main page, via postMessage) may still be
    // finishing async work, and the reload wiped the main page's state
    // mid-flight (2026-11-22 user test: the flow reached the Muse page,
    // but the connection never completed). Instead, mark the callback
    // loaded and wait: the popup closes when it navigates on to a
    // post-callback page, when the page closes itself (webViewDidClose),
    // or after an 8s timeout; the session reloads 2s after the popup
    // closes (see destroyPopupWebView).
    guard webView === popupWebView,
      popupDidOAuth,
      let spec = currentSpec,
      let url = webView.url,
      AccountSessionCoordinator.hostMatchesAllowed(url, allowedHosts: spec.allowedHosts)
    else { return }
    if isOAuthCallbackURL(url) {
      guard !popupCallbackLoaded else { return }
      popupCallbackLoaded = true
      popupOAuthTimeout?.invalidate()
      popupOAuthTimeout = Timer.scheduledTimer(withTimeInterval: 8.0, repeats: false) {
        [weak self] _ in
        self?.handlePopupOAuthTimeout()
      }
      return
    }
    // A post-callback page loaded (the flow redirected on): done.
    guard popupCallbackLoaded else { return }
    destroyPopupWebView()
  }

  /// The OAuth authorization-code callback carries a `code` query item.
  private func isOAuthCallbackURL(_ url: URL) -> Bool {
    URLComponents(url: url, resolvingAgainstBaseURL: false)?
      .queryItems?.contains { $0.name == "code" } == true
  }

  /// 0.1.14: the 8s OAuth popup timeout. Before auto-closing, check
  /// whether the callback page is showing an error — if it is, leave
  /// the popup open so the user can read it and close it manually
  /// (0.1.13 closed it blindly and any error text was lost). The manual
  /// close still triggers the delayed session reload via
  /// destroyPopupWebView.
  private func handlePopupOAuthTimeout() {
    guard let popup = popupWebView, popupCallbackLoaded else { return }
    popup.evaluateJavaScript(
      "(document.title + ' ' + (document.body ? document.body.innerText.slice(0, 800) : '')).toLowerCase()"
    ) { [weak self] result, _ in
      guard let self, self.popupWebView === popup, self.popupCallbackLoaded else { return }
      let text = (result as? String) ?? ""
      let errorHints = [
        "error", "错误", "失败", "denied", "invalid", "unauthorized",
        "forbidden", "something went wrong", "出错", "拒绝访问",
        "无法完成", "expired",
      ]
      if errorHints.contains(where: { text.contains($0) }) {
        // Looks like an error page: leave it open for the user.
        return
      }
      self.destroyPopupWebView()
    }
  }

  func webView(
    _ webView: WKWebView,
    didFailProvisionalNavigation navigation: WKNavigation!,
    withError error: Error
  ) {
    if webView === mainWebView {
      mainLoadFailed = true
      committedAuthority = nil
    }
  }

  func webView(
    _ webView: WKWebView,
    didFail navigation: WKNavigation!,
    withError error: Error
  ) {
    if webView === mainWebView {
      mainLoadFailed = true
      committedAuthority = nil
    }
  }

  func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
    if webView === popupWebView {
      destroyPopupWebView()
      return
    }
    guard webView === mainWebView, let spec = currentSpec else { return }
    // 0.1.8: the shared WebContent process was jetsammed — lower the
    // parked-page cap so the cache rebuilds more conservatively.
    RecentAccountPages.noteProcessTermination()
    // Batch-1 recovery, in the user's eviction order: inactive spaces
    // first (drop every parked page to free memory), then reload the
    // active page in place — its cookies live in the data store, so a
    // reload normally restores the session. Only if the process dies
    // twice within 60 seconds do we give up and go back to the manager.
    let now = Date()
    if let last = lastContentProcessCrash,
      now.timeIntervalSince(last) > 60
    {
      contentProcessCrashCount = 0
    }
    contentProcessCrashCount += 1
    lastContentProcessCrash = now
    guard contentProcessCrashCount < 2 else {
      AccountSessionCoordinator.shared.emitSessionFailed(
        switchSeq: spec.switchSeq,
        viewInstanceId: spec.viewInstanceId,
        reason: "账号页面已被系统回收，请重新打开"
      )
      closeAndFinishFromManager()
      return
    }
    RecentAccountPages.clearAll()
    destroyPopupWebView()
    headerTitleLabel.text = "页面被系统回收，正在恢复…"
    loadingIndicator.startAnimating()
    progressView.progress = 0
    progressView.alpha = 1
    webView.reload()
  }

  // MARK: - WKUIDelegate (auth popup)

  func webView(
    _ webView: WKWebView,
    createWebViewWith configuration: WKWebViewConfiguration,
    for navigationAction: WKNavigationAction,
    windowFeatures: WKWindowFeatures
  ) -> WKWebView? {
    guard let spec = currentSpec,
      popupWebView == nil,
      ownsMainView(webView, spec: spec)
    else { return nil }
    // 0.1.11: Google OAuth opens in a same-store popup tab — the app is
    // the browser, the popup is a new tab sharing the session's data
    // store, so Muse's OAuth state (even if cookie-based) validates and
    // the callback lands on muse.ai in the same jar. (0.1.10's Safari
    // sheet failed in practice: the sheet shares Safari's cookie jar, a
    // different Muse session, so the connection never reached the app's
    // page.) 0.1.11's test cleared both risks it carried: no crash (the
    // 0.1.6 crash was creating a popup with a FRESH configuration; 0.1.8
    // switched to the configuration WebKit prepared for the popup) and
    // no Google 403 — but the callback was then hijacked by iOS via
    // Universal Link (fixed in 0.1.12 by cancelling the callback
    // navigation and reloading it programmatically), and 0.1.12 closed
    // the popup and reloaded the session the moment the callback loaded,
    // which killed the completion (fixed in 0.1.13 by waiting for the
    // flow to finish before closing).
    // About:blank popups (URL unknown yet) still go through and are
    // filtered by decidePolicyFor when they navigate.
    if let url = navigationAction.request.url,
      url.absoluteString != "about:blank"
    {
      if !AccountSessionCoordinator.hostMatchesAllowed(url, allowedHosts: spec.allowedHosts)
        && !AccountSessionCoordinator.isGoogleOAuthHost(url)
      {
        return nil
      }
    }
    return createBoundAuthPopup(spec: spec, configuration: configuration)
  }

  func webViewDidClose(_ webView: WKWebView) {
    if webView === popupWebView {
      destroyPopupWebView()
    }
  }

  private func createBoundAuthPopup(
    spec: IosAccountSessionSpec,
    configuration: WKWebViewConfiguration
  ) -> WKWebView? {
    guard let mainView = mainWebView else { return nil }
    // 0.1.8: use the configuration WebKit prepared for this popup (it
    // carries the opener's process pool and SSO state) instead of a
    // fresh one. A fresh configuration drops the opener association,
    // which is what the crashing SSO path chokes on. Copy before
    // tweaking; the passed configuration is WebKit's.
    guard let config = configuration.copy() as? WKWebViewConfiguration else {
      return nil
    }
    // Same data store as the main view: the popup shares the account's
    // isolated profile, exactly like Android's setProfile(popup, spec.uuid).
    // (The passed configuration already inherits it from the opener;
    // set explicitly so the invariant never depends on that.)
    config.websiteDataStore = mainView.configuration.websiteDataStore
    config.defaultWebpagePreferences.allowsContentJavaScript = true
    config.preferences.javaScriptCanOpenWindowsAutomatically = false
    // Batch-1: same inline-playback fix as the main view.
    config.allowsInlineMediaPlayback = true
    let popup = WKWebView(frame: .zero, configuration: config)

    popupInputEnabled = false
    popup.isHidden = true
    popup.isUserInteractionEnabled = false
    popup.navigationDelegate = self
    popup.uiDelegate = self

    popupDidOAuth = false
    popupCallbackReloaded = false
    popupCallbackLoaded = false
    popupWebView = popup
    popupDomainLabel.text = ""
    popupOverlayRoot.isHidden = true
    popupWebContainer.subviews.forEach { $0.removeFromSuperview() }
    popupWebContainer.addSubview(popup)
    popup.translatesAutoresizingMaskIntoConstraints = false
    NSLayoutConstraint.activate([
      popup.topAnchor.constraint(equalTo: popupWebContainer.topAnchor),
      popup.leadingAnchor.constraint(equalTo: popupWebContainer.leadingAnchor),
      popup.trailingAnchor.constraint(equalTo: popupWebContainer.trailingAnchor),
      popup.bottomAnchor.constraint(equalTo: popupWebContainer.bottomAnchor),
    ])
    return popup
  }

  private func destroyPopupWebView() {
    let popup = popupWebView
    popupWebView = nil
    popupDidOAuth = false
    popupCallbackReloaded = false
    popupOAuthTimeout?.invalidate()
    popupOAuthTimeout = nil
    popupInputEnabled = false
    // 0.1.13: the popup carried an OAuth callback to completion — reload
    // the session after a short delay so any in-flight completion on
    // the main page (e.g. a postMessage-triggered API call) can settle
    // before the reload. No reload for popups that never reached a
    // callback (cancelled flows, blocked navigations).
    let didCallback = popupCallbackLoaded
    popupCallbackLoaded = false
    if didCallback, let spec = currentSpec {
      schedulePostOAuthReload(spec: spec)
    }
    popupOverlayRoot.isHidden = true
    popupDomainLabel.text = ""
    popupWebContainer.subviews.forEach { $0.removeFromSuperview() }
    popup?.stopLoading()
    popup?.navigationDelegate = nil
    popup?.uiDelegate = nil
    popup?.removeFromSuperview()
  }

  /// 0.1.13: reloads the session 2s after an OAuth popup closes. The
  /// delay lets any in-flight completion on the main page settle; the
  /// guards skip the reload if the session moved on or another popup
  /// opened in the meantime.
  private func schedulePostOAuthReload(spec: IosAccountSessionSpec) {
    postOAuthReloadTimer?.invalidate()
    let seq = spec.switchSeq
    let viewId = spec.viewInstanceId
    postOAuthReloadTimer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: false) {
      [weak self] _ in
      guard let self,
        self.popupWebView == nil,
        AccountSessionCoordinator.shared.isCurrent(switchSeq: seq, viewInstanceId: viewId)
      else { return }
      self.mainWebView?.reload()
    }
  }

  // MARK: - Identity probe loop

  private func startIdentityProbeLoop(spec: IosAccountSessionSpec) {
    stopIdentityProbeLoop()
    consecutiveLoggedOut = 0
    loggedOutReported = false
    // Batch-1: backoff state resets with every new session.
    probeInterval = 4.5
    lastProbeSignature = nil
    stableProbeCount = 0
    scheduleProbe(after: 1.4, spec: spec)
  }

  private func stopIdentityProbeLoop() {
    probeTimer?.invalidate()
    probeTimer = nil
    probeGeneration += 1
  }

  private func scheduleProbe(after delay: TimeInterval, spec: IosAccountSessionSpec) {
    let generation = probeGeneration
    probeTimer?.invalidate()
    probeTimer = Timer.scheduledTimer(withTimeInterval: delay, repeats: false) {
      [weak self] _ in
      guard let self, generation == self.probeGeneration else { return }
      self.runProbe(spec: spec, generation: generation)
    }
  }

  private func runProbe(spec: IosAccountSessionSpec, generation: Int) {
    guard AccountSessionCoordinator.shared.isCurrent(
      switchSeq: spec.switchSeq, viewInstanceId: spec.viewInstanceId)
    else {
      stopIdentityProbeLoop()
      return
    }

    // Popup probe only captures a typed identity hint.
    if let popup = popupWebView {
      popup.evaluateJavaScript(Scripts.identityProbeScript) { [weak self] result, _ in
        guard let self, generation == self.probeGeneration,
          popup === self.popupWebView,
          AccountSessionCoordinator.shared.isCurrent(
            switchSeq: spec.switchSeq, viewInstanceId: spec.viewInstanceId),
          let parsed = self.parseProbeOutput(result),
          !parsed.typed.isEmpty
        else { return }
        self.capturedIdentityHint = parsed.typed
      }
    }

    mainWebView?.evaluateJavaScript(Scripts.identityProbeScript) { [weak self] result, _ in
      guard let self, generation == self.probeGeneration,
        AccountSessionCoordinator.shared.isCurrent(
          switchSeq: spec.switchSeq, viewInstanceId: spec.viewInstanceId),
        let parsed = self.parseProbeOutput(result)
      else { return }
      // Batch-1 backoff: when everything the probe reports stays identical
      // for 4 consecutive probes, relax the interval to 30s; any change
      // snaps it back to 4.5s on the next cycle.
      let signature = [
        parsed.auth ? "1" : "0",
        parsed.loggedOut ? "1" : "0",
        parsed.typed, parsed.email, parsed.avatar, parsed.quotaText,
        String(parsed.quotaPct ?? -1), parsed.quotaDetailJson,
      ].joined(separator: "|")
      if signature == self.lastProbeSignature {
        self.stableProbeCount += 1
      } else {
        self.stableProbeCount = 0
        self.lastProbeSignature = signature
      }
      self.probeInterval = self.stableProbeCount >= 4 ? 30 : 4.5
      if !parsed.typed.isEmpty {
        self.capturedIdentityHint = parsed.typed
      }
      if parsed.auth {
        self.consecutiveLoggedOut = 0
        self.loggedOutReported = false
        let resolved = parsed.email.isEmpty ? (self.capturedIdentityHint ?? "") : parsed.email
        AccountSessionCoordinator.shared.emitIdentityDetected(
          spec: spec,
          identity: resolved,
          avatarUrl: parsed.avatar.isEmpty ? nil : parsed.avatar,
          quotaText: parsed.quotaText.isEmpty ? nil : parsed.quotaText,
          quotaPercent: parsed.quotaPct,
          quotaDetailJson: parsed.quotaDetailJson.isEmpty ? nil : parsed.quotaDetailJson
        )
      } else if parsed.loggedOut {
        self.consecutiveLoggedOut += 1
        if self.consecutiveLoggedOut == 1 {
          // First sighting: re-probe after 1s instead of 4.5s.
          self.scheduleProbe(after: 1.0, spec: spec)
        } else if self.consecutiveLoggedOut >= 2 && !self.loggedOutReported {
          self.loggedOutReported = true
          AccountSessionCoordinator.shared.emitSessionLoggedOut(spec: spec)
        }
      } else {
        self.consecutiveLoggedOut = 0
      }
    }

    // 0.1.15: task-completion check for the active Muse session. Runs
    // alongside the identity probe; when the conversation content
    // changed, the probe stays at the 4.5s interval (instead of
    // backing off to 30s) while the task is streaming.
    if spec.providerId == "muse" {
      mainWebView?.evaluateJavaScript(TaskWatcher.taskStateScript) { [weak self] result, _ in
        guard let self, generation == self.probeGeneration,
          AccountSessionCoordinator.shared.isCurrent(
            switchSeq: spec.switchSeq, viewInstanceId: spec.viewInstanceId)
        else { return }
        if TaskWatcher.shared.noteSample(
          providerId: spec.providerId, uuid: spec.uuid, result: result)
        {
          self.stableProbeCount = 0
        }
      }
    }

    scheduleProbe(after: probeInterval, spec: spec)
  }

  private func parseProbeOutput(_ result: Any?) -> ProbeData? {
    // evaluateJavaScript returns the script's return value directly: the
    // probe script returns a JSON string (Android additionally JSON-decodes
    // the evaluateJavascript string wrapper, which iOS does not need).
    guard let jsonString = result as? String,
      let data = jsonString.data(using: .utf8),
      let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
    else { return nil }
    let pctRaw = obj["quotaPct"] as? Int ?? -1
    return ProbeData(
      auth: obj["auth"] as? Bool ?? false,
      loggedOut: obj["loggedOut"] as? Bool ?? false,
      typed: obj["typed"] as? String ?? "",
      email: obj["email"] as? String ?? "",
      avatar: obj["avatar"] as? String ?? "",
      quotaText: obj["quotaText"] as? String ?? "",
      quotaPct: (0...100).contains(pctRaw) ? pctRaw : nil,
      quotaDetailJson: obj["quotaDetail"] as? String ?? ""
    )
  }

  // MARK: - Close paths

  func closeAndFinishFromManager(keepPage: Bool = false) {
    closingFromManager = true
    releaseSessionViews(keepPage: keepPage)
    dismiss(animated: true)
  }

  /// 0.1.17: foreground return after a long absence — same as tapping
  /// "← 账号". Skipped while an OAuth popup is open, so a login flow
  /// in progress is never interrupted.
  func returnToListAfterLongAbsence() {
    guard popupWebView == nil else { return }
    returnToAccountList()
  }

  private func returnToAccountList() {
    let spec = currentSpec
    closingFromManager = true
    releaseSessionViews(keepPage: true)
    if let spec {
      AccountSessionCoordinator.shared.emitActivityClosed(
        switchSeq: spec.switchSeq, viewInstanceId: spec.viewInstanceId)
    }
    dismiss(animated: true)
  }

  private func releaseSessionViews(keepPage: Bool = false) {
    let spec = currentSpec
    let view = mainWebView
    let retain = keepPage && mainInputEnabled && popupWebView == nil
      && committedAuthority != nil
      && view?.url.map { authority(of: $0) } == committedAuthority
    mainWebView = nil
    currentSpec = nil
    committedAuthority = nil
    progressObservation?.invalidate()
    progressObservation = nil
    progressView.alpha = 0
    stopIdentityProbeLoop()
    capturedIdentityHint = nil
    mainInputEnabled = false
    destroyPopupWebView()
    mainWebContainer.subviews.forEach { $0.removeFromSuperview() }
    guard let view, let spec else { return }
    if retain {
      view.isHidden = true
      view.isUserInteractionEnabled = false
      view.navigationDelegate = nil
      view.uiDelegate = nil
      RecentAccountPages.park(view, spec: spec)
    } else {
      view.stopLoading()
      view.navigationDelegate = nil
      view.uiDelegate = nil
      view.removeFromSuperview()
    }
  }

  private func authority(of url: URL) -> String? {
    // Host + explicit port, mirroring Android's Uri.encodedAuthority
    // (userinfo never occurs for these https account URLs).
    guard let host = url.host else { return nil }
    if let port = url.port {
      return "\(host):\(port)"
    }
    return host
  }
}

// MARK: - UIColor hex helper

private extension UIColor {
  convenience init(hex: Int) {
    self.init(
      red: CGFloat((hex >> 16) & 0xFF) / 255,
      green: CGFloat((hex >> 8) & 0xFF) / 255,
      blue: CGFloat(hex & 0xFF) / 255,
      alpha: 1
    )
  }
}
