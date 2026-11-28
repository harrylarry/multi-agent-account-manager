import Foundation

enum MuseActivityError: Error { case unavailable, signIn, timeout, invalidFrame, disconnected, rejected(Int) }

@MainActor
protocol MuseActivityConnection: AnyObject {
    func events() throws -> AsyncThrowingStream<Data, Error>
    func history() async throws -> [MuseReply]
    func ping() async throws
    func close()
}

private final class MuseWire {
    private let handle: OpaquePointer

    init() throws {
        guard let handle = diehao_wire_new() else { throw MuseActivityError.unavailable }
        self.handle = handle
    }
    deinit { diehao_wire_free(handle) }

    func start() throws -> Data { try Self.read(diehao_wire_message_one(handle)) }
    func finish(_ data: Data) throws -> Data { try invoke(data, using: diehao_wire_message_three) }
    func request(_ path: String) throws -> Data { try invoke(Data(path.utf8), using: diehao_wire_request) }
    func receive(_ data: Data) throws -> Data { try invoke(data, using: diehao_wire_receive) }
    func post(_ path: String, body: Data) throws -> Data {
        try Data(path.utf8).withUnsafeBytes { route in
            try body.withUnsafeBytes { payload in
                try Self.read(diehao_wire_post(handle, route.bindMemory(to: UInt8.self).baseAddress, route.count,
                    payload.bindMemory(to: UInt8.self).baseAddress, payload.count))
            }
        }
    }

    static func ack(_ line: Data) throws -> Data {
        try line.withUnsafeBytes {
            try read(diehao_wire_chat_ack($0.bindMemory(to: UInt8.self).baseAddress, $0.count))
        }
    }

    private func invoke(_ data: Data, using call: (OpaquePointer?, UnsafePointer<UInt8>?, Int) -> OpaquePointer?) throws -> Data {
        try data.withUnsafeBytes { bytes in
            try Self.read(call(handle, bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count))
        }
    }
    private static func read(_ handle: OpaquePointer?) throws -> Data {
        guard let handle else { throw MuseActivityError.invalidFrame }
        defer { diehao_wire_blob_free(handle) }
        var size = 0
        guard let bytes = diehao_wire_blob_read(handle, &size), (0...8_388_608).contains(size) else {
            throw MuseActivityError.invalidFrame
        }
        return Data(bytes: bytes, count: size)
    }
}

@MainActor
final class NativeMuseActivityConnection: MuseActivityConnection {
    private struct Pending {
        let continuation: AsyncThrowingStream<Data, Error>.Continuation
        let persistent: Bool
        var timeout: Task<Void, Never>?
    }

    private let session: URLSession
    private let socket: URLSessionWebSocketTask
    private let wire: MuseWire
    private let outbound: AsyncStream<Data>.Continuation
    private var pending: [UInt64: Pending] = [:]
    private var sessionRevisions: [String: String] = [:]
    private var receiver: Task<Void, Never>?
    private var sender: Task<Void, Never>?
    private var closed = false

    static func open(registry: AccountStoreRegistry, id: UUID) async throws -> NativeMuseActivityConnection {
        let (store, configuration) = try registry.accountResources(for: id)
        let cookies = try await MuseQuotaHTTP.readCookies(store, timeout: .seconds(15))
        let cookie = MuseQuotaHTTP.cookieHeader(cookies, path: "/")
        guard !cookie.isEmpty else { throw MuseActivityError.signIn }
        let control = URLSession(configuration: configuration, delegate: MuseRedirectPolicy(), delegateQueue: nil)
        defer { control.invalidateAndCancel() }
        let url = try await gatewayURL(control: control, cookie: cookie)
        try Task.checkCancellation()
        configuration.timeoutIntervalForResource = 24 * 60 * 60
        let session = URLSession(configuration: configuration, delegate: MuseRedirectPolicy(), delegateQueue: nil)
        var request = URLRequest(url: url)
        request.setValue("https://muse.ai", forHTTPHeaderField: "Origin")
        let socket = session.webSocketTask(with: request)
        socket.maximumMessageSize = 65_535
        let connection = try NativeMuseActivityConnection(session: session, socket: socket)
        do {
            try await connection.start()
            return connection
        } catch {
            connection.close()
            throw error
        }
    }

    private init(session: URLSession, socket: URLSessionWebSocketTask) throws {
        self.session = session
        self.socket = socket
        wire = try MuseWire()
        let (writes, outbound) = AsyncStream<Data>.makeStream(bufferingPolicy: .bufferingOldest(64))
        self.outbound = outbound
        sender = Task { [weak self] in
            do {
                for await bytes in writes { try await socket.send(.data(bytes)) }
            } catch {
                self?.close()
            }
        }
    }

    private func start() async throws {
        socket.resume()
        let socket = self.socket
        let watchdog = Task {
            do { try await Task.sleep(for: .seconds(10)) } catch { return }
            socket.cancel(with: .goingAway, reason: nil)
        }
        defer { watchdog.cancel() }
        try await socket.send(.data(wire.start()))
        guard case .data(let answer) = try await socket.receive() else { throw MuseActivityError.invalidFrame }
        try await socket.send(.data(wire.finish(answer)))
        receiver = Task { [weak self] in
            do {
                while !Task.isCancelled {
                    guard case .data(let bytes) = try await socket.receive(), let self else { break }
                    try self.dispatch(bytes)
                }
            } catch {}
            self?.close()
        }
    }

    func events() throws -> AsyncThrowingStream<Data, Error> {
        try issue("/chat/subscribe", persistent: true)
    }

    func history() async throws -> [MuseReply] {
        let list = try await call("/api/session/list")
        let sessions = try MuseReply.sessions(from: list)
        var replies: [MuseReply] = []
        for snapshot in sessions.prefix(12) {
            try Task.checkCancellation()
            let id = snapshot.id
            if let revision = snapshot.revision, sessionRevisions[id] == revision { continue }
            var parts = URLComponents()
            parts.path = "/chat/history"
            parts.queryItems = [.init(name: "limit", value: "50"), .init(name: "transcript_mode", value: "messages"),
                                .init(name: "session_id", value: id)]
            guard let route = parts.string else { continue }
            replies += try MuseReply.history(from: await call(route), thread: id)
        }
        sessionRevisions = Dictionary(
            (sessions.prefix(12).compactMap { snapshot in snapshot.revision.map { (snapshot.id, $0) } }),
            uniquingKeysWith: { first, _ in first })
        return replies
    }

    func ping() async throws {
        guard !closed else { throw MuseActivityError.disconnected }
        let (pongs, continuation) = AsyncStream<Bool>.makeStream(bufferingPolicy: .bufferingNewest(1))
        socket.sendPing { error in
            continuation.yield(error == nil)
            continuation.finish()
        }
        let deadline = Task {
            do { try await Task.sleep(for: .seconds(8)) } catch { return }
            continuation.finish()
        }
        defer { deadline.cancel() }
        for await pong in pongs where pong { return }
        throw MuseActivityError.timeout
    }

    func close() {
        guard !closed else { return }
        closed = true
        socket.cancel(with: .goingAway, reason: nil)
        session.invalidateAndCancel()
        sender?.cancel()
        receiver?.cancel()
        outbound.finish()
        for entry in pending.values {
            entry.timeout?.cancel()
            entry.continuation.finish(throwing: MuseActivityError.disconnected)
        }
        pending.removeAll()
    }

    private func issue(_ route: String, persistent: Bool, body: Data? = nil,
                       deadline: Duration = .seconds(12)) throws -> AsyncThrowingStream<Data, Error> {
        guard !closed else { throw MuseActivityError.disconnected }
        let encoded = try body.map { try wire.post(route, body: $0) } ?? wire.request(route)
        guard let streamID: UInt64 = encoded.integer(at: 0) else { throw MuseActivityError.invalidFrame }
        let (stream, continuation) = AsyncThrowingStream<Data, Error>.makeStream(bufferingPolicy: .bufferingOldest(128))
        let timeout = Task { [weak self] in
            do { try await Task.sleep(for: deadline) } catch { return }
            self?.finish(streamID, error: MuseActivityError.timeout)
        }
        pending[streamID] = Pending(continuation: continuation, persistent: persistent, timeout: timeout)
        continuation.onTermination = { [weak self] _ in
            Task { @MainActor in self?.finish(streamID) }
        }
        var offset = 8
        while offset < encoded.count {
            guard let length: UInt32 = encoded.integer(at: offset), length > 0, encoded.count - offset - 4 >= Int(length) else {
                close()
                throw MuseActivityError.invalidFrame
            }
            offset += 4
            let frame = encoded.subdata(in: offset..<(offset + Int(length)))
            guard case .enqueued = outbound.yield(frame) else {
                close()
                throw MuseActivityError.disconnected
            }
            offset += Int(length)
        }
        return stream
    }

    private func dispatch(_ bytes: Data) throws {
        let frame = try wire.receive(bytes)
        if frame.isEmpty { return }
        guard frame.count >= 12, let stream: UInt64 = frame.integer(at: 1),
              let status: UInt16 = frame.integer(at: 9) else { throw MuseActivityError.invalidFrame }
        guard var entry = pending[stream] else { return }
        switch frame[0] {
        case 1:
            guard status == 0 || (200..<300).contains(status) else {
                finish(stream, error: [401, 403].contains(status) ? MuseActivityError.signIn : MuseActivityError.rejected(Int(status)))
                return
            }
            if entry.persistent {
                entry.timeout?.cancel()
                entry.timeout = nil
                pending[stream] = entry
            }
        case 2: break
        default:
            finish(stream, error: MuseActivityError.disconnected)
            return
        }
        if frame.count > 12, case .dropped = entry.continuation.yield(Data(frame.dropFirst(12))) {
            finish(stream, error: MuseActivityError.disconnected)
        }
        if frame[11] == 1 { finish(stream) }
    }

    func call(_ route: String, approvalDecision: Data? = nil) async throws -> Data {
        var result = Data()
        let stream = try issue(route, persistent: false, body: approvalDecision)
        for try await bytes in stream {
            guard result.count + bytes.count <= 4_194_304 else { throw MuseActivityError.invalidFrame }
            result.append(bytes)
        }
        return result
    }

    func createThread() async throws -> String {
        let result = try MuseApproval.object(await call("/session/start"))
        let data = result["data"] as? [String: Any] ?? result
        let scope = (data["session"] ?? data["thread"]) as? [String: Any] ?? data
        let raw = ["session_id", "sessionId", "thread_id", "threadId", "id"].compactMap { scope[$0] as? String }.first
        guard let raw, let id = UUID(uuidString: raw) else { throw MuseActivityError.invalidFrame }
        let listed = try MuseReply.sessions(from: await call("/api/session/list"))
        guard listed.contains(where: { UUID(uuidString: $0.id) == id }) else { throw MuseActivityError.invalidFrame }
        return id.uuidString.lowercased()
    }

    func threadHistory(_ thread: String) async throws -> Data {
        guard let id = UUID(uuidString: thread) else { throw MuseActivityError.invalidFrame }
        return try await call("/chat/history?limit=50&transcript_mode=messages&session_id=" + id.uuidString.lowercased())
    }

    func sendNew(_ text: String, thread: String, key: UUID) async throws -> MuseSendAck {
        let body = try JSONSerialization.data(withJSONObject: [
            "items": [["type": "text", "text": text]],
            "node_id": "", "session_id": thread, "new_conversation": true,
            "idempotencyKey": key.uuidString.lowercased(), "clientMessageId": key.uuidString.lowercased(),
            "capabilities": ["hotel_results", "chat_cancel", "delta_stream", "custom_reactions",
                             "custom_reactions_facebook_thumbs_up_v1"]
        ])
        let stream = try issue("/chat/stream", persistent: false, body: body, deadline: .seconds(30))
        var buffer = Data()
        for try await chunk in stream {
            guard buffer.count + chunk.count <= 1_048_576 else { throw MuseActivityError.invalidFrame }
            buffer.append(chunk)
            while let newline = buffer.firstIndex(of: 10) {
                let line = Data(buffer.prefix(upTo: newline))
                buffer.removeSubrange(...newline)
                if let ack = try Self.parseAck(line) { return ack }
            }
        }
        if let ack = try Self.parseAck(buffer) { return ack }
        throw MuseActivityError.disconnected
    }

    private static func parseAck(_ line: Data) throws -> MuseSendAck? {
        guard !line.isEmpty else { return nil }
        let body = try MuseWire.ack(line)
        guard !body.isEmpty else { return nil }
        return try JSONDecoder().decode(MuseSendAck.self, from: body)
    }

    private func finish(_ id: UInt64, error: Error? = nil) {
        guard let entry = pending.removeValue(forKey: id) else { return }
        entry.timeout?.cancel()
        entry.continuation.finish(throwing: error)
    }

    private static func gatewayURL(control: URLSession, cookie: String) async throws -> URL {
        _ = try? await controlCall("/api/hatch/vm/wake", body: ["retry_count": 0], session: control, cookie: cookie)
        var lease: [String: Any]?
        for kind in ["standard", "hatch_vm"] {
            do {
                let row = try await controlCall("/api/hatch/lease-vm", body: ["vmType": kind], session: control, cookie: cookie)
                if row["status"] as? String == "assigned" {
                    lease = row
                    break
                }
            } catch MuseActivityError.signIn {
                throw MuseActivityError.signIn
            } catch {}
        }
        if lease == nil {
            let existing = try await controlCall("/api/session?vm_state=live", body: nil, session: control, cookie: cookie)
            lease = ["vmId": existing["vm_id"] ?? "", "vmName": existing["vm_id"] ?? "",
                     "gatewayUrl": existing["endpoint_url"] ?? ""]
        }
        guard let lease, let address = lease["gatewayUrl"] as? String,
              let endpoint = URLComponents(string: address), ["wss", "https"].contains(endpoint.scheme ?? ""),
              endpoint.user == nil, endpoint.password == nil, endpoint.query == nil, endpoint.fragment == nil,
              endpoint.port == nil || endpoint.port == 443, let host = endpoint.host,
              host.hasSuffix(".metaaivm.com"), host != ".metaaivm.com" else { throw MuseActivityError.unavailable }
        let vmID = lease["vmId"] as? String ?? String(host.dropLast(".metaaivm.com".count))
        let vmName = (lease["vmName"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? vmID
        guard !vmID.isEmpty, vmID.count <= 96 else { throw MuseActivityError.unavailable }
        let response = try await controlCall("/api/hatch/token", body: ["vmAddress": address, "vmName": vmName],
                                             session: control, cookie: cookie)
        guard let token = response["token"] as? String, !token.isEmpty, token.count <= 16_384 else {
            throw MuseActivityError.unavailable
        }
        var notary = response["notary_token"] as? String
        if notary?.isEmpty != false {
            let result = try? await controlCall("/api/hatch/noise-notary-token", body: ["vmName": vmName],
                                               session: control, cookie: cookie)
            notary = (result?["notaryToken"] ?? result?["notary_token"]) as? String
        }
        var components = URLComponents(string: "wss://hatch.metaaivm.com/v1/noise")!
        components.queryItems = [.init(name: "vm_id", value: vmID), .init(name: "auth_token", value: token),
                                 .init(name: "app_id", value: "hatch-web"), .init(name: "request_id", value: UUID().uuidString)]
        if let notary, !notary.isEmpty, notary.count <= 16_384, !notary.hasPrefix("delegation.") {
            components.queryItems?.append(.init(name: "notary_token", value: notary))
        }
        guard let url = components.url else { throw MuseActivityError.unavailable }
        return url
    }

    private static func controlCall(_ path: String, body: [String: Any]?, session: URLSession,
                                    cookie: String) async throws -> [String: Any] {
        guard let url = URL(string: "https://muse.ai" + path), url.host == "muse.ai" else { throw MuseActivityError.unavailable }
        var request = URLRequest(url: url, timeoutInterval: 8)
        request.setValue(cookie, forHTTPHeaderField: "Cookie")
        request.setValue("https://muse.ai", forHTTPHeaderField: "Origin")
        request.setValue("https://muse.ai/", forHTTPHeaderField: "Referer")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body {
            request.httpMethod = "POST"
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse else { throw MuseActivityError.unavailable }
        if [401, 403].contains(response.statusCode) { throw MuseActivityError.signIn }
        guard (200..<300).contains(response.statusCode), data.count <= 1_048_576,
              let result = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw MuseActivityError.unavailable
        }
        return result
    }
}

private extension Data {
    func integer<T: FixedWidthInteger>(at offset: Int) -> T? {
        guard offset >= 0, count - offset >= MemoryLayout<T>.size else { return nil }
        return withUnsafeBytes { T(littleEndian: $0.loadUnaligned(fromByteOffset: offset, as: T.self)) }
    }
}
