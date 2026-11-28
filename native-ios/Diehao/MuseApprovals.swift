import CryptoKit
import Foundation
import Observation
import SwiftUI

enum ApprovalDecision: String, Codable {
    case deny, once = "allow_once", task, always = "allow_always"
}

@MainActor
struct MuseApprovalView: View {
    let account: AccountIdentity
    let queue: MuseApprovalQueue
    @State private var confirmation: (MuseApproval, ApprovalDecision)?
    @State private var showingConfirmation = false
    @State private var busy = false
    @State private var failure = false

    var body: some View {
        List {
            if let notice = queue.notice[account.id] { Text(notice).accessibilityIdentifier("approval.notice") }
            if failure { Text("审批请求未确认。请查看当前会话状态。") }
            if queue.queue(account.id).isEmpty { Text("暂无待审批请求。") }
            ForEach(queue.queue(account.id)) { item in
                Section {
                    if let tool = item.tool { LabeledContent("工具", value: tool) }
                    if let destination = item.destination { LabeledContent("目标", value: destination) }
                    if !item.session.isEmpty { LabeledContent("会话", value: item.session) }
                    if let expiry = item.expires {
                        LabeledContent("到期", value: expiry.formatted(date: .omitted, time: .standard))
                    }
                    if item.blocked { Text("此操作受硬拒绝规则约束。") }
                    if queue.canAllow(item, account: account.id, decision: .once) {
                        Button("允许一次") { submit(item, .once) }
                            .accessibilityIdentifier("approval.once." + item.id)
                    }
                    if queue.canAllow(item, account: account.id, decision: .task) {
                        Button("允许本次任务") { confirm(item, .task) }
                    }
                    if queue.canAllow(item, account: account.id, decision: .always) {
                        Button("始终允许此工具和目标") { confirm(item, .always) }
                    }
                    if item.allowed.contains("deny") {
                        Button("拒绝", role: .destructive) { submit(item, .deny) }
                    }
                } header: { Text(item.label) }
            }
        }
        .disabled(busy)
        .navigationTitle("待审批")
        .task { await queue.refresh(account.id) }
        .refreshable { await queue.refresh(account.id) }
        .confirmationDialog("确认授权", isPresented: $showingConfirmation, titleVisibility: .visible,
                            presenting: confirmation) { value in
            Button(value.1 == .always ? "确认长期授权" : "确认本次任务授权") { submit(value.0, value.1) }
            Button("取消", role: .cancel) {}
        } message: { value in
            Text("\(value.0.tool ?? value.0.label) · \(value.0.destination ?? "")")
        }
    }

    private func confirm(_ item: MuseApproval, _ decision: ApprovalDecision) {
        confirmation = (item, decision)
        showingConfirmation = true
    }

    private func submit(_ item: MuseApproval, _ decision: ApprovalDecision) {
        guard !busy else { return }
        busy = true
        failure = false
        Task {
            defer { busy = false }
            do { try await queue.decide(item, account: account.id, decision: decision) }
            catch { failure = true }
        }
    }
}

#if DEBUG && targetEnvironment(simulator)
extension MuseApprovalQueue {
    static func fixtures(account: UUID?) -> MuseApprovalQueue {
        var handled = Set<String>()
        let anchor = "approval-fixture." + UUID().uuidString
        let expiry = Date().timeIntervalSince1970 * 1000 + 120_000
        func row(_ id: String, _ tool: String) -> [String: Any] {
            ["approval_id": id, "status": "pending", "session_scope_id": "thread",
             "run_id": "fixture-run", "tool_id": tool, "request_expires_at_ms": expiry,
             "decision_options": ["deny", "allow_once"]]
        }
        let rows = ["safe": row("safe", "read_document"), "blocked": row("blocked", "delete_account")]
        return MuseApprovalQueue(call: { id, path, payload in
            guard account == id else { return Data(#"{"pending":[]}"#.utf8) }
            if path == "/approvals" {
                return try JSONSerialization.data(withJSONObject: ["pending": rows.values.filter {
                    !handled.contains(($0["approval_id"] as? String) ?? "")
                }])
            }
            let parts = path.split(separator: "/")
            guard parts.count >= 2, let item = rows[String(parts[1])] else { throw MuseActivityError.unavailable }
            guard let payload else { return try JSONSerialization.data(withJSONObject: ["approval": item]) }
            let submitted = try MuseApproval.object(payload)
            let id = String(parts[1])
            let expected = id == "safe" ? "allow_once" : "deny"
            guard submitted["approval_id"] as? String == id, submitted["decision"] as? String == expected else {
                throw MuseActivityError.invalidFrame
            }
            handled.insert(id)
            return try JSONSerialization.data(withJSONObject: ["approval_id": id, "decision": expected, "status": "decided"])
        }, root: URL.temporaryDirectory.appending(path: anchor), keyName: anchor)
    }
}
#endif

struct MuseApproval: Identifiable, Codable, Equatable {
    let id: String
    let session: String
    let run: String?
    let label: String
    let tool: String?
    let scope: String?
    let destination: String?
    let expires: Date?
    let allowed: Set<String>
    let blocked: Bool

    var hasExactScope: Bool { tool != nil && scope != nil && destination != nil }

    static func list(_ bytes: Data) throws -> [Self] {
        let envelope = try object(bytes)
        guard let rows = (envelope["pending_approvals"] ?? envelope["pending"]) as? [[String: Any]],
              rows.count <= 500 else { throw MuseActivityError.invalidFrame }
        let mapped = rows.compactMap(decode)
        guard Set(mapped.map(\.id)).count == mapped.count else { throw MuseActivityError.invalidFrame }
        return mapped
    }

    static func object(_ data: Data) throws -> [String: Any] {
        guard let raw = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              raw["ok"] as? Bool != false,
              raw["error"].map({ !($0 is NSNull) }) != true else { throw MuseActivityError.invalidFrame }
        return raw["result"] as? [String: Any] ?? raw
    }

    static func decode(_ row: [String: Any]) -> Self? {
        guard (row["status"] as? String ?? "pending") == "pending", let id = MuseReply.token(row["approval_id"]) else {
            return nil
        }
        let display = row["display"] as? [String: Any] ?? [:]
        let envelope = row["scope"] as? [String: Any] ?? [:]
        let target = row["target"] as? [String: Any] ?? [:]
        let session = MuseReply.token(row["session_scope_id"] ?? envelope["session_scope_id"]) ?? ""
        let run = MuseReply.token(row["run_id"] ?? row["task_id"] ?? envelope["run_id"])
        let labels = [row["approval_type"], row["tool_id"], row["tool_name"], row["action_label"],
                      display["action_label"], display["display_name"], row["display_name"]]
            .compactMap { $0 as? String }
        let tool = MuseReply.token(row["tool_id"] ?? row["tool_name"] ?? display["action_label"] ?? row["action_label"])
        let label = String((display["display_name"] as? String ?? row["display_name"] as? String ?? tool ?? "Muse 请求").prefix(100))
        let rawExpiry = (row["request_expires_at_ms"] ?? row["expires_at_ms"]) as? Double
        let expires = rawExpiry.flatMap { $0.isFinite && $0 > 0 ? Date(timeIntervalSince1970: $0 / 1000) : nil }
        let options = row["decision_options"] as? [Any] ?? []
        var allowed = Set<String>()
        var scopes = Set<String>()
        for option in options {
            let map = option as? [String: Any] ?? [:]
            let choice = option as? String ?? map["kind"] as? String
            if let choice, ["deny", "allow_once", "allow_always"].contains(choice) { allowed.insert(choice) }
            if choice == "allow_always", let scope = (map["label"] as? [String: Any])?["scope"] as? String,
               ["entity", "destination_domain"].contains(scope) { scopes.insert(scope) }
        }
        let scoped = scopes.count == 1 ? scopes.first : nil
        let destination: String?
        switch scoped {
        case "entity":
            destination = MuseReply.token(target["entity_id"] ?? target["entity"] ?? envelope["entity_id"] ?? row["entity_id"])
        case "destination_domain":
            let raw = (target["host"] ?? envelope["destination_domain"] ?? row["destination_domain"]) as? String
            destination = raw.flatMap { domain in
                let domain = domain.lowercased()
                return domain.utf8.count <= 253 && domain.contains(".") && domain.utf8.allSatisfy {
                    (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 46
                } ? domain : nil
            }
        default: destination = nil
        }
        let normalized = labels.joined(separator: " ").lowercased().replacingOccurrences(of: #"[_-]+"#, with: " ", options: .regularExpression)
        let hard = #"delete (?:my |user )?account|account (?:delete|deletion)|log\s*out|sign\s*out|payment|pay (?:invoice|bill)|purchase|change (?:my )?password|reset (?:my )?password|password (?:change|reset)|删除(?:账号|账户)|退出登录|登出|付款|支付|购买|修改密码|重置密码"#
        let blocked = normalized.range(of: hard, options: .regularExpression) != nil
        return Self(id: id, session: session, run: run, label: label, tool: tool, scope: scoped,
                    destination: destination, expires: expires, allowed: allowed, blocked: blocked)
    }
}

@MainActor
@Observable
final class MuseApprovalQueue {
    typealias Call = @MainActor (UUID, String, Data?) async throws -> Data
    private struct Receipt: Codable {
        let approval: String
        let account: UUID
        let decision: ApprovalDecision
        let session: String
        let run: String?
        let tool: String?
        let phase: String
        let at: Date
        let previous: Data
        var signature: Data = Data()
    }
    private struct Grant: Hashable {
        let account: UUID
        let session: String
        let run: String
        let tool: String
        let scope: String
        let destination: String
    }
    private struct Run: Hashable {
        let account: UUID
        let session: String
        let id: String
    }
    private static let runLimit = 20
    private static let toolLimit = 5
    private let root: URL
    private let call: Call
    private let keyName: String
    private var items: [UUID: [MuseApproval]]
    private var receipts: [Receipt] = []
    private var grants: Set<Grant> = []
    private var spent: [Run: [String: Int]] = [:]
    private var refreshing: Set<UUID> = []
    private var pending: Set<String> = []
    private var activeAccounts: Set<UUID> = []
    private var lastPoll: [UUID: Date] = [:]
    private var storageValid = true
    private(set) var notice: [UUID: String] = [:]

    init(call: @escaping Call, root: URL = URL.applicationSupportDirectory.appending(path: "DiehaoNative/Approvals"),
         keyName: String = "approval-audit-hmac") {
        self.call = call
        self.root = root
        self.keyName = keyName
        items = (try? JSONDecoder().decode([UUID: [MuseApproval]].self,
            from: Data(contentsOf: root.appending(path: "pending.json")))) ?? [:]
        do {
            let path = root.appending(path: "receipts.json")
            if FileManager.default.fileExists(atPath: path.path) {
                receipts = try JSONDecoder().decode([Receipt].self, from: Data(contentsOf: path))
                let key = try auditKey()
                var previous = Data()
                for var receipt in receipts {
                    let stored = receipt.signature
                    guard receipt.previous == previous else { throw MuseActivityError.invalidFrame }
                    receipt.signature = Data()
                    let payload = try Self.encoder().encode(receipt)
                    guard HMAC<SHA256>.isValidAuthenticationCode(stored, authenticating: payload, using: key) else {
                        throw MuseActivityError.invalidFrame
                    }
                    previous = stored
                    if receipt.phase == "attempt", receipt.decision != .deny,
                       let id = receipt.run, let tool = receipt.tool {
                        let run = Run(account: receipt.account, session: receipt.session, id: id)
                        spent[run, default: [:]][tool, default: 0] += 1
                    }
                }
            }
            if let anchor = try T2Keychain.read(keyName + ".head"), !receipts.contains(where: { $0.signature == anchor }) {
                throw MuseActivityError.invalidFrame
            }
        } catch { storageValid = false }
    }

    func queue(_ id: UUID) -> [MuseApproval] {
        (items[id] ?? []).filter { $0.expires.map { $0 > .now } ?? true }
    }

    func counts() -> [UUID: DashboardActivity] {
        items.mapValues { collection in
            DashboardActivity(pendingApprovals: collection.count { $0.expires.map { $0 > .now } ?? true })
        }
    }

    func configure(_ ids: [UUID], active: Bool) {
        activeAccounts = active ? Set(ids) : []
        if !active { grants.removeAll() }
    }

    func poll(running: Set<UUID>) async {
        for id in activeAccounts {
            let interval: TimeInterval = running.contains(id) ? 3 : 12
            if Date().timeIntervalSince(lastPoll[id] ?? .distantPast) >= interval { await refresh(id) }
        }
    }

    func finishRuns(for account: UUID) {
        grants = grants.filter { $0.account != account }
    }

    func refresh(_ id: UUID) async {
        guard activeAccounts.contains(id), refreshing.insert(id).inserted else { return }
        defer { refreshing.remove(id) }
        lastPoll[id] = .now
        do {
            let response = try await call(id, "/approvals", nil)
            let rows = try MuseApproval.list(response)
            guard activeAccounts.contains(id) else { return }
            items[id] = rows
            persistPending()
            for item in rows {
                if let expiry = item.expires, expiry <= .now {
                    try? await expire(item, account: id)
                } else if item.blocked, item.allowed.contains("deny") {
                    try? await decide(item, account: id, decision: .deny)
                } else if let grant = grant(for: item, account: id), grants.contains(grant),
                          item.allowed.contains("allow_once"), !item.blocked {
                    try? await decide(item, account: id, decision: .once)
                }
            }
        } catch { notice[id] = "暂时无法同步审批。" }
    }

    func canAllow(_ item: MuseApproval, account: UUID, decision: ApprovalDecision) -> Bool {
        guard storageValid, !item.blocked, item.tool != nil, !item.session.isEmpty,
              let expires = item.expires, expires > .now,
              !hasAttempt(account: account, approval: item.id) else { return false }
        if decision == .always, !item.hasExactScope { return false }
        if decision == .task, grant(for: item, account: account) == nil { return false }
        if let run = run(for: item, account: account), let tool = item.tool, let counts = spent[run] {
            guard counts.values.reduce(0, +) < Self.runLimit, (counts[tool] ?? 0) < Self.toolLimit else { return false }
        }
        return item.allowed.contains(decision == .task ? ApprovalDecision.once.rawValue : decision.rawValue)
    }

    func decide(_ item: MuseApproval, account: UUID, decision: ApprovalDecision) async throws {
        guard decision == .deny || canAllow(item, account: account, decision: decision) else {
            throw MuseActivityError.unavailable
        }
        let remote = decision == .task ? .once : decision
        try await submit(item, account: account, remote: remote, receiptDecision: decision, expired: false)
        if decision == .task, let grant = grant(for: item, account: account) { grants.insert(grant) }
    }

    private func expire(_ item: MuseApproval, account: UUID) async throws {
        guard item.allowed.contains("deny"), let deadline = item.expires, deadline <= .now else { return }
        try await submit(item, account: account, remote: .deny, receiptDecision: .deny, expired: true)
    }

    private func submit(_ item: MuseApproval, account: UUID, remote: ApprovalDecision,
                        receiptDecision: ApprovalDecision, expired: Bool) async throws {
        let key = account.uuidString + "/" + item.id
        guard activeAccounts.contains(account), !item.session.isEmpty, item.expires != nil,
              item.allowed.contains(remote.rawValue), !hasAttempt(account: account, approval: item.id),
              pending.insert(key).inserted else { throw MuseActivityError.unavailable }
        defer { pending.remove(key) }

        let path = "/approvals/" + item.id
        let detail = try MuseApproval.object(await call(account, path, nil))
        guard let current = MuseApproval.decode(detail["approval"] as? [String: Any] ?? detail), current == item,
              activeAccounts.contains(account), let expiry = current.expires,
              expired ? expiry <= .now : expiry > .now else {
            notice[account] = "审批内容或期限已变，请刷新。"
            throw MuseActivityError.unavailable
        }
        guard remote == .deny || canAllow(current, account: account, decision: receiptDecision) else {
            throw MuseActivityError.unavailable
        }
        try record(account: account, item: item, decision: receiptDecision, phase: expired ? "expiryAttempt" : "attempt")
        if remote != .deny, let run = run(for: item, account: account), let tool = item.tool {
            spent[run, default: [:]][tool, default: 0] += 1
        }
        var fields: [String: Any] = ["approval_id": item.id, "decision": remote.rawValue, "notification_channel": "ui",
                                    "reason": NSNull(), "expires_at_ms": NSNull(), "expected_message_revision": NSNull()]
        if remote == .always { fields["allow_always_scope"] = item.scope; fields["always_scope"] = item.scope }
        do {
            let payload = try JSONSerialization.data(withJSONObject: fields)
            let response = try MuseApproval.object(await call(account, path + "/decide", payload))
            let ack = response["approval"] as? [String: Any] ?? response
            guard ack["approval_id"] as? String == item.id, ack["decision"] as? String == remote.rawValue,
                  ["approved", "denied", "decided"].contains(ack["status"] as? String ?? "") else {
                throw MuseActivityError.invalidFrame
            }
            try record(account: account, item: item, decision: receiptDecision, phase: expired ? "expired" : "confirmed")
            items[account]?.removeAll { $0.id == item.id }
            persistPending()
            notice[account] = expired ? "过期审批已拒绝。" : "Muse 已确认审批结果。"
        } catch {
            notice[account] = "审批结果未知。请回到对应会话核对。"
            throw error
        }
    }

    private func grant(for item: MuseApproval, account: UUID) -> Grant? {
        guard let run = item.run, let tool = item.tool, let scope = item.scope,
              let destination = item.destination else { return nil }
        return Grant(account: account, session: item.session, run: run, tool: tool, scope: scope, destination: destination)
    }
    private func run(for item: MuseApproval, account: UUID) -> Run? {
        item.run.map { Run(account: account, session: item.session, id: $0) }
    }

    private func hasAttempt(account: UUID, approval: String) -> Bool {
        receipts.contains { $0.approval == approval && $0.account == account }
    }

    private static func encoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        return encoder
    }

    private func auditKey() throws -> SymmetricKey {
        if let data = try T2Keychain.read(keyName), data.count == 32 { return SymmetricKey(data: data) }
        let key = SymmetricKey(size: .bits256)
        try T2Keychain.save(key.withUnsafeBytes { Data($0) }, as: keyName)
        return key
    }

    private func record(account: UUID, item: MuseApproval, decision: ApprovalDecision, phase: String) throws {
        guard storageValid else { throw MuseActivityError.unavailable }
        var receipt = Receipt(approval: item.id, account: account, decision: decision,
                              session: item.session, run: item.run, tool: item.tool, phase: phase,
                              at: .now, previous: receipts.last?.signature ?? Data())
        let encoder = Self.encoder()
        receipt.signature = Data(HMAC<SHA256>.authenticationCode(for: try encoder.encode(receipt), using: try auditKey()))
        let next = receipts + [receipt]
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try encoder.encode(next).write(to: root.appending(path: "receipts.json"), options: [.atomic, .completeFileProtection])
        receipts = next
        try T2Keychain.replace(receipt.signature, as: keyName + ".head")
    }

    private func persistPending() {
        try? FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try? JSONEncoder().encode(items).write(to: root.appending(path: "pending.json"), options: [.atomic, .completeFileProtection])
    }
}
