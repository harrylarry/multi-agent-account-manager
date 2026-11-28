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
}

enum RegistryError: LocalizedError {
    case invalidIndex
    case unavailableIdentity
    case proxyUnavailable
    case legacyStoreIsShared
    case removalInProgress
    case storeStillExists

    var errorDescription: String? {
        switch self {
        case .invalidIndex: "账号注册表无效。"
        case .unavailableIdentity: "账号不存在或正在清理。"
        case .proxyUnavailable: "此账号的代理尚未就绪。"
        case .legacyStoreIsShared: "旧默认资料由旧版管理。"
        case .removalInProgress: "正在删除这个资料。"
        case .storeStillExists: "WebKit 仍报告该资料存在，可以重试清理。"
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
        let identity = try activeIdentity(id)
        guard !identity.requiresProxy || proxyConfigurations[id]?.isEmpty == false else {
            throw RegistryError.proxyUnavailable
        }
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = try dataStore(for: id)
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        let handle = AccountWebView(configuration: configuration)
        webViews[id, default: []].removeAll { $0.value == nil }
        webViews[id, default: []].append(WeakView(handle))
        return handle
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
        let existing = await WKWebsiteDataStore.allDataStoreIdentifiers
        if existing.contains(id) {
            try await WKWebsiteDataStore.remove(forIdentifier: id)
        }
        let remaining = await WKWebsiteDataStore.allDataStoreIdentifiers
        guard !remaining.contains(id) else { throw RegistryError.storeStillExists }
        next = index
        next.identities.removeAll { $0.id == id }
        next.pendingRemoval.remove(id)
        try save(next)
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
