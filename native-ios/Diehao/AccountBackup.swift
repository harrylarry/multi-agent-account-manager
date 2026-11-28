import Argon2
import CryptoKit
import Foundation
import Security
import WebKit

enum BackupMode: String, CaseIterable, Codable, Identifiable, Sendable {
    case light, full, custom
    var id: Self { self }
    var label: String {
        switch self {
        case .light: "轻量"
        case .full: "全量"
        case .custom: "自定义"
        }
    }
}

enum BackupError: LocalizedError {
    case invalid, credentials, expired, collision, rollback, concurrent
    var errorDescription: String? {
        switch self {
        case .invalid: "备份内容或版本无法验证。"
        case .credentials: "口令或恢复码无法解开备份。"
        case .expired: "此备份已过期或在本机吊销。"
        case .collision: "账号资料冲突，无法安全恢复。"
        case .rollback: "部分资料等待恢复。请重启后回到备份页面。"
        case .concurrent: "资料已变化，请重新预览备份。"
        }
    }
}

struct BackupField<Value: Codable & Sendable>: Codable, Sendable {
    let time: Date
    let value: Value

    func merged(with other: Self?) throws -> Self {
        guard let other else { return self }
        if time != other.time { return time > other.time ? self : other }
        let lhs = try BackupCodec.encode(value)
        let rhs = try BackupCodec.encode(other.value)
        return lhs.lexicographicallyPrecedes(rhs) ? other : self
    }
}

struct BackupProxy: Codable, Sendable {
    let scheme: String
    let host: String
    let port: UInt16
    let username: String
    let password: String

    func settings() throws -> AccountProxySettings {
        guard let kind = AccountProxySettings.Scheme(rawValue: scheme), username.count <= 512,
              password.count <= 4096, !username.isEmpty || password.isEmpty else { throw BackupError.invalid }
        return try AccountProxySettings(scheme: kind, host: host, port: String(port), username: username)
    }
}

struct BackupCookies: Codable, Sendable {
    let viewerHash: Data
    let records: [MuseCookie]

    func preview() throws -> CookiePreview {
        guard viewerHash.count == 32 else { throw BackupError.invalid }
        let live = records.filter { $0.expirationDate.map { $0 > Date().timeIntervalSince1970 } ?? true }
        let preview = try CookiePreview(live)
        guard preview.notes.isEmpty else { throw BackupError.invalid }
        return preview
    }

    func merged(with other: Self) throws -> Self {
        guard viewerHash == other.viewerHash else { throw BackupError.collision }
        var byKey = Dictionary(uniqueKeysWithValues: records.map { ($0.key, $0) })
        for new in other.records {
            if let old = byKey[new.key] {
                let oldExpiry = old.expirationDate ?? -.infinity
                let newExpiry = new.expirationDate ?? -.infinity
                if oldExpiry != newExpiry {
                    if newExpiry > oldExpiry { byKey[new.key] = new }
                } else if try BackupCodec.encode(old).lexicographicallyPrecedes(BackupCodec.encode(new)) {
                    byKey[new.key] = new
                }
            } else { byKey[new.key] = new }
        }
        return Self(viewerHash: viewerHash, records: byKey.values.sorted { $0.key < $1.key })
    }
}

struct BackupAccount: Codable, Identifiable, Sendable {
    let id: UUID
    var alias: BackupField<String>
    var proxy: BackupField<BackupProxy?>?
    var cookies: BackupField<BackupCookies>?
    var requiresProxy: Bool

    func merged(with incoming: Self) throws -> Self {
        let combinedCookies: BackupField<BackupCookies>?
        if let cookies, let rhs = incoming.cookies {
            combinedCookies = BackupField(time: max(cookies.time, rhs.time), value: try cookies.value.merged(with: rhs.value))
        } else { combinedCookies = cookies ?? incoming.cookies }
        let selectedProxy = try proxy?.merged(with: incoming.proxy) ?? incoming.proxy
        return Self(id: id, alias: try alias.merged(with: incoming.alias), proxy: selectedProxy,
                    cookies: combinedCookies, requiresProxy: selectedProxy.map { $0.value != nil } ??
                        (requiresProxy || incoming.requiresProxy))
    }
}

struct BackupSnapshot: Codable, Sendable {
    let version: Int
    let id: UUID
    let created: Date
    let expires: Date?
    let mode: BackupMode
    let entries: [BackupAccount]
    let manifest: [String: Data]

    init(id: UUID = UUID(), created: Date = .now, expires: Date?, mode: BackupMode, entries: [BackupAccount]) throws {
        version = 1
        self.id = id
        self.created = created
        self.expires = expires
        self.mode = mode
        self.entries = entries.sorted { $0.id.uuidString < $1.id.uuidString }
        manifest = try Self.digests(entries)
        try validate()
    }

    func validate() throws {
        guard version == 1, entries.count <= 250, expires.map({ $0 > created }) ?? true,
              manifest == (try Self.digests(entries)) else { throw BackupError.invalid }
        for entry in entries {
            guard entry.alias.value.utf8.count <= 512 else { throw BackupError.invalid }
            _ = try entry.proxy?.value?.settings()
            if let cookies = entry.cookies?.value {
                guard cookies.viewerHash.count == 32, cookies.records.count <= 2_000,
                      Set(cookies.records.map(\.key)).count == cookies.records.count,
                      cookies.records.allSatisfy({ $0.family != nil }) else { throw BackupError.invalid }
            }
        }
    }

    private static func digests(_ entries: [BackupAccount]) throws -> [String: Data] {
        var manifest: [String: Data] = [:]
        for entry in entries {
            guard manifest[entry.id.uuidString] == nil else { throw BackupError.invalid }
            manifest[entry.id.uuidString] = Data(SHA256.hash(data: try BackupCodec.encode(entry)))
        }
        return manifest
    }
}

enum BackupCodec {
    struct Header: Codable, Sendable {
        let version: Int
        let id: UUID
        let memory: Int
        let iterations: Int
        let parallelism: Int
        let salt: Data
    }
    private struct Envelope: Codable, Sendable {
        let header: Header
        let passphrase: Data
        let recovery: Data
        let contents: Data
    }
    static let maxSize = 32 * 1024 * 1024

    static func encode<T: Encodable>(_ value: T) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        return try encoder.encode(value)
    }

    static func seal(_ snapshot: BackupSnapshot, password: String, memory: Int) async throws -> (data: Data, code: String) {
        guard (12...512).contains(password.count), [32_768, 65_536].contains(memory) else { throw BackupError.credentials }
        return try await Task.detached(priority: .userInitiated) {
            try snapshot.validate()
            let header = Header(version: 1, id: snapshot.id, memory: memory, iterations: 3, parallelism: 1,
                                salt: try random(32))
            let publicBytes = try encode(header)
            let recover = try random(32)
            let contentKey = try random(32)
            let primary = try derive(password, header)
            let recovery = recoveryKey(recover, header: header)
            let contents = try box(encode(snapshot), key: SymmetricKey(data: contentKey), context: publicBytes)
            let envelope = try encode(Envelope(header: header,
                passphrase: box(contentKey, key: primary, context: publicBytes + Data([1])),
                recovery: box(contentKey, key: recovery, context: publicBytes + Data([2])), contents: contents))
            guard envelope.count <= maxSize else { throw BackupError.invalid }
            let text = recover.map { String(format: "%02x", $0) }.joined()
            let compact = Array(text)
            let code = stride(from: 0, to: compact.count, by: 8).map { String(compact[$0..<$0 + 8]) }.joined(separator: "-")
            return (envelope, code)
        }.value
    }

    static func open(_ bytes: Data, secret: String, useRecovery: Bool) async throws -> BackupSnapshot {
        guard bytes.count <= maxSize else { throw BackupError.invalid }
        return try await Task.detached(priority: .userInitiated) {
            let envelope = try JSONDecoder().decode(Envelope.self, from: bytes)
            let h = envelope.header
            guard h.version == 1, [32_768, 65_536].contains(h.memory), h.iterations == 3,
                  h.parallelism == 1, h.salt.count == 32 else { throw BackupError.invalid }
            let headerBytes = try encode(h)
            let wrapKey: SymmetricKey
            let wrapped: Data
            if useRecovery {
                let code = secret.filter { !$0.isWhitespace && $0 != "-" }
                guard code.utf8.count == 64, code.utf8.allSatisfy({ (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) })
                else { throw BackupError.credentials }
                var raw = Data()
                var start = code.startIndex
                while start < code.endIndex {
                    let end = code.index(start, offsetBy: 2)
                    guard let byte = UInt8(code[start..<end], radix: 16) else { throw BackupError.credentials }
                    raw.append(byte)
                    start = end
                }
                wrapKey = recoveryKey(raw, header: h)
                wrapped = envelope.recovery
            } else {
                guard (12...512).contains(secret.count) else { throw BackupError.credentials }
                wrapKey = try derive(secret, h)
                wrapped = envelope.passphrase
            }
            guard let contentKey = try? AES.GCM.open(AES.GCM.SealedBox(combined: wrapped), using: wrapKey,
                authenticating: headerBytes + Data([useRecovery ? 2 : 1])), contentKey.count == 32 else {
                throw BackupError.credentials
            }
            let plaintext = try AES.GCM.open(AES.GCM.SealedBox(combined: envelope.contents), using: SymmetricKey(data: contentKey),
                                             authenticating: headerBytes)
            let snapshot = try JSONDecoder().decode(BackupSnapshot.self, from: plaintext)
            try snapshot.validate()
            guard snapshot.id == h.id else { throw BackupError.invalid }
            if let expires = snapshot.expires, expires <= .now { throw BackupError.expired }
            return snapshot
        }.value
    }

    private static func box(_ bytes: Data, key: SymmetricKey, context: Data) throws -> Data {
        guard let sealed = try AES.GCM.seal(bytes, using: key, authenticating: context).combined else { throw BackupError.invalid }
        return sealed
    }

    private static func derive(_ password: String, _ header: Header) throws -> SymmetricKey {
        let key = try Argon2.hash(password: Data(password.utf8), salt: header.salt, iterations: header.iterations,
                                  memory: header.memory, parallelism: header.parallelism,
                                  outputLength: 32, variant: .id)
        return SymmetricKey(data: key)
    }

    private static func recoveryKey(_ bytes: Data, header: Header) -> SymmetricKey {
        HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: bytes), salt: header.salt,
                              info: Data("Diehao backup recovery v1".utf8), outputByteCount: 32)
    }

    private static func random(_ count: Int) throws -> Data {
        var bytes = Data(count: count)
        let status = bytes.withUnsafeMutableBytes { SecRandomCopyBytes(kSecRandomDefault, count, $0.baseAddress!) }
        guard status == errSecSuccess else { throw BackupError.invalid }
        return bytes
    }
}

@MainActor
final class AccountBackupService {
    private struct Undo: Codable {
        let id: UUID
        let original: Bool
        let alias: String?
        let proxy: BackupProxy?
        let cookies: [MuseCookie]?
    }
    private struct Journal: Codable {
        var completed = false
        let rows: [Undo]
    }

    private let registry: AccountStoreRegistry
    private let folder: URL
    private var journalURL: URL { folder.appending(path: "restore-journal.sealed") }
    private let keyName = "backup-recovery-journal-v1"
    private static var transactionInProgress = false

    init(registry: AccountStoreRegistry, folder: URL = URL.applicationSupportDirectory.appending(path: "DiehaoNative/Backups")) {
        self.registry = registry
        self.folder = folder
    }

    func capture(mode: BackupMode, includeCookies: Bool, includeProxy: Bool, expires: Date?) async throws -> BackupSnapshot {
        let observedAt = Date()
        let all = registry.accounts
        _ = try registry.backupAccounts(Set(all.map(\.id)))
        var rows: [BackupAccount] = []
        for account in all {
            try Task.checkCancellation()
            let proxy = includeProxy ? BackupField(time: observedAt, value: try exportProxy(account)) : nil
            var captured: BackupField<BackupCookies>?
            if includeCookies {
                let records = try await readCookies(account.id)
                if !records.isEmpty {
                    guard !account.usesLegacyDefault, !account.requiresProxy || includeProxy else { throw BackupError.collision }
                    let initial = try CookiePreview(records)
                    let hash = try await viewer(account.id)
                    let final = try await readCookies(account.id)
                    guard try BackupCodec.encode(initial.cookies) == BackupCodec.encode(CookiePreview(final).cookies) else {
                        throw BackupError.concurrent
                    }
                    captured = BackupField(time: observedAt, value: BackupCookies(viewerHash: hash, records: initial.cookies))
                }
            }
            rows.append(BackupAccount(id: account.id, alias: BackupField(time: observedAt, value: account.displayName),
                                      proxy: proxy, cookies: captured, requiresProxy: account.requiresProxy))
        }
        return try BackupSnapshot(expires: expires, mode: mode, entries: rows)
    }

    func preview(_ incoming: BackupSnapshot) throws -> (new: Int, existing: Int) {
        try incoming.validate()
        let local = Set(registry.accounts.map(\.id))
        let already = incoming.entries.count { local.contains($0.id) }
        return (incoming.entries.count - already, already)
    }

    func restore(_ incoming: BackupSnapshot, merge: Bool) async throws {
        guard !Self.transactionInProgress else { throw BackupError.concurrent }
        Self.transactionInProgress = true
        defer { Self.transactionInProgress = false }
        _ = try preview(incoming)
        if let expiry = incoming.expires, expiry <= .now { throw BackupError.expired }
        guard !FileManager.default.fileExists(atPath: journalURL.path) else { throw BackupError.rollback }
        let accounts = Dictionary(uniqueKeysWithValues: registry.accounts.map { ($0.id, $0) })
        let touched = Set(incoming.entries.map(\.id))
        _ = try registry.backupAccounts(touched.intersection(accounts.keys))
        var goals: [BackupAccount] = []
        var undo: [Undo] = []
        for entry in incoming.entries {
            try Task.checkCancellation()
            let current = accounts[entry.id]
            if current?.usesLegacyDefault == true || (current?.requiresProxy == true && current?.proxy == nil) {
                throw BackupError.collision
            }
            if let proposed = entry.cookies?.value { _ = try proposed.preview() }
            let originalCookies: [MuseCookie]?
            if current != nil, entry.cookies != nil {
                originalCookies = try await readCookies(entry.id)
            } else { originalCookies = nil }
            let originalProxy = try current.flatMap(exportProxy)
            var next = entry
            if let current {
                // A signed-in slot may only accept the same authenticated Muse viewer.
                if let candidate = entry.cookies?.value, let originalCookies, !originalCookies.isEmpty {
                    let identity = try await viewer(entry.id)
                    guard identity == candidate.viewerHash else { throw BackupError.collision }
                    if merge {
                        let original = BackupCookies(viewerHash: identity, records: originalCookies)
                        next.cookies = BackupField(time: entry.cookies?.time ?? incoming.created,
                                                   value: try original.merged(with: candidate))
                    }
                }
                if merge {
                    let localTime = try fieldTimes()[entry.id]
                    if let localTime {
                        let local = BackupAccount(id: entry.id, alias: BackupField(time: localTime.alias, value: current.displayName),
                            proxy: localTime.proxy.map { BackupField(time: $0, value: originalProxy) }, cookies: nil,
                            requiresProxy: current.requiresProxy)
                        next = try local.merged(with: next)
                    }
                }
                if current.requiresProxy, let change = next.proxy, change.value == nil { throw BackupError.collision }
            }
            if next.cookies != nil, (current?.requiresProxy ?? next.requiresProxy),
               next.proxy?.value == nil && originalProxy == nil { throw BackupError.collision }
            undo.append(Undo(id: entry.id, original: current != nil, alias: current?.alias,
                             proxy: originalProxy, cookies: entry.cookies == nil ? nil : originalCookies))
            goals.append(next)
        }
        // No persistent change precedes validation of the entire backup.
        var journal = Journal(rows: undo)
        try writeJournal(journal)
        do {
            let existing = touched.intersection(accounts.keys)
            try registry.beginBackup(existing)
            for row in undo {
                guard let original = row.cookies else { continue }
                let actual = try await readCookies(row.id)
                if try BackupCodec.encode(original) != BackupCodec.encode(actual) {
                    try registry.finishBackup(existing)
                    try FileManager.default.removeItem(at: journalURL)
                    throw BackupError.concurrent
                }
            }
            for goal in goals {
                try Task.checkCancellation()
                if accounts[goal.id] == nil {
                    try await registry.createBackupAccount(id: goal.id, alias: goal.alias.value,
                                                           requiresProxy: goal.requiresProxy)
                } else {
                    try registry.setBackupAlias(goal.alias.value, for: goal.id)
                }
                if let proxy = goal.proxy { try setProxy(proxy.value, id: goal.id) }
                if let cookie = goal.cookies?.value {
                    let store = try registry.dataStore(for: goal.id).httpCookieStore
                    let records = try cookie.preview().cookies.map { try $0.native() }
                    try await registry.storeStep(for: goal.id) { try await CookieTransfer.replace(records, in: store) }
                    guard try await viewer(goal.id, restoring: true) == cookie.viewerHash else { throw BackupError.collision }
                }
            }
            journal.completed = true
            try writeJournal(journal)
            try registry.finishBackup(touched)
            try FileManager.default.removeItem(at: journalURL)
            try? saveFieldTimes(goals)
        } catch let cause {
            do { try await recoverJournal() } catch { throw BackupError.rollback }
            if !journal.completed { throw cause }
        }
    }

    func recover() async throws {
        guard !Self.transactionInProgress else { throw BackupError.concurrent }
        Self.transactionInProgress = true
        defer { Self.transactionInProgress = false }
        try await recoverJournal()
    }

    private func recoverJournal() async throws {
        guard FileManager.default.fileExists(atPath: journalURL.path) else { return }
        let sealed = try AES.GCM.SealedBox(combined: Data(contentsOf: journalURL))
        let journal = try JSONDecoder().decode(Journal.self, from: AES.GCM.open(sealed, using: journalKey()))
        if !journal.completed {
            for row in journal.rows.reversed() {
                if !row.original {
                    if registry.hasPendingBackupWrite(row.id) { try await registry.remove(row.id, recoveringBackup: true) }
                    continue
                }
                guard registry.hasPendingBackupWrite(row.id) else { continue }
                try registry.setBackupAlias(row.alias, for: row.id)
                try setProxy(row.proxy, id: row.id)
                if let old = row.cookies {
                    let records = try old.filter { $0.expirationDate.map { $0 > Date().timeIntervalSince1970 } ?? true }
                        .map { try $0.native() }
                    let store = try registry.dataStore(for: row.id).httpCookieStore
                    try await registry.storeStep(for: row.id) { try await CookieTransfer.replace(records, in: store) }
                }
            }
        }
        try registry.finishBackup(Set(journal.rows.map(\.id)))
        try FileManager.default.removeItem(at: journalURL)
    }

    private func readCookies(_ id: UUID) async throws -> [MuseCookie] {
        let store = try registry.dataStore(for: id).httpCookieStore
        let cookies = try await registry.storeStep(for: id) { await store.allCookies() }
        return cookies.map(MuseCookie.init).filter { $0.family != nil }.sorted { $0.key < $1.key }
    }

    private func viewer(_ id: UUID, restoring: Bool = false) async throws -> Data {
        let handle = try restoring ? registry.makeVerificationView(for: id) : registry.makeWebView(for: id)
        guard let view = handle.view else { throw BackupError.invalid }
        return try await CookieLoginProbe().viewerHash(view)
    }

    private func exportProxy(_ account: AccountIdentity) throws -> BackupProxy? {
        guard let settings = account.proxy else {
            if account.requiresProxy { throw BackupError.collision }
            return nil
        }
        var password = ""
        if let key = settings.credentialID {
            guard let secret = try T2Keychain.read(key), let decoded = String(data: secret, encoding: .utf8) else {
                throw BackupError.collision
            }
            password = decoded
        }
        return BackupProxy(scheme: settings.scheme.rawValue, host: settings.host, port: settings.port,
                           username: settings.username, password: password)
    }

    private func setProxy(_ proxy: BackupProxy?, id: UUID) throws {
        try registry.setProxy(proxy?.settings(), password: proxy?.password ?? "", for: id, restoringBackup: true)
    }

    private func writeJournal(_ journal: Journal) throws {
        let key = try journalKey(create: true)
        guard let data = try AES.GCM.seal(BackupCodec.encode(journal), using: key).combined else { throw BackupError.invalid }
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try data.write(to: journalURL, options: [.atomic, .completeFileProtection])
    }

    private func journalKey(create: Bool = false) throws -> SymmetricKey {
        if let stored = try T2Keychain.read(keyName), stored.count == 32 { return SymmetricKey(data: stored) }
        guard create, !FileManager.default.fileExists(atPath: journalURL.path) else { throw BackupError.rollback }
        let key = SymmetricKey(size: .bits256)
        try T2Keychain.save(key.withUnsafeBytes { Data($0) }, as: keyName)
        return key
    }

    private struct FieldTimes: Codable { let alias: Date; let proxy: Date? }
    private func fieldTimes() throws -> [UUID: FieldTimes] {
        let path = folder.appending(path: "field-times.json")
        do { return try JSONDecoder().decode([UUID: FieldTimes].self, from: Data(contentsOf: path)) }
        catch let error as CocoaError where error.code == .fileReadNoSuchFile { return [:] }
    }

    private func saveFieldTimes(_ rows: [BackupAccount]) throws {
        var fields = try fieldTimes()
        for row in rows {
            fields[row.id] = FieldTimes(alias: row.alias.time, proxy: row.proxy?.time ?? fields[row.id]?.proxy)
        }
        try BackupCodec.encode(fields).write(to: folder.appending(path: "field-times.json"), options: [.atomic, .completeFileProtection])
    }
}
