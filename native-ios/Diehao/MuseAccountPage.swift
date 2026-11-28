import CryptoKit
import SwiftUI
import WebKit

@MainActor
final class MusePageDelegate: NSObject, WKNavigationDelegate, WKUIDelegate {
    static let entryURL = URL(string: "https://muse.ai/")!
    static let inlineVideo = WKUserScript(source: """
        const prepareInlineVideos = () => {
            for (const video of document.querySelectorAll('video:not([playsinline])')) {
                video.setAttribute('playsinline', '');
            }
        };
        new MutationObserver(prepareInlineVideos).observe(document, {childList: true, subtree: true});
        prepareInlineVideos();
        """, injectionTime: .atDocumentStart, forMainFrameOnly: false)

    private let root: WKWebView
    private let onFinish: (WKWebView, WKNavigation?) -> Void
    private let onError: (Error) -> Void
    private var popup: WKWebView?
    var appliedRetry = 0

    init(root: WKWebView, onFinish: @escaping (WKWebView, WKNavigation?) -> Void, onError: @escaping (Error) -> Void) {
        self.root = root
        self.onFinish = onFinish
        self.onError = onError
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        onFinish(webView, navigation)
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        if (error as? URLError)?.code != .cancelled { onError(error) }
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        if (error as? URLError)?.code != .cancelled { onError(error) }
    }

    func webView(_ webView: WKWebView, didReceive challenge: URLAuthenticationChallenge,
                 completionHandler: @escaping @MainActor @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        if challenge.protectionSpace.isProxy() {
            completionHandler(.cancelAuthenticationChallenge, nil)
            onError(RegistryError.unavailableProxyCredential)
        } else {
            completionHandler(.performDefaultHandling, nil)
        }
    }

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                 decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void) {
        let scheme = action.request.url?.scheme?.lowercased()
        decisionHandler(scheme == "https" || action.request.url?.absoluteString == "about:blank" ? .allow : .cancel)
    }

    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        guard webView === root, popup == nil,
              configuration.websiteDataStore === root.configuration.websiteDataStore else { return nil }
        let child = WKWebView(frame: root.bounds, configuration: configuration)
        child.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        child.navigationDelegate = self
        child.uiDelegate = self
        root.addSubview(child)
        popup = child
        return child
    }

    func webViewDidClose(_ webView: WKWebView) {
        guard popup === webView else { return }
        webView.removeFromSuperview()
        popup = nil
        root.reload()
    }

    func close() {
        popup?.stopLoading()
        popup?.removeFromSuperview()
        popup = nil
        root.stopLoading()
    }
}

struct MuseAccountPage: UIViewRepresentable {
    let webView: WKWebView
    let retry: Int
    let entryURL: URL
    let onResult: (String?) -> Void
    var onPage: ((WKWebView) -> Void)? = nil

    func makeCoordinator() -> MusePageDelegate {
        MusePageDelegate(root: webView, onFinish: { page, _ in
            onResult(nil)
            if page === webView { onPage?(page) }
        }, onError: { error in
            if (error as? URLError)?.code == .timedOut {
                onResult("Muse 页面请求超时。可在当前账号重试。")
            } else if let error = error as? RegistryError {
                onResult(error.localizedDescription)
            } else {
                let code = (error as NSError).code
                onResult("Muse 页面未能加载（\(code)）。可在当前账号重试。")
            }
        })
    }

    func makeUIView(context: Context) -> WKWebView {
        webView.navigationDelegate = context.coordinator
        webView.uiDelegate = context.coordinator
        webView.load(URLRequest(url: entryURL, timeoutInterval: 30))
        return webView
    }

    func updateUIView(_ uiView: WKWebView, context: Context) {
        guard context.coordinator.appliedRetry != retry else { return }
        context.coordinator.appliedRetry = retry
        // Restart at the entry instead of resubmitting a failed login form.
        context.coordinator.close()
        uiView.load(URLRequest(url: entryURL, timeoutInterval: 30))
    }

    static func dismantleUIView(_ uiView: WKWebView, coordinator: MusePageDelegate) {
        coordinator.close()
    }
}

@MainActor
final class CookieLoginProbe {
    private var delegate: MusePageDelegate?
    private var continuation: CheckedContinuation<Void, Error>?
    private var watchdog: Task<Void, Never>?

    func verify(_ webView: WKWebView, email: String) async throws {
        let expected = email.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard expected.range(of: #"^[^\s@]+@[^\s@]+\.[^\s@]+$"#, options: .regularExpression) != nil else {
            throw CookieError.invalidEmail
        }
        try await check(webView, expected: expected)
    }

    func verifySession(_ webView: WKWebView) async throws {
        try await check(webView, expected: nil)
    }

    private func check(_ webView: WKWebView, expected: String?) async throws {
        let museURL = MusePageDelegate.entryURL
        defer {
            delegate?.close()
            delegate = nil
            webView.navigationDelegate = nil
        }
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            self.continuation = continuation
            let observer = MusePageDelegate(root: webView, onFinish: { [weak self] page, _ in
                guard page.url?.scheme == "https", page.url?.host == museURL.host else {
                    self?.finish(CookieError.sessionUnconfirmed)
                    return
                }
                self?.finish(nil)
            }, onError: { [weak self] error in
                self?.finish((error as? URLError)?.code == .timedOut ? CookieError.verificationTimeout : CookieError.sessionUnconfirmed)
            })
            delegate = observer
            webView.navigationDelegate = observer
            var request = URLRequest(url: museURL)
            request.timeoutInterval = 30
            watchdog = Task { [weak self] in
                do { try await Task.sleep(for: .seconds(30)) } catch { return }
                self?.finish(CookieError.verificationTimeout)
            }
            webView.load(request)
        }
        let result = try? await webView.callAsyncJavaScript(Self.identityScript,
            arguments: ["expected": expected.map { $0 as Any } ?? NSNull()], in: nil, contentWorld: .defaultClient)
        switch result as? String {
        case "verified": return
        case "mismatch": throw CookieError.wrongIdentity
        case "timeout": throw CookieError.verificationTimeout
        case "identityUnavailable": throw CookieError.identityUnavailable
        default: throw CookieError.sessionUnconfirmed
        }
    }

    private func finish(_ error: Error?) {
        guard let continuation else { return }
        self.continuation = nil
        watchdog?.cancel()
        watchdog = nil
        if let error { continuation.resume(throwing: error) } else { continuation.resume() }
    }

    private static let identityScript = #"""
        if (location.origin !== 'https://muse.ai') return 'sessionUnconfirmed';
        let proof;
        try {
            const response = await fetch('/api/auth/check', {
                method: 'POST', credentials: 'same-origin', redirect: 'error', cache: 'no-store',
                headers: {'Content-Type': 'application/json'}, body: '{}',
                signal: AbortSignal.timeout(15000)
            });
            if (!response.ok) return 'sessionUnconfirmed';
            proof = await response.json();
        } catch (error) {
            return error.name === 'TimeoutError' ? 'timeout' : 'sessionUnconfirmed';
        }
        if (proof?.ok !== true || typeof proof.viewer_id !== 'string' || !proof.viewer_id) return 'sessionUnconfirmed';
        if (expected === null) return 'verified';
        const proofs = new Set();
        const paths = ['viewer', 'user', 'account', 'profile', 'session', 'props', 'pageProps', 'data'];
        const add = email => {
            if (typeof email === 'string' && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim())) {
                proofs.add(email.trim().toLowerCase());
            }
        };
        const visit = (object, depth) => {
            if (!object || typeof object !== 'object' || depth > 5) return;
            const id = object.viewer_id ?? object.viewerId ?? object.user_id ?? object.userId ?? object.id;
            if (id != null && String(id) === proof.viewer_id) {
                add(object.email);
                add(object.viewer_email);
                add(object.viewerEmail);
            }
            for (const key of paths) visit(object[key], depth + 1);
        };
        visit(proof, 0);
        for (const script of document.querySelectorAll('script#__NEXT_DATA__, script#client-bootstrap, script[data-client-bootstrap]')) {
            try { visit(JSON.parse(script.textContent), 0); } catch {}
        }
        if (proofs.size !== 1) return 'identityUnavailable';
        return proofs.has(expected) ? 'verified' : 'mismatch';
        """#
}

@MainActor
enum CookieTransfer {
    static func capture(store: WKWebsiteDataStore) async throws -> CookiePreview {
        let records = await store.httpCookieStore.allCookies().map(MuseCookie.init)
        return try CookiePreview(records.filter { $0.family != nil })
    }

    static func apply(_ preview: CookiePreview, id: UUID, email: String, registry: AccountStoreRegistry) async throws {
        try await apply(preview, id: id, registry: registry) {
            let handle = try registry.makeVerificationView(for: id)
            guard let view = handle.view else { throw CookieError.verification }
            try await CookieLoginProbe().verify(view, email: email)
        }
    }

    static func apply(_ preview: CookiePreview, id: UUID, registry: AccountStoreRegistry,
                      verify: () async throws -> Void) async throws {
        let store = try registry.dataStore(for: id).httpCookieStore
        let existing = try await registry.storeStep(for: id) { await scopedCookies(store) }
        try saveRollback(existing.map(MuseCookie.init), id: id)
        do { try registry.setCookieWritePending(true, for: id) }
        catch {
            try? T2Keychain.delete("cookie-rollback." + id.uuidString)
            throw error
        }
        do {
            let incoming = try preview.cookies.map { try $0.native() }
            try await registry.storeStep(for: id) { try await replace(incoming, in: store) }
            try await verify()
            let domains = try await registry.storeStep(for: id) {
                Set(await scopedCookies(store).map { MuseCookie($0).family ?? "" })
            }
            guard domains == ["muse.ai", "meta.com"] else {
                throw CookieError.missingDomain
            }
            try registry.setCookieWritePending(false, for: id)
        } catch RegistryError.storeAwaitingRestart {
            throw RegistryError.storeAwaitingRestart
        } catch {
            let cause = error
            do {
                try await registry.storeStep(for: id) { try await replace(existing, in: store) }
                try registry.setCookieWritePending(false, for: id)
            } catch RegistryError.storeAwaitingRestart {
                throw RegistryError.storeAwaitingRestart
            } catch {
                throw CookieError.rollback
            }
            try? T2Keychain.delete("cookie-rollback." + id.uuidString)
            throw cause
        }
        try? T2Keychain.delete("cookie-rollback." + id.uuidString)
    }

    private static func scopedCookies(_ store: WKHTTPCookieStore) async -> [HTTPCookie] {
        await store.allCookies().filter { MuseCookie($0).family != nil }
    }

    private static func replace(_ cookies: [HTTPCookie], in store: WKHTTPCookieStore) async throws {
        for old in await scopedCookies(store) { await store.deleteCookie(old) }
        for cookie in cookies { await store.setCookie(cookie) }
        let expected = fingerprints(cookies)
        // WebKit's write callback can precede visibility in the cookie readback.
        for attempt in 0...4 {
            if fingerprints(await scopedCookies(store)) == expected { return }
            if attempt < 4 { try? await Task.sleep(for: .milliseconds(150 * (attempt + 1))) }
        }
        throw CookieError.writeVerification
    }

    private static func fingerprints(_ cookies: [HTTPCookie]) -> [String: Data] {
        cookies.reduce(into: [:]) {
            $0[MuseCookie($1).key] = Data(SHA256.hash(data: Data($1.value.utf8)))
        }
    }

    private static func saveRollback(_ records: [MuseCookie], id: UUID) throws {
        let name = "cookie-rollback." + id.uuidString
        guard try T2Keychain.read(name) == nil else { throw CookieError.rollback }
        try T2Keychain.save(JSONEncoder().encode(records), as: name)
    }

    static func recover(id: UUID, registry: AccountStoreRegistry) async throws {
        let key = "cookie-rollback." + id.uuidString
        guard let stored = try T2Keychain.read(key) else { throw CookieError.rollback }
        let backup = try JSONDecoder().decode([MuseCookie].self, from: stored)
        let previous = try backup.map { try $0.native() }
        let store = try registry.dataStore(for: id).httpCookieStore
        try await registry.storeStep(for: id) { try await replace(previous, in: store) }
        try registry.setCookieWritePending(false, for: id)
        try T2Keychain.delete(key)
    }
}
