import CryptoKit
import Network
import SwiftUI
import UniformTypeIdentifiers

struct CookieFile: FileDocument {
    static let readableContentTypes: [UTType] = [.data, .json, .plainText]
    var data: Data

    init(data: Data) { self.data = data }
    init(configuration: ReadConfiguration) throws {
        guard let bytes = configuration.file.regularFileContents else { throw CookieError.invalidFile }
        data = bytes
    }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: data)
    }
}

@MainActor
enum SavedCookies {
    struct Entry: Codable, Identifiable {
        var id: String { email }
        let email: String
        let updated: Date
        let records: [MuseCookie]
    }

    private static let url = URL.applicationSupportDirectory.appending(path: "DiehaoNative/cookie-pool.dat")
    private static let keyName = "cookie-pool.aes256"

    static func load() throws -> [Entry] {
        let bytes: Data
        do { bytes = try Data(contentsOf: url) }
        catch let error as CocoaError where error.code == .fileReadNoSuchFile { return [] }
        guard let data = try T2Keychain.read(keyName) else { throw CookieError.invalidFile }
        let box = try AES.GCM.SealedBox(combined: bytes)
        return try JSONDecoder().decode([Entry].self, from: AES.GCM.open(box, using: SymmetricKey(data: data)))
    }

    static func save(_ records: [MuseCookie], email: String) throws {
        let email = email.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        var entries = try load()
        if let previous = entries.first(where: { $0.email == email }) {
            let old = Dictionary(previous.records.map { ($0.key, $0) }, uniquingKeysWith: { first, _ in first })
            let older = records.contains { record in
                guard let former = old[record.key], former.value != record.value,
                      let a = former.expirationDate, let b = record.expirationDate else { return false }
                return a > b
            }
            guard !older else { return }
        }
        entries.removeAll { $0.email == email }
        entries.append(Entry(email: email, updated: .now, records: records))
        let key: SymmetricKey
        if let data = try T2Keychain.read(keyName) {
            key = SymmetricKey(data: data)
        } else {
            key = SymmetricKey(size: .bits256)
            try T2Keychain.save(key.withUnsafeBytes { Data($0) }, as: keyName)
        }
        guard let sealed = try AES.GCM.seal(JSONEncoder().encode(entries), using: key).combined else {
            throw CookieError.invalidFile
        }
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try sealed.write(to: url, options: [.atomic, .completeFileProtection])
    }
}

@MainActor
struct AccountTools: View {
    let registry: AccountStoreRegistry
    @State private var accounts: [AccountIdentity] = []
    @State private var alias = ""
    @State private var status = ""
    @State private var selected: AccountIdentity?
    @State private var busy = false

    var body: some View {
        List {
            Section("Muse 账号") {
                ForEach(accounts) { account in
                    Button(account.alias ?? "Muse · " + String(account.id.uuidString.prefix(8))) {
                        selected = account
                    }
                }
                TextField("新账号备注", text: $alias)
                Button("新增独立账号") {
                    busy = true
                    Task {
                        defer { busy = false }
                        do {
                            let row = try await registry.createAccount(alias: alias.isEmpty ? "Muse" : alias)
                            refresh()
                            selected = row
                            alias = ""
                        } catch { status = error.localizedDescription }
                    }
                }
                .disabled(busy)
                if !status.isEmpty { Text(status) }
            }
        }
        .navigationTitle("账号资料")
        .onAppear(perform: refresh)
        .sheet(item: $selected, onDismiss: refresh) { account in
            NavigationStack { AccountDetail(registry: registry, account: account) }
                .interactiveDismissDisabled()
        }
    }

    private func refresh() { accounts = registry.accounts }
}

@MainActor
private struct AccountDetail: View {
    @Environment(\.dismiss) private var dismiss
    let registry: AccountStoreRegistry
    let account: AccountIdentity
    @State private var scheme: AccountProxySettings.Scheme = .http
    @State private var host = ""
    @State private var port = ""
    @State private var username = ""
    @State private var proxyPassword = ""
    @State private var email = ""
    @State private var archivePassword = ""
    @State private var counts = ""
    @State private var status = ""
    @State private var busy = false
    @State private var chooseFile = false
    @State private var exportFile = false
    @State private var file: CookieFile?
    @State private var preview: CookiePreview?
    @State private var pool: [SavedCookies.Entry] = []
    @State private var page: AccountWebView?
    @State private var showPage = false
    @State private var deletePrompt = false

    var body: some View {
        Form {
            Section {
                Button("打开 Muse 登录") {
                    do {
                        page = try registry.makeWebView(for: account.id)
                        showPage = true
                    } catch { status = error.localizedDescription }
                }
                Text("资料 ID · " + String(account.id.uuidString.prefix(8)))
                    .font(.caption)
                if !status.isEmpty { Text(status).accessibilityIdentifier("account.status") }
            }
            Section("账号代理") {
                Picker("协议", selection: $scheme) {
                    ForEach(AccountProxySettings.Scheme.allCases) { Text($0.rawValue.uppercased()).tag($0) }
                }
                TextField("主机", text: $host).textInputAutocapitalization(.never)
                TextField("端口", text: $port).keyboardType(.numberPad)
                TextField("用户名（可选）", text: $username).textInputAutocapitalization(.never)
                SecureField("代理密码", text: $proxyPassword)
                Button("保存并应用代理") { perform {
                    let settings = try AccountProxySettings(scheme: scheme, host: host, port: port, username: username)
                    try registry.setProxy(settings, password: proxyPassword, for: account.id)
                    proxyPassword = ""
                    status = "代理已应用。下次打开时重建 WebView。"
                } }
                Button("检测代理") { perform {
                    guard let settings = registry.accounts.first(where: { $0.id == account.id })?.proxy else {
                        throw RegistryError.proxyUnavailable
                    }
                    status = try await ProxyTest.run(settings)
                } }
                Button("清除本账号代理") { perform {
                    try registry.setProxy(nil, password: "", for: account.id)
                    status = "本账号代理已清除。"
                } }
            }
            Section("双域 Cookie") {
                TextField("要验证的 Muse 邮箱", text: $email)
                    .keyboardType(.emailAddress).textInputAutocapitalization(.never)
                SecureField("备份文件口令", text: $archivePassword)
                if !counts.isEmpty { Text(counts) }
                Button("按域清点") { perform {
                    counts = try await capture().counts
                } }
                Button("验证并保存到 Cookie 池") { perform {
                    try await saveCurrent()
                    status = "已验证并加密保存。"
                } }
                Button("加密导出 Cookie") { perform {
                    let records = try await capture()
                    counts = records.counts
                    let sealed = try await CookieArchive.seal(records.cookies, password: archivePassword)
                    file = CookieFile(data: sealed)
                    archivePassword = ""
                    exportFile = true
                } }
                Button("选择 Cookie 文件") { chooseFile = true }
                ForEach(pool) { entry in
                    Button("选择已保存的 Cookie · " + entry.email) {
                        do {
                            preview = try CookiePreview(entry.records)
                            email = entry.email
                        } catch { status = error.localizedDescription }
                    }
                }
                if let preview {
                    Text(preview.counts)
                    ForEach(preview.notes, id: \.self) { Text($0).font(.caption) }
                    Button("验证并覆盖此账号") { perform { try await restore(preview, newAccount: false) } }
                    Button("验证并新建独立账号") { perform { try await restore(preview, newAccount: true) } }
                }
                Button("恢复导入前的 Cookie") { perform {
                    try await CookieTransfer.recover(id: account.id, registry: registry)
                    status = "已恢复导入前的 Cookie。"
                } }
            }
            Section {
                Button("删除账号", role: .destructive) { deletePrompt = true }
            }
        }
        .disabled(busy)
        .navigationTitle(account.alias ?? "Muse")
        .toolbar { ToolbarItem(placement: .confirmationAction) { Button("完成") { dismiss() }.disabled(busy) } }
        .task {
            if let proxy = account.proxy {
                scheme = proxy.scheme
                host = proxy.host
                port = String(proxy.port)
                username = proxy.username
            }
            loadPool()
        }
        .sheet(isPresented: $showPage, onDismiss: { page = nil }) {
            NavigationStack {
                if let webView = page?.view {
                    MuseAccountPage(webView: webView) { status = $0; showPage = false }
                        .toolbar { ToolbarItem(placement: .confirmationAction) {
                            Button("关闭 Muse") { showPage = false }
                        } }
                }
            }
        }
        .fileImporter(isPresented: $chooseFile, allowedContentTypes: CookieFile.readableContentTypes) { result in
            perform {
                let url = try result.get()
                let accessed = url.startAccessingSecurityScopedResource()
                defer { if accessed { url.stopAccessingSecurityScopedResource() } }
                let values = try url.resourceValues(forKeys: [.fileSizeKey])
                guard (values.fileSize ?? Int.max) <= 6_000_000 else { throw CookieError.invalidFile }
                let data = try Data(contentsOf: url)
                preview = try CookiePreview(await CookieArchive.open(data, password: archivePassword))
                archivePassword = ""
                status = "请核对域计数和预期邮箱。"
            }
        }
        .fileExporter(isPresented: $exportFile, document: file, contentType: .json,
                      defaultFilename: "diehao-cookies.encrypted.json") { result in
            file = nil
            if case .failure(let error) = result { status = error.localizedDescription }
        }
        .confirmationDialog("删除前保留 Cookie？", isPresented: $deletePrompt, titleVisibility: .visible) {
            Button("加密保留并删除", role: .destructive) { perform {
                try await saveCurrent()
                try await registry.remove(account.id)
                dismiss()
            } }
            Button("删除账号及本槽 Cookie", role: .destructive) { perform {
                try await registry.remove(account.id)
                dismiss()
            } }
        }
    }

    private func perform(_ operation: @escaping @MainActor () async throws -> Void) {
        guard !busy else { return }
        busy = true
        Task {
            defer { busy = false }
            do { try await operation() }
            catch { status = error.localizedDescription }
        }
    }

    private func capture() async throws -> CookiePreview {
        let store = try registry.dataStore(for: account.id)
        return try await CookieTransfer.capture(store: store)
    }

    private func loadPool() {
        do { pool = try SavedCookies.load() }
        catch { status = error.localizedDescription }
    }

    private func saveCurrent() async throws {
        let handle = try registry.makeWebView(for: account.id)
        guard let view = handle.view else { throw CookieError.verification }
        try await CookieLoginProbe().verify(view, email: email)
        let snapshot = try await capture()
        try SavedCookies.save(snapshot.cookies, email: email)
        counts = snapshot.counts
        loadPool()
    }

    private func restore(_ preview: CookiePreview, newAccount: Bool) async throws {
        let target: AccountIdentity
        if newAccount {
            target = try await registry.createAccount(alias: "Muse", reserving: account.id, pendingImport: true)
            do {
                if let source = registry.accounts.first(where: { $0.id == account.id }), source.requiresProxy {
                    guard let configured = source.proxy else { throw RegistryError.proxyUnavailable }
                    var password = ""
                    if let reference = configured.credentialID {
                        guard let data = try T2Keychain.read(reference),
                              let value = String(data: data, encoding: .utf8) else {
                            throw RegistryError.unavailableProxyCredential
                        }
                        password = value
                    }
                    let assigned = try AccountProxySettings(scheme: configured.scheme, host: configured.host,
                                                            port: String(configured.port), username: configured.username)
                    try registry.setProxy(assigned, password: password, for: target.id)
                }
            } catch {
                try? await registry.remove(target.id)
                throw error
            }
        } else {
            target = account
        }
        do {
            try await CookieTransfer.apply(preview, id: target.id, email: email, registry: registry)
        } catch {
            if newAccount { try? await registry.remove(target.id) }
            throw error
        }
        try SavedCookies.save(preview.cookies, email: email)
        loadPool()
        counts = preview.counts
        self.preview = nil
        status = newAccount ? "已验证，已建独立账号。" : "已验证，保留原账号 UUID。"
    }
}

enum ProxyTest {
    static func run(_ settings: AccountProxySettings) async throws -> String {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.proxyConfigurations = [try settings.configuration()]
        configuration.httpCookieStorage = nil
        configuration.urlCache = nil
        configuration.timeoutIntervalForResource = 10
        let session = URLSession(configuration: configuration)
        defer { session.invalidateAndCancel() }
        let start = Date()
        var live = URLRequest(url: URL(string: "https://www.google.com/generate_204")!)
        live.timeoutInterval = 5
        let (_, response) = try await session.data(for: live)
        guard (response as? HTTPURLResponse)?.statusCode == 204 else { throw RegistryError.proxyUnavailable }
        let latency = Date().timeIntervalSince(start)
        var speed = URLRequest(url: URL(string: "https://speed.cloudflare.com/__down?bytes=10000000")!)
        speed.timeoutInterval = 10
        let begin = Date()
        let (stream, download) = try await session.bytes(for: speed)
        guard (download as? HTTPURLResponse)?.statusCode == 200 else { throw RegistryError.proxyUnavailable }
        let headers = Date().timeIntervalSince(begin)
        var received = 0
        for try await _ in stream {
            received += 1
            if received == 10_000_000 { break }
        }
        guard received == 10_000_000 else { throw RegistryError.proxyUnavailable }
        let bandwidth = Double(received) * 8 / max(0.001, Date().timeIntervalSince(begin)) / 1_000_000
        return "代理有效 · 延迟 \(Int(latency * 1000)) ms · TTFB \(Int(headers * 1000)) ms · \(String(format: "%.1f", bandwidth)) Mbps"
    }
}
