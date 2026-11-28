import CryptoKit
import Foundation
import Observation
import SQLite3

struct MuseSendAck: Decodable {
    var message: String?
    var thread: String?
    var rejected: Bool?
}

private enum SendPhase: String, Codable {
    case sending, unknown, confirmed, failed, retryAvailable, deadLetter

    var unresolved: Bool {
        switch self {
        case .sending, .unknown, .retryAvailable, .deadLetter: true
        case .confirmed, .failed: false
        }
    }
}

private struct SendReceipt: Codable {
    let key: UUID
    let account: UUID
    let digest: Data
    let thread: String
    let started: Date
    var updated: Date
    var phase: SendPhase
    var attemptedAgain = false
    var failedChecks = 0
}

@MainActor
private struct SwiftSendLedger {
    private let path: URL
    private let keyName: String

    init(path: URL, keyName: String) {
        self.path = path
        self.keyName = keyName
    }

    func digest(_ text: String) throws -> Data {
        try Data(HMAC<SHA256>.authenticationCode(for: Data(text.utf8), using: key()))
    }

    func all() throws -> [SendReceipt] {
        let encryptionKey = try key()
        return try database { db in
            var query: OpaquePointer?
            guard sqlite3_prepare_v2(db, "SELECT id, sealed FROM sends", -1, &query, nil) == SQLITE_OK else {
                throw MuseActivityError.unavailable
            }
            defer { sqlite3_finalize(query) }
            var receipts: [SendReceipt] = []
            while true {
                let state = sqlite3_step(query)
                if state == SQLITE_DONE { return receipts }
                guard state == SQLITE_ROW, let id = sqlite3_column_text(query, 0),
                      let bytes = sqlite3_column_blob(query, 1), sqlite3_column_bytes(query, 1) > 0 else {
                    throw MuseActivityError.unavailable
                }
                let identifier = String(cString: id)
                let sealed = Data(bytes: bytes, count: Int(sqlite3_column_bytes(query, 1)))
                let row = try AES.GCM.open(try AES.GCM.SealedBox(combined: sealed), using: encryptionKey,
                                           authenticating: Data(identifier.utf8))
                let receipt = try JSONDecoder().decode(SendReceipt.self, from: row)
                guard identifier == receipt.key.uuidString else { throw MuseActivityError.invalidFrame }
                receipts.append(receipt)
            }
        }
    }

    func save(_ receipt: SendReceipt) throws {
        let id = receipt.key.uuidString
        let row = try JSONEncoder().encode(receipt)
        guard let sealed = try AES.GCM.seal(row, using: key(), authenticating: Data(id.utf8)).combined else {
            throw MuseActivityError.unavailable
        }
        try database { db in
            var query: OpaquePointer?
            let sql = "INSERT INTO sends(id,sealed) VALUES(?,?) ON CONFLICT(id) DO UPDATE SET sealed=excluded.sealed"
            guard sqlite3_prepare_v2(db, sql, -1, &query, nil) == SQLITE_OK else { throw MuseActivityError.unavailable }
            defer { sqlite3_finalize(query) }
            let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
            guard sqlite3_bind_text(query, 1, id, -1, transient) == SQLITE_OK,
                  sealed.withUnsafeBytes({ sqlite3_bind_blob(query, 2, $0.baseAddress, Int32($0.count), transient) }) == SQLITE_OK,
                  sqlite3_step(query) == SQLITE_DONE else { throw MuseActivityError.unavailable }
        }
    }

    private func key() throws -> SymmetricKey {
        if let stored = try T2Keychain.read(keyName) {
            guard stored.count == 32 else { throw MuseActivityError.unavailable }
            return SymmetricKey(data: stored)
        }
        guard !FileManager.default.fileExists(atPath: path.path) else { throw MuseActivityError.unavailable }
        let encryptionKey = SymmetricKey(size: .bits256)
        try T2Keychain.save(encryptionKey.withUnsafeBytes { Data($0) }, as: keyName)
        return encryptionKey
    }

    private func database<T>(_ operation: (OpaquePointer?) throws -> T) throws -> T {
        let files = FileManager.default
        try files.createDirectory(at: path.deletingLastPathComponent(), withIntermediateDirectories: true)
        if !files.fileExists(atPath: path.path) {
            guard files.createFile(atPath: path.path, contents: nil,
                                   attributes: [.posixPermissions: 0o600, .protectionKey: FileProtectionType.complete]) else {
                throw MuseActivityError.unavailable
            }
        }
        try files.setAttributes([.posixPermissions: 0o600], ofItemAtPath: path.path)
        var db: OpaquePointer?
        defer { sqlite3_close(db) }
        guard sqlite3_open_v2(path.path, &db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, nil) == SQLITE_OK,
              sqlite3_exec(db, "PRAGMA synchronous=FULL;CREATE TABLE IF NOT EXISTS sends(id TEXT PRIMARY KEY,sealed BLOB NOT NULL);",
                           nil, nil, nil) == SQLITE_OK else { throw MuseActivityError.unavailable }
        return try operation(db)
    }
}

enum SendHistoryVerdict { case matching, empty, unknown }

@MainActor
@Observable
final class MuseSendCoordinator {
    private let ledger: SwiftSendLedger
    private let create: @MainActor (UUID) async throws -> String
    private let send: @MainActor (UUID, String, String, UUID) async throws -> MuseSendAck
    private let history: @MainActor (UUID, String) async throws -> Data
    private var locked: Set<UUID> = []

    convenience init(registry: AccountStoreRegistry) {
        self.init(path: URL.applicationSupportDirectory.appending(path: "DiehaoNative/send.sqlite"),
                  keyName: "swift-send-ledger", create: { account in
            let connection = try await NativeMuseActivityConnection.open(registry: registry, id: account)
            defer { connection.close() }
            let result = try MuseApproval.object(await connection.call("/session/start"))
            let data = result["data"] as? [String: Any] ?? result
            let scope = (data["session"] ?? data["thread"]) as? [String: Any] ?? data
            let raw = ["session_id", "sessionId", "thread_id", "threadId", "id"].compactMap { scope[$0] as? String }.first
            guard let raw, let id = UUID(uuidString: raw) else {
                throw MuseActivityError.invalidFrame
            }
            let listed = try MuseReply.sessions(from: await connection.call("/api/session/list"))
            guard listed.contains(where: { UUID(uuidString: $0.id) == id }) else { throw MuseActivityError.invalidFrame }
            return id.uuidString.lowercased()
        }, send: { account, text, thread, key in
            let connection = try await NativeMuseActivityConnection.open(registry: registry, id: account)
            defer { connection.close() }
            return try await connection.sendNew(text, thread: thread, key: key)
        }, history: { account, thread in
            let connection = try await NativeMuseActivityConnection.open(registry: registry, id: account)
            defer { connection.close() }
            return try await connection.call("/chat/history?limit=50&transcript_mode=messages&session_id=" + thread)
        })
    }

    init(path: URL, keyName: String, create: @escaping @MainActor (UUID) async throws -> String,
         send: @escaping @MainActor (UUID, String, String, UUID) async throws -> MuseSendAck,
         history: @escaping @MainActor (UUID, String) async throws -> Data) {
        ledger = SwiftSendLedger(path: path, keyName: keyName)
        self.create = create
        self.send = send
        self.history = history
    }

    #if DEBUG && targetEnvironment(simulator)
    static func fixtures() -> MuseSendCoordinator {
        let suffix = "send-fixture." + UUID().uuidString
        let thread = "df60b78e-6b23-4fb5-bc42-e1bfad667a83"
        var written: String?
        return MuseSendCoordinator(path: URL.temporaryDirectory.appending(path: suffix + ".sqlite"),
            keyName: suffix, create: { _ in thread }, send: { _, text, routed, _ in
                guard routed == thread, written == nil else { throw MuseActivityError.rejected(400) }
                written = text
                throw MuseActivityError.timeout
            }, history: { _, routed in
                guard let written, routed == thread else { throw MuseActivityError.unavailable }
                return try JSONSerialization.data(withJSONObject: ["session_id": thread, "has_more": false,
                    "messages": [["role": "user", "session_id": thread, "text": written]]])
            })
    }
    #endif

    func submit(account: UUID, message: String, retry: Bool = false) async -> ComposeResult {
        let message = message.trimmingCharacters(in: .whitespacesAndNewlines)
        guard (1...16_000).contains(message.count) else { return .failed }
        guard locked.insert(account).inserted else { return .busy }
        defer { locked.remove(account) }
        let digest: Data
        let receipts: [SendReceipt]
        do {
            digest = try ledger.digest(message)
            receipts = try ledger.all()
        } catch { return .failed }
        if let same = receipts.first(where: { $0.account == account && $0.phase.unresolved && $0.digest == digest }) {
            guard retry, same.phase == .retryAvailable, !same.attemptedAgain else {
                return same.phase == .retryAvailable ? .retryAvailable : .unknown
            }
            var attempt = same
            attempt.attemptedAgain = true
            return await execute(attempt, message: message)
        }
        guard !retry else { return .failed }
        do {
            let thread = try await create(account)
            guard UUID(uuidString: thread) != nil else { return .failed }
            let receipt = SendReceipt(key: UUID(), account: account, digest: digest, thread: thread,
                                      started: .now, updated: .now, phase: .sending)
            return await execute(receipt, message: message)
        } catch { return .failed }
    }

    private func execute(_ receipt: SendReceipt, message: String) async -> ComposeResult {
        var receipt = receipt
        receipt.phase = .sending
        receipt.updated = .now
        do { try ledger.save(receipt) } catch { return .failed }
        let outcome: ComposeResult
        do {
            let ack = try await send(receipt.account, message, receipt.thread, receipt.key)
            if ack.rejected == true {
                receipt.phase = .failed
                outcome = .failed
            } else if MuseReply.token(ack.message) != nil,
                      ack.thread.flatMap(UUID.init(uuidString:)) == UUID(uuidString: receipt.thread) {
                receipt.phase = .confirmed
                outcome = .confirmed(receipt.thread)
            } else if ack.thread != nil {
                receipt.phase = .deadLetter
                outcome = .unknown
            } else {
                receipt.phase = .unknown
                outcome = .unknown
            }
        } catch MuseActivityError.signIn {
            receipt.phase = .failed
            outcome = .failed
        } catch MuseActivityError.rejected(let status) where [400, 404, 422].contains(status) {
            receipt.phase = .failed
            outcome = .failed
        } catch {
            receipt.phase = .unknown
            outcome = .unknown
        }
        receipt.updated = .now
        do { try ledger.save(receipt) } catch { return .unknown }
        return outcome
    }

    func check(account: UUID, message: String) async -> ComposeResult {
        guard locked.insert(account).inserted else { return .busy }
        defer { locked.remove(account) }
        let digest = try? ledger.digest(message.trimmingCharacters(in: .whitespacesAndNewlines))
        guard let digest, let records = try? ledger.all(),
              let receipt = records.first(where: { $0.account == account && $0.digest == digest && $0.phase.unresolved }) else {
            return .unknown
        }
        return await check(receipt)
    }

    func resume(accounts: Set<UUID>) async {
        guard let records = try? ledger.all() else { return }
        for receipt in records where accounts.contains(receipt.account) && [.unknown, .sending].contains(receipt.phase) {
            if Task.isCancelled { return }
            guard locked.insert(receipt.account).inserted else { continue }
            _ = await check(receipt)
            locked.remove(receipt.account)
        }
    }

    private func check(_ receipt: SendReceipt) async -> ComposeResult {
        if receipt.phase == .retryAvailable { return .retryAvailable }
        if receipt.phase == .deadLetter { return .unknown }
        let (results, continuation) = AsyncStream<SendHistoryVerdict>.makeStream(bufferingPolicy: .bufferingNewest(1))
        let worker = Task {
            let deadline = Date().addingTimeInterval(26)
            var secondEmpty = false
            do {
                repeat {
                    let snapshot = try await history(receipt.account, receipt.thread)
                    let verdict = try verify(snapshot, receipt: receipt)
                    if verdict == .matching { continuation.yield(.matching); break }
                    let settled = Date().timeIntervalSince(receipt.updated) >= 26
                    if verdict == .empty && secondEmpty && settled { continuation.yield(.empty); break }
                    secondEmpty = verdict == .empty
                    try await Task.sleep(for: .seconds(2))
                } while Date() < deadline
            } catch {}
            continuation.finish()
        }
        let timer = Task {
            do { try await Task.sleep(for: .seconds(26)) } catch { return }
            continuation.finish()
        }
        defer { worker.cancel(); timer.cancel() }
        var verdict = SendHistoryVerdict.unknown
        for await result in results { verdict = result }
        guard !Task.isCancelled else { return .unknown }
        var next = receipt
        let outcome: ComposeResult
        switch verdict {
        case .matching:
            next.phase = .confirmed
            outcome = .confirmed(receipt.thread)
        case .empty where !receipt.attemptedAgain:
            next.phase = .retryAvailable
            outcome = .retryAvailable
        default:
            next.failedChecks += 1
            next.phase = next.failedChecks >= 3 ? .deadLetter : .unknown
            outcome = .unknown
        }
        do { try ledger.save(next) } catch { return .unknown }
        return outcome
    }

    private func verify(_ data: Data, receipt: SendReceipt) throws -> SendHistoryVerdict {
        let result = try MuseApproval.object(data)
        guard let scope = (result["session_id"] ?? result["thread_id"]) as? String,
              UUID(uuidString: scope) == UUID(uuidString: receipt.thread),
              result["has_more"] as? Bool != true,
              let rows = (result["chat_events"] ?? result["messages"] ?? result["events"]) as? [[String: Any]] else {
            return .unknown
        }
        if rows.isEmpty, result["has_more"] as? Bool == false { return .empty }
        let user = rows.compactMap { row -> [String: Any]? in
            let payload = row["payload"] as? [String: Any] ?? row
            return payload["role"] as? String == "user" || row["event_name"] as? String == "message.user" ? payload : nil
        }
        guard user.count == 1, let first = user.first,
              let raw = first["session_id"] as? String ?? result["session_id"] as? String,
              UUID(uuidString: raw) == UUID(uuidString: receipt.thread) else { return .unknown }
        let parts = first["content"] as? [[String: Any]]
        let strings = parts?.compactMap { $0["type"] as? String == "text" ? $0["text"] as? String : nil }
        guard strings?.count == parts?.count else { return .unknown }
        let text = first["text"] as? String ?? first["content"] as? String ??
            strings?.joined()
        guard let text, try ledger.digest(text.trimmingCharacters(in: .whitespacesAndNewlines)) == receipt.digest else {
            return .unknown
        }
        return .matching
    }
}
