import Foundation
import Network
import Security
import WebKit

enum T2Keychain {
    private static func query(_ account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: (Bundle.main.bundleIdentifier ?? "DiehaoNative") + ".t2",
            kSecAttrAccount as String: account,
        ]
    }

    static func read(_ account: String) throws -> Data? {
        var query = query(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else { throw KeyError.status(status) }
        return data
    }

    static func save(_ data: Data, as account: String) throws {
        var attributes = query(account)
        attributes[kSecValueData as String] = data
        attributes[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let status = SecItemAdd(attributes as CFDictionary, nil)
        guard status == errSecSuccess else { throw KeyError.status(status) }
    }

    static func delete(_ account: String) throws {
        let status = SecItemDelete(query(account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw KeyError.status(status) }
    }

    private enum KeyError: LocalizedError {
        case status(OSStatus)

        var errorDescription: String? {
            switch self {
            case .status(let status): "钥匙串操作失败（\(status)）。"
            }
        }
    }
}

struct AccountProxySettings: Codable {
    enum Scheme: String, CaseIterable, Identifiable, Codable {
        case http, https, socks5

        var id: Self { self }
    }

    let scheme: Scheme
    let host: String
    let port: UInt16
    let username: String
    let credentialID: String?

    init(scheme: Scheme, host: String, port: String, username: String) throws {
        let host = host.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let port = UInt16(port), port > 0, !host.isEmpty,
              host.utf8.count <= 253,
              host.utf8.allSatisfy({ (45...46).contains($0) || (48...58).contains($0) ||
                  (65...90).contains($0) || (97...122).contains($0) }) else {
            throw RegistryError.unavailableProxyCredential
        }
        self.scheme = scheme
        self.host = host
        self.port = port
        self.username = username
        credentialID = username.isEmpty ? nil : UUID().uuidString
    }

    func configuration(password supplied: String? = nil) throws -> ProxyConfiguration {
        guard let port = NWEndpoint.Port(rawValue: port), port.rawValue > 0 else {
            throw RegistryError.unavailableProxyCredential
        }
        let endpoint = NWEndpoint.hostPort(host: NWEndpoint.Host(host), port: port)
        var proxy: ProxyConfiguration
        switch scheme {
        case .http: proxy = ProxyConfiguration(httpCONNECTProxy: endpoint, tlsOptions: nil)
        case .https: proxy = ProxyConfiguration(httpCONNECTProxy: endpoint, tlsOptions: NWProtocolTLS.Options())
        case .socks5: proxy = ProxyConfiguration(socksv5Proxy: endpoint)
        }
        proxy.allowFailover = false
        if !username.isEmpty {
            guard let credentialID else { throw RegistryError.unavailableProxyCredential }
            let password: String
            if let supplied {
                password = supplied
            } else {
                guard let data = try T2Keychain.read(credentialID), let decoded = String(data: data, encoding: .utf8) else {
                    throw RegistryError.unavailableProxyCredential
                }
                password = decoded
            }
            proxy.applyCredential(username: username, password: password)
        } else if supplied?.isEmpty == false {
            throw RegistryError.unavailableProxyCredential
        }
        return proxy
    }

    // Layered with WebKit navigation policy. A document-start script is not a
    // claim that every WebKit networking subsystem honors the account proxy.
    @MainActor
    static var restrictions: WKUserScript {
        WKUserScript(source: """
            (() => {
                for (const name of ['WebTransport', 'RTCPeerConnection', 'webkitRTCPeerConnection', 'PublicKeyCredential']) {
                    Object.defineProperty(window, name, {value: undefined, configurable: false});
                }
                for (const name of ['get', 'create']) {
                    if (window.CredentialsContainer) {
                        const original = CredentialsContainer.prototype[name];
                        if (typeof original === 'function') {
                            Object.defineProperty(CredentialsContainer.prototype, name, {
                                value(options) {
                                    if (options?.publicKey) return Promise.reject(new DOMException('WebAuthn unavailable with this proxy', 'NotAllowedError'));
                                    return original.call(this, options);
                                }, configurable: false
                            });
                        }
                    }
                }
                const strip = link => {
                    if (link.relList?.contains('dns-prefetch')) link.remove();
                };
                new MutationObserver(records => {
                    for (const item of records) {
                        if (item.type === 'attributes') strip(item.target);
                        for (const node of item.addedNodes) {
                            if (node.nodeType !== 1) continue;
                            strip(node);
                            node.querySelectorAll('link').forEach(strip);
                        }
                    }
                }).observe(document, {childList: true, subtree: true, attributes: true, attributeFilter: ['rel']});
            })();
            """, injectionTime: .atDocumentStart, forMainFrameOnly: false)
    }
}
