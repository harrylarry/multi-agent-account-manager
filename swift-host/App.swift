import SwiftUI
import WebKit

@main
struct DiehaoManagerApp: App {
    var body: some Scene {
        WindowGroup {
            ManagerScreen().ignoresSafeArea()
        }
    }
}

struct ManagerScreen: UIViewControllerRepresentable {
    func makeUIViewController(context: Context) -> ManagerController { ManagerController() }
    func updateUIViewController(_ controller: ManagerController, context: Context) {}
}

@MainActor
final class ManagerController: UIViewController, WKNavigationDelegate {
    private let glass = ManagerGlassNavigation()
    private var webView: WKWebView?
    private var snapshot: ManagerSnapshot?
    private lazy var bridge = ManagerMessageHandler(controller: self)

    override func viewDidLoad() {
        super.viewDidLoad()
        guard let bundle = Bundle.main.url(forResource: "Assets", withExtension: nil) else {
            showError("缺少原版界面资源。")
            return
        }
        do {
            snapshot = try ManagerSnapshot.load(from: ManagerSnapshot.accountFile)
        } catch {
            showError("无法读取原版账号资料。资料未修改。")
            return
        }
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.setURLSchemeHandler(ManagerAssets(directory: bundle), forURLScheme: ManagerAssets.scheme)
        configuration.userContentController.addScriptMessageHandler(bridge, contentWorld: .page, name: "diehaoManager")
        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.scrollView.contentInsetAdjustmentBehavior = .never
        webView.navigationDelegate = self
        self.webView = webView
        pin(webView)
        pin(glass)
        webView.load(URLRequest(url: ManagerAssets.entry))
    }

    private func pin(_ child: UIView) {
        child.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(child)
        NSLayoutConstraint.activate([
            child.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            child.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            child.topAnchor.constraint(equalTo: view.topAnchor),
            child.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
    }

    private func showError(_ message: String) {
        let label = UILabel()
        label.text = message
        label.textAlignment = .center
        label.numberOfLines = 0
        view.backgroundColor = .systemBackground
        pin(label)
    }

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        decisionHandler(action.targetFrame?.isMainFrame == true && action.request.url == ManagerAssets.entry ? .allow : .cancel)
    }

    fileprivate func request(_ message: WKScriptMessage, reply: (Any?, String?) -> Void) {
        guard message.webView === webView, message.frameInfo.isMainFrame,
              message.frameInfo.request.url == ManagerAssets.entry,
              let data = message.body as? [String: Any],
              let command = data["command"] as? String,
              let args = data["args"] as? [String: Any],
              var snapshot else {
            reply(nil, "无效的管理页请求。")
            return
        }
        switch command {
        case "update_iphone_glass":
            guard let input = args["layout"], JSONSerialization.isValidJSONObject(input),
                  let json = try? JSONSerialization.data(withJSONObject: input),
                  let layout = try? JSONDecoder().decode(ManagerGlassLayout.self, from: json) else {
                reply(nil, "无法读取导航状态。")
                return
            }
            let events: (([String: String]) -> Void)? = (args["events"] as? Bool) == true ? { [weak self] event in
                self?.webView?.callAsyncJavaScript(
                    "globalThis.diehaoManagerNavigation?.(event)", arguments: ["event": event],
                    in: nil, in: .page, completionHandler: nil
                )
            } : nil
            view.layoutIfNeeded()
            glass.apply(layout, events: events)
            reply(["ready": true], nil)
            return
        case "is_spike_build":
            reply(false, nil)
            return
        case "select_provider":
            guard let provider = args["providerId"] as? String, snapshot.providers.contains(provider) else {
                reply(nil, "未知服务。")
                return
            }
            snapshot.selectProvider(provider)
        case "set_modal_active":
            guard let active = args["active"] as? Bool else {
                reply(nil, "无效的弹窗状态。")
                return
            }
            snapshot.setModalActive(active)
        case "get_manager_state":
            break
        default:
            reply(nil, "这个入口的原版服务尚未接入 Swift 宿主。")
            return
        }
        self.snapshot = snapshot
        reply(snapshot.payload, nil)
    }
}

@MainActor
private final class ManagerMessageHandler: NSObject, WKScriptMessageHandlerWithReply {
    weak var controller: ManagerController?

    init(controller: ManagerController) { self.controller = controller }

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage,
                               replyHandler: @escaping (Any?, String?) -> Void) {
        guard let controller else { replyHandler(nil, "管理页已关闭。"); return }
        controller.request(message, reply: replyHandler)
    }
}

private final class ManagerAssets: NSObject, WKURLSchemeHandler {
    static let scheme = "diehao-manager"
    static let entry = URL(string: "\(scheme)://local/index.html")!
    private let directory: URL

    init(directory: URL) { self.directory = directory.resolvingSymlinksInPath() }

    func webView(_ webView: WKWebView, start task: WKURLSchemeTask) {
        guard let url = task.request.url, url.scheme == Self.scheme, url.host == "local",
              url.user == nil, url.password == nil, url.port == nil,
              url.query == nil, task.request.httpMethod == "GET" else {
            task.didFailWithError(URLError(.resourceUnavailable))
            return
        }
        let resource = directory.appending(path: String(url.path.dropFirst())).resolvingSymlinksInPath().standardizedFileURL
        let contentTypes = [
            "html": "text/html; charset=utf-8", "css": "text/css; charset=utf-8",
            "js": "text/javascript; charset=utf-8", "svg": "image/svg+xml",
            "png": "image/png", "webp": "image/webp", "ico": "image/x-icon",
        ]
        guard resource.path.hasPrefix(directory.path + "/"), let type = contentTypes[resource.pathExtension],
              let bytes = try? Data(contentsOf: resource) else {
            task.didFailWithError(URLError(.resourceUnavailable))
            return
        }
        let headers = [
            "Content-Type": type,
            "Content-Security-Policy": "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ]
        guard let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: headers) else {
            task.didFailWithError(URLError(.badServerResponse))
            return
        }
        task.didReceive(response)
        task.didReceive(bytes)
        task.didFinish()
    }

    func webView(_ webView: WKWebView, stop task: WKURLSchemeTask) {}
}
