import UIKit

struct ManagerGlassLayout: Decodable {
  enum Page: String, Decodable {
    case accounts, sessions, manage, overview, search, detail, diagnostics, modal, hidden
  }
  let page: Page
  let provider: String
  let composeEnabled: Bool
  let backEnabled: Bool
  let query: String
}

/// Lives above the manager WebView. Only the native controls intercept touches;
/// the scrollable webpage underneath provides the glass backdrop.
@available(iOS 26.0, *)
@MainActor
final class ManagerGlassNavigation: UIView, UITextFieldDelegate {
  typealias Page = ManagerGlassLayout.Page

  private let top = UIView()
  private let bottom = UIStackView()
  private let service = UIButton(type: .system)
  private let overview = UIButton(type: .system)
  private let back = UIButton(type: .system)
  private let query = UITextField()
  private let tabs = UISegmentedControl(items: ["账号", "会话"])
  private let compose = UIButton(type: .system)
  private var primary: UIStackView!
  private var search: UIStackView!
  private var current: Page?
  private var selectedProvider = ""
  private var lastInsets: [Int] = []
  private var keyboardBottom: CGFloat = 0
  private var event: (([String: String]) -> Void)?

  init() {
    super.init(frame: .zero)
    tintColor = .systemBlue

    configure(back, icon: "chevron.left", title: nil, label: "返回", action: "back")
    configure(overview, icon: "chart.bar.xaxis", title: nil, label: "总览", action: "overview")
    let manage = button("slider.horizontal.3", title: nil, label: "管理", action: "manage")
    let searchButton = button("magnifyingglass", title: nil, label: "全局搜索", action: "search")
    configure(service, icon: "chevron.down", title: "Muse", label: "服务", action: nil)
    service.setContentHuggingPriority(.required, for: .horizontal)
    service.showsMenuAsPrimaryAction = true
    let spacer = UIView()
    spacer.isUserInteractionEnabled = false
    primary = row([manage, service, spacer, overview, searchButton])

    let searchGlass = UIVisualEffectView(effect: UIGlassEffect(style: .regular))
    searchGlass.cornerConfiguration = .corners(radius: 14)
    let searchIcon = UIImageView(image: UIImage(systemName: "magnifyingglass"))
    searchIcon.tintColor = .secondaryLabel
    searchIcon.contentMode = .center
    searchIcon.setContentHuggingPriority(.required, for: .horizontal)
    query.font = .preferredFont(forTextStyle: .body)
    query.adjustsFontForContentSizeCategory = true
    query.placeholder = "搜索账号或会话"
    query.accessibilityLabel = "全局搜索"
    query.clearButtonMode = .whileEditing
    query.returnKeyType = .search
    query.delegate = self
    query.addTarget(self, action: #selector(queryChanged), for: .editingChanged)
    let field = row([searchIcon, query])
    embed(field, in: searchGlass.contentView, horizontal: 12, vertical: 0)
    let cancel = button(nil, title: "取消", label: "取消搜索", action: "cancelSearch")
    cancel.setContentCompressionResistancePriority(.required, for: .horizontal)
    cancel.setContentHuggingPriority(.required, for: .horizontal)
    search = row([searchGlass, cancel])
    embed(primary, in: top)
    embed(search, in: top)
    add(top, to: self)
    add(back, to: top)
    NSLayoutConstraint.activate([
      top.topAnchor.constraint(equalTo: safeAreaLayoutGuide.topAnchor, constant: 6),
      top.leadingAnchor.constraint(equalTo: safeAreaLayoutGuide.leadingAnchor, constant: 16),
      top.trailingAnchor.constraint(equalTo: safeAreaLayoutGuide.trailingAnchor, constant: -16),
      top.heightAnchor.constraint(equalToConstant: 48),
      back.leadingAnchor.constraint(equalTo: top.leadingAnchor),
      back.topAnchor.constraint(equalTo: top.topAnchor),
      back.bottomAnchor.constraint(equalTo: top.bottomAnchor),
    ])

    // UIKit owns the glass thumb, touch tracking and accessibility of both segments.
    // An additional glass track would layer a second material beneath the control.
    tabs.accessibilityIdentifier = "manager-navigation-tabs"
    tabs.selectedSegmentIndex = 0
    tabs.addTarget(self, action: #selector(tabChanged), for: .valueChanged)
    configure(compose, icon: "plus", title: nil, label: "新建对话", action: "compose", prominent: true, size: 52)
    bottom.axis = .horizontal
    bottom.spacing = 10
    bottom.addArrangedSubview(tabs)
    bottom.addArrangedSubview(compose)
    let dockContainer = UIView()
    add(dockContainer, to: self)
    embed(bottom, in: dockContainer)
    NSLayoutConstraint.activate([
      dockContainer.leadingAnchor.constraint(equalTo: safeAreaLayoutGuide.leadingAnchor, constant: 16),
      dockContainer.trailingAnchor.constraint(equalTo: safeAreaLayoutGuide.trailingAnchor, constant: -16),
      dockContainer.bottomAnchor.constraint(equalTo: safeAreaLayoutGuide.bottomAnchor, constant: -6),
      dockContainer.heightAnchor.constraint(equalToConstant: 52),
    ])
    top.isHidden = true
    bottom.isHidden = true
    NotificationCenter.default.addObserver(self, selector: #selector(keyboardFrameChanged(_:)),
      name: UIResponder.keyboardWillChangeFrameNotification, object: nil)
  }

  required init?(coder: NSCoder) { return nil }

  @objc private func keyboardFrameChanged(_ notification: Notification) {
    guard let window, let frame = notification.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? CGRect else { return }
    let visible = convert(window.convert(frame, from: window.screen.coordinateSpace), from: window)
    keyboardBottom = max(0, bounds.intersection(visible).height)
    if !keyboardBottom.isFinite { keyboardBottom = 0 }
    publishInsets()
  }

  private func row(_ views: [UIView]) -> UIStackView {
    let stack = UIStackView(arrangedSubviews: views)
    stack.axis = .horizontal
    stack.spacing = 8
    return stack
  }

  private func add(_ child: UIView, to parent: UIView) {
    child.translatesAutoresizingMaskIntoConstraints = false
    parent.addSubview(child)
  }

  private func embed(_ child: UIView, in parent: UIView, horizontal: CGFloat = 0, vertical: CGFloat = 0) {
    add(child, to: parent)
    NSLayoutConstraint.activate([
      child.leadingAnchor.constraint(equalTo: parent.leadingAnchor, constant: horizontal),
      child.trailingAnchor.constraint(equalTo: parent.trailingAnchor, constant: -horizontal),
      child.topAnchor.constraint(equalTo: parent.topAnchor, constant: vertical),
      child.bottomAnchor.constraint(equalTo: parent.bottomAnchor, constant: -vertical),
    ])
  }

  private func button(_ icon: String?, title: String?, label: String, action: String) -> UIButton {
    let result = UIButton(type: .system)
    configure(result, icon: icon, title: title, label: label, action: action)
    return result
  }

  private func configure(
    _ button: UIButton, icon: String?, title: String?, label: String,
    action: String?, prominent: Bool = false, size: CGFloat = 48
  ) {
    var config: UIButton.Configuration = prominent ? .prominentGlass() : .glass()
    config.cornerStyle = .capsule
    config.image = icon.flatMap { UIImage(systemName: $0) }
    config.title = title
    config.imagePlacement = .trailing
    config.imagePadding = 6
    config.contentInsets = NSDirectionalEdgeInsets(top: 0, leading: title == nil ? 0 : 12, bottom: 0, trailing: title == nil ? 0 : 12)
    button.configuration = config
    button.accessibilityLabel = label
    if title == nil {
      let width = button.widthAnchor.constraint(equalToConstant: size)
      width.priority = .init(999)
      width.isActive = true
    }
    if let action {
      button.accessibilityIdentifier = "manager-" + action
      button.addAction(UIAction { [weak self] _ in self?.send(action) }, for: .touchUpInside)
    }
  }

  @objc private func tabChanged() {
    switch tabs.selectedSegmentIndex {
    case 0: send("accounts")
    case 1: send("sessions")
    default: break
    }
  }

  func apply(_ state: ManagerGlassLayout, events: (([String: String]) -> Void)?) {
    if let events {
      event = events
      lastInsets = []
    }
    let page = state.page
    let previous = current
    let main = page == .accounts || page == .sessions
    let wasMain = previous == .accounts || previous == .sessions
    let modeChanged = previous != page
    let directional = [Page.manage, .overview, .search].contains(page) ||
      previous.map { [Page.manage, .overview, .search].contains($0) } == true
    let slide = directional && modeChanged && previous != nil && !UIAccessibility.isReduceMotionEnabled

    current = page
    top.isHidden = page == .hidden
    primary.isHidden = !main
    overview.isHidden = page != .accounts
    search.isHidden = page != .search
    back.isHidden = main || page == .search || page == .hidden
    back.isEnabled = state.backEnabled
    compose.isEnabled = state.composeEnabled
    bottom.isHidden = !main && !(slide && wasMain)
    bottom.isUserInteractionEnabled = main
    if main {
      let index = page == .accounts ? 0 : 1
      // Leave the native touch interaction alone. JS echoes the selection
      // back through this bridge after the control has already changed it.
      if (!wasMain || modeChanged) && !tabs.isTracking && tabs.selectedSegmentIndex != index {
        tabs.selectedSegmentIndex = index
      }
    }
    if selectedProvider != state.provider {
      selectedProvider = state.provider
      updateService(animated: previous != nil)
    }

    if page == .search {
      if modeChanged {
        query.text = state.query
        query.becomeFirstResponder()
      }
    } else if previous == .search {
      query.resignFirstResponder()
    }
    layoutIfNeeded()
    if slide {
      let direction: CGFloat = (page == .manage || previous == .manage) ? -1 : 1
      let entering = [Page.manage, .overview, .search].contains(page)
      top.transform = CGAffineTransform(translationX: 22 * direction * (entering ? 1 : -1), y: 0)
      if main && !wasMain {
        bottom.transform = CGAffineTransform(translationX: 0, y: 52 + safeAreaInsets.bottom)
      }
      UIView.animate(withDuration: 0.24, delay: 0, options: [.beginFromCurrentState, .allowUserInteraction]) {
        self.top.transform = .identity
        self.bottom.transform = main ? .identity :
          CGAffineTransform(translationX: 0, y: 52 + self.safeAreaInsets.bottom)
      } completion: { _ in
        guard self.current == page else { return }
        self.bottom.isHidden = !main
      }
    } else {
      top.transform = .identity
      bottom.transform = .identity
    }
    publishInsets()
  }

  private func updateService(animated: Bool) {
    let title = selectedProvider == "hark" ? "Hark" : "Muse"
    var config = service.configuration
    config?.title = title
    service.configuration = config
    service.accessibilityLabel = "当前服务，" + title
    if animated && !UIAccessibility.isReduceMotionEnabled {
      service.transform = CGAffineTransform(scaleX: 0.94, y: 0.94)
      UIView.animate(withDuration: 0.27, delay: 0, usingSpringWithDamping: 0.62, initialSpringVelocity: 0,
                     options: [.beginFromCurrentState, .allowUserInteraction]) { self.service.transform = .identity }
    }
    service.menu = UIMenu(children: [
      UIAction(title: "Muse", state: selectedProvider == "muse" ? .on : .off) { [weak self] _ in self?.send("provider", value: "muse") },
      UIAction(title: "Hark", state: selectedProvider == "hark" ? .on : .off) { [weak self] _ in self?.send("provider", value: "hark") },
      UIMenu(options: .displayInline, children: [
        UIAction(title: "新增账号", image: UIImage(systemName: "person.badge.plus")) { [weak self] _ in self?.send("addAccount") },
      ]),
    ])
  }

  private func send(_ action: String, value: String? = nil) {
    var data = ["action": action]
    if let value { data["value"] = value }
    event?(data)
  }

  @objc private func queryChanged() { send("query", value: query.text ?? "") }
  func textFieldShouldReturn(_ textField: UITextField) -> Bool {
    textField.resignFirstResponder()
    return true
  }

  override func layoutSubviews() {
    super.layoutSubviews()
    publishInsets()
  }

  private func publishInsets() {
    guard bounds.height > 0 else { return }
    let above = safeAreaInsets.top + (current == .hidden || current == nil ? 0 : 62)
    let chromeBottom = safeAreaInsets.bottom + (current == .accounts || current == .sessions ? 66 : 0)
    let below = current == .search ? max(chromeBottom, keyboardBottom) : chromeBottom
    let insets = [Int(ceil(above)), Int(ceil(below)), Int(bounds.width.rounded())]
    guard insets != lastInsets else { return }
    lastInsets = insets
    event?(["action": "insets", "top": String(insets[0]), "bottom": String(insets[1]), "width": String(insets[2])])
  }

  override func hitTest(_ point: CGPoint, with event: UIEvent?) -> UIView? {
    let target = super.hitTest(point, with: event)
    var view = target
    while let hit = view, hit !== self {
      if hit is UIControl { return target }
      view = hit.superview
    }
    return nil
  }
}

