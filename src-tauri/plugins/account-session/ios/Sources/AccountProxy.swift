// AccountProxy.swift — 每账号 WKWebView 代理（#11/#16，补充 W 切片）。
//
// iOS 17 起 WKWebsiteDataStore 支持 proxyConfigurations（Network.ProxyConfiguration）。
// 设计约束：
// - 代理配置按账号（uuid）1:1 保存，切换账号不影响其他账号；
// - 口令不进 UserDefaults：只从系统 Keychain 读取（Rust 侧写入同一
//   service/account），Swift 侧只处理“是否启用 + 主机端口 + 用户名”；
// - allowFailover = false：代理不可用时不得回退直连（fail-closed）；
// - 配置变更后丢弃该账号缓存的 WebView，下次打开按新配置重建；
// - **本地配置损坏**（无法解码）视为「所有账号都不可放行」，而不是当作
//   「没有配置」——后者会静默走直连，属于绕过代理。
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

  /// 是否是「到代理的 TLS 连接」。WebKit 的 httpCONNECTProxy 是**明文** CONNECT，
  /// 不能拿来当 https 代理用，否则等于把预期加密的代理链路降级成明文。
  var isHttpsProxy: Bool {
    scheme.lowercased() == "https"
  }
}

/// 本地配置的加载结果。区分「没有」和「坏了」：
/// 后者必须阻断，不能当成「没有代理」直接走直连。
enum AccountProxyLoadState {
  case missing
  case ok([String: AccountProxyConfig])
  case corrupt
}

enum AccountProxyStore {
  private static let key = "maam.accountProxyConfigs"

  static func load() -> AccountProxyLoadState {
    guard let data = UserDefaults.standard.data(forKey: key) else { return .missing }
    guard let map = try? JSONDecoder().decode([String: AccountProxyConfig].self, from: data) else {
      return .corrupt
    }
    return .ok(map)
  }

  static func all() -> [String: AccountProxyConfig] {
    if case .ok(let map) = load() { return map }
    return [:]
  }

  static func config(for uuid: String) -> AccountProxyConfig? {
    all()[uuid]
  }

  /// 写入/删除某账号配置。store 已损坏时用本次写入覆盖它（Rust 是权威源，
  /// 下发即修复），不会保留坏数据。
  static func set(uuid: String, config: AccountProxyConfig?) {
    var map = all()
    if let config {
      map[uuid] = config
    } else {
      map.removeValue(forKey: uuid)
    }
    guard let data = try? JSONEncoder().encode(map) else { return }
    UserDefaults.standard.set(data, forKey: key)
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

/// 应用结果。**不用 Bool**：调用方需要知道「为什么没生效」才能给出正确阻断，
/// 也不能把「需要认证但没有口令」当成「不需要认证」放行。
enum AccountProxyApplyResult {
  case applied
  /// 该账号没有配置：按未启用代理处理（正常走系统网络）。
  case notConfigured
  /// iOS < 17 或端口非法：无法应用。
  case unsupported
  /// 配了用户名但 Keychain 里没有口令：不能发起「无认证」连接。
  case missingCredential
  /// 本地配置存储损坏：无法判断该账号该走什么出口。
  case corrupt

  var isApplied: Bool {
    if case .applied = self { return true }
    return false
  }

  /// 给用户看的阻断原因（不含任何口令/主机细节之外的信息）。
  var blockReason: String? {
    switch self {
    case .applied, .notConfigured:
      return nil
    case .unsupported:
      return "该设备不支持 WebView 代理（需要 iOS 17+），已按 fail-closed 阻断"
    case .missingCredential:
      return "代理认证缺少口令（Keychain 中找不到），已按 fail-closed 阻断"
    case .corrupt:
      return "本地代理配置损坏，无法确认出口，已按 fail-closed 阻断；请在管理端重新保存代理"
    }
  }
}

enum AccountProxyApplier {
  /// 把账号代理应用到指定 DataStore。
  /// `allowFailover = false`：代理不可用时 URLSession/WebKit 不会回退直连。
  static func apply(cfg: AccountProxyConfig, uuid: String, to store: WKWebsiteDataStore)
    -> AccountProxyApplyResult
  {
    guard cfg.enabled else { return .notConfigured }
    guard #available(iOS 17.0, *) else { return .unsupported }
    guard let port = NWEndpoint.Port(rawValue: cfg.port) else { return .unsupported }
    let endpoint = NWEndpoint.hostPort(host: NWEndpoint.Host(cfg.host), port: port)

    let proxy: ProxyConfiguration
    if cfg.isSocks {
      // Network.framework 的 SOCKS 初始化器叫 `socksv5Proxy`（不是 socksProxy）。
      // SOCKS5 的域名地址类型本身就是远端解析，socks5h 不需要单独映射。
      proxy = ProxyConfiguration(socksv5Proxy: endpoint)
    } else if cfg.isHttpsProxy {
      // `https://` 代理 = 到代理先建 TLS，再在 TLS 里做 HTTP CONNECT。
      // 这就是 `httpCONNECTProxy` + `tlsOptions` 的组合；**不能**当成
      // 明文 CONNECT（tlsOptions: nil）处理，否则等于把代理链路降级成明文。
      proxy = ProxyConfiguration(
        httpCONNECTProxy: endpoint, tlsOptions: NWProtocolTLS.Options())
    } else {
      proxy = ProxyConfiguration(httpCONNECTProxy: endpoint, tlsOptions: nil)
    }
    // fail-closed：代理不可用时绝不回退直连。
    proxy.allowFailover = false

    if let user = cfg.username, !user.isEmpty {
      // 有用户名就必须有口令：拿不到口令时不能以「无认证」方式连代理，
      // 那会被代理以 407 拒绝或（更糟）被当成匿名放行，都不是我们要的语义。
      guard let pass = AccountProxyKeychain.read(account: "proxy-\(uuid)") else {
        return .missingCredential
      }
      proxy.applyCredential(username: user, password: pass)
    }
    store.proxyConfigurations = [proxy]
    return .applied
  }
}
