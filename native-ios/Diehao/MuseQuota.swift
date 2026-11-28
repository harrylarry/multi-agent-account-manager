import Foundation
import Observation
import WebKit

enum QuotaSource: String, Codable {
    case serverAction = "Server Action"
    case usagePage = "Usage 页面"
    case webView = "WebView"
}

enum QuotaFailure: Error, Equatable {
    case signedOut, exhausted, actionChanged, unreadable, transport
}

struct QuotaReading: Codable, Equatable {
    let remaining: Int?
    let reset: Date?
    let plan: String?
    let exhausted: Bool
    let asOf: Date
    let source: QuotaSource
}

struct QuotaCard: Codable, Equatable {
    var allowance: QuotaReading?
    var extra: QuotaReading?
    var spending: QuotaReading?
    var unavailable = false
    var stale = false
    var actionID: String?
    var previousActionID: String?
}

struct QuotaObservation {
    let allowance: QuotaReading?
    var extra: QuotaReading? = nil
    var spending: QuotaReading? = nil
    var actionID: String? = nil
}

@MainActor
@Observable
final class QuotaCoordinator {
    enum Trigger { case foreground, manual, account }

    private let cacheURL: URL
    private let now: () -> Date
    private let native: @MainActor (UUID, String?) async throws -> QuotaObservation
    private let usage: @MainActor (UUID) async throws -> QuotaObservation
    private let activePage: @MainActor (WKWebView, String?) async throws -> QuotaObservation
    private var inFlight: Set<UUID> = []
    private var lastManual: [UUID: Date] = [:]
    private var lastProbe: [UUID: Date] = [:]
    private var failedAt: [UUID: Date] = [:]
    private(set) var cards: [UUID: QuotaCard]

    convenience init(registry: AccountStoreRegistry, cacheURL: URL) {
        self.init(cacheURL: cacheURL,
                  native: { try await MuseQuotaHTTP.read(account: $0, cachedAction: $1, registry: registry) },
                  usage: { try await MuseQuotaPage.readUsage(account: $0, registry: registry) },
                  activePage: { try await MuseQuotaPage.probe($0, actionID: $1) })
    }

    init(cacheURL: URL, now: @escaping () -> Date = Date.init,
         native: @escaping @MainActor (UUID, String?) async throws -> QuotaObservation,
         usage: @escaping @MainActor (UUID) async throws -> QuotaObservation,
         activePage: @escaping @MainActor (WKWebView, String?) async throws -> QuotaObservation) {
        self.cacheURL = cacheURL
        self.now = now
        self.native = native
        self.usage = usage
        self.activePage = activePage
        cards = (try? JSONDecoder().decode([UUID: QuotaCard].self, from: Data(contentsOf: cacheURL))) ?? [:]
    }

    #if DEBUG && targetEnvironment(simulator)
    static func fixtures(accounts: [AccountIdentity]) -> QuotaCoordinator {
        let freshness = Date()
        let readings = #"""
            1:{"success":true,"result":{"subscription":{"tier":{"name":"测试方案"},"usage":{"percentUsed":0},"extraUsage":{"percentUsed":25}}}}
            """#
        return QuotaCoordinator(cacheURL: URL.temporaryDirectory.appending(path: UUID().uuidString + ".json"),
            native: { id, _ in
                guard id == accounts.first?.id else { throw QuotaFailure.transport }
                guard let result = MuseQuotaPayload.parseFlight(readings, source: .serverAction, now: freshness) else {
                    throw QuotaFailure.unreadable
                }
                return result
            },
            usage: { _ in
                QuotaObservation(allowance: QuotaReading(remaining: 0, reset: nil, plan: nil,
                    exhausted: true, asOf: freshness, source: .usagePage))
            },
            activePage: { _, _ in throw QuotaFailure.unreadable })
    }
    #endif

    func card(for id: UUID, at time: Date) -> QuotaCard {
        var card = cards[id] ?? QuotaCard()
        if let last = card.allowance?.asOf, time.timeIntervalSince(last) >= 1_800 { card.stale = true }
        return card
    }

    func refresh(_ ids: [UUID], trigger: Trigger) async {
        // Bound simultaneous I/O across accounts while keeping the UI responsive.
        for batchStart in stride(from: 0, to: ids.count, by: 3) {
            if Task.isCancelled { return }
            let batch = ids[batchStart..<min(batchStart + 3, ids.count)].map { id in
                Task { await refresh(id, trigger: trigger) }
            }
            for task in batch { await task.value }
        }
    }

    func refresh(_ id: UUID, trigger: Trigger, page: WKWebView? = nil) async {
        guard !inFlight.contains(id) else { return }
        let started = now()
        let cached = card(for: id, at: started)
        guard trigger == .account || !cached.unavailable else { return }
        if trigger != .manual, !cached.stale,
           let asOf = cached.allowance?.asOf, started.timeIntervalSince(asOf) < 1_800 { return }
        if trigger != .manual, let failed = failedAt[id], started.timeIntervalSince(failed) < 15 { return }
        switch trigger {
        case .manual:
            if let last = lastManual[id], started.timeIntervalSince(last) < 15 { return }
            lastManual[id] = started
        case .account:
            guard page != nil else { return }
            if let last = lastProbe[id], started.timeIntervalSince(last) < 90 { return }
            lastProbe[id] = started
        case .foreground: break
        }
        inFlight.insert(id)
        defer { inFlight.remove(id) }

        do {
            let result: QuotaObservation
            if let page, trigger == .account {
                result = try await activePage(page, cached.actionID)
            } else {
                do {
                    result = try await native(id, cached.actionID)
                } catch let error as QuotaFailure where trigger == .manual && error != .signedOut && error != .exhausted {
                    result = try await usage(id)
                }
            }
            apply(result, id: id)
        } catch QuotaFailure.exhausted {
            let source: QuotaSource = trigger == .account ? .webView : .serverAction
            apply(QuotaObservation(allowance: QuotaReading(remaining: 0, reset: cached.allowance?.reset,
                plan: cached.allowance?.plan, exhausted: true, asOf: now(), source: source)), id: id)
        } catch {
            failedAt[id] = now()
            var next = cards[id] ?? QuotaCard()
            next.stale = true
            if (error as? QuotaFailure) == .signedOut { next.unavailable = true }
            cards[id] = next
            persist()
        }
    }

    private func apply(_ result: QuotaObservation, id: UUID) {
        var next = cards[id] ?? QuotaCard()
        if let reading = result.allowance { next.allowance = reading; next.stale = false }
        if let reading = result.extra { next.extra = reading }
        if let reading = result.spending { next.spending = reading }
        if let hash = result.actionID, hash != next.actionID {
            next.previousActionID = next.actionID
            next.actionID = hash
        }
        next.unavailable = false
        failedAt.removeValue(forKey: id)
        cards[id] = next
        persist()
    }

    private func persist() {
        do {
            try FileManager.default.createDirectory(at: cacheURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            try JSONEncoder().encode(cards).write(to: cacheURL, options: [.atomic, .completeFileProtection])
        } catch {
            // Cached observations remain available in memory.
        }
    }
}

enum MuseQuotaPayload {
    static func parseFlight(_ body: String, source: QuotaSource, now: Date = .now) -> QuotaObservation? {
        for line in body.split(whereSeparator: \.isNewline) {
            let row = line.trimmingCharacters(in: .whitespaces)
            let payload: String
            if let colon = row.firstIndex(of: ":"), !row[..<colon].isEmpty,
               row[..<colon].allSatisfy(\.isHexDigit) {
                payload = String(row[row.index(after: colon)...])
            } else { payload = row }
            guard let value = (try? JSONSerialization.jsonObject(with: Data(payload.utf8))) as? [String: Any],
                  value["success"] as? Bool == true else { continue }
            let root = value["result"] as? [String: Any] ?? value
            guard let sub = root["subscription"] as? [String: Any] else { continue }
            let tier = sub["tier"] as? [String: Any] ?? [:]
            let plan = string(tier, ["name", "tierId", "tier_id", "tierCode", "tier_code"])
            let primary = sub["usage"] as? [String: Any] ?? [:]
            let extra = (sub["extraUsage"] ?? sub["extra_usage"] ?? sub["extraCreditsUsage"] ?? sub["extra_credits_usage"]) as? [String: Any]
            let spending = (sub["spending"] ?? sub["spendUsage"] ?? sub["spend_usage"]) as? [String: Any]
            let allowance = reading(primary, plan: plan, source: source, now: now)
            let additional = extra.flatMap { reading($0, plan: nil, source: source, now: now) }
            let spent = spending.flatMap { reading($0, plan: nil, source: source, now: now) }
            guard allowance != nil || additional != nil || spent != nil else { continue }
            return QuotaObservation(allowance: allowance, extra: additional, spending: spent)
        }
        return nil
    }

    private static func string(_ value: [String: Any], _ keys: [String]) -> String? {
        for key in keys {
            if let text = value[key] as? String, !text.isEmpty, text.count <= 80 { return text }
        }
        return nil
    }

    static func reading(_ value: [String: Any], plan: String?, source: QuotaSource, now: Date) -> QuotaReading? {
        let usage = (value["percentUsed"] ?? value["percent_used"]) as? Double
        let state = string(value, ["state", "quotaStatus", "quota_status"])?.lowercased()
        let exhausted = ["exhausted", "out_of_credits", "quota_exceeded", "limit_reached"].contains(state ?? "")
        let percent = usage.flatMap { $0.isFinite && (0...100).contains($0) ? 100 - Int($0.rounded()) : nil }
        guard percent != nil || exhausted || plan != nil else { return nil }
        let timestamp = (value["resetsAt"] ?? value["resets_at"]) as? Double
        let reset = timestamp.flatMap { time -> Date? in
            guard time.isFinite, time > 0 else { return nil }
            return Date(timeIntervalSince1970: time < 1e12 ? time : time / 1000)
        }
        return QuotaReading(remaining: exhausted ? 0 : percent, reset: reset, plan: plan,
                            exhausted: exhausted, asOf: now, source: source)
    }
}

final class MuseRedirectPolicy: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping @Sendable (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

@MainActor
enum MuseQuotaHTTP {
    private static let origin = "https://muse.ai"
    private static let settings = URL(string: origin + "/settings")!
    private static let deadlineSeconds: TimeInterval = 30

    static func read(account: UUID, cachedAction: String?, registry: AccountStoreRegistry) async throws -> QuotaObservation {
        let deadline = Date().addingTimeInterval(deadlineSeconds)
        let (store, configuration) = try registry.accountResources(for: account)
        let cookies = try await readCookies(store)
        guard !cookieHeader(cookies, path: "/").isEmpty else { throw QuotaFailure.signedOut }
        let session = URLSession(configuration: configuration, delegate: MuseRedirectPolicy(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        if let cachedAction {
            do { return try await post(cachedAction, cookies: cookies, session: session, deadline: deadline) }
            catch QuotaFailure.actionChanged {}
        }
        let discovered = try await discover(cookies: cookies, session: session, deadline: deadline)
        return try await post(discovered, cookies: cookies, session: session, deadline: deadline)
    }

    static func readCookies(_ store: WKHTTPCookieStore, timeout interval: Duration = .seconds(5)) async throws -> [HTTPCookie] {
        let (stream, continuation) = AsyncStream<[HTTPCookie]>.makeStream(bufferingPolicy: .bufferingNewest(1))
        let read = Task {
            let cookies = await store.allCookies()
            continuation.yield(cookies)
            continuation.finish()
        }
        let timeout = Task {
            do { try await Task.sleep(for: interval) } catch { return }
            continuation.finish()
        }
        defer { read.cancel(); timeout.cancel() }
        for await cookies in stream { return cookies }
        throw QuotaFailure.transport
    }

    static func cookieHeader(_ cookies: [HTTPCookie], path: String) -> String {
        let scoped = cookies.filter { cookie in
            let domain = cookie.domain.lowercased()
            let cookiePath = cookie.path
            return (domain == "muse.ai" || domain == ".muse.ai")
                && (path == cookiePath || (path.hasPrefix(cookiePath) &&
                    (cookiePath.hasSuffix("/") || path.dropFirst(cookiePath.count).first == "/")))
                && (cookie.expiresDate.map { $0 > .now } ?? true)
                && cookie.name.rangeOfCharacter(from: .controlCharacters) == nil
                && cookie.value.rangeOfCharacter(from: .controlCharacters) == nil
        }.sorted { $0.path.count > $1.path.count }
        return HTTPCookie.requestHeaderFields(with: scoped)["Cookie"] ?? ""
    }

    private static func post(_ actionID: String, cookies: [HTTPCookie], session: URLSession,
                             deadline: Date) async throws -> QuotaObservation {
        guard (40...64).contains(actionID.count), actionID.allSatisfy(\.isHexDigit) else {
            throw QuotaFailure.actionChanged
        }
        var request = URLRequest(url: MusePageDelegate.entryURL)
        request.httpMethod = "POST"
        request.httpBody = Data(#"[{"includeAgreement":true}]"#.utf8)
        request.setValue("text/x-component", forHTTPHeaderField: "Accept")
        request.setValue("text/plain;charset=UTF-8", forHTTPHeaderField: "Content-Type")
        request.setValue(actionID, forHTTPHeaderField: "Next-Action")
        request.setValue(origin, forHTTPHeaderField: "Origin")
        request.setValue(origin + "/", forHTTPHeaderField: "Referer")
        request.setValue(cookieHeader(cookies, path: "/"), forHTTPHeaderField: "Cookie")
        let (response, body) = try await fetch(request, session: session, deadline: deadline)
        try checkSignedOut(response)
        if response.statusCode == 429 { throw QuotaFailure.exhausted }
        if [400, 404, 409].contains(response.statusCode) { throw QuotaFailure.actionChanged }
        guard response.statusCode == 200 else { throw QuotaFailure.transport }
        guard response.value(forHTTPHeaderField: "Content-Type")?.contains("text/x-component") == true,
              var result = MuseQuotaPayload.parseFlight(body, source: .serverAction) else {
            throw QuotaFailure.actionChanged
        }
        result.actionID = actionID
        return result
    }

    private static func discover(cookies: [HTTPCookie], session: URLSession, deadline: Date) async throws -> String {
        let chunkPattern = try NSRegularExpression(pattern: #"/_next/static/(?:immutable/)?chunks/[A-Za-z0-9~._-]+\.js"#)
        let reference = try NSRegularExpression(pattern: #"createServerReference\)?\(\s*["']([a-fA-F0-9]{40,64})["'][^()\r\n]{0,500}["']fetchSubscriptionAction["']"#)
        func chunks(in text: String) -> [String] {
            chunkPattern.matches(in: text, range: NSRange(text.startIndex..., in: text)).compactMap {
                Range($0.range, in: text).map { String(text[$0]) }
            }
        }
        var queue: [String] = []
        for url in [settings, MusePageDelegate.entryURL] {
            var request = URLRequest(url: url)
            request.setValue(cookieHeader(cookies, path: url.path.isEmpty ? "/" : url.path), forHTTPHeaderField: "Cookie")
            let (response, body) = try await fetch(request, session: session, deadline: deadline)
            try checkSignedOut(response)
            if response.statusCode == 200 { queue += chunks(in: body) }
        }
        var seen = Set<String>()
        var index = 0
        var totalBytes = 0
        while index < queue.count, seen.count < 96, totalBytes < 8 * 1024 * 1024 {
            let path = queue[index]
            index += 1
            guard seen.insert(path).inserted, let url = URL(string: origin + path) else { continue }
            let (response, script) = try await fetch(URLRequest(url: url), session: session, deadline: deadline)
            guard response.statusCode == 200 else { continue }
            totalBytes += script.utf8.count
            if let match = reference.firstMatch(in: script, range: NSRange(script.startIndex..., in: script)),
               let value = Range(match.range(at: 1), in: script) {
                return String(script[value])
            }
            queue += chunks(in: script).prefix(128)
        }
        throw QuotaFailure.unreadable
    }

    private static func checkSignedOut(_ response: HTTPURLResponse) throws {
        if response.statusCode == 401 || response.statusCode == 403 { throw QuotaFailure.signedOut }
        if (300..<400).contains(response.statusCode),
           let location = response.value(forHTTPHeaderField: "Location"),
           let target = URL(string: location, relativeTo: response.url),
           target.host == "auth.muse.ai" || target.path.hasPrefix("/login") || target.path.hasPrefix("/signin") {
            throw QuotaFailure.signedOut
        }
    }

    private static func fetch(_ request: URLRequest, session: URLSession, deadline: Date) async throws -> (HTTPURLResponse, String) {
        let time = deadline.timeIntervalSinceNow
        guard time > 0, request.url?.scheme == "https", request.url?.host == "muse.ai" else {
            throw QuotaFailure.transport
        }
        var request = request
        request.timeoutInterval = min(8, time)
        do {
            let (bytes, response) = try await session.bytes(for: request)
            guard let response = response as? HTTPURLResponse else { throw QuotaFailure.transport }
            guard (200..<300).contains(response.statusCode) else { return (response, "") }
            var body = Data()
            for try await byte in bytes {
                guard body.count < 2 * 1024 * 1024 else { throw QuotaFailure.unreadable }
                if body.count.isMultiple(of: 4096), Date() >= deadline { throw QuotaFailure.transport }
                body.append(byte)
            }
            return (response, String(decoding: body, as: UTF8.self))
        } catch let error as QuotaFailure {
            throw error
        } catch {
            throw QuotaFailure.transport
        }
    }
}

@MainActor
enum MuseQuotaPage {
    static func readUsage(account: UUID, registry: AccountStoreRegistry) async throws -> QuotaObservation {
        let handle = try registry.makeWebView(for: account)
        guard let view = handle.view else { throw QuotaFailure.transport }
        view.frame = CGRect(x: 0, y: 0, width: 390, height: 844)
        let (navigation, continuation) = AsyncStream<Bool>.makeStream(bufferingPolicy: .bufferingNewest(1))
        let observer = MusePageDelegate(root: view, onFinish: { page, _ in
            continuation.yield(page.url?.host == "muse.ai")
            continuation.finish()
        }, onError: { _ in continuation.finish() })
        let timeout = Task {
            do { try await Task.sleep(for: .seconds(12)) } catch { return }
            continuation.finish()
        }
        view.navigationDelegate = observer
        view.load(URLRequest(url: URL(string: "https://muse.ai/thread/new")!, timeoutInterval: 12))
        defer {
            timeout.cancel()
            observer.close()
            view.navigationDelegate = nil
        }
        var ready = false
        for await loaded in navigation { ready = loaded }
        guard ready else { throw QuotaFailure.transport }
        return try await read(view, script: usageScript, arguments: [:], source: .usagePage)
    }

    static func probe(_ page: WKWebView, actionID: String?) async throws -> QuotaObservation {
        try await read(page, script: activeScript + usageScript,
                       arguments: ["actionID": actionID.map { $0 as Any } ?? NSNull()], source: .webView)
    }

    private static func read(_ page: WKWebView, script: String, arguments: [String: Any],
                             source: QuotaSource) async throws -> QuotaObservation {
        guard page.url?.scheme == "https", page.url?.host == "muse.ai" else { throw QuotaFailure.transport }
        let (results, continuation) = AsyncStream<String>.makeStream(bufferingPolicy: .bufferingNewest(1))
        page.callAsyncJavaScript(script, arguments: arguments, in: nil, in: .defaultClient) { result in
            if case .success(let value) = result, let text = value as? String { continuation.yield(text) }
            continuation.finish()
        }
        let timeout = Task {
            do { try await Task.sleep(for: .seconds(18)) } catch { return }
            continuation.finish()
        }
        defer { timeout.cancel() }
        for await result in results {
            if result == "signedOut" { throw QuotaFailure.signedOut }
            let observedAt = Date()
            if result == "exhausted" {
                return QuotaObservation(allowance: QuotaReading(remaining: 0, reset: nil, plan: nil,
                    exhausted: true, asOf: observedAt, source: source))
            }
            if result.hasPrefix("flight\n"),
               let reading = MuseQuotaPayload.parseFlight(String(result.dropFirst(7)), source: source, now: observedAt) {
                return reading
            }
            if let data = result.data(using: .utf8),
               let payload = try? JSONDecoder().decode(UsageMeters.self, from: data) {
                let allowance = payload.weekly?.reading(source: source, now: observedAt)
                let extra = payload.extra?.reading(source: source, now: observedAt)
                if allowance != nil || extra != nil { return QuotaObservation(allowance: allowance, extra: extra) }
            }
        }
        throw QuotaFailure.unreadable
    }

    private struct UsageMeters: Decodable {
        struct Meter: Decodable {
            let used: Double
            let resetMillis: Double?
            let plan: String?

            func reading(source: QuotaSource, now: Date) -> QuotaReading? {
                guard used.isFinite, (0...100).contains(used) else { return nil }
                let reset = resetMillis.flatMap { $0.isFinite && $0 > 0 ? Date(timeIntervalSince1970: $0 / 1000) : nil }
                return QuotaReading(remaining: 100 - Int(used.rounded()), reset: reset, plan: plan,
                                    exhausted: used == 100, asOf: now, source: source)
            }
        }
        let weekly: Meter?
        let extra: Meter?
    }

    private static let activeScript = #"""
        if (location.origin !== 'https://muse.ai') return '';
        if (typeof actionID === 'string' && /^[0-9a-f]{40,64}$/i.test(actionID)) {
            try {
                const response = await fetch('/', {
                    method: 'POST', credentials: 'same-origin', redirect: 'error',
                    headers: { Accept: 'text/x-component', 'Content-Type': 'text/plain;charset=UTF-8', 'Next-Action': actionID },
                    body: '[{"includeAgreement":true}]', signal: AbortSignal.timeout(7000)
                });
                if (response.status === 401 || response.status === 403) return 'signedOut';
                if (response.status === 429) return 'exhausted';
                if (response.ok && response.headers.get('content-type')?.includes('text/x-component')) {
                    return 'flight\n' + (await response.text()).slice(0, 2000000);
                }
            } catch {}
        }
        const openSettings = false;
        """# + "\n"

    private static let usageScript = #"""
        if (location.origin !== 'https://muse.ai') return '';
        const canOpen = typeof openSettings === 'undefined' || openSettings;
        const visible = el => {
            const r = el.getBoundingClientRect();
            return r.width > 0 && r.height > 0 && getComputedStyle(el).visibility !== 'hidden';
        };
        const label = el => (el.getAttribute('aria-label') || el.innerText || '').trim();
        const text = el => (el.innerText || '').replace(/\s+/g, ' ').trim();
        const resetDate = value => {
            const chinese = value.match(/(\d{1,2})\s*月\s*(\d{1,2})\s*日/);
            const english = value.match(/\b(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[a-z]*\.?\s+(\d{1,2})\b/i);
            if (!chinese && !english) return null;
            const month = chinese ? Number(chinese[1]) - 1 : 'jan feb mar apr may jun jul aug sep oct nov dec'.split(' ').indexOf(english[1].toLowerCase());
            const day = Number((chinese || english)[2]);
            let year = new Date().getFullYear();
            let date = new Date(year, month, day);
            if (date.getMonth() !== month || date.getDate() !== day) return null;
            if (date.getTime() < Date.now() - 86400000) date = new Date(year + 1, month, day);
            return date.getTime();
        };
        const meters = () => {
            const result = {};
            for (const bar of document.querySelectorAll('[role="progressbar"][aria-valuenow]')) {
                if (!visible(bar)) continue;
                const row = bar.parentElement;
                const name = label(bar) || text(row?.querySelector('[data-slot="text"]') || bar);
                const key = /weekly|每周|每週/i.test(name) ? 'weekly' : /extra|额外|額外/i.test(name) ? 'extra' : '';
                if (!key) continue;
                const detail = Array.from(row?.querySelectorAll('[data-slot="text"]') || [], text);
                result[key] = { used: Number(bar.getAttribute('aria-valuenow')),
                    resetMillis: resetDate(detail.join(' ')), plan: null };
            }
            return Object.keys(result).length ? JSON.stringify(result) : '';
        };
        let opened = false;
        for (let attempt = 0; attempt < (canOpen ? 40 : 1); attempt++) {
            if (document.querySelector('input[autocomplete="one-time-code"], input[type="password"]')) return 'signedOut';
            const readings = meters();
            if (readings) return readings;
            if (/usage limit reached|out of (?:usage|credits)|已用完额度|已达到每周限额/i.test(
                [...document.querySelectorAll('[role="alert"]')].map(text).join(' '))) return 'exhausted';
            if (canOpen) {
                const settings = [...document.querySelectorAll('[role="menuitem"],a')].find(el =>
                    visible(el) && /^(settings|设置|設定)(?:\s*[⌘,].*)?$/i.test(label(el)));
                if (settings && !opened) {
                    settings.click();
                    opened = true;
                } else if (!opened && attempt % 8 === 0) {
                    const shell = document.querySelector('[data-hatch-shell-hydration-state]');
                    if (!shell || shell.getAttribute('data-hatch-shell-hydration-state') === 'hydrated') {
                        const buttons = [...document.querySelectorAll('button[aria-haspopup="menu"]')].filter(visible);
                        const trigger = buttons.find(el => /settings|account|设置|設定|账号/i.test(label(el))) ||
                            buttons.filter(el => el.getBoundingClientRect().left < 80).sort((a,b) =>
                                b.getBoundingClientRect().top - a.getBoundingClientRect().top)[0];
                        if (trigger?.getAttribute('aria-expanded') !== 'true') {
                            trigger?.dispatchEvent(new PointerEvent('pointerdown', {bubbles:true, button:0, pointerType:'mouse'}));
                        }
                    }
                }
                await new Promise(resolve => setTimeout(resolve, 300));
            }
        }
        return '';
        """#
}
