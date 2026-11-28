import Foundation
import Observation
import WebKit

struct MuseReply: Codable, Equatable {
    let thread: String
    let id: String
    let at: Date
    let version: Int

    func isNewer(than other: Self) -> Bool {
        if id == other.id { return version > other.version }
        if at != other.at { return at > other.at }
        return id > other.id
    }

    struct SessionVersion {
        let id: String
        let revision: String?
    }

    static func sessions(from data: Data) throws -> [SessionVersion] {
        let document = try JSONSerialization.jsonObject(with: data)
        let rows = try array(document, keys: ["sessions", "items", "chat_sessions", "threads"])
        return rows.compactMap { row in
            guard let id = token(row["session_id"] ?? row["id"]) else { return nil }
            let revision = row["last_message_at_ms"] ?? row["updated_at_ms"] ?? row["updated_at"] ?? row["version"]
            return SessionVersion(id: id, revision: revision.map { String(describing: $0) })
        }
    }

    static func history(from data: Data, thread: String) throws -> [Self] {
        let root = try JSONSerialization.jsonObject(with: data)
        let envelope = (root as? [String: Any])?["result"] as? [String: Any] ?? root as? [String: Any]
        if let returned = envelope?["session_id"] as? String, returned != thread {
            throw MuseActivityError.invalidFrame
        }
        let rows = try array(root, keys: ["chat_events", "messages", "events"])
        return rows.compactMap { row in
            let payload = row["payload"] as? [String: Any] ?? [:]
            let fields = row.merging(payload) { _, payload in payload }
            if let returned = fields["session_id"] as? String, returned != thread { return nil }
            let event = row["event_name"] as? String ?? row["event"] as? String
            guard fields["role"] as? String == "assistant" || event == "message.assistant" || event == "delta.message_done",
                  fields["display_text_ready"] as? Bool != false else { return nil }
            let status = fields["status"] as? String
            guard status == nil || ["complete", "completed", "done", "finished"].contains(status!) else { return nil }
            return from(fields, thread: thread)
        }
    }

    private static func array(_ root: Any, keys: [String]) throws -> [[String: Any]] {
        let object = root as? [String: Any]
        if object?["ok"] as? Bool == false || object?["error"].map({ !($0 is NSNull) }) == true {
            throw MuseActivityError.unavailable
        }
        let result = object?["result"] ?? root
        if let rows = result as? [[String: Any]] { return rows }
        for key in keys {
            if let rows = (result as? [String: Any])?[key] as? [[String: Any]] { return rows }
        }
        throw MuseActivityError.invalidFrame
    }

    static func token(_ value: Any?) -> String? {
        guard let value = value as? String, !value.isEmpty, value.utf8.count <= 128,
              value.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0)
                  || (97...122).contains($0) || $0 == 45 || $0 == 95 }) else { return nil }
        return value
    }

    static func from(_ fields: [String: Any], thread: String) -> Self? {
        guard let id = token(fields["message_id"] ?? fields["id"]) else { return nil }
        let rawTime = fields["ts_ms"] ?? fields["timestamp_ms"] ?? fields["created_at_ms"] ?? fields["created_at"]
        let numeric = rawTime as? Double ?? (rawTime as? String).flatMap(Double.init)
        let timestamp = numeric.flatMap { value in
            value.isFinite && value > 0 ? Date(timeIntervalSince1970: value > 1e12 ? value / 1000 : value) : nil
        } ?? (rawTime as? String).flatMap { ISO8601DateFormatter().date(from: $0) }
        guard let timestamp else { return nil }
        return Self(thread: thread, id: id, at: timestamp, version: fields["version"] as? Int ?? 0)
    }
}

private struct ActivityRecord: Codable {
    var latest: [String: MuseReply] = [:]
    var seen: [String: MuseReply] = [:]
    var lastSubscription: Date?
    var lastPoll: Date?
    var lastBackfill: Date?
    var gapSince: Date?
    var heartbeat: Date?
    var decodeFailures = 0

    var unread: Int {
        latest.values.count { reply in seen[reply.thread].map { $0.id != reply.id && reply.isNewer(than: $0) } ?? true }
    }

    mutating func merge(_ reply: MuseReply) {
        if let former = latest[reply.thread], !reply.isNewer(than: former) { return }
        latest[reply.thread] = reply
        if latest.count > 500, let oldest = latest.values.min(by: { $0.at < $1.at }) {
            latest.removeValue(forKey: oldest.thread)
            seen.removeValue(forKey: oldest.thread)
        }
    }
}

@MainActor
@Observable
final class MuseActivityCoordinator {
    @ObservationIgnored var onNeedsApproval: (@MainActor (UUID) -> Void)?
    @ObservationIgnored var onRunEnded: (@MainActor (UUID) -> Void)?
    private let storage: URL
    private let open: @MainActor (UUID) async throws -> any MuseActivityConnection
    private var records: [UUID: ActivityRecord]
    private var workers: [UUID: Task<Void, Never>] = [:]
    private var connections: [UUID: any MuseActivityConnection] = [:]
    private var routes: [UUID: String] = [:]
    private var busyAt: [UUID: Date] = [:]
    private var busyChanges: [UUID: Task<Void, Never>] = [:]
    private var sequences: [UUID: Set<Int64>] = [:]
    private var reconciling: Set<UUID> = []
    private var pollSeconds = 20
    private var focused: UUID?

    convenience init(registry: AccountStoreRegistry) {
        self.init(storage: URL.applicationSupportDirectory.appending(path: "DiehaoNative/activity.json")) {
            try await NativeMuseActivityConnection.open(registry: registry, id: $0)
        }
    }

    init(storage: URL, open: @escaping @MainActor (UUID) async throws -> any MuseActivityConnection) {
        self.storage = storage
        self.open = open
        records = (try? JSONDecoder().decode([UUID: ActivityRecord].self, from: Data(contentsOf: storage))) ?? [:]
    }

    #if DEBUG && targetEnvironment(simulator)
    static func fixtures(account: UUID?) -> MuseActivityCoordinator {
        var connections = 0
        return MuseActivityCoordinator(storage: URL.temporaryDirectory.appending(path: UUID().uuidString + ".json")) { id in
            guard account == id else { throw MuseActivityError.signIn }
            connections += 1
            return ActivityFixtureConnection(reconnecting: connections > 1)
        }
    }
    #endif

    func configure(_ accounts: [AccountIdentity], active: Bool, interval: Int) {
        pollSeconds = interval
        let current = Dictionary(uniqueKeysWithValues: accounts.map { account in
            let route = [account.proxy?.scheme.rawValue, account.proxy?.host,
                         account.proxy.map { String($0.port) }, account.proxy?.credentialID,
                         account.requiresProxy ? "proxy" : "direct"].compactMap { $0 }.joined(separator: "/")
            return (account.id, route)
        })
        for id in Array(workers.keys) where !active || current[id] == nil || current[id] != routes[id] {
            stop(id)
        }
        routes = current
        guard active else { return }
        for id in current.keys where workers[id] == nil {
            workers[id] = Task { [weak self] in await self?.watch(id) }
        }
    }

    func activity() -> [UUID: DashboardActivity]? {
        guard !records.isEmpty || !busyAt.isEmpty else { return nil }
        return records.mapValues { DashboardActivity(unreadReplies: $0.unread) }.merging(
            busyAt.filter { Date().timeIntervalSince($0.value) < 600 }.mapValues { _ in DashboardActivity(runningTasks: 1) }
        ) { $0 + $1 }
    }

    func diagnostic(_ id: UUID) -> String {
        guard let item = records[id] else { return "未观测" }
        func stamp(_ date: Date?) -> String { date?.formatted(date: .omitted, time: .standard) ?? "未观测" }
        return "订阅 \(stamp(item.lastSubscription)) · 轮询 \(stamp(item.lastPoll)) · 回填 \(stamp(item.lastBackfill))"
            + " · 心跳 \(stamp(item.heartbeat))"
            + (item.gapSince == nil ? "" : " · 可能有新消息") + " · 解码失败 \(item.decodeFailures)"
    }

    func focus(_ account: UUID?) { focused = account }

    var runningAccounts: Set<UUID> { Set(busyAt.keys) }

    func approvalCall(_ id: UUID, path: String, decision: Data?) async throws -> Data {
        guard let connection = connections[id] as? NativeMuseActivityConnection else {
            throw MuseActivityError.unavailable
        }
        return try await connection.call(path, approvalDecision: decision)
    }

    func checkVisibleReply(in view: WKWebView, account: UUID) async {
        guard focused == account, view.url?.scheme == "https", view.url?.host == "muse.ai",
              let url = view.url else { return }
        let path = url.pathComponents
        guard path.count == 3, path[1] == "thread", let reply = records[account]?.latest[path[2]] else { return }
        let script = #"""
            if (location.origin !== 'https://muse.ai' || location.pathname !== expectedPath) return false;
            return [...document.querySelectorAll('[data-message-id]')].some(el => {
                const r = el.getBoundingClientRect();
                return el.dataset.messageId === expected &&
                    r.height > 0 && r.width > 0 && r.top < innerHeight && r.bottom > 0 &&
                    r.left < innerWidth && r.right > 0 && getComputedStyle(el).visibility === 'visible';
            });
            """#
        let visible = try? await view.callAsyncJavaScript(script, arguments: ["expected": reply.id, "expectedPath": url.path],
                                                          in: nil, contentWorld: .defaultClient) as? Bool
        guard visible == true, focused == account, records[account]?.latest[reply.thread] == reply else { return }
        records[account]?.seen[reply.thread] = reply
        save()
    }

    func reconcileParked() async {
        for (id, connection) in connections where id != focused {
            if Task.isCancelled { return }
            try? await reconcile(id, connection: connection, source: \.lastPoll)
        }
    }

    private func stop(_ id: UUID) {
        workers.removeValue(forKey: id)?.cancel()
        connections.removeValue(forKey: id)?.close()
        busyChanges.removeValue(forKey: id)?.cancel()
        busyAt.removeValue(forKey: id)
        sequences.removeValue(forKey: id)
        if records[id] != nil, records[id]?.gapSince == nil { records[id]?.gapSince = .now }
        save()
    }

    private func watch(_ id: UUID) async {
        var attempts = 0
        while !Task.isCancelled {
            do {
                let connection = try await open(id)
                if Task.isCancelled { connection.close(); return }
                connections[id] = connection
                sequences.removeValue(forKey: id)
                let events = try connection.events()
                let interval = Task { [weak self] in
                    await self?.maintain(id, connection: connection)
                }
                let heartbeat = Task { [weak self] in
                    await self?.heartbeat(id, connection: connection)
                }
                defer { interval.cancel(); heartbeat.cancel(); connection.close() }
                for try await chunk in eventLines(events) {
                    if Task.isCancelled { return }
                    attempts = 0
                    consume(chunk, account: id)
                }
            } catch MuseActivityError.signIn {
                // Wait for an explicit foreground restart after the user signs in.
                break
            } catch {}
            if Task.isCancelled { return }
            connections.removeValue(forKey: id)?.close()
            var item = records[id] ?? ActivityRecord()
            if item.gapSince == nil { item.gapSince = .now }
            records[id] = item
            save()
            let delay = Double((1 << min(attempts, 5)) * 2)
            attempts += 1
            do { try await Task.sleep(for: .seconds(Double.random(in: 0.25...delay))) } catch { return }
        }
        if !Task.isCancelled {
            connections.removeValue(forKey: id)?.close()
            workers.removeValue(forKey: id)
        }
    }

    private func maintain(_ id: UUID, connection: any MuseActivityConnection) async {
        do {
            // Reconnect starts the stream before reconciling history to close the race.
            try await reconcile(id, connection: connection, source: \.lastBackfill)
            while !Task.isCancelled {
                try await Task.sleep(for: .seconds(2))
                if let busy = busyAt[id], Date().timeIntervalSince(busy) >= 600 { busyAt.removeValue(forKey: id) }
                let previous = records[id]?.lastPoll ?? records[id]?.lastBackfill ?? .distantPast
                if Date().timeIntervalSince(previous) >= Double(pollSeconds) {
                    try await reconcile(id, connection: connection, source: \.lastPoll)
                }
            }
        } catch {
            if !Task.isCancelled { connection.close() }
        }
    }

    private func heartbeat(_ id: UUID, connection: any MuseActivityConnection) async {
        do {
            while !Task.isCancelled {
                try await Task.sleep(for: .seconds(24))
                try await connection.ping()
                records[id]?.heartbeat = .now
            }
        } catch {
            if !Task.isCancelled { connection.close() }
        }
    }

    private func reconcile(_ id: UUID, connection: any MuseActivityConnection,
                           source: WritableKeyPath<ActivityRecord, Date?>) async throws {
        guard reconciling.insert(id).inserted else { return }
        defer { reconciling.remove(id) }
        let history = try await connection.history()
        guard !Task.isCancelled, connections[id] === connection else { return }
        var item = records[id] ?? ActivityRecord()
        for reply in history { item.merge(reply) }
        item[keyPath: source] = .now
        item.gapSince = nil
        records[id] = item
        save()
    }

    private func consume(_ line: Data, account: UUID) {
        var item = records[account] ?? ActivityRecord()
        guard let root = (try? JSONSerialization.jsonObject(with: line)) as? [String: Any] else {
            item.decodeFailures += 1
            records[account] = item
            return
        }
        item.lastSubscription = .now
        guard root["type"] as? String == "event" else {
            records[account] = item
            return
        }
        if let sequence = root["seq"] as? Int64, sequence > 0 {
            guard sequences[account, default: []].insert(sequence).inserted else { return }
            if sequences[account, default: []].count > 2_048 {
                sequences[account] = Set(sequences[account, default: []].sorted().suffix(1_024))
            }
        }
        let payload = root["payload"] as? [String: Any] ?? [:]
        let fields = root.merging(payload) { _, payload in payload }
        let event = root["event"] as? String ?? root["event_name"] as? String
        if event == "agent.status" {
            let busyCodes: Set<String> = ["running", "thinking", "working", "responding", "tool_call",
                "waiting_for_input", "waiting_for_subagents", "compacting", "needs_approval"]
            let code = fields["activity_code"] as? String ?? ""
            updateBusy(account, busy: busyCodes.contains(code))
            if code == "needs_approval" { onNeedsApproval?(account) }
        }
        if event == "needs_approval" || event == "egress.approval_requested" { onNeedsApproval?(account) }
        if event == "message.assistant" || event == "delta.message_done",
           fields["display_text_ready"] as? Bool != false,
           let thread = MuseReply.token(fields["session_id"] ?? fields["thread_id"]),
           let reply = MuseReply.from(fields, thread: thread) {
            item.merge(reply)
        }
        records[account] = item
        save()
    }

    private func updateBusy(_ id: UUID, busy: Bool) {
        busyChanges.removeValue(forKey: id)?.cancel()
        busyChanges[id] = Task {
            do { try await Task.sleep(for: .milliseconds(1_500)) } catch { return }
            if busy { busyAt[id] = busyAt[id] ?? .now }
            else {
                busyAt.removeValue(forKey: id)
                onRunEnded?(id)
            }
        }
    }

    private func eventLines(_ stream: AsyncThrowingStream<Data, Error>) -> AsyncThrowingStream<Data, Error> {
        AsyncThrowingStream(bufferingPolicy: .bufferingOldest(128)) { continuation in
            let task = Task {
                do {
                    var buffer = Data()
                    for try await chunk in stream {
                        if buffer.count + chunk.count > 1_048_576 { throw MuseActivityError.invalidFrame }
                        buffer.append(chunk)
                        while let newline = buffer.firstIndex(of: 10) {
                            let line = Data(buffer.prefix(upTo: newline))
                            buffer.removeSubrange(...newline)
                            if !line.isEmpty, case .dropped = continuation.yield(line) { throw MuseActivityError.disconnected }
                        }
                    }
                    if !buffer.isEmpty { continuation.yield(buffer) }
                    continuation.finish()
                } catch { continuation.finish(throwing: error) }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    private func save() {
        guard !records.isEmpty else { return }
        try? FileManager.default.createDirectory(at: storage.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? JSONEncoder().encode(records).write(to: storage, options: [.atomic, .completeFileProtection])
    }
}

#if DEBUG && targetEnvironment(simulator)
@MainActor
private final class ActivityFixtureConnection: MuseActivityConnection {
    private let reconnecting: Bool
    private let reply = MuseReply(thread: "one", id: "reply-a", at: Date(timeIntervalSince1970: 1_789_900_000), version: 1)
    private var continuation: AsyncThrowingStream<Data, Error>.Continuation?
    private var simulatedDrop: Task<Void, Never>?

    init(reconnecting: Bool) { self.reconnecting = reconnecting }

    func events() throws -> AsyncThrowingStream<Data, Error> {
        let (stream, continuation) = AsyncThrowingStream<Data, Error>.makeStream()
        self.continuation = continuation
        if !reconnecting {
            let fixture = #"{"type":"event","event":"message.assistant","seq":7,"ts_ms":1789900000000,"payload":{"session_id":"one","message_id":"reply-a","version":1}}"#
            continuation.yield(Data((fixture + "\nmalformed\n" + fixture + "\n").utf8))
            simulatedDrop = Task {
                do { try await Task.sleep(for: .seconds(4)) } catch { return }
                continuation.finish(throwing: MuseActivityError.disconnected)
            }
        }
        return stream
    }

    func history() async throws -> [MuseReply] {
        if !reconnecting { return [reply] }
        return [reply, MuseReply(thread: "two", id: "reply-b", at: reply.at.addingTimeInterval(5), version: 1)]
    }

    func ping() async throws {}
    func close() {
        simulatedDrop?.cancel()
        continuation?.finish()
    }
}
#endif
