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
    case failed, busy, unknown, retryAvailable, needsReview
    case confirmed(String)
}

@MainActor
struct DashboardServices {
    var accounts: @MainActor () -> [AccountIdentity]
    var activity: @MainActor () async -> [UUID: DashboardActivity]?
    var hasSession: @MainActor (UUID) async -> Bool
    var send: @MainActor (UUID, String) async -> ComposeResult
    var checkSend: @MainActor (UUID, String) async -> ComposeResult = { _, _ in .unknown }
    var retrySend: @MainActor (UUID, String) async -> ComposeResult = { _, _ in .unknown }

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

    #if DEBUG && targetEnvironment(simulator)
    static func fixtures(registry: AccountStoreRegistry, accountID: UUID = UUID()) -> Self {
        let a = AccountIdentity(id: accountID, purpose: .account, alias: "测试账号 A")
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
    @State private var quota: QuotaCoordinator
    @State private var activityCoordinator: MuseActivityCoordinator
    @State private var approvals: MuseApprovalQueue
    @State private var sender: MuseSendCoordinator
    @Environment(\.scenePhase) private var scenePhase
    @AppStorage("dashboard.foregroundPollSeconds") private var savedInterval = ForegroundPollInterval.normal.rawValue
    @State private var rows: [AccountIdentity] = []
    @State private var activity: [UUID: DashboardActivity]?
    @State private var refreshing = false
    @State private var activityEnabled = false
    @State private var selected: AccountIdentity?
    @State private var selectedThread: String?
    @State private var confirmedRoute: (UUID, String)?
    @State private var showCompose = false

    init(registry: AccountStoreRegistry, check: IsolationCheck) {
        self.registry = registry
        self.check = check
        let activityCoordinator: MuseActivityCoordinator
        let sender: MuseSendCoordinator
        #if DEBUG && targetEnvironment(simulator)
        let arguments = ProcessInfo.processInfo.arguments
        if let index = arguments.firstIndex(of: "-diehao-send-fixtures"),
           arguments.indices.contains(index + 1), let runID = UUID(uuidString: arguments[index + 1]) {
            sender = .fixtures(runID: runID)
            var fixtures = DashboardServices.fixtures(registry: registry, accountID: runID)
            fixtures.send = { await sender.submit(account: $0, message: $1) }
            fixtures.checkSend = { await sender.check(account: $0, message: $1) }
            fixtures.retrySend = { await sender.submit(account: $0, message: $1, retry: true) }
            _sender = State(initialValue: sender)
            _services = State(initialValue: fixtures)
            _quota = State(initialValue: .fixtures(accounts: fixtures.accounts()))
            _activityCoordinator = State(initialValue: .fixtures(account: nil))
            _approvals = State(initialValue: .fixtures(account: nil))
            return
        }
        #endif
        sender = MuseSendCoordinator(registry: registry)
        _sender = State(initialValue: sender)
        #if DEBUG && targetEnvironment(simulator)
        if ProcessInfo.processInfo.arguments.contains("-diehao-approval-fixtures") {
            let fixtures = DashboardServices.fixtures(registry: registry)
            _services = State(initialValue: fixtures)
            _quota = State(initialValue: .fixtures(accounts: fixtures.accounts()))
            _activityCoordinator = State(initialValue: .fixtures(account: nil))
            _approvals = State(initialValue: .fixtures(account: fixtures.accounts().first?.id))
            return
        }
        if ProcessInfo.processInfo.arguments.contains("-diehao-activity-fixtures") {
            let fixtures = DashboardServices.fixtures(registry: registry)
            _services = State(initialValue: fixtures)
            _quota = State(initialValue: .fixtures(accounts: fixtures.accounts()))
            activityCoordinator = .fixtures(account: fixtures.accounts().first?.id)
            _activityCoordinator = State(initialValue: activityCoordinator)
            _approvals = State(initialValue: Self.makeApprovals(activityCoordinator))
            return
        }
        if ProcessInfo.processInfo.arguments.contains("-diehao-dashboard-fixtures") ||
            ProcessInfo.processInfo.arguments.contains("-diehao-quota-fixtures") {
            let fixtures = DashboardServices.fixtures(registry: registry)
            _services = State(initialValue: fixtures)
            _quota = State(initialValue: .fixtures(accounts: fixtures.accounts()))
            activityCoordinator = .fixtures(account: nil)
            _activityCoordinator = State(initialValue: activityCoordinator)
            _approvals = State(initialValue: Self.makeApprovals(activityCoordinator))
            return
        }
        #endif
        activityCoordinator = MuseActivityCoordinator(registry: registry)
        _activityCoordinator = State(initialValue: activityCoordinator)
        _approvals = State(initialValue: Self.makeApprovals(activityCoordinator))
        var services = DashboardServices(registry: registry)
        services.send = { await sender.submit(account: $0, message: $1) }
        services.checkSend = { await sender.check(account: $0, message: $1) }
        services.retrySend = { await sender.submit(account: $0, message: $1, retry: true) }
        _services = State(initialValue: services)
        _quota = State(initialValue: QuotaCoordinator(registry: registry,
            cacheURL: URL.applicationSupportDirectory.appending(path: "DiehaoNative/quota.json")))
    }

    private var totals: DashboardActivity? {
        if rows.isEmpty { return DashboardActivity() }
        return displayedActivity.map { counts in rows.compactMap { counts[$0.id] }.reduce(DashboardActivity(), +) }
    }

    private var displayedActivity: [UUID: DashboardActivity]? {
        #if DEBUG && targetEnvironment(simulator)
        if ProcessInfo.processInfo.arguments.contains("-diehao-dashboard-fixtures") ||
            ProcessInfo.processInfo.arguments.contains("-diehao-quota-fixtures") { return activity }
        #endif
        let counts = approvals.counts()
        if let observed = activityCoordinator.activity() { return observed.merging(counts, uniquingKeysWith: +) }
        return counts.isEmpty ? nil : counts
    }

    private static func makeApprovals(_ activity: MuseActivityCoordinator) -> MuseApprovalQueue {
        let queue = MuseApprovalQueue(call: { try await activity.approvalCall($0, path: $1, decision: $2) })
        activity.onNeedsApproval = { [weak queue] id in Task { await queue?.refresh(id) } }
        activity.onRunEnded = { [weak queue] in queue?.finishRuns(for: $0) }
        return queue
    }

    private var pollSeconds: Int {
        (ForegroundPollInterval(rawValue: savedInterval) ?? .normal).rawValue
    }

    var body: some View {
        List {
            Section("总览") {
                activityRow("需要处理", count: totals?.pendingApprovals, identifier: "dashboard.approvals")
                activityRow("未读回复", count: totals?.unreadReplies, identifier: "dashboard.unread")
                activityRow("进行中的任务", count: totals?.runningTasks, identifier: "dashboard.running")
                if totals?.isEmpty == true {
                    Text("暂无待处理事项").foregroundStyle(.secondary)
                } else if totals == nil {
                    Text("尚未收到 Muse 活动记录。").foregroundStyle(.secondary)
                }
            }
            Section("账号") {
                if rows.isEmpty { Text("暂无账号").foregroundStyle(.secondary) }
                ForEach(rows) { account in
                    Button {
                        selectedThread = nil
                        selected = account
                    } label: {
                        Label {
                            VStack(alignment: .leading, spacing: 3) {
                                Text(account.displayName)
                                TimelineView(.periodic(from: .now, by: 60)) { context in
                                    QuotaSummary(card: quota.card(for: account.id, at: context.date), now: context.date)
                                }
                            }
                        } icon: {
                            Image(systemName: "person.crop.circle")
                                .overlay(alignment: .topTrailing) {
                                    if displayedActivity?[account.id]?.needsAttention == true {
                                        Circle().fill(.red).frame(width: 7, height: 7)
                                            .accessibilityHidden(true)
                                    }
                                }
                        }
                    }
                    .accessibilityLabel(account.displayName)
                    .accessibilityValue(accountSummary(account.id))
                }
                NavigationLink("管理账号、代理与 Cookie") {
                    AccountTools(registry: registry, quota: quota, museActivity: activityCoordinator, approvals: approvals)
                        .onDisappear { rows = services.accounts() }
                }
            }
            Section("同步") {
                Menu {
                    Picker("前台轮询", selection: $savedInterval) {
                        ForEach(ForegroundPollInterval.allCases) { interval in
                            Text("\(interval.rawValue) 秒").tag(interval.rawValue)
                        }
                    }
                } label: {
                    LabeledContent("前台轮询", value: "\(pollSeconds) 秒")
                }
                .accessibilityLabel("前台轮询")
                .accessibilityValue("\(pollSeconds) 秒")
                .accessibilityIdentifier("dashboard.poll")
                NavigationLink("隔离自检") { IsolationDiagnostics(check: check) }
                    .accessibilityIdentifier("dashboard.isolation")
            }
        }
        .navigationTitle("总览")
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button("刷新额度", systemImage: "arrow.clockwise") {
                    Task { await refreshManually() }
                }
                .accessibilityIdentifier("dashboard.refreshQuota")
            }
            ToolbarItem(placement: .primaryAction) {
                Button("新建对话", systemImage: "square.and.pencil") { showCompose = true }
                    .accessibilityIdentifier("dashboard.compose")
            }
        }
        .refreshable { await refreshManually() }
        .task(id: scenePhase) {
            let accounts = services.accounts()
            guard scenePhase == .active else {
                activityEnabled = false
                activityCoordinator.configure(accounts, active: false, interval: pollSeconds)
                approvals.configure([], active: false)
                return
            }
            await quota.refresh(services.accounts().map(\.id), trigger: .foreground)
            if !Task.isCancelled {
                activityEnabled = true
                activityCoordinator.configure(services.accounts(), active: true, interval: pollSeconds)
                approvals.configure(services.accounts().map(\.id), active: true)
            }
            while !Task.isCancelled {
                await approvals.poll(running: activityCoordinator.runningAccounts)
                do { try await Task.sleep(for: .seconds(1)) } catch { return }
            }
        }
        .task(id: scenePhase == .active) {
            guard scenePhase == .active else { return }
            let accounts = services.accounts()
            if !accounts.isEmpty { await sender.resume(accounts: Set(accounts.map(\.id))) }
        }
        .task(id: scenePhase == .active ? pollSeconds : 0) {
            guard scenePhase == .active else { return }
            await refresh()
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(pollSeconds)) } catch { break }
                await refresh()
            }
        }
        .sheet(item: $selected, onDismiss: { Task {
            selectedThread = nil
            await activityCoordinator.reconcileParked()
            await refresh()
        } }) { account in
            NavigationStack {
                AccountDetail(registry: registry, account: account, quota: quota,
                    museActivity: activityCoordinator, approvals: approvals, initialThread: selectedThread)
            }
                .interactiveDismissDisabled()
        }
        .sheet(isPresented: $showCompose, onDismiss: {
            guard let (id, thread) = confirmedRoute else { return }
            confirmedRoute = nil
            selectedThread = thread
            selected = rows.first { $0.id == id }
        }) {
            DashboardCompose(accounts: rows, services: services) { id, thread in
                confirmedRoute = (id, thread)
                showCompose = false
            }
        }
    }

    private func activityRow(_ title: String, count: Int?, identifier: String) -> some View {
        let value = count.map { String($0) } ?? "未同步"
        return LabeledContent(title, value: value)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(title)
            .accessibilityValue(value)
            .accessibilityIdentifier(identifier)
    }

    private func accountSummary(_ id: UUID) -> String {
        let card = quota.card(for: id, at: .now)
        var summary = displayedActivity?[id]?.needsAttention == true ? ["有待处理事项"] : []
        if card.unavailable {
            summary.append("Muse 会话已失效")
        } else if let allowance = card.allowance, allowance.exhausted {
            summary.append("额度已用尽")
        } else if let remaining = card.allowance?.remaining {
            summary.append("额度剩余 \(remaining)%")
        } else {
            summary.append("额度未观测")
        }
        if card.stale { summary.append("读数已过期") }
        return summary.joined(separator: "，")
    }

    private func refresh() async {
        guard !refreshing else { return }
        refreshing = true
        defer { refreshing = false }
        rows = services.accounts()
        if activityEnabled {
            activityCoordinator.configure(rows, active: scenePhase == .active, interval: pollSeconds)
            approvals.configure(rows.map(\.id), active: scenePhase == .active)
        }
        let updated = await services.activity()
        if !Task.isCancelled { activity = updated }
    }

    private func refreshManually() async {
        await quota.refresh(services.accounts().map(\.id), trigger: .manual)
        await refresh()
    }
}

struct QuotaSummary: View {
    let card: QuotaCard
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            if card.unavailable { Text("Muse 会话已失效") }
            if let allowance = card.allowance {
                let remaining = allowance.remaining.map { "剩余 \($0)%" } ?? "额度未观测"
                Text([allowance.plan, allowance.exhausted ? "额度已用尽" : remaining]
                    .compactMap { $0 }.joined(separator: " · "))
                Text(readingTime(allowance, stale: card.stale))
                if let reset = allowance.reset {
                    ViewThatFits(in: .horizontal) {
                        Text("重置 \(reset, format: .dateTime.month().day().hour().minute()) · \(reset, style: .relative)")
                        Text("重置 \(reset, style: .relative)")
                    }
                }
            } else {
                Text(card.stale ? "额度未观测 · 同步未完成" : "额度未观测")
            }
            if let extra = card.extra, let remaining = extra.remaining {
                Text("额外额度剩余 \(remaining)% · \(readingTime(extra))")
            }
            if let spending = card.spending, let remaining = spending.remaining {
                Text("花费额度剩余 \(remaining)% · \(readingTime(spending))")
            } else {
                Text("花费未观测")
            }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }

    private func readingTime(_ reading: QuotaReading, stale: Bool = false) -> String {
        let stale = stale || now.timeIntervalSince(reading.asOf) >= 1_800
        return reading.source.rawValue + " · " + reading.asOf.formatted(date: .omitted, time: .shortened)
            + (stale ? " · 读数已过期" : "")
    }
}

@MainActor
private struct DashboardCompose: View {
    let accounts: [AccountIdentity]
    let services: DashboardServices
    let onConfirmed: (UUID, String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var signedIn: [AccountIdentity] = []
    @State private var selection: UUID?
    @State private var message = ""
    @State private var status = ""
    @State private var checking = true
    @State private var sending = false
    @State private var canCheck = false
    @State private var canRetry = false
    @State private var pendingText: String?
    @State private var pendingAccount: UUID?

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
                        Menu {
                            ForEach(signedIn) { account in
                                Button {
                                    selection = account.id
                                } label: {
                                    if selection == account.id {
                                        Label("用 \(account.displayName) 发送", systemImage: "checkmark")
                                    } else {
                                        Text("用 \(account.displayName) 发送")
                                    }
                                }
                            }
                        } label: {
                            LabeledContent("账号", value: signedIn.first { $0.id == selection }?.displayName ?? "请选择")
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
                    if canCheck {
                        Button("核对发送结果") { verifySend() }
                            .disabled(sending)
                            .accessibilityIdentifier("compose.checkSend")
                    }
                    if canRetry {
                        Button("手动重试一次") { verifySend(retry: true) }.disabled(sending)
                    }
                }
            }
            .scrollDismissesKeyboard(.interactively)
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
        canCheck = false
        canRetry = false
        let text = message
        Task {
            defer { sending = false }
            handle(await services.send(selection, text), id: selection, text: text)
        }
    }

    private func verifySend(retry: Bool = false) {
        guard !sending, let id = pendingAccount, let text = pendingText else { return }
        sending = true
        Task {
            defer { sending = false }
            let outcome = retry ? await services.retrySend(id, text) : await services.checkSend(id, text)
            handle(outcome, id: id, text: text)
        }
    }

    private func handle(_ result: ComposeResult, id: UUID, text: String) {
        canCheck = false
        canRetry = false
        switch result {
        case .notConnected: status = "发送接口已调用，当前版本未发送消息。"
        case .failed: status = "发送未完成。请检查账号状态。"
        case .busy: status = "这个账号正在发送或核对。"
        case .unknown:
            status = "发送结果未知，可核对历史。"
            pendingAccount = id
            pendingText = text
            canCheck = true
        case .retryAvailable:
            status = "历史已确认该会话为空，可手动重试一次。"
            pendingAccount = id
            pendingText = text
            canRetry = true
        case .needsReview:
            status = "自动核对已停止。请在 Muse 会话中确认发送结果。"
        case .confirmed(let thread):
            onConfirmed(id, thread)
        }
    }
}
