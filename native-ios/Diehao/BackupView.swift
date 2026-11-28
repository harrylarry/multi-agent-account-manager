import Foundation
import SwiftUI
import UniformTypeIdentifiers

@MainActor
struct BackupView: View {
    let registry: AccountStoreRegistry
    @AppStorage("backup.retainedCount") private var retainCount = 5
    @AppStorage("backup.retentionDays") private var retentionDays = 30
    @AppStorage("backup.reducedMemory") private var reducedMemory = false
    @State private var mode = BackupMode.light
    @State private var cookies = false
    @State private var proxy = false
    @State private var password = ""
    @State private var useRecovery = false
    @State private var code: String?
    @State private var status = ""
    @State private var busy = false
    @State private var exporting = false
    @State private var importing = false
    @State private var document: CookieFile?
    @State private var snapshots: [URL] = []
    @State private var chosen: URL?
    @State private var decrypted: BackupSnapshot?
    @State private var newPassword = ""
    @State private var confirmedRestore: Bool?
    @State private var showRestoreConfirmation = false

    private var folder: URL {
        URL.applicationSupportDirectory.appending(path: "DiehaoNative/Backups/Archives")
    }
    private var service: AccountBackupService { AccountBackupService(registry: registry) }

    var body: some View {
        Form {
            Section("创建备份") {
                Picker("范围", selection: $mode) {
                    ForEach(BackupMode.allCases) { Text($0.label).tag($0) }
                }
                if mode == .custom {
                    Toggle("逐条 Cookie", isOn: $cookies)
                    Toggle("代理和凭据", isOn: $proxy)
                }
                SecureField("主口令（至少 12 个字符）", text: $newPassword)
                Button("加密并存入本机") { run { try await create() } }
            }
            if let code {
                Section("恢复码") {
                    Text(code).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                    Text("请离线保存恢复码。主口令和恢复码都遗失时，备份无法打开。")
                    Button("我已保存") { self.code = nil }
                }
            }
            Section("恢复") {
                Button("选择备份文件") { importing = true }
                ForEach(snapshots, id: \.self) { url in
                    Button(url.deletingPathExtension().lastPathComponent) { select(url) }
                }
                if let chosen {
                    Text(chosen.lastPathComponent)
                    Toggle("使用恢复码", isOn: $useRecovery)
                    SecureField(useRecovery ? "恢复码" : "主口令", text: $password)
                    Button("验证并预览") { run { try await prepare(chosen) } }
                    Button("导出这个加密备份") { run {
                        let bytes = try await Task.detached { try Self.read(chosen) }.value
                        document = CookieFile(data: bytes)
                        exporting = true
                    } }
                }
                if let snapshot = decrypted {
                    let counts = try? service.preview(snapshot)
                    Text("\(snapshot.mode.label) · \(snapshot.entries.count) 个账号")
                    Text(snapshot.created, format: .dateTime)
                    if let counts { Text("新增 \(counts.new) · 同 UUID \(counts.existing)") }
                    if let expires = snapshot.expires { LabeledContent("到期", value: expires.formatted()) }
                    ForEach(snapshot.entries) { Text($0.alias.value) }
                    Button("合并恢复") { confirm(merge: true) }
                    Button("覆盖备份中同 UUID 的资料", role: .destructive) { confirm(merge: false) }
                    Button("换主口令与恢复码") { run { try await rotate(snapshot) } }
                    Button("在本机吊销此备份", role: .destructive) { run { try revoke(snapshot.id) } }
                }
                if !status.isEmpty { Text(status).accessibilityIdentifier("backup.status") }
            }
            Section("保留设置") {
                Stepper("最多保留 \(retainCount) 份", value: $retainCount, in: 1...20)
                Stepper("备份 \(retentionDays) 天后过期", value: $retentionDays, in: 1...365)
                Toggle("降低加密内存用量（32 MiB）", isOn: $reducedMemory)
                Text("备份保存在本机。吊销和更换恢复码可阻止此设备使用旧副本，已导出副本仍可用原密钥打开。")
            }
        }
        .disabled(busy)
        .navigationTitle("加密备份")
        .fileImporter(isPresented: $importing, allowedContentTypes: [.json]) { result in
            do { select(try result.get()) }
            catch { status = error.localizedDescription }
        }
        .fileExporter(isPresented: $exporting, document: document, contentType: .json,
                      defaultFilename: "diehao-backup.json") { result in
            document = nil
            if case .failure(let error) = result { status = error.localizedDescription }
        }
        .confirmationDialog("确认恢复账号资料", isPresented: $showRestoreConfirmation, titleVisibility: .visible) {
            Button("恢复") {
                guard let snapshot = decrypted, let merge = confirmedRestore else { return }
                run {
                    try await service.restore(snapshot, merge: merge)
                    decrypted = nil
                    status = "账号资料已恢复。"
                }
            }
        } message: {
            Text("恢复前会保存当前资料，以便失败时回滚。")
        }
        .task {
            do { try await service.recover(); try recompute() }
            catch { status = error.localizedDescription }
            #if DEBUG && targetEnvironment(simulator)
            if ProcessInfo.processInfo.arguments.contains("-diehao-backup-fixtures") {
                do { try await Self.selfCheck(); status = "三种模式、恢复码、坏包和 UUID 合并通过。" }
                catch { status = "备份自检失败：" + error.localizedDescription }
            }
            #endif
        }
        .onChange(of: retainCount) { _, _ in try? recompute() }
    }

    private func select(_ url: URL) {
        chosen = url
        decrypted = nil
        password = ""
        status = ""
    }

    private func confirm(merge: Bool) {
        confirmedRestore = merge
        showRestoreConfirmation = true
    }

    private func run(_ action: @escaping @MainActor () async throws -> Void) {
        guard !busy else { return }
        busy = true
        status = ""
        Task {
            defer { busy = false }
            do { try await action() }
            catch { status = error.localizedDescription }
        }
    }

    private func create() async throws {
        guard newPassword.count >= 12 else { throw CookieError.credentials }
        let expiry = Calendar.current.date(byAdding: .day, value: retentionDays, to: .now)
        let snapshot = try await service.capture(mode: mode, includeCookies: mode == .full || (mode == .custom && cookies),
                                                 includeProxy: mode == .full || (mode == .custom && proxy), expires: expiry)
        try await store(snapshot)
    }

    private func store(_ snapshot: BackupSnapshot) async throws {
        let encrypted = try await BackupCodec.seal(snapshot, password: newPassword, memory: reducedMemory ? 32_768 : 65_536)
        let url = folder.appending(path: snapshot.id.uuidString + ".json")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try encrypted.data.write(to: url, options: [.atomic, .completeFileProtection])
        newPassword = ""
        code = encrypted.code
        select(url)
        try recompute()
        status = "备份已加密并保存。"
    }

    private func rotate(_ current: BackupSnapshot) async throws {
        let replacement = try BackupSnapshot(expires: current.expires, mode: current.mode, entries: current.entries)
        try await store(replacement)
        try revoke(current.id, clearSelection: false)
        status = "新恢复码已生成，旧备份已在本机吊销。"
    }

    private func prepare(_ url: URL) async throws {
        decrypted = nil
        let data = try await Task.detached { try Self.read(url) }.value
        let snapshot = try await BackupCodec.open(data, secret: password, useRecovery: useRecovery)
        if try revoked().contains(snapshot.id) { throw BackupError.expired }
        _ = try service.preview(snapshot)
        decrypted = snapshot
        password = ""
        status = "完整性核验通过。"
    }

    private func revoke(_ id: UUID, clearSelection: Bool = true) throws {
        var list = try revoked()
        list.insert(id)
        let data = try JSONEncoder().encode(list)
        try T2Keychain.replace(data, as: "backup-revoked-v1")
        let local = folder.appending(path: id.uuidString + ".json")
        if FileManager.default.fileExists(atPath: local.path) { try FileManager.default.removeItem(at: local) }
        if clearSelection { chosen = nil; decrypted = nil; status = "已在本机吊销。" }
        try recompute()
    }

    private func revoked() throws -> Set<UUID> {
        guard let data = try T2Keychain.read("backup-revoked-v1") else { return [] }
        return try JSONDecoder().decode(Set<UUID>.self, from: data)
    }

    private func recompute() throws {
        guard FileManager.default.fileExists(atPath: folder.path) else { snapshots = []; return }
        let urls = try FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: [.creationDateKey])
            .filter { $0.pathExtension == "json" }
        let sorted = urls.map { ($0, (try? $0.resourceValues(forKeys: [.creationDateKey]).creationDate) ?? .distantPast) }
            .sorted { $0.1 > $1.1 }.map(\.0)
        snapshots = Array(sorted.prefix(retainCount))
        for old in sorted.dropFirst(retainCount) { try FileManager.default.removeItem(at: old) }
    }

    nonisolated private static func read(_ url: URL) throws -> Data {
        let secured = url.startAccessingSecurityScopedResource()
        defer { if secured { url.stopAccessingSecurityScopedResource() } }
        var coordinationError: NSError?
        var result: Result<Data, Error>?
        NSFileCoordinator().coordinate(readingItemAt: url, options: [], error: &coordinationError) { file in
            result = Result {
                let values = try file.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
                guard values.isRegularFile == true, let size = values.fileSize, size <= BackupCodec.maxSize else {
                    throw BackupError.invalid
                }
                return try Data(contentsOf: file)
            }
        }
        if let coordinationError { throw coordinationError }
        guard let result else { throw BackupError.invalid }
        return try result.get()
    }

    #if DEBUG && targetEnvironment(simulator)
    private static func selfCheck() async throws {
        let now = Date()
        let id = UUID()
        let old = BackupAccount(id: id, alias: BackupField(time: now, value: "first"), requiresProxy: false)
        let new = BackupAccount(id: id, alias: BackupField(time: now.addingTimeInterval(1), value: "second"), requiresProxy: false)
        guard try old.merged(with: new).alias.value == new.merged(with: old).alias.value else { throw BackupError.invalid }
        let password = "simulator test passphrase"
        for mode in BackupMode.allCases {
            let snapshot = try BackupSnapshot(expires: nil, mode: mode, entries: [new])
            let sealed = try await BackupCodec.seal(snapshot, password: password, memory: 32_768)
            let a = try await BackupCodec.open(sealed.data, secret: password, useRecovery: false)
            let b = try await BackupCodec.open(sealed.data, secret: sealed.code, useRecovery: true)
            let wrong = try? await BackupCodec.open(sealed.data, secret: "incorrect fixture passphrase", useRecovery: false)
            var corrupted = sealed.data
            corrupted[0] ^= 0xff
            let bad = try? await BackupCodec.open(corrupted, secret: sealed.code, useRecovery: true)
            guard a.id == snapshot.id, b.manifest == a.manifest, wrong == nil, bad == nil else { throw BackupError.invalid }
        }
    }
    #endif
}
