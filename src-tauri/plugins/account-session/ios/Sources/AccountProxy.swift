// AccountProxy.swift — 每账号 WKWebView 代理（#11/#16，补充 W 切片）。
//
// iOS 17 起 WKWebsiteDataStore 支持 proxyConfigurations（Network.ProxyConfiguration）。
// 设计约束：
// - 代理配置按账号（uuid）1:1 保存，切换账号不影响其他账号；
// - 口令不进 UserDefaults：只从系统 Keychain 读取（Rust 侧写入同一
//   service/account），Swift 侧只处理“是否启用 + 主机端口 + 用户名”；
// - allowFailover = false：代理不可用时不得回退直连（fail-closed）；
// - 配置变更后丢弃该账号缓存的 WebView，下次打开按新配置重建。
//
// 真机验收前状态：UNVERIFIED。iOS 编译与真实代理连通性需在 Xcode / 设备上验证。

import Foundation
import Network
import Security
import WebKit

struct AccountProxyConfig: Codable, Equatable {
  /// http | https | socks5 | socks5h
  var scheme: String
  var host: String
  var port: UInt16
  /// 代理用户名；口令在 Keychain（account = "proxy-<uuid>"）。
  var username: String?
  var enabled: Bool

  var isSocks: Bool {
    let s = scheme.lowercased()
    return s == "socks5" || s == "socks5h"
  }
}

enum AccountProxyStore {
  private static let key = "maam.accountProxyConfigs"

  static func all() -> [String: AccountProxyConfig] {
    guard let data = UserDefaults.standard.data(forKey: key) else { return [:] }
    return (try? JSONDecoder().decode([String: AccountProxyConfig].self, from: data)) ?? [:]
  }

  static func config(for uuid: String) -> AccountProxyConfig? {
    all()[uuid]
  }

  static func set(uuid: String, config: AccountProxyConfig?) {
    var map = all()
    if let config {
      map[uuid] = config
    } else {
      map.removeValue(forKey: uuid)
    }
    if let data = try? JSONEncoder().encode(map) {
      UserDefaults.standard.set(data, forKey: key)
    }
  }
}

enum AccountProxyKeychain {
  /// 与 Rust `secrets::SERVICE` 保持一致（Keychain 是系统级，跨进程可见）。
  static let service = "com.harrylarry.multi-agent-account-manager.proxy"

  static func read(account: String) -> String? {
    let query: [String: Any] = [
      kSecClass as String: kSecClassGenericPassword,
      kSecAttrService as String: service,
      kSecAttrAccount as String: account,
      kSecReturnData as String: true,
      kSecMatchLimit as String: kSecMatchLimitOne,
    ]
    var item: CFTypeRef?
    let status = SecItemCopyMatching(query as CFDictionary, &item)
    guard status == errSecSuccess, let data = item as? Data else { return nil }
    return String(data: data, encoding: .utf8)
  }
}

enum AccountProxyApplier {
  /// 把账号代理应用到指定 DataStore。返回是否成功应用（无配置视为成功）。
  /// iOS < 17 没有 proxyConfigurations，返回 false 由调用方决定是否阻断。
  @discardableResult
  static func apply(cfg: AccountProxyConfig, uuid: String, to store: WKWebsiteDataStore) -> Bool {
    guard cfg.enabled else { return true }
    guard #available(iOS 17.0, *) else { return false }
    guard let port = NWEndpoint.Port(rawValue: cfg.port) else { return false }
    let endpoint = NWEndpoint.hostPort(host: NWEndpoint.Host(cfg.host), port: port)
    let proxy: ProxyConfiguration
    if cfg.isSocks {
      proxy = ProxyConfiguration(socksProxy: endpoint)
    } else {
      proxy = ProxyConfiguration(httpCONNECTProxy: endpoint)
    }
    // fail-closed：代理不可用时绝不回退直连。
    proxy.allowFailover = false
    if let user = cfg.username, !user.isEmpty,
       let pass = AccountProxyKeychain.read(account: "proxy-\(uuid)") {
      proxy.applyCredential(username: user, password: pass)
    }
    store.proxyConfigurations = [proxy]
    return true
  }
}
