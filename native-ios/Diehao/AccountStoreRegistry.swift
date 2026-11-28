import Foundation
import Network
import WebKit

struct AccountIdentity: Codable, Identifiable {
    enum Purpose: String, Codable {
        case account
        case isolationA
        case isolationB
    }

    let id: UUID
    let purpose: Purpose
    var usesLegacyDefault = false
    var requiresProxy = false
    var alias: String? = nil
    var proxy: AccountProxySettings? = nil

    var displayName: String { alias ?? "Muse · " + String(id.uuidString.prefix(8)) }
}

enum RegistryError: LocalizedError {
    case invalidIndex
    case unavailableIdentity
    case proxyUnavailable
    case legacyStoreIsShared
    case removalInProgress
    case storeStillExists
    case storeAwaitingRestart
    case unavailableProxyCredential

    var errorDescription: String? {
        switch self {
        case .invalidIndex: "账号注册表无效。"
        case .unavailableIdentity: "账号不存在或正在清理。"
        case .proxyUnavailable: "此账号的代理尚未就绪。"
        case .legacyStoreIsShared: "旧默认资料由旧版管理。"
        case .removalInProgress: "正在删除这个资料。"
        case .storeStillExists: "WebKit 仍报告该资料存在，可以重试清理。"
        case .storeAwaitingRestart: "WebKit 尚在释放测试或账号资料。请彻底退出并重开叠号，再重试清理。"
        case .unavailableProxyCredential: "代理配置或钥匙串凭据不可读，已阻止连接。"
        }
    }
}

@MainActor
final class AccountWebView {
    private(set) var view: WKWebView?

    fileprivate init(configuration: WKWebViewConfiguration) {
        view = WKWebView(frame: .zero, configuration: configuration)
    }

    fileprivate func invalidate() {
        view?.stopLoading()
        view?.navigationDelegate = nil
        view?.uiDelegate = nil
        view?.removeFromSuperview()
        view = nil
    }
}

@MainActor
final class AccountStoreRegistry {
    private struct Index: Codable {
        var version = 1
        var identities: [AccountIdentity] = []
        var pendingRemoval: Set<UUID> = []
        var legacyReferencesChecked = false
        var pendingCookieWrites: Set<UUID>? = nil
    }

    private final class WeakView {
        weak var value: AccountWebView?

        init(_ value: AccountWebView) {
            self.value = value
        }
    }

    private let indexURL: URL
    private var index: Index
    private var stores: [UUID: WKWebsiteDataStore] = [:]
    private var proxyConfigurations: [UUID: [ProxyConfiguration]] = [:]
    private var webViews: [UUID: [WeakView]] = [:]
    private var removing: Set<UUID> = []
    private static var restartRequired: Set<UUID> = []

    init(indexURL: URL) throws {
        self.indexURL = indexURL
        do {
            index = try JSONDecoder().decode(Index.self, from: Data(contentsOf: indexURL))
        } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
            index = Index()
        }
        let ids = index.identities.map(\.id)
        guard index.version == 1, Set(ids).count == ids.count,
              index.pendingRemoval.isSubset(of: Set(ids)),
              index.identities.filter(\.usesLegacyDefault).count <= 1 else {
            throw RegistryError.invalidIndex
        }
    }

    var isolationIdentities: [AccountIdentity] {
        index.identities.filter { $0.purpose != .account }
    }

    var referencedAccountCount: Int {
        index.identities.count { $0.purpose == .account }
    }

    var accounts: [AccountIdentity] {
        index.identities.filter { $0.purpose == .account && !index.pendingRemoval.contains($0.id) }
    }

    @discardableResult
    func retryPendingRemoval(includeIsolation: Bool = true) async -> Bool {
        for id in index.pendingRemoval {
            if !includeIsolation && isolationIdentities.contains(where: { $0.id == id }) { continue }
            // A failed WebKit/Keychain removal remains queued in the index.
            try? await remove(id)
        }
        return index.pendingRemoval.isEmpty
    }

    func createAccount(alias: String, reserving formerID: UUID? = nil, pendingImport: Bool = false) async throws -> AccountIdentity {
        _ = WKWebsiteDataStore.default()
        let existing = await WKWebsiteDataStore.allDataStoreIdentifiers
        var ids = Set(index.identities.map(\.id)).union(existing)
        if let formerID { ids.insert(formerID) }
        let identity = AccountIdentity(id: uniqueID(reserving: &ids), purpose: .account, alias: alias)
        var next = index
        next.identities.append(identity)
        if pendingImport { next.pendingCookieWrites = (next.pendingCookieWrites ?? []).union([identity.id]) }
        try save(next)
        return identity
    }

    func createIsolationPair() async throws -> (a: UUID, b: UUID) {
        _ = WKWebsiteDataStore.default()
        let existing = await WKWebsiteDataStore.allDataStoreIdentifiers
        guard isolationIdentities.isEmpty else { throw RegistryError.invalidIndex }
        var registered = Set(index.identities.map(\.id))
        registered.formUnion(existing)
        let a = uniqueID(reserving: &registered)
        let b = uniqueID(reserving: &registered)
        var next = index
        next.identities += [
            AccountIdentity(id: a, purpose: .isolationA),
            AccountIdentity(id: b, purpose: .isolationB),
        ]
        try save(next)
        return (a, b)
    }

    func dataStore(for id: UUID) throws -> WKWebsiteDataStore {
        let identity = try activeIdentity(id)
        if let store = stores[id] { return store }
        let store = identity.usesLegacyDefault ? WKWebsiteDataStore.default() : WKWebsiteDataStore(forIdentifier: id)
        if let proxies = proxyConfigurations[id] {
            store.proxyConfigurations = proxies
        }
        stores[id] = store
        return store
    }

    func makeWebView(for id: UUID) throws -> AccountWebView {
        guard index.pendingCookieWrites?.contains(id) != true else { throw CookieError.rollback }
        return try makeConfiguredWebView(for: id)
    }

    func makeVerificationView(for id: UUID) throws -> AccountWebView {
        try makeConfiguredWebView(for: id)
    }

    private func makeConfiguredWebView(for id: UUID) throws -> AccountWebView {
        let identity = try activeIdentity(id)
        if identity.requiresProxy && proxyConfigurations[id]?.isEmpty != false {
            guard let settings = identity.proxy else { throw RegistryError.proxyUnavailable }
            let resolved = try settings.configuration()
            let store = try dataStore(for: id)
            store.proxyConfigurations = [resolved]
            proxyConfigurations[id] = [resolved]
        }
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = try dataStore(for: id)
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        configuration.allowsInlineMediaPlayback = true
        configuration.mediaTypesRequiringUserActionForPlayback = .all
        configuration.userContentController.addUserScript(MusePageDelegate.inlineVideo)
        if identity.requiresProxy {
            configuration.userContentController.addUserScript(AccountProxySettings.restrictions)
        }
        let handle = AccountWebView(configuration: configuration)
        webViews[id, default: []].removeAll { $0.value == nil }
        webViews[id, default: []].append(WeakView(handle))
        return handle
    }

    func setCookieWritePending(_ pending: Bool, for id: UUID) throws {
        _ = try activeIdentity(id)
        var next = index
        if pending {
            next.pendingCookieWrites = (next.pendingCookieWrites ?? []).union([id])
        } else {
            next.pendingCookieWrites?.remove(id)
        }
        try save(next)
        if pending { invalidateViews(for: id) }
    }

    func setProxy(_ settings: AccountProxySettings?, password: String, for id: UUID) throws {
        let identity = try activeIdentity(id)
        guard !identity.usesLegacyDefault else { throw RegistryError.legacyStoreIsShared }
        // Store each credential under a fresh reference. A failed index write
        // cannot replace the credential still used by the previous profile.
        let configuration = try settings?.configuration(password: password)
        if let key = settings?.credentialID { try T2Keychain.save(Data(password.utf8), as: key) }
        do {
            var next = index
            guard let position = next.identities.firstIndex(where: { $0.id == id }) else {
                throw RegistryError.unavailableIdentity
            }
            next.identities[position].proxy = settings
            next.identities[position].requiresProxy = settings != nil
            try save(next)
        } catch {
            if let key = settings?.credentialID { try? T2Keychain.delete(key) }
            throw error
        }
        invalidateViews(for: id)
        let configs = configuration.map { [$0] } ?? []
        proxyConfigurations[id] = configs
        let store = try dataStore(for: id)
        store.proxyConfigurations = configs
        if let old = identity.proxy?.credentialID { try? T2Keychain.delete(old) }
    }

    // Store configuration is applied before the next view is created. Old views
    // are invalidated so their connections cannot outlive an egress change.
    func replaceProxies(_ configurations: [ProxyConfiguration], for id: UUID) throws {
        let identity = try activeIdentity(id)
        guard !identity.usesLegacyDefault else { throw RegistryError.legacyStoreIsShared }
        var list = configurations
        for position in list.indices {
            list[position].allowFailover = false
        }
        var next = index
        if let position = next.identities.firstIndex(where: { $0.id == id }) {
            next.identities[position].requiresProxy = !list.isEmpty
        }
        try save(next)
        invalidateViews(for: id)
        proxyConfigurations[id] = list
        let store = try dataStore(for: id)
        store.proxyConfigurations = list
    }

    func remove(_ id: UUID) async throws {
        guard !Self.restartRequired.contains(id) else { throw RegistryError.storeAwaitingRestart }
        guard !removing.contains(id) else { throw RegistryError.removalInProgress }
        guard let identity = index.identities.first(where: { $0.id == id }) else { return }
        guard !identity.usesLegacyDefault else { throw RegistryError.legacyStoreIsShared }
        var next = index
        next.pendingRemoval.insert(id)
        try save(next)
        removing.insert(id)
        defer { removing.remove(id) }
        invalidateViews(for: id)
        stores.removeValue(forKey: id)
        proxyConfigurations.removeValue(forKey: id)

        // Initialize the default store before enumeration/removal on a cold launch.
        _ = WKWebsiteDataStore.default()
        let existing = try await removalStep(for: id) { await WKWebsiteDataStore.allDataStoreIdentifiers }
        if existing.contains(id) {
            do {
                try await removalStep(for: id) { try await WKWebsiteDataStore.remove(forIdentifier: id) }
            } catch where Self.isStoreInUse(error) {
                // Clear the target's WebKit caches and storage. WebKit also
                // releases cached processes for this session during this call.
                try await removalStep(for: id) { await Self.purgeWebsiteData(for: id) }
                for delay in [200, 500, 1_000] {
                    try await Task.sleep(for: .milliseconds(delay))
                    do {
                        try await removalStep(for: id) { try await WKWebsiteDataStore.remove(forIdentifier: id) }
                        break
                    } catch where Self.isStoreInUse(error) {
                        if delay == 1_000 {
                            Self.restartRequired.insert(id)
                            throw RegistryError.storeAwaitingRestart
                        }
                    }
                }
            }
        }
        let remaining = try await removalStep(for: id) { await WKWebsiteDataStore.allDataStoreIdentifiers }
        guard !remaining.contains(id) else { throw RegistryError.storeStillExists }
        if let key = identity.proxy?.credentialID { try T2Keychain.delete(key) }
        try T2Keychain.delete("cookie-rollback." + id.uuidString)
        next = index
        next.identities.removeAll { $0.id == id }
        next.pendingRemoval.remove(id)
        next.pendingCookieWrites?.remove(id)
        try save(next)
    }

    private static func isStoreInUse(_ error: Error) -> Bool {
        let native = error as NSError
        return (native.domain == "WKWebSiteDataStore" || native.domain == WKErrorDomain) &&
            native.localizedDescription.lowercased().contains("data store is in use")
    }

    private static func purgeWebsiteData(for id: UUID) async {
        let store = WKWebsiteDataStore(forIdentifier: id)
        await store.removeData(ofTypes: WKWebsiteDataStore.allWebsiteDataTypes(), modifiedSince: .distantPast)
    }

    private func removalStep<Value: Sendable>(for id: UUID,
                                               _ operation: @escaping @MainActor () async throws -> Value) async throws -> Value {
        try await withCheckedThrowingContinuation { continuation in
            let step = RemovalStep(continuation)
            let watchdog = Task {
                do { try await Task.sleep(for: .seconds(8)) } catch { return }
                // A public WebKit callback cannot be cancelled. Keep this UUID
                // blocked in this process instead of starting a second removal.
                if step.resolve(.failure(RegistryError.storeAwaitingRestart)) {
                    Self.restartRequired.insert(id)
                }
            }
            Task {
                do { step.resolve(.success(try await operation())) }
                catch { step.resolve(.failure(error)) }
                watchdog.cancel()
            }
        }
    }

    @MainActor
    private final class RemovalStep<Value: Sendable> {
        private var continuation: CheckedContinuation<Value, Error>?

        init(_ continuation: CheckedContinuation<Value, Error>) { self.continuation = continuation }

        @discardableResult
        func resolve(_ result: Result<Value, Error>) -> Bool {
            guard let continuation else { return false }
            self.continuation = nil
            continuation.resume(with: result)
            return true
        }
    }

    func referenceLegacyProfiles(in applicationSupport: URL, bundleID: String) async throws {
        guard !index.legacyReferencesChecked, bundleID == "com.ldjx.accountmanager" else { return }
        let legacyURL = applicationSupport.appending(path: bundleID).appending(path: "accounts.json")
        let references = try LegacyAccountReferences.read(legacyURL)
        let defaultStore = WKWebsiteDataStore.default()
        let existing = Set(await WKWebsiteDataStore.allDataStoreIdentifiers)
        let records = references.canUseDefault ? await defaultStore.dataRecords(ofTypes: [WKWebsiteDataTypeCookies]) : []
        let hasDefaultMuseCookies = records.contains {
            $0.displayName == "muse.ai" || $0.displayName.hasSuffix(".muse.ai")
        }
        var next = index
        var registered = Set(next.identities.map(\.id))
        for (position, reference) in references.accounts.enumerated() {
            // Only an older index with no other Muse slot can unambiguously use
            // the default profile. v2 rows always point at their original UUID.
            let usesDefault = references.canUseDefault && position == 0 &&
                !existing.contains(reference.id) && hasDefaultMuseCookies
            guard existing.contains(reference.id) || usesDefault,
                  registered.insert(reference.id).inserted else { continue }
            next.identities.append(AccountIdentity(
                id: reference.id, purpose: .account, usesLegacyDefault: usesDefault,
                requiresProxy: reference.requiresProxy
            ))
        }
        next.legacyReferencesChecked = true
        try save(next)
    }

    private func activeIdentity(_ id: UUID) throws -> AccountIdentity {
        guard !index.pendingRemoval.contains(id),
              let identity = index.identities.first(where: { $0.id == id }) else {
            throw RegistryError.unavailableIdentity
        }
        return identity
    }

    private func invalidateViews(for id: UUID) {
        for reference in webViews.removeValue(forKey: id) ?? [] {
            reference.value?.invalidate()
        }
    }

    private func uniqueID(reserving registered: inout Set<UUID>) -> UUID {
        var id = UUID()
        while !registered.insert(id).inserted { id = UUID() }
        return id
    }

    private func save(_ next: Index) throws {
        try FileManager.default.createDirectory(at: indexURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try JSONEncoder().encode(next).write(to: indexURL, options: .atomic)
        index = next
    }
}

private struct LegacyAccountReferences {
    struct Account {
        let id: UUID
        let requiresProxy: Bool
    }

    private struct File: Decodable {
        struct Row: Decodable {
            struct Proxy: Decodable { let enabled: Bool? }
            enum CodingKeys: String, CodingKey { case providerId, provider_id, uuid, id, proxy }
            let providerId: String?
            let uuid: UUID
            let proxy: Proxy?

            init(from decoder: Decoder) throws {
                let fields = try decoder.container(keyedBy: CodingKeys.self)
                providerId = try fields.decodeIfPresent(String.self, forKey: .providerId) ??
                    fields.decodeIfPresent(String.self, forKey: .provider_id)
                uuid = try fields.decodeIfPresent(UUID.self, forKey: .uuid) ?? fields.decode(UUID.self, forKey: .id)
                proxy = try fields.decodeIfPresent(Proxy.self, forKey: .proxy)
            }
        }
        struct Key: Decodable {
            enum CodingKeys: String, CodingKey { case uuid, accountUuid, id, providerId, provider_id }
            let uuid: UUID
            let providerId: String

            init(from decoder: Decoder) throws {
                if let id = try? decoder.singleValueContainer().decode(UUID.self) {
                    uuid = id
                    providerId = "muse"
                    return
                }
                let fields = try decoder.container(keyedBy: CodingKeys.self)
                uuid = try fields.decodeIfPresent(UUID.self, forKey: .uuid) ??
                    fields.decodeIfPresent(UUID.self, forKey: .accountUuid) ?? fields.decode(UUID.self, forKey: .id)
                providerId = try fields.decodeIfPresent(String.self, forKey: .providerId) ??
                    fields.decodeIfPresent(String.self, forKey: .provider_id) ?? "muse"
            }
        }
        enum CodingKeys: String, CodingKey { case version, accounts, pendingCleanup, pending_cleanup }
        let version: Int?
        let accounts: [Row]
        let pendingCleanup: [Key]?

        init(from decoder: Decoder) throws {
            let fields = try decoder.container(keyedBy: CodingKeys.self)
            version = try fields.decodeIfPresent(Int.self, forKey: .version)
            accounts = try fields.decodeIfPresent([Row].self, forKey: .accounts) ?? []
            pendingCleanup = try fields.decodeIfPresent([Key].self, forKey: .pendingCleanup) ??
                fields.decodeIfPresent([Key].self, forKey: .pending_cleanup)
        }
    }

    let canUseDefault: Bool
    let accounts: [Account]

    static func read(_ url: URL) throws -> Self {
        let file: File
        do {
            file = try JSONDecoder().decode(File.self, from: Data(contentsOf: url))
        } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
            return Self(canUseDefault: false, accounts: [])
        }
        let version = file.version ?? 1
        guard (0...2).contains(version) else { throw RegistryError.invalidIndex }
        let pending = Set((file.pendingCleanup ?? []).filter { $0.providerId == "muse" }.map(\.uuid))
        let eligible = file.accounts.filter {
            !pending.contains($0.uuid) && ($0.providerId == "muse" || ($0.providerId == nil && version < 2))
        }
        return Self(canUseDefault: version < 2 && file.accounts.count == 1, accounts: eligible.map {
            Account(id: $0.uuid, requiresProxy: $0.proxy.map { $0.enabled != false } ?? false)
        })
    }
}
