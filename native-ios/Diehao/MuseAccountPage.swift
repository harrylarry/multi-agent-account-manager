import CryptoKit
import SwiftUI
import WebKit

@MainActor
final class MusePageDelegate: NSObject, WKNavigationDelegate, WKUIDelegate {
    private let root: WKWebView
    private let onFinish: (WKWebView, WKNavigation?) -> Void
    private let onError: (Error) -> Void
    private var popup: WKWebView?

    init(root: WKWebView, onFinish: @escaping (WKWebView, WKNavigation?) -> Void, onError: @escaping (Error) -> Void) {
        self.root = root
        self.onFinish = onFinish
        self.onError = onError
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        onFinish(webView, navigation)
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        onError(error)
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        onError(error)
    }

    func webView(_ webView: WKWebView, didReceive challenge: URLAuthenticationChallenge,
                 completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        if challenge.protectionSpace.isProxy() {
            completionHandler(.cancelAuthenticationChallenge, nil)
            onError(RegistryError.unavailableProxyCredential)
        } else {
            completionHandler(.performDefaultHandling, nil)
        }
    }

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
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
    let onError: (String) -> Void

    func makeCoordinator() -> MusePageDelegate {
        MusePageDelegate(root: webView, onFinish: { _, _ in }, onError: { _ in
            onError("页面未能加载。若账号已绑定代理，请检查代理连通性或认证。")
        })
    }

    func makeUIView(context: Context) -> WKWebView {
        webView.navigationDelegate = context.coordinator
        webView.uiDelegate = context.coordinator
        webView.load(URLRequest(url: URL(string: "https://muse.ai/")!))
        return webView
    }

    func updateUIView(_ uiView: WKWebView, context: Context) {}

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
        let museURL = URL(string: "https://muse.ai/")!
        defer {
            delegate?.close()
            delegate = nil
            webView.navigationDelegate = nil
        }
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            self.continuation = continuation
            let observer = MusePageDelegate(root: webView, onFinish: { [weak self] page, _ in
                guard page.url?.host == "muse.ai" else { self?.finish(CookieError.verification); return }
                self?.finish(nil)
            }, onError: { [weak self] _ in self?.finish(CookieError.verification) })
            delegate = observer
            webView.navigationDelegate = observer
            var request = URLRequest(url: museURL)
            request.timeoutInterval = 20
            watchdog = Task { [weak self] in
                do { try await Task.sleep(for: .seconds(20)) } catch { return }
                self?.finish(CookieError.verification)
            }
            webView.load(request)
        }
        let result = try? await webView.callAsyncJavaScript(Self.identityScript,
            arguments: ["expected": expected], in: nil, contentWorld: .page)
        guard let result = result as? String else { throw CookieError.verification }
        if result == "mismatch" { throw CookieError.wrongIdentity }
        guard result == "verified" else { throw CookieError.verification }
    }

    private func finish(_ error: Error?) {
        guard let continuation else { return }
        self.continuation = nil
        watchdog?.cancel()
        watchdog = nil
        if let error { continuation.resume(throwing: error) } else { continuation.resume() }
    }

    private static let identityScript = #"""
        const proof = await fetch('/api/auth/check', {
            method: 'POST', credentials: 'same-origin',
            headers: {'Content-Type': 'application/json'}, body: '{}',
            signal: AbortSignal.timeout(10000)
        }).then(r => r.ok ? r.json() : null).catch(() => null);
        if (proof?.ok !== true || !proof.viewer_id) return 'unverified';
        const proofs = new Set();
        const paths = ['viewer', 'user', 'account', 'profile', 'session', 'props', 'pageProps', 'data'];
        const visit = (object, depth) => {
            if (!object || typeof object !== 'object' || depth > 5) return;
            if (typeof object.email === 'string') {
                const id = object.viewer_id ?? object.user_id ?? object.id;
                if (id == null || String(id) === String(proof.viewer_id)) proofs.add(object.email.toLowerCase());
            }
            for (const key of paths) visit(object[key], depth + 1);
        };
        for (const script of document.querySelectorAll('script#__NEXT_DATA__, script#client-bootstrap, script[data-client-bootstrap]')) {
            try { visit(JSON.parse(script.textContent), 0); } catch {}
        }
        if (proofs.size !== 1) return 'unverified';
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
        let existing = await scopedCookies(store)
        try saveRollback(existing.map(MuseCookie.init), id: id)
        do { try registry.setCookieWritePending(true, for: id) }
        catch {
            try? T2Keychain.delete("cookie-rollback." + id.uuidString)
            throw error
        }
        do {
            try await replace(try preview.cookies.map { try $0.native() }, in: store)
            try await verify()
            guard Set(await scopedCookies(store).map { MuseCookie($0).family ?? "" }) == ["muse.ai", "meta.com"] else {
                throw CookieError.missingDomain
            }
            try registry.setCookieWritePending(false, for: id)
        } catch {
            let cause = error
            do {
                try await replace(existing, in: store)
                try registry.setCookieWritePending(false, for: id)
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
        for old in await scopedCookies(store) { await store.delete(old) }
        for cookie in cookies { await store.setCookie(cookie) }
        guard fingerprints(await scopedCookies(store)) == fingerprints(cookies) else { throw CookieError.verification }
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
        try await replace(previous, in: store)
        try registry.setCookieWritePending(false, for: id)
        try T2Keychain.delete(key)
    }
}
