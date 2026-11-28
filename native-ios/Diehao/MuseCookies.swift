import Argon2
import CryptoKit
import Foundation
import Security
import WebKit

enum CookieError: LocalizedError {
    case invalidFile, missingDomain, invalidEmail, credentials, verification, wrongIdentity, rollback
    case sessionUnconfirmed, identityUnavailable, verificationTimeout, writeVerification

    var errorDescription: String? {
        switch self {
        case .invalidFile: "Cookie 文件损坏、格式不支持或口令错误。"
        case .missingDomain: "需要 muse.ai 和 meta.com 两域的有效 Cookie。"
        case .invalidEmail: "请输入要核验的 Muse 账号邮箱。"
        case .credentials: "请输入至少 12 个字符的导出文件口令。"
        case .verification: "无法验证 Cookie 或登录状态。请重新登录后再试。"
        case .sessionUnconfirmed: "Muse 未确认这个资料的登录态。请先在同一账号资料打开 Muse 并确认已登录。"
        case .identityUnavailable: "Muse 已确认登录，但未提供可与当前账号绑定的邮箱。无法安全完成验证。"
        case .verificationTimeout: "验证 Muse 登录状态时超时。请检查当前账号的网络或代理后重试。"
        case .writeVerification: "WebKit Cookie 写入回读与预期不一致。"
        case .wrongIdentity: "实际登录邮箱与预期不一致。"
        case .rollback: "导入失败，无法完整恢复之前的 Cookie。此账号已暂停使用。"
        }
    }
}

struct MuseCookie: Codable, Sendable {
    let name: String
    let value: String
    let domain: String
    let path: String
    var secure = false
    var httpOnly = false
    var hostOnly: Bool?
    var session: Bool?
    var sameSite: String?
    var expirationDate: Double?

    init(_ cookie: HTTPCookie) {
        name = cookie.name
        value = cookie.value
        domain = cookie.domain
        path = cookie.path
        secure = cookie.isSecure
        httpOnly = cookie.isHTTPOnly
        hostOnly = !cookie.domain.hasPrefix(".")
        session = cookie.isSessionOnly
        sameSite = cookie.properties?[.sameSitePolicy] as? String
        expirationDate = cookie.expiresDate?.timeIntervalSince1970
    }

    init(name: String, value: String, domain: String, path: String, secure: Bool,
         httpOnly: Bool, hostOnly: Bool, expirationDate: Double?) {
        self.name = name
        self.value = value
        self.domain = domain
        self.path = path
        self.secure = secure
        self.httpOnly = httpOnly
        self.hostOnly = hostOnly
        self.expirationDate = expirationDate
        session = expirationDate == nil
    }

    var family: String? {
        let host = domain.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "."))
        return ["muse.ai", "meta.com"].first { host == $0 || host.hasSuffix("." + $0) }
    }

    var key: String { "\(domain.lowercased())\t\(path)\t\(name)" }

    func native() throws -> HTTPCookie {
        let host = domain.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "."))
        guard family != nil, !name.isEmpty, name.rangeOfCharacter(from: .controlCharacters) == nil,
              value.rangeOfCharacter(from: .controlCharacters) == nil, path.hasPrefix("/"),
              host.utf8.allSatisfy({ (45...46).contains($0) || (48...57).contains($0) || (97...122).contains($0) }),
              sameSite?.lowercased() != "none" || secure,
              !name.hasPrefix("__Secure-") || secure,
              !name.hasPrefix("__Host-") || (secure && path == "/" && hostOnly != false && !domain.hasPrefix(".")),
              expirationDate.map({ $0.isFinite && $0 > Date().timeIntervalSince1970 }) ?? true else {
            throw CookieError.invalidFile
        }
        let scopedDomain = hostOnly == false || (hostOnly == nil && domain.hasPrefix(".")) ? "." + host : host
        var properties: [HTTPCookiePropertyKey: Any] = [
            .name: name, .value: value, .domain: scopedDomain, .path: path,
        ]
        if secure { properties[.secure] = "TRUE" }
        if httpOnly { properties[HTTPCookiePropertyKey("HttpOnly")] = "TRUE" }
        if let expirationDate, session != true { properties[.expires] = Date(timeIntervalSince1970: expirationDate) }
        if let sameSite {
            switch sameSite.lowercased() {
            case "strict": properties[.sameSitePolicy] = "Strict"
            case "lax": properties[.sameSitePolicy] = "Lax"
            case "none", "no_restriction": properties[.sameSitePolicy] = "None"
            default: break
            }
        }
        guard let cookie = HTTPCookie(properties: properties) else { throw CookieError.invalidFile }
        return cookie
    }

    static func read(_ data: Data) throws -> [Self] {
        guard data.count <= 4_000_000 else { throw CookieError.invalidFile }
        if let cookies = try? JSONDecoder().decode([Self].self, from: data) { return cookies }
        guard let text = String(data: data, encoding: .utf8) else { throw CookieError.invalidFile }
        if text.contains("Cookie:") && !text.contains("\t") {
            // A bare Cookie header carries no scope. Require the user to
            // explicitly attribute each line to one of the supported origins.
            return try text.split(whereSeparator: \.isNewline).flatMap { line in
                let pieces = line.split(separator: " ", maxSplits: 2)
                guard pieces.count == 3, ["muse.ai", "meta.com"].contains(String(pieces[0])),
                      pieces[1].lowercased() == "cookie:" else { throw CookieError.invalidFile }
                return try pieces[2].split(separator: ";").map { pair in
                    let split = pair.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
                    guard split.count == 2 else { throw CookieError.invalidFile }
                    return Self(name: split[0].trimmingCharacters(in: .whitespaces), value: String(split[1]),
                                domain: String(pieces[0]), path: "/", secure: true, httpOnly: false,
                                hostOnly: true, expirationDate: nil)
                }
            }
        }
        var cookies: [Self] = []
        for line in text.split(whereSeparator: \.isNewline) {
            let isHTTPOnly = line.hasPrefix("#HttpOnly_")
            if line.hasPrefix("#") && !isHTTPOnly { continue }
            let parts = line.split(separator: "\t", omittingEmptySubsequences: false)
            guard parts.count == 7, let expiry = Double(parts[4]) else { throw CookieError.invalidFile }
            let domain = isHTTPOnly ? String(parts[0].dropFirst("#HttpOnly_".count)) : String(parts[0])
            cookies.append(Self(name: String(parts[5]), value: String(parts[6]), domain: domain,
                path: String(parts[2]), secure: parts[3].uppercased() == "TRUE", httpOnly: isHTTPOnly,
                hostOnly: parts[1].uppercased() != "TRUE", expirationDate: expiry == 0 ? nil : expiry))
        }
        return cookies
    }
}

struct CookiePreview {
    let cookies: [MuseCookie]
    let notes: [String]

    init(_ records: [MuseCookie]) throws {
        var keys = Set<String>()
        var valid: [MuseCookie] = []
        var warnings: [String] = []
        guard records.count <= 2_000 else { throw CookieError.invalidFile }
        for (index, record) in records.enumerated() {
            guard record.family != nil else {
                warnings.append("第 \(index + 1) 条跳过：域名超出范围。")
                continue
            }
            guard (try? record.native()) != nil else {
                warnings.append("第 \(index + 1) 条跳过：属性、前缀或有效期无效。")
                continue
            }
            guard keys.insert(MuseCookie(try record.native()).key).inserted else {
                throw CookieError.invalidFile
            }
            valid.append(record)
        }
        guard Set(valid.compactMap(\.family)) == ["muse.ai", "meta.com"] else { throw CookieError.missingDomain }
        cookies = valid
        notes = warnings
    }

    var counts: String {
        "muse.ai \(cookies.filter { $0.family == "muse.ai" }.count) 条 · meta.com \(cookies.filter { $0.family == "meta.com" }.count) 条"
    }
}

enum CookieArchive {
    private struct Envelope: Codable {
        let format: String
        let salt: Data
        let ciphertext: Data
    }

    private static let format = "diehao-cookie-v1-argon2id-65536-3-1-aes256gcm"
    private static let aad = Data(format.utf8)

    static func seal(_ cookies: [MuseCookie], password: String) async throws -> Data {
        guard password.count >= 12 else { throw CookieError.credentials }
        let plaintext = try JSONEncoder().encode(cookies)
        return try await Task.detached(priority: .userInitiated) {
            var salt = Data(count: 32)
            let result = salt.withUnsafeMutableBytes {
                SecRandomCopyBytes(kSecRandomDefault, $0.count, $0.baseAddress!)
            }
            guard result == errSecSuccess else { throw CookieError.invalidFile }
            let key = try derive(password, salt: salt)
            guard let sealed = try AES.GCM.seal(plaintext, using: key, authenticating: aad).combined else {
                throw CookieError.invalidFile
            }
            return try JSONEncoder().encode(Envelope(format: format, salt: salt, ciphertext: sealed))
        }.value
    }

    static func open(_ data: Data, password: String) async throws -> [MuseCookie] {
        guard data.count <= 6_000_000 else { throw CookieError.invalidFile }
        return try await Task.detached(priority: .userInitiated) {
            guard let envelope = try? JSONDecoder().decode(Envelope.self, from: data) else {
                return try MuseCookie.read(data)
            }
            guard envelope.format == format, envelope.salt.count == 32 else { throw CookieError.invalidFile }
            let key = try derive(password, salt: envelope.salt)
            guard let box = try? AES.GCM.SealedBox(combined: envelope.ciphertext),
                  let opened = try? AES.GCM.open(box, using: key, authenticating: aad) else {
                throw CookieError.invalidFile
            }
            return try MuseCookie.read(opened)
        }.value
    }

    private static func derive(_ password: String, salt: Data) throws -> SymmetricKey {
        let output = try Argon2.hash(password: Data(password.utf8), salt: salt, iterations: 3,
                                    memory: 65_536, parallelism: 1, outputLength: 32, variant: .id)
        return SymmetricKey(data: output)
    }
}
