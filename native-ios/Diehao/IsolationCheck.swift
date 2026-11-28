import Foundation
import Network
import Observation
import WebKit

private enum IsolationError: LocalizedError {
    case mismatch(String)
    case navigation
    case restartRequired

    var errorDescription: String? {
        switch self {
        case .mismatch(let check): "自检不符合预期：\(check)"
        case .navigation: "本地 WebKit 自检页未成功载入。"
        case .restartRequired: "请彻底退出并重新打开叠号 T1 后再验证。"
        }
    }
}

@MainActor
@Observable
final class IsolationCheck {
    private let processID = UUID()
    private let defaults = UserDefaults.standard
    private let phaseKey = "isolation-a.phase"
    private let seededProcessKey = "isolation-a.seededProcess"
    private let bcKey = "isolation-bc.checkpoint"
    private(set) var registry: AccountStoreRegistry?

    private(set) var busy = false
    private(set) var ready = false
    private(set) var hasTestSlots = false
    private(set) var referencedAccountCount = 0
    private(set) var phase = ""
    private(set) var status = "正在启动原生骨架…"

    var canSeed: Bool { ready && !busy && !hasTestSlots }
    var canVerify: Bool { ready && !busy && hasTestSlots && (phase == "seeded" || phase == "verified") }
    var canClean: Bool { ready && !busy && hasTestSlots }

    func start() async {
        guard registry == nil, !busy else { return }
        busy = true
        defer { busy = false }
        do {
            let support = URL.applicationSupportDirectory
            let path = support.appending(path: "DiehaoNative", directoryHint: .isDirectory)
                .appending(path: "accounts.json")
            let registry = try AccountStoreRegistry(indexURL: path)
            try await registry.referenceLegacyProfiles(in: support, bundleID: Bundle.main.bundleIdentifier ?? "")
            self.registry = registry
            await registry.retryPendingRemoval(includeIsolation: false)
            hasTestSlots = !registry.isolationIdentities.isEmpty
            referencedAccountCount = registry.referencedAccountCount
            phase = defaults.string(forKey: phaseKey) ?? ""
            ready = true
            if hasTestSlots {
                status = phase == "verified"
                    ? "A 组通过。现在可清理测试资料。"
                    : phase == "seeded"
                        ? "写入和双向隔离已通过。请验证重启持久性。"
                        : "上次自检未完成。请先清理测试资料。"
            } else {
                status = phase == "cleaned" ? "A 组通过，测试资料已清理。" : "骨架已启动。"
            }
        } catch {
            status = error.localizedDescription
        }
    }

    func run() async {
        guard canSeed, let registry else { return }
        await perform("正在写入和检查两个测试资料…") {
            self.savePhase("")
            let pair = try await registry.createIsolationPair()
            self.hasTestSlots = true
            try self.verifyProxyWiring(registry, a: pair.a, b: pair.b)

            let a = try IsolationPage(registry: registry, id: pair.a)
            let b = try IsolationPage(registry: registry, id: pair.b)
            try await a.open()
            try await b.open()
            try await a.expect("", label: "A 初始为空")
            try await b.expect("", label: "B 初始为空")
            try await a.write(pair.a.uuidString)
            try await a.expect(pair.a.uuidString, label: "A 写入")
            try await b.expect("", label: "B 看不到 A")
            try await b.write(pair.b.uuidString)
            try await b.expect(pair.b.uuidString, label: "B 写入")
            try await a.expect(pair.a.uuidString, label: "A 看不到 B")
            self.defaults.set(self.processID.uuidString, forKey: self.seededProcessKey)
            self.savePhase("seeded")
            self.status = "双向隔离通过。请彻底退出并重开 App，再点「验证重启后隔离」。"
        }
    }

    func verifyAfterRestart() async {
        guard canVerify, let registry else { return }
        await perform("正在检查重启后的资料…") {
            guard self.defaults.string(forKey: self.seededProcessKey) != self.processID.uuidString else {
                throw IsolationError.restartRequired
            }
            let identities = registry.isolationIdentities
            for purpose in [AccountIdentity.Purpose.isolationA, .isolationB] {
                guard let identity = identities.first(where: { $0.purpose == purpose }) else {
                    throw IsolationError.mismatch("缺少已写入的测试资料")
                }
                let page = try IsolationPage(registry: registry, id: identity.id)
                try await page.open()
                try await page.expect(identity.id.uuidString, label: "重启后 \(purpose.rawValue)")
            }
            self.savePhase("verified")
            self.status = "A 组通过。两个资料重启后各自保留 Cookie 和 localStorage。"
        }
    }

    func clean() async {
        guard canClean, let registry else { return }
        await perform("正在删除测试资料…") {
            for identity in registry.isolationIdentities {
                try await registry.remove(identity.id)
            }
            self.hasTestSlots = false
            self.defaults.removeObject(forKey: self.seededProcessKey)
            let wasVerified = self.phase == "verified"
            self.savePhase(wasVerified ? "cleaned" : "")
            self.status = wasVerified ? "A 组通过，测试资料已清理。" : "测试资料已清理。"
        }
    }

    func runBC() async {
        guard ready, !busy, !hasTestSlots else { return }
        await perform("正在验证删除、恢复和 UUID 冲突…") {
            let folder = URL.applicationSupportDirectory.appending(path: "DiehaoNative/check-bc")
            let tests = try AccountStoreRegistry(indexURL: folder.appending(path: "accounts.json"))
            if let saved = self.defaults.data(forKey: self.bcKey) {
                try await self.finishBC(JSONDecoder().decode(BCCheckpoint.self, from: saved), tests: tests)
                return
            }
            for stale in tests.accounts { try await tests.remove(stale.id) }
            guard await tests.retryPendingRemoval() else { throw RegistryError.storeAwaitingRestart }
            let original = try await tests.createAccount(alias: "Synthetic original")
            let restored = try await tests.createAccount(alias: "Synthetic restore", reserving: original.id)
            let values = ["muse.ai", "meta.com"].map { domain in
                MuseCookie(name: "diehao_t2", value: UUID().uuidString, domain: domain, path: "/",
                           secure: true, httpOnly: true, hostOnly: true,
                           expirationDate: Date().timeIntervalSince1970 + 3600)
            }
            let password = UUID().uuidString
            let data = try await CookieArchive.seal(values, password: password)
            let imported = try CookiePreview(await CookieArchive.open(data, password: password))
            let rejected = (try? await CookieArchive.open(data, password: UUID().uuidString)) == nil
            guard rejected, restored.id != original.id else {
                throw IsolationError.mismatch("C 组口令或 UUID")
            }
            try await CookieTransfer.apply(imported, id: original.id, registry: tests) {}
            try await CookieTransfer.apply(imported, id: restored.id, registry: tests) {}
            let different = try CookiePreview(values.map {
                MuseCookie(name: $0.name, value: "different", domain: $0.domain, path: $0.path,
                           secure: $0.secure, httpOnly: $0.httpOnly, hostOnly: true, expirationDate: $0.expirationDate)
            })
            do {
                try await CookieTransfer.apply(different, id: original.id, registry: tests) {
                    throw CookieError.wrongIdentity
                }
                throw IsolationError.mismatch("C 组应拒绝错误邮箱")
            } catch CookieError.wrongIdentity {}
            let after = try await tests.storeStep(for: original.id) {
                try await CookieTransfer.capture(store: tests.dataStore(for: original.id))
            }
            guard Set(tests.accounts.map(\.id)) == [original.id, restored.id],
                  Set(after.cookies.map(\.value)) == Set(values.map(\.value)) else {
                throw IsolationError.mismatch("C 组覆盖与回滚")
            }
            let checkpoint = BCCheckpoint(original: original.id, restored: restored.id, expected: Set(values.map(\.value)))
            try self.saveBC(checkpoint)
            let handle = try tests.makeWebView(for: restored.id)
            try await self.finishBC(checkpoint, tests: tests, opened: handle)
        }
    }

    private struct BCCheckpoint: Codable {
        let original: UUID
        let restored: UUID
        let expected: Set<String>
        var checkedTargetRemoval = false
    }

    private func saveBC(_ checkpoint: BCCheckpoint) throws {
        defaults.set(try JSONEncoder().encode(checkpoint), forKey: bcKey)
    }

    private func finishBC(_ saved: BCCheckpoint, tests: AccountStoreRegistry, opened: AccountWebView? = nil) async throws {
        var checkpoint = saved
        if !checkpoint.checkedTargetRemoval {
            try await tests.remove(checkpoint.restored)
            let original = checkpoint.original
            let remaining = try await tests.storeStep(for: original) {
                try await CookieTransfer.capture(store: tests.dataStore(for: original))
            }
            guard opened?.view == nil, tests.accounts.map(\.id) == [checkpoint.original],
                  Set(remaining.cookies.map(\.value)) == checkpoint.expected else {
                throw IsolationError.mismatch("B 组仅删除目标资料")
            }
            checkpoint.checkedTargetRemoval = true
            try saveBC(checkpoint)
        }
        try await tests.remove(checkpoint.original)
        defaults.removeObject(forKey: bcKey)
        status = "B / C 组通过，测试资料已清理。"
    }

    private func verifyProxyWiring(_ registry: AccountStoreRegistry, a: UUID, b: UUID) throws {
        let storeA = try registry.dataStore(for: a)
        let storeB = try registry.dataStore(for: b)
        let original = try registry.makeWebView(for: a)
        guard storeA !== storeB, try registry.dataStore(for: a) === storeA,
              original.view?.configuration.websiteDataStore === storeA else {
            throw IsolationError.mismatch("DataStore 注册表")
        }
        let endpoint = NWEndpoint.hostPort(host: "127.0.0.1", port: 9)
        try registry.replaceProxies([ProxyConfiguration(socksv5Proxy: endpoint)], for: a)
        guard original.view == nil, storeA.proxyConfigurations.count == 1,
              storeA.proxyConfigurations[0].allowFailover == false, storeB.proxyConfigurations.isEmpty else {
            throw IsolationError.mismatch("代理绑定或 WebView 释放")
        }
        let replacement = try registry.makeWebView(for: a)
        guard replacement.view?.configuration.websiteDataStore === storeA else {
            throw IsolationError.mismatch("重建后的 WebView")
        }
        try registry.replaceProxies([], for: a)
        guard replacement.view == nil, storeA.proxyConfigurations.isEmpty, storeB.proxyConfigurations.isEmpty else {
            throw IsolationError.mismatch("代理清理")
        }
    }

    private func perform(_ message: String, _ operation: () async throws -> Void) async {
        busy = true
        status = message
        defer { busy = false }
        do {
            try await operation()
        } catch {
            status = error.localizedDescription
        }
    }

    private func savePhase(_ value: String) {
        phase = value
        defaults.set(value, forKey: phaseKey)
    }
}

@MainActor
private final class IsolationPage: NSObject, WKNavigationDelegate {
    private static let origin = URL(string: "https://isolation.diehao.invalid/")!
    private static let cookieName = "diehao_t1_isolation"
    private let handle: AccountWebView
    private var navigation: CheckedContinuation<Void, Error>?
    private var activeNavigation: WKNavigation?
    private var watchdog: Task<Void, Never>?

    init(registry: AccountStoreRegistry, id: UUID) throws {
        handle = try registry.makeWebView(for: id)
    }

    func open() async throws {
        let view = try activeView()
        view.navigationDelegate = self
        do {
            try await load(view)
        } catch IsolationError.navigation {
            view.stopLoading()
            try await load(view)
        }
        let actualOrigin = try await view.evaluateJavaScript("location.origin") as? String
        guard actualOrigin == "https://isolation.diehao.invalid" else {
            throw IsolationError.mismatch("自检页来源")
        }
    }

    private func load(_ view: WKWebView) async throws {
        try await withCheckedThrowingContinuation { continuation in
            navigation = continuation
            watchdog = Task { [weak self] in
                do { try await Task.sleep(for: .seconds(10)) } catch { return }
                self?.finish(throwing: IsolationError.navigation)
            }
            let page = "<!doctype html><meta charset=utf-8><title>Isolation Check</title>"
            activeNavigation = view.loadSimulatedRequest(URLRequest(url: Self.origin), responseHTML: page)
        }
    }

    func write(_ marker: String) async throws {
        let view = try activeView()
        let script = "localStorage.setItem('isolation-marker', marker)"
        _ = try await view.callAsyncJavaScript(script, arguments: ["marker": marker], in: nil, contentWorld: .page)
        guard let host = Self.origin.host, let cookie = HTTPCookie(properties: [
            .domain: host, .path: "/", .name: Self.cookieName, .value: marker,
            .secure: "TRUE", .expires: Date().addingTimeInterval(7 * 24 * 60 * 60),
        ]) else { throw IsolationError.mismatch("创建测试 Cookie") }
        await view.configuration.websiteDataStore.httpCookieStore.setCookie(cookie)
    }

    func expect(_ marker: String, label: String) async throws {
        let view = try activeView()
        let observation = try await view.evaluateJavaScript("localStorage.getItem('isolation-marker') ?? ''")
        let cookies = await view.configuration.websiteDataStore.httpCookieStore.allCookies()
        let matching = cookies.filter { $0.name == Self.cookieName }.map(\.value)
        let expected = marker.isEmpty ? [] : [marker]
        guard observation as? String == marker, matching == expected else {
            throw IsolationError.mismatch("\(label)（\(observation as? String == marker ? "Cookie" : "localStorage")）")
        }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        guard navigation === activeNavigation else { return }
        finish()
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        guard navigation === activeNavigation else { return }
        finish(throwing: error)
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        guard navigation === activeNavigation else { return }
        finish(throwing: error)
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        finish(throwing: IsolationError.navigation)
    }

    private func activeView() throws -> WKWebView {
        guard let view = handle.view else { throw IsolationError.navigation }
        return view
    }

    private func finish(throwing error: Error? = nil) {
        guard let continuation = navigation else { return }
        navigation = nil
        activeNavigation = nil
        watchdog?.cancel()
        watchdog = nil
        if let error { continuation.resume(throwing: error) } else { continuation.resume() }
    }
}
