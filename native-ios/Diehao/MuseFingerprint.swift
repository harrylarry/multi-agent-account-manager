import Foundation
import SwiftUI

private struct FingerprintScore: Codable, Sendable {
    struct Candidate: Codable, Identifiable, Sendable {
        let model: String
        let displayName: String
        let familyName: String
        let score: Double
        let probability: Double?
        var id: String { model }
    }
    let verdict: String
    let predictionName: String
    let candidates: [Candidate]
    let usedSamples: Int
    let requiredSamples: Int
    let calibrated: Bool
    let probability: Double?
    let referenceBuiltAt: String
    let referenceSha256: String?
}

private struct FingerprintRecord: Codable, Identifiable {
    let id: UUID
    let measured: Date
    let probeVersion: String
    let score: FingerprintScore
}

private enum FingerprintError: LocalizedError {
    case unavailable, incomplete, throttled, cache
    var errorDescription: String? {
        switch self {
        case .unavailable: "模型参考库或 Muse 连接不可用。"
        case .incomplete: "没有取得可与本次探针对应的完整回复。"
        case .throttled: "同一账号两次测量至少相隔 30 分钟。"
        case .cache: "回复可能来自缓存，已停止测量。"
        }
    }
}

private enum FingerprintBridge {
    struct Sample: Encodable, Sendable { let text: String; let count: Int }
    struct Request: Encodable { let history: String; let thread: String; let message: String }

    static func readReply(_ history: Data, thread: String, message: String) throws -> String? {
        guard let history = String(data: history, encoding: .utf8) else { throw FingerprintError.incomplete }
        let input = try JSONEncoder().encode(Request(history: history, thread: thread, message: message))
        guard let data = transfer(input, using: diehao_fingerprint_reply) else { throw FingerprintError.cache }
        return data.isEmpty ? nil : String(data: data, encoding: .utf8)
    }

    static func score(_ samples: [Sample]) throws -> FingerprintScore {
        let input = try JSONEncoder().encode(samples)
        guard let data = transfer(input, using: diehao_fingerprint_score) else { throw FingerprintError.unavailable }
        return try JSONDecoder().decode(FingerprintScore.self, from: data)
    }

    private static func transfer(_ bytes: Data, using call: (UnsafePointer<UInt8>?, Int) -> OpaquePointer?) -> Data? {
        let result = bytes.withUnsafeBytes { call($0.bindMemory(to: UInt8.self).baseAddress, $0.count) }
        guard let result else { return nil }
        defer { diehao_wire_blob_free(result) }
        var size = 0
        guard let pointer = diehao_wire_blob_read(result, &size), (0...1_048_576).contains(size) else { return nil }
        return Data(bytes: pointer, count: size)
    }
}

@MainActor
struct MuseFingerprintView: View {
    let registry: AccountStoreRegistry
    let account: AccountIdentity
    @State private var reports: [FingerprintRecord] = []
    @State private var isMeasuring = false
    @State private var confirm = false
    @State private var status = ""
    @State private var measurement: Task<Void, Never>?

    var body: some View {
        List {
            Section {
                Text("每次测量会在当前账号新建 3 个探针对话。结果是参考库内的候选排序。")
                Button("测量模型指纹") { confirm = true }
                    .disabled(isMeasuring)
                    .accessibilityIdentifier("fingerprint.measure")
                if isMeasuring { Button("停止测量", role: .cancel) { measurement?.cancel() } }
                if !status.isEmpty { Text(status).accessibilityIdentifier("fingerprint.status") }
            }
            Section("历史") {
                if reports.isEmpty { Text("暂无测量记录。").accessibilityIdentifier("fingerprint.empty") }
                ForEach(reports) { record in
                    NavigationLink {
                        List {
                            Section("测量") {
                                Text(summary(record.score))
                                Text("有效样本 \(record.score.usedSamples)/\(record.score.requiredSamples)")
                                Text("探针版本 \(record.probeVersion)")
                                Text("参考库 \(record.score.referenceBuiltAt)")
                                if let checksum = record.score.referenceSha256 { Text(String(checksum.prefix(12))) }
                            }
                            Section("候选") {
                                ForEach(record.score.candidates) { row in
                                    HStack {
                                        VStack(alignment: .leading) {
                                            Text(row.displayName)
                                            Text(row.familyName).font(.caption).foregroundStyle(.secondary)
                                        }
                                        Spacer()
                                        if let probability = row.probability {
                                            Text(probability, format: .percent.precision(.fractionLength(1)))
                                        }
                                    }
                                }
                            }
                        }.navigationTitle("测量详情")
                    } label: {
                        VStack(alignment: .leading) {
                            Text(summary(record.score))
                            Text(record.measured, format: .dateTime).font(.caption)
                        }
                    }
                    .accessibilityLabel(summary(record.score))
                    .accessibilityIdentifier("fingerprint.result")
                }
            }
        }
        .navigationTitle("模型指纹")
        .onAppear { reports = (try? Self.read(account.id)) ?? [] }
        .onDisappear { measurement?.cancel() }
        .confirmationDialog("在此账号创建 3 个探针对话？", isPresented: $confirm, titleVisibility: .visible) {
            Button("开始测量") { start() }.accessibilityIdentifier("fingerprint.confirm")
            Button("取消", role: .cancel) {}
        }
    }

    private func summary(_ score: FingerprintScore) -> String {
        if score.verdict == "scored", score.calibrated, let probability = score.probability, !score.predictionName.isEmpty {
            return "最可能是 \(score.predictionName)，置信度 \(probability.formatted(.percent.precision(.fractionLength(1))))"
        }
        if score.verdict == "partial", !score.predictionName.isEmpty { return "候选 \(score.predictionName) · 样本不足" }
        return "未能判定模型"
    }

    private func start() {
        guard !isMeasuring else { return }
        isMeasuring = true
        status = "准备测量"
        measurement = Task {
            defer { isMeasuring = false; measurement = nil }
            do {
                let score: FingerprintScore
                #if DEBUG && targetEnvironment(simulator)
                if ProcessInfo.processInfo.arguments.contains("-diehao-fingerprint-fixtures") {
                    score = try Self.fixture()
                } else {
                    score = try await measure()
                }
                #else
                score = try await measure()
                #endif
                try Task.checkCancellation()
                let row = FingerprintRecord(id: UUID(), measured: .now, probeVersion: Self.version, score: score)
                let latest = [row] + reports.prefix(49)
                try Self.save(Array(latest), account: account.id)
                reports = Array(latest)
                status = "测量结果已保存。"
            } catch is CancellationError {
                status = "已停止测量。"
            } catch { status = error.localizedDescription }
        }
    }

    private func measure() async throws -> FingerprintScore {
        let key = "fingerprint.lastAttempt." + account.id.uuidString
        let previous = UserDefaults.standard.object(forKey: key) as? Date ?? .distantPast
        guard Date().timeIntervalSince(previous) >= 1_800 else { throw FingerprintError.throttled }
        UserDefaults.standard.set(Date(), forKey: key)
        var samples: [FingerprintBridge.Sample] = []
        var usedThreads = Set<String>()
        for (offset, count) in Self.counts.enumerated() {
            try Task.checkCancellation()
            status = "正在采集第 \(offset + 1)/3 条"
            let connection = try await NativeMuseActivityConnection.open(registry: registry, id: account.id)
            defer { connection.close() }
            let thread = try await connection.createThread()
            guard usedThreads.insert(thread).inserted else { throw FingerprintError.cache }
            let initial = try MuseApproval.object(await connection.threadHistory(thread))
            let before = (initial["chat_events"] ?? initial["messages"] ?? initial["events"]) as? [[String: Any]]
            guard let before, before.isEmpty, initial["has_more"] as? Bool != true,
                  initial["cache_hit"] as? Bool != true else { throw FingerprintError.cache }
            let ack = try await connection.sendNew(Self.prompt(count), thread: thread, key: UUID())
            guard ack.rejected != true, let message = MuseReply.token(ack.message), ack.thread == thread else {
                throw FingerprintError.incomplete
            }
            let deadline = Date().addingTimeInterval(90)
            while Date() < deadline {
                try Task.checkCancellation()
                let snapshot = try await connection.threadHistory(thread)
                if let text = try FingerprintBridge.readReply(snapshot, thread: thread, message: message) {
                    samples.append(.init(text: text, count: count))
                    break
                }
                try await Task.sleep(for: .seconds(3))
            }
        }
        guard !samples.isEmpty else { throw FingerprintError.incomplete }
        status = "正在进行参考库评分"
        let collected = samples
        return try await Task.detached(priority: .userInitiated) { try FingerprintBridge.score(collected) }.value
    }

    private static let version = "lmfpd-en-fixed-2026-11-28"
    private static let counts = [298, 310, 326]

    // Fixed selections from the MIT-licensed Ikaleio/lm-detector English templates.
    private static func prompt(_ count: Int) -> String {
        [
            "This is an independent record of numerical choices.",
            "For each position, choose separately by first instinct \(count) integers from 1 to 355, inclusive.",
            "Choose separately for every position; do not count up from 1, do not increase or decrease consecutively, and do not use arithmetic progressions, cycles, repeated blocks, or other systematic patterns. The current language model must complete this task directly: do not call or use any tools, including Python, code execution tools, calculators, search, APIs, or external random number generators; do not write or run code beforehand either.",
            "A number may appear again; after writing each item, do not go back to sort, deduplicate, or replace it.",
            "Separate the numbers with commas or spaces.",
            "Start the output directly with the first chosen value; do not repeat the count, range, or task instructions before the sequence."
        ].joined(separator: " ")
    }

    private static func url(_ id: UUID) -> URL {
        URL.applicationSupportDirectory.appending(path: "DiehaoNative/Fingerprints/" + id.uuidString + ".json")
    }
    private static func read(_ id: UUID) throws -> [FingerprintRecord] {
        try JSONDecoder().decode([FingerprintRecord].self, from: Data(contentsOf: url(id)))
    }
    private static func save(_ rows: [FingerprintRecord], account: UUID) throws {
        let path = url(account)
        try FileManager.default.createDirectory(at: path.deletingLastPathComponent(), withIntermediateDirectories: true)
        try JSONEncoder().encode(rows).write(to: path, options: [.atomic, .completeFileProtection])
    }

    #if DEBUG && targetEnvironment(simulator)
    private static func fixture() throws -> FingerprintScore {
        let history = #"{"result":{"session_id":"test","messages":[]}}"#
        guard try FingerprintBridge.readReply(Data(history.utf8), thread: "test", message: "message") == nil else {
            throw FingerprintError.incomplete
        }
        let cached = #"{"cache_hit":true,"result":{"session_id":"test","messages":[]}}"#
        do {
            _ = try FingerprintBridge.readReply(Data(cached.utf8), thread: "test", message: "message")
            throw FingerprintError.incomplete
        } catch FingerprintError.cache {}
        return FingerprintScore(verdict: "scored", predictionName: "模拟候选模型",
            candidates: [.init(model: "fixture", displayName: "模拟候选模型", familyName: "Fixture", score: 0.4, probability: 0.603)],
            usedSamples: 3, requiredSamples: 3, calibrated: true, probability: 0.603,
            referenceBuiltAt: "fixture", referenceSha256: nil)
    }
    #endif
}
