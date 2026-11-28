import CryptoKit
import Foundation
import Security

enum SwiftNoiseError: Error { case invalidMessage, invalidState, randomness }

private struct SwiftNoiseCipher {
    private var key: SymmetricKey?
    private var counter: UInt64 = 0

    init(key: SymmetricKey? = nil) { self.key = key }

    private mutating func nextNonce() throws -> AES.GCM.Nonce {
        guard counter < .max else { throw SwiftNoiseError.invalidState }
        var bigEndian = counter.bigEndian
        let bytes = Data(repeating: 0, count: 4) + withUnsafeBytes(of: &bigEndian) { Data($0) }
        counter += 1
        return try AES.GCM.Nonce(data: bytes)
    }

    mutating func encrypt(_ plaintext: Data, authenticating hash: Data) throws -> Data {
        guard let key else { return plaintext }
        let nonce = try nextNonce()
        let box = try AES.GCM.seal(plaintext, using: key, nonce: nonce, authenticating: hash)
        return box.ciphertext + box.tag
    }

    mutating func decrypt(_ ciphertext: Data, authenticating hash: Data) throws -> Data {
        guard let key else { return ciphertext }
        guard ciphertext.count >= 16 else { throw SwiftNoiseError.invalidMessage }
        let nonce = try nextNonce()
        let box = try AES.GCM.SealedBox(nonce: nonce, ciphertext: ciphertext.dropLast(16), tag: ciphertext.suffix(16))
        return try AES.GCM.open(box, using: key, authenticating: hash)
    }
}

/// Noise_XX_25519_AESGCM_SHA256 initiator. One instance owns both counters for one socket.
final class SwiftNoiseXX {
    static let preference = "muse.swiftNoise"
    private static let maxMessage = 65_535
    private typealias PrivateKey = Curve25519.KeyAgreement.PrivateKey
    private typealias PublicKey = Curve25519.KeyAgreement.PublicKey

    private enum Phase {
        case initial, waitingForResponder, readyForFinal(PublicKey), transport, closed
    }

    private var phase = Phase.initial
    private var ephemeral: PrivateKey?
    private var identity: PrivateKey?
    private var chainingKey: SymmetricKey
    private(set) var handshakeHash: Data
    private var handshakeCipher = SwiftNoiseCipher()
    private var send = SwiftNoiseCipher()
    private var receive = SwiftNoiseCipher()

    convenience init() { self.init(ephemeral: PrivateKey(), identity: PrivateKey(), prologue: Data()) }

    private init(ephemeral: PrivateKey, identity: PrivateKey, prologue: Data) {
        self.ephemeral = ephemeral
        self.identity = identity
        let name = Data("Noise_XX_25519_AESGCM_SHA256".utf8)
        let initial = name + Data(repeating: 0, count: 32 - name.count)
        self.chainingKey = SymmetricKey(data: initial)
        self.handshakeHash = Data(SHA256.hash(data: initial + prologue))
    }

    func start() throws -> Data {
        var bytes = Data(repeating: 0, count: 32)
        let result = bytes.withUnsafeMutableBytes { buffer -> OSStatus in
            guard let address = buffer.baseAddress else { return errSecParam }
            return SecRandomCopyBytes(kSecRandomDefault, buffer.count, address)
        }
        guard result == errSecSuccess else { throw SwiftNoiseError.randomness }
        return try writeFirst(Data([0x0a, 0x20]) + bytes)
    }

    func finish(_ response: Data, reject: (Data) -> Bool) throws -> Data {
        let payload = try readSecond(response)
        guard !reject(payload) else {
            phase = .closed
            throw SwiftNoiseError.invalidMessage
        }
        return try writeFinal(Data())
    }

    func seal(_ plaintext: Data) throws -> Data {
        guard case .transport = phase else { throw SwiftNoiseError.invalidState }
        do {
            guard plaintext.count <= Self.maxMessage - 16 else { throw SwiftNoiseError.invalidMessage }
            return try send.encrypt(plaintext, authenticating: Data())
        } catch {
            phase = .closed
            throw error
        }
    }

    func open(_ ciphertext: Data) throws -> Data {
        guard case .transport = phase else { throw SwiftNoiseError.invalidState }
        do {
            guard ciphertext.count <= Self.maxMessage else { throw SwiftNoiseError.invalidMessage }
            return try receive.decrypt(ciphertext, authenticating: Data())
        } catch {
            phase = .closed
            throw error
        }
    }

    private func writeFirst(_ payload: Data) throws -> Data {
        guard case .initial = phase, let ephemeral else { throw SwiftNoiseError.invalidState }
        phase = .closed
        guard payload.count <= Self.maxMessage - 32 else { throw SwiftNoiseError.invalidMessage }
        let key = ephemeral.publicKey.rawRepresentation
        mixHash(key)
        let frame = key + (try encryptAndHash(payload))
        phase = .waitingForResponder
        return frame
    }

    private func readSecond(_ message: Data) throws -> Data {
        guard case .waitingForResponder = phase, let ephemeral else { throw SwiftNoiseError.invalidState }
        phase = .closed
        guard (96...Self.maxMessage).contains(message.count) else { throw SwiftNoiseError.invalidMessage }
        let remoteEphemeral = try PublicKey(rawRepresentation: message.prefix(32))
        mixHash(remoteEphemeral.rawRepresentation)
        try mixKey(ephemeral.sharedSecretFromKeyAgreement(with: remoteEphemeral))
        let remoteStatic = try PublicKey(rawRepresentation: decryptAndHash(Data(message.dropFirst(32).prefix(48))))
        try mixKey(ephemeral.sharedSecretFromKeyAgreement(with: remoteStatic))
        let payload = try decryptAndHash(Data(message.dropFirst(80)))
        phase = .readyForFinal(remoteEphemeral)
        return payload
    }

    private func writeFinal(_ payload: Data) throws -> Data {
        guard case .readyForFinal(let remoteEphemeral) = phase, let identity else {
            throw SwiftNoiseError.invalidState
        }
        phase = .closed
        guard payload.count <= Self.maxMessage - 64 else { throw SwiftNoiseError.invalidMessage }
        let publicKey = try encryptAndHash(identity.publicKey.rawRepresentation)
        try mixKey(identity.sharedSecretFromKeyAgreement(with: remoteEphemeral))
        let frame = publicKey + (try encryptAndHash(payload))
        let (sendKey, receiveKey) = deriveKeys(Data())
        send = SwiftNoiseCipher(key: sendKey)
        receive = SwiftNoiseCipher(key: receiveKey)
        handshakeCipher = SwiftNoiseCipher()
        chainingKey = SymmetricKey(data: Data(repeating: 0, count: 32))
        ephemeral = nil
        self.identity = nil
        phase = .transport
        return frame
    }

    private func mixHash(_ bytes: Data) {
        handshakeHash = Data(SHA256.hash(data: handshakeHash + bytes))
    }

    private func mixKey(_ secret: SharedSecret) {
        let (chain, key) = deriveKeys(secret.withUnsafeBytes { Data($0) })
        chainingKey = chain
        handshakeCipher = SwiftNoiseCipher(key: key)
    }

    private func deriveKeys(_ input: Data) -> (SymmetricKey, SymmetricKey) {
        let extracted = SymmetricKey(data: Data(HMAC<SHA256>.authenticationCode(for: input, using: chainingKey)))
        let first = Data(HMAC<SHA256>.authenticationCode(for: Data([1]), using: extracted))
        let second = Data(HMAC<SHA256>.authenticationCode(for: first + Data([2]), using: extracted))
        return (SymmetricKey(data: first), SymmetricKey(data: second))
    }

    private func encryptAndHash(_ bytes: Data) throws -> Data {
        let encrypted = try handshakeCipher.encrypt(bytes, authenticating: handshakeHash)
        mixHash(encrypted)
        return encrypted
    }

    private func decryptAndHash(_ bytes: Data) throws -> Data {
        let decrypted = try handshakeCipher.decrypt(bytes, authenticating: handshakeHash)
        mixHash(bytes)
        return decrypted
    }
}

#if DEBUG && targetEnvironment(simulator) && DIEHAO_NOISE_TESTS
extension SwiftNoiseXX {
    private struct FixtureFailure: Error {
        let line: UInt
    }

    static func verifyFixtures() throws {
        func hex(_ value: String) -> Data {
            var bytes = Data()
            var start = value.startIndex
            while start < value.endIndex {
                let end = value.index(start, offsetBy: 2)
                bytes.append(UInt8(value[start..<end], radix: 16)!)
                start = end
            }
            return bytes
        }
        func require(_ condition: Bool, line: UInt = #line) throws {
            guard condition else { throw FixtureFailure(line: line) }
        }
        func mustReject(line: UInt = #line, _ operation: () throws -> Data) throws {
            do { _ = try operation() } catch { return }
            throw FixtureFailure(line: line)
        }

        let noise = SwiftNoiseXX(
            ephemeral: try PrivateKey(rawRepresentation: hex("893e28b9dc6ca8d611ab664754b8ceb7bac5117349a4439a6b0569da977c464a")),
            identity: try PrivateKey(rawRepresentation: hex("e61ef9919cde45dd5f82166404bd08e38bceb5dfdfded0a34c8df7ed542214d1")),
            prologue: Data("John Galt".utf8)
        )
        let response = hex("95ebc60d2b1fa672c1f46a8aa265ef51bfe38e7ccb39ec5be34069f144808843757117acceb05bd7a45733bc22015c97a9d0cbaf41b80446d5988ff5127235d76b79eade70f473d6a4ef521fdcbeda5340d01e028ba793fc059f2724a83af05f12dda0448a7621a926b379a92477fd")
        try require(noise.writeFirst(Data("Ludwig von Mises".utf8)) ==
            hex("ca35def5ae56cec33dc2036731ab14896bc4c75dbb07a61f879f8e3afa4c79444c756477696720766f6e204d69736573"))
        try require(noise.readSecond(response) == Data("Murray Rothbard".utf8))
        try require(noise.writeFinal(Data("F. A. Hayek".utf8)) ==
            hex("c90f1cf77eba4e50edb038991565e36c9758943a989229b6051244dc4fbecb6946744b401af2ee1a5881b65fbb87fd07cb6a328ececc9ce6ce84c399dc332d4fd521fa4bb7f467ce909395"))
        try require(noise.handshakeHash == hex("1b7aefb1125762aa21a252890d00af54519638b76437444538f9a52f21e2e0dc"))
        try require(noise.open(hex("bc3fa77f6aca3e8466d7dc6bea10013e88a6a29add5132b461806c")) == Data("Carl Menger".utf8))
        try require(noise.seal(Data("Jean-Baptiste Say".utf8)) == hex("250b01074cdfe0df2ecf8ccbf1737b15a2ddb5b52fd9a396604e9c793cee3b3bb9"))
        let next = hex("449d4d433b3cdc3d02bf6fc881774b9df54366ebcffb9689bb13f14709822cd7ef42bcdb4d")
        try require(noise.open(next) == hex("457567656e2042f6686d20766f6e2042617765726b"))
        try mustReject { try noise.open(next) }
        try mustReject { try noise.seal(Data("closed".utf8)) }

        let wrongOrder = SwiftNoiseXX()
        try mustReject { try wrongOrder.seal(Data("before handshake".utf8)) }
        _ = try wrongOrder.start()
        try mustReject { try wrongOrder.finish(Data(repeating: 0, count: 95)) { _ in false } }
        try mustReject { try wrongOrder.finish(response) { _ in false } }

        guard let rust = diehao_noise_test_new() else { throw SwiftNoiseError.invalidState }
        defer { diehao_noise_test_free(rust) }
        func exchange(_ data: Data) throws -> Data {
            let result = data.withUnsafeBytes {
                diehao_noise_test_step(rust, $0.bindMemory(to: UInt8.self).baseAddress, $0.count)
            }
            guard let result else { throw SwiftNoiseError.invalidMessage }
            defer { diehao_wire_blob_free(result) }
            var size = 0
            guard let bytes = diehao_wire_blob_read(result, &size) else { throw SwiftNoiseError.invalidMessage }
            return Data(bytes: bytes, count: size)
        }

        let client = SwiftNoiseXX()
        let rustResponse = try exchange(client.start())
        let clientFinal = try client.finish(rustResponse) { $0 != Data("Rust responder".utf8) }
        try require(exchange(clientFinal) == client.handshakeHash)
        let request = Data("Swift to Rust".utf8)
        for _ in 0..<2 {
            let echoed = try exchange(client.seal(request))
            try require(client.open(echoed) == request)
        }
    }
}
#endif
