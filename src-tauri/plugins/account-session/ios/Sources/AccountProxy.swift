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
//   「没有配置」——后者会静默走直连，属于绕过代理；
// - **所有** WebView（可见会话 / 缓存页 / 隐藏额度同步）都必须经
//   `AccountProxyResolver` 解析出口，不允许各自读 store 后「没读到就当直连」。
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

  /// 直接取整张表。**只给写入路径与诊断用**：读的时候必须走
  /// `AccountProxyResolver`，否则 `corrupt` 会被当成「空表 = 没代理」，
  /// 于是静默直连（这正是补充 W 要堵的漏洞）。
  static func rawMap() -> [String: AccountProxyConfig]? {
    if case .ok(let map) = load() { return map }
    return nil
  }

  /// 「明确直连」的显式标记。
  ///
  /// 不能用「删掉条目」表示直连：解析器把「没有条目」判为未知态（下发失败 /
  /// 两端未同步）并阻断。两种语义完全不同，必须分开存。
  static let directMarker = AccountProxyConfig(
    scheme: "http", host: "", port: 0, username: nil, enabled: false)

  /// 写入某账号的出口配置（含「明确直连」）。store 已损坏时用本次写入覆盖它
  /// （Rust 是权威源，下发即修复），不会保留坏数据。
  static func set(uuid: String, config: AccountProxyConfig?) {
    // 损坏时从空表重建：Rust 是权威源，下一次下发会补齐其它账号；
    // 期间其它账号因「没有可信配置」被阻断（fail-closed，见解析器）。
    var map = rawMap() ?? [:]
    if let config, config.enabled {
      map[uuid] = config
    } else {
      map[uuid] = directMarker
    }
    guard let data = try? JSONEncoder().encode(map) else { return }
    UserDefaults.standard.set(data, forKey: key)
  }

  /// 删除某账号的条目（删除账号时用）。删掉后该账号回到「未知」态。
  static func remove(uuid: String) {
    var map = rawMap() ?? [:]
    map.removeValue(forKey: uuid)
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
    guard cfg.enabled else {
      // 「关闭代理」必须把 store 上的旧配置（含隔离态）清掉，否则会一直连不出去。
      if #available(iOS 17.0, *) {
        clear(store: store)
      }
      return .notConfigured
    }
    guard #available(iOS 17.0, *) else { return .unsupported }
    guard let port = NWEndpoint.Port(rawValue: cfg.port) else { return .unsupported }
    let endpoint = NWEndpoint.hostPort(host: NWEndpoint.Host(cfg.host), port: port)

    // `ProxyConfiguration` 是 struct：设置 `allowFailover`、调用
    // `applyCredential` 都是 mutating，必须用 `var`，否则 Swift 直接编译失败。
    var proxy: ProxyConfiguration
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

  /// 清除该 DataStore 的代理配置（也用于解除隔离）。
  @available(iOS 17.0, *)
  static func clear(store: WKWebsiteDataStore) {
    store.proxyConfigurations = []
  }
}

/// 一个账号「该怎么出站」的唯一解析结果。
enum AccountProxyResolution {
  /// 明确没有代理（Rust 下发过 enabled=false 的条目）。
  case direct
  /// 有代理配置，按它应用。
  case use(AccountProxyConfig)
  /// 无法确认出口：必须阻断，不得当作直连。
  case blocked(String)

  var blockReason: String? {
    if case .blocked(let r) = self { return r }
    return nil
  }
}

/// **所有** WebView / 隐藏额度同步的唯一代理解析入口。
///
/// 三种状态必须分开（补充 W 的第三条）：
/// - `.corrupt`：本地配置坏了 → 全部阻断；
/// - 表中**没有这个账号**（含整表尚不存在）：无法证明它是直连 → 阻断。
///   Rust 每次打开会话与每次额度同步前都会先下发该账号的条目（enabled=false
///   也是显式条目），所以「没有条目」只可能是下发失败或还没同步，属未知态；
/// - 有条目：enabled=false → 直连；enabled=true → 按配置应用。
enum AccountProxyResolver {
  static func resolve(uuid: String) -> AccountProxyResolution {
    switch AccountProxyStore.load() {
    case .corrupt:
      return .blocked("本地代理配置损坏，无法确认出口，已按 fail-closed 阻断；请在管理端重新保存代理")
    case .missing:
      return .blocked("尚未收到该账号的出口配置（fail-closed）：请回到账号列表重新打开该账号")
    case .ok(let map):
      guard let cfg = map[uuid] else {
        return .blocked("尚未收到该账号的出口配置（fail-closed）：请回到账号列表重新打开该账号")
      }
      return cfg.enabled ? .use(cfg) : .direct
    }
  }

  /// 把解析结果落到 DataStore 上。返回 nil 表示可用；返回字符串表示阻断原因。
  static func apply(uuid: String, to store: WKWebsiteDataStore) -> String? {
    switch resolve(uuid: uuid) {
    case .blocked(let reason):
      return reason
    case .direct:
      if #available(iOS 17.0, *) {
        AccountProxyApplier.clear(store: store)
      }
      return nil
    case .use(let cfg):
      let result = AccountProxyApplier.apply(cfg: cfg, uuid: uuid, to: store)
      return result.blockReason
    }
  }
}

/// 网络层隔离：把一个账号的 DataStore 出口指向**不可达端点**。
///
/// 为什么需要它：`stopLoading()` / 隐藏 WebView / 取消导航都**不**能证明
/// 已建立的 XHR、WebSocket、EventSource 停了 —— JS 定时器还会继续发请求。
/// WebKit 没有「杀掉这个 data store 全部连接」的 API。本机制提供的是
/// **新连接**层面的约束：把出口换成一个连不通的代理并关掉 failover，之后
/// 该 store 上任何新连接都会在 CONNECT 阶段失败。
///
/// 已建立的连接只能靠销毁 WebView、由系统回收内容进程来断——回收时机不受
/// 我们控制，因此这条路径的真机效果标记为 UNVERIFIED，不以推理代替证据。
enum AccountProxyQuarantine {
  /// 本机回环 + 未监听端口：连接立即被拒，不会有任何字节离开设备。
  private static let deadPort: UInt16 = 9

  /// 返回是否成功把出口隔离。`false` 表示当前系统版本无法隔离，
  /// 调用方必须改用「销毁 WebView」并拒绝重新打开。
  static func engage(uuid: String) -> Bool {
    guard #available(iOS 17.0, *) else { return false }
    guard let identifier = UUID(uuidString: uuid) else { return false }
    guard let port = NWEndpoint.Port(rawValue: deadPort) else { return false }
    let store = WKWebsiteDataStore(forIdentifier: identifier)
    let endpoint = NWEndpoint.hostPort(host: .ipv4(.loopback), port: port)
    var proxy = ProxyConfiguration(httpCONNECTProxy: endpoint, tlsOptions: nil)
    // 不可达时**不得**回退直连——这是隔离的全部意义。
    proxy.allowFailover = false
    store.proxyConfigurations = [proxy]
    return true
  }
}
