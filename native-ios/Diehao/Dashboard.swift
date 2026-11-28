import SwiftUI

struct DashboardActivity: Equatable {
    var pendingApprovals = 0
    var unreadReplies = 0
    var runningTasks = 0

    var needsAttention: Bool { pendingApprovals > 0 || unreadReplies > 0 }
    var isEmpty: Bool { !needsAttention && runningTasks == 0 }

    static func + (lhs: Self, rhs: Self) -> Self {
        Self(pendingApprovals: lhs.pendingApprovals + rhs.pendingApprovals,
             unreadReplies: lhs.unreadReplies + rhs.unreadReplies,
             runningTasks: lhs.runningTasks + rhs.runningTasks)
    }
}

enum ForegroundPollInterval: Int, CaseIterable, Identifiable {
    case fast = 10, normal = 20, slow = 30
    var id: Int { rawValue }
}

enum ComposeResult {
    case notConnected
}

@MainActor
struct DashboardServices {
    var accounts: @MainActor () -> [AccountIdentity]
    var activity: @MainActor () async -> [UUID: DashboardActivity]?
    var hasSession: @MainActor (UUID) async -> Bool
    var send: @MainActor (UUID, String) async -> ComposeResult

    init(registry: AccountStoreRegistry) {
        accounts = { registry.accounts }
        // T5 supplies activity. nil distinguishes a missing source from verified zeros.
        activity = { nil }
        hasSession = { id in
            do {
                let handle = try registry.makeWebView(for: id)
                guard let webView = handle.view else { return false }
                try await CookieLoginProbe().verifySession(webView)
                return true
            } catch { return false }
        }
        // T7 connects the transport through this account-scoped interface.
        send = { _, _ in .notConnected }
    }

    #if DEBUG
    static func fixtures(registry: AccountStoreRegistry) -> Self {
        let a = AccountIdentity(id: UUID(), purpose: .account, alias: "测试账号 A")
        let b = AccountIdentity(id: UUID(), purpose: .account, alias: "测试账号 B")
        var services = Self(registry: registry)
        var reads = 0
        services.accounts = { [a, b] }
        services.activity = {
            reads += 1
            return reads == 1
                ? [a.id: DashboardActivity(pendingApprovals: 2, unreadReplies: 1),
                   b.id: DashboardActivity(unreadReplies: 3, runningTasks: 1)]
                : [:]
        }
        services.hasSession = { $0 == a.id }
        services.send = { id, text in
            precondition(id == a.id && text == "Simulator compose")
            return .notConnected
        }
        return services
    }
    #endif
}

@MainActor
struct Dashboard: View {
    let registry: AccountStoreRegistry
    let check: IsolationCheck
    @State private var services: DashboardServices
    @Environment(\.scenePhase) private var scenePhase
    @AppStorage("dashboard.foregroundPollSeconds") private var savedInterval = ForegroundPollInterval.normal.rawValue
    @State private var rows: [AccountIdentity] = []
    @State private var activity: [UUID: DashboardActivity]?
    @State private var refreshing = false
    @State private var selected: AccountIdentity?
    @State private var showCompose = false

    init(registry: AccountStoreRegistry, check: IsolationCheck) {
        self.registry = registry
        self.check = check
        #if DEBUG
        if ProcessInfo.processInfo.arguments.contains("-diehao-dashboard-fixtures") {
            _services = State(initialValue: .fixtures(registry: registry))
            return
        }
        #endif
        _services = State(initialValue: DashboardServices(registry: registry))
    }

    private var totals: DashboardActivity? {
        if rows.isEmpty { return DashboardActivity() }
        return activity.map { counts in rows.compactMap { counts[$0.id] }.reduce(DashboardActivity(), +) }
    }

    private var pollSeconds: Int {
        (ForegroundPollInterval(rawValue: savedInterval) ?? .normal).rawValue
    }

    var body: some View {
        List {
            Section("总览") {
                LabeledContent("需要处理", value: totals.map { "\($0.pendingApprovals)" } ?? "未同步")
                    .accessibilityIdentifier("dashboard.approvals")
                LabeledContent("未读回复", value: totals.map { "\($0.unreadReplies)" } ?? "未同步")
                    .accessibilityIdentifier("dashboard.unread")
                LabeledContent("进行中的任务", value: totals.map { "\($0.runningTasks)" } ?? "未同步")
                    .accessibilityIdentifier("dashboard.running")
                if totals?.isEmpty == true {
                    Text("暂无待处理事项").foregroundStyle(.secondary)
                } else if totals == nil {
                    Text("活动同步将在后续版本接入。").foregroundStyle(.secondary)
                }
            }
            Section("账号") {
                if rows.isEmpty { Text("暂无账号").foregroundStyle(.secondary) }
                ForEach(rows) { account in
                    Button { selected = account } label: {
                        Label {
                            Text(account.displayName)
                        } icon: {
                            Image(systemName: "person.crop.circle")
                                .overlay(alignment: .topTrailing) {
                                    if activity?[account.id]?.needsAttention == true {
                                        Circle().fill(.red).frame(width: 7, height: 7)
                                            .accessibilityHidden(true)
                                    }
                                }
                        }
                    }
                    .accessibilityValue(activity?[account.id]?.needsAttention == true ? "有待处理事项" : "")
                }
                NavigationLink("管理账号、代理与 Cookie") {
                    AccountTools(registry: registry)
                        .onDisappear { rows = services.accounts() }
                }
            }
            Section("同步") {
                Picker("前台轮询", selection: $savedInterval) {
                    ForEach(ForegroundPollInterval.allCases) { interval in
                        Text("\(interval.rawValue) 秒").tag(interval.rawValue)
                    }
                }
                .accessibilityIdentifier("dashboard.poll")
                NavigationLink("隔离自检") { IsolationDiagnostics(check: check) }
                    .accessibilityIdentifier("dashboard.isolation")
            }
        }
        .navigationTitle("总览")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("新建对话", systemImage: "square.and.pencil") { showCompose = true }
                    .accessibilityIdentifier("dashboard.compose")
            }
        }
        .refreshable { await refresh() }
        .task(id: scenePhase == .active ? pollSeconds : 0) {
            guard scenePhase == .active else { return }
            await refresh()
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(pollSeconds)) } catch { break }
                await refresh()
            }
        }
        .sheet(item: $selected, onDismiss: { Task { await refresh() } }) { account in
            NavigationStack { AccountDetail(registry: registry, account: account) }
                .interactiveDismissDisabled()
        }
        .sheet(isPresented: $showCompose) {
            DashboardCompose(accounts: rows, services: services)
        }
    }

    private func refresh() async {
        guard !refreshing else { return }
        refreshing = true
        defer { refreshing = false }
        rows = services.accounts()
        let updated = await services.activity()
        if !Task.isCancelled { activity = updated }
    }
}

@MainActor
private struct DashboardCompose: View {
    let accounts: [AccountIdentity]
    let services: DashboardServices
    @Environment(\.dismiss) private var dismiss
    @State private var signedIn: [AccountIdentity] = []
    @State private var selection: UUID?
    @State private var message = ""
    @State private var status = ""
    @State private var checking = true
    @State private var sending = false

    private var allowed: Bool {
        selection != nil && (1...16_000).contains(message.count)
            && !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !sending
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("发送账号") {
                    if checking {
                        ProgressView("正在确认登录状态")
                    } else if signedIn.isEmpty {
                        Text("暂无已确认登录的账号。请从账号资料打开 Muse 登录。")
                    } else {
                        Picker("账号", selection: $selection) {
                            Text("请选择").tag(UUID?.none)
                            ForEach(signedIn) { account in
                                Text(account.displayName).tag(Optional(account.id))
                            }
                        }
                        .accessibilityIdentifier("compose.account")
                    }
                }
                Section("消息") {
                    TextEditor(text: $message)
                        .frame(minHeight: 130)
                        .accessibilityIdentifier("compose.message")
                    Text("\(message.count) / 16000").foregroundStyle(.secondary)
                }
                Section {
                    Button("发送并打开", action: send)
                        .disabled(!allowed)
                        .accessibilityIdentifier("compose.send")
                    if !status.isEmpty {
                        Text(status).accessibilityIdentifier("compose.status")
                    }
                    Text("发送功能将在 T7 接入。").foregroundStyle(.secondary)
                }
            }
            .navigationTitle("新建对话")
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("关闭") { dismiss() } } }
            .task { await verifyAccounts() }
        }
    }

    private func verifyAccounts() async {
        for account in accounts {
            guard !Task.isCancelled else { return }
            if await services.hasSession(account.id), !Task.isCancelled {
                signedIn.append(account)
                checking = false
            }
        }
        checking = false
    }

    private func send() {
        guard allowed, let selection else { return }
        sending = true
        let text = message
        Task {
            defer { sending = false }
            switch await services.send(selection, text) {
            case .notConnected: status = "发送接口已调用，当前版本未发送消息。"
            }
        }
    }
}
