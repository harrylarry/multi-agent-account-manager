import CryptoKit
import Network
import SwiftUI
import UIKit
import UniformTypeIdentifiers

struct CookieFile: FileDocument {
    static let readableContentTypes: [UTType] = [.json]
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

private struct CookieDocumentPicker: UIViewControllerRepresentable {
    let onPick: (URL?) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(onPick: onPick) }

    func makeUIViewController(context: Context) -> UIDocumentPickerViewController {
        let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.item], asCopy: false)
        picker.delegate = context.coordinator
        picker.allowsMultipleSelection = false
        return picker
    }

    func updateUIViewController(_ controller: UIDocumentPickerViewController, context: Context) {}

    @MainActor
    final class Coordinator: NSObject, UIDocumentPickerDelegate {
        private let onPick: (URL?) -> Void

        init(onPick: @escaping (URL?) -> Void) { self.onPick = onPick }

        func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
            onPick(urls.first)
        }

        func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) { onPick(nil) }
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
    var quota: QuotaCoordinator? = nil
    @State private var accounts: [AccountIdentity] = []
    @State private var alias = ""
    @State private var status = ""
    @State private var selected: AccountIdentity?
    @State private var busy = false

    var body: some View {
        List {
            Section("Muse 账号") {
                ForEach(accounts) { account in
                    Button(account.displayName) {
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
            NavigationStack { AccountDetail(registry: registry, account: account, quota: quota) }
                .interactiveDismissDisabled()
        }
    }

    private func refresh() { accounts = registry.accounts }
}

@MainActor
struct AccountDetail: View {
    @Environment(\.dismiss) private var dismiss
    let registry: AccountStoreRegistry
    let account: AccountIdentity
    var quota: QuotaCoordinator? = nil
    @State private var scheme: AccountProxySettings.Scheme = .http
    @State private var host = ""
    @State private var port = ""
    @State private var username = ""
    @State private var proxyPassword = ""
    @State private var email = ""
    @State private var archivePassword = ""
    @State private var counts = ""
    @State private var status = ""
    @State private var cookieStatus = ""
    @State private var busy = false
    @State private var chooseFile = false
    @State private var exportFile = false
    @State private var file: CookieFile?
    @State private var preview: CookiePreview?
    @State private var selectedFile: URL?
    @State private var pool: [SavedCookies.Entry] = []
    @State private var page: AccountWebView?
    @State private var showPage = false
    @State private var pageError: String?
    @State private var pageRetry = 0
    @State private var deletePrompt = false

    var body: some View {
        Form {
            Section {
                Button("打开 Muse 登录") {
                    do {
                        page = try registry.makeWebView(for: account.id)
                        pageError = nil
                        pageRetry = 0
                        showPage = true
                    } catch { status = error.localizedDescription }
                }
                .accessibilityIdentifier("account.openMuse")
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
            cookieSection
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
        .navigationDestination(isPresented: $showPage) {
            if let webView = page?.view {
                VStack(spacing: 0) {
                    if let pageError {
                        HStack {
                            Text(pageError)
                            Spacer()
                            Button("重试") {
                                self.pageError = nil
                                pageRetry += 1
                            }
                        }
                        .padding()
                    }
                    MuseAccountPage(webView: webView, retry: pageRetry, onResult: { pageError = $0 },
                        onPage: { view in
                            guard let quota else { return }
                            Task { await quota.refresh(account.id, trigger: .account, page: view) }
                        })
                }
                .navigationTitle("Muse")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar { ToolbarItem(placement: .confirmationAction) {
                    Button("关闭 Muse") { showPage = false }
                        .accessibilityIdentifier("account.closeMuse")
                } }
            }
        }
        .onChange(of: showPage) { _, presented in
            if !presented { page = nil }
        }
        .sheet(isPresented: $chooseFile) {
            CookieDocumentPicker { url in
                chooseFile = false
                guard let url else { return }
                selectedFile = url
                preview = nil
                cookieStatus = "加密备份请先输入口令，再点「读取并预览 Cookie」。"
            }
        }
        .fileExporter(isPresented: $exportFile, document: file, contentType: .json,
                      defaultFilename: "diehao-cookies.encrypted.json") { result in
            file = nil
            if case .failure(let error) = result { cookieStatus = error.localizedDescription }
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

    private var cookieSection: some View {
        Section("双域 Cookie") {
            TextField("要验证的 Muse 邮箱", text: $email)
                .keyboardType(.emailAddress).textInputAutocapitalization(.never)
            SecureField("备份文件口令", text: $archivePassword)
            if !counts.isEmpty { Text(counts) }
            if !cookieStatus.isEmpty { Text(cookieStatus).accessibilityIdentifier("cookie.status") }
            Button("按域清点") { perform(cookie: true) {
                counts = try await capture().counts
            } }
            Button("验证并保存到 Cookie 池") { perform(cookie: true) {
                try await saveCurrent()
                cookieStatus = "已验证并加密保存。"
            } }
            Button("加密导出 Cookie") { perform(cookie: true) {
                let records = try await capture()
                counts = records.counts
                let sealed = try await CookieArchive.seal(records.cookies, password: archivePassword)
                file = CookieFile(data: sealed)
                archivePassword = ""
                exportFile = true
            } }
            Button("选择 Cookie 文件") { chooseFile = true }
            if let selectedFile {
                Text("已选择 \(selectedFile.lastPathComponent)")
                Button("读取并预览 Cookie") { perform(cookie: true) {
                    let data = try await Task.detached(priority: .userInitiated) {
                        try Self.readCookieFile(selectedFile)
                    }.value
                    preview = try CookiePreview(await CookieArchive.open(data, password: archivePassword))
                    archivePassword = ""
                    self.selectedFile = nil
                    cookieStatus = "请核对域计数和预期邮箱。"
                } }
            }
            ForEach(pool) { entry in
                Button("选择已保存的 Cookie · " + entry.email) {
                    do {
                        preview = try CookiePreview(entry.records)
                        selectedFile = nil
                        email = entry.email
                    } catch { cookieStatus = error.localizedDescription }
                }
            }
            if let preview {
                Text(preview.counts)
                ForEach(preview.notes, id: \.self) { Text($0).font(.caption) }
                Button("验证并覆盖此账号") { perform(cookie: true) { try await restore(preview, newAccount: false) } }
                Button("验证并新建独立账号") { perform(cookie: true) { try await restore(preview, newAccount: true) } }
            }
            Button("恢复导入前的 Cookie") { perform(cookie: true) {
                try await CookieTransfer.recover(id: account.id, registry: registry)
                cookieStatus = "已恢复导入前的 Cookie。"
            } }
        }
    }

    private func perform(cookie: Bool = false, _ operation: @escaping @MainActor () async throws -> Void) {
        guard !busy else { return }
        if cookie { cookieStatus = "" }
        else { status = "" }
        busy = true
        Task {
            defer { busy = false }
            do { try await operation() }
            catch {
                if cookie { cookieStatus = error.localizedDescription }
                else { status = error.localizedDescription }
            }
        }
    }

    nonisolated private static func readCookieFile(_ url: URL) throws -> Data {
        let accessed = url.startAccessingSecurityScopedResource()
        defer { if accessed { url.stopAccessingSecurityScopedResource() } }
        var coordinationError: NSError?
        var result: Result<Data, Error>?
        NSFileCoordinator().coordinate(readingItemAt: url, options: [], error: &coordinationError) { file in
            result = Result {
                let values = try file.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
                guard values.isRegularFile == true, (values.fileSize ?? Int.max) <= 6_000_000 else {
                    throw CookieError.invalidFile
                }
                return try Data(contentsOf: file)
            }
        }
        if let coordinationError { throw coordinationError }
        guard let result else { throw CookieError.invalidFile }
        return try result.get()
    }

    private func capture() async throws -> CookiePreview {
        let store = try registry.dataStore(for: account.id)
        return try await CookieTransfer.capture(store: store)
    }

    private func loadPool() {
        do { pool = try SavedCookies.load() }
        catch { cookieStatus = error.localizedDescription }
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
        cookieStatus = newAccount ? "已验证，已建独立账号。" : "已验证，保留原账号 UUID。"
    }
}

enum ProxyTest {
    @MainActor
    static func run(_ settings: AccountProxySettings) async throws -> String {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.proxyConfigurations = [try settings.configuration()]
        configuration.httpCookieStorage = nil
        configuration.urlCache = nil
        configuration.timeoutIntervalForResource = 20
        let session = URLSession(configuration: configuration)
        defer { session.invalidateAndCancel() }

        let start = Date()
        var live = URLRequest(url: MusePageDelegate.entryURL, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 12)
        live.httpMethod = "HEAD"
        let response: URLResponse
        do { response = try await session.data(for: live).1 }
        catch let error as URLError where error.code == .timedOut {
            return "代理下 Muse 入口请求超时。"
        }
        guard let http = response as? HTTPURLResponse else { throw RegistryError.proxyUnavailable }
        guard (200..<300).contains(http.statusCode), http.url?.scheme == "https",
              http.url?.host == MusePageDelegate.entryURL.host else {
            return "代理下 Muse 入口检测失败（HTTP \(http.statusCode) 或跳转到其他站点）。"
        }
        let latency = Date().timeIntervalSince(start)
        let reachability = "代理下 Muse 入口可达 · \(Int(latency * 1000)) ms"
        let speed = URLRequest(url: URL(string: "https://speed.cloudflare.com/__down?bytes=10000000")!, timeoutInterval: 15)
        let begin = Date()
        do {
            let (stream, download) = try await session.bytes(for: speed)
            guard (download as? HTTPURLResponse)?.statusCode == 200 else {
                return reachability + " · 下载测速未完成"
            }
            let headers = Date().timeIntervalSince(begin)
            var received = 0
            for try await _ in stream {
                received += 1
                if received == 10_000_000 { break }
            }
            guard received == 10_000_000 else { return reachability + " · 下载测速未完成" }
            let bandwidth = Double(received) * 8 / max(0.001, Date().timeIntervalSince(begin) - headers) / 1_000_000
            return reachability + " · 测速首包 \(Int(headers * 1000)) ms · \(String(format: "%.1f", bandwidth)) Mbps"
        } catch let error as URLError where error.code == .timedOut {
            return reachability + " · 下载测速超时"
        } catch {
            return reachability + " · 下载测速未完成"
        }
    }
}
