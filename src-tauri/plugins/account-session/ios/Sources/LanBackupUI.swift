import AVFoundation
import CoreImage
import Darwin
import UIKit

enum LanBackupUI {
  static func wifi() -> [String: String]? {
    guard UIApplication.shared.applicationState == .active else { return nil }
    var interfaces: UnsafeMutablePointer<ifaddrs>?
    guard getifaddrs(&interfaces) == 0, let first = interfaces else { return nil }
    defer { freeifaddrs(first) }
    var next: UnsafeMutablePointer<ifaddrs>? = first
    while let interface = next {
      let row = interface.pointee
      next = row.ifa_next
      guard String(cString: row.ifa_name) == "en0", let addr = row.ifa_addr, let mask = row.ifa_netmask,
            addr.pointee.sa_family == UInt8(AF_INET), let address = numeric(addr), let subnet = numeric(mask) else { continue }
      return ["address":address, "mask":subnet]
    }
    return nil
  }

  private static func numeric(_ address: UnsafePointer<sockaddr>) -> String? {
    var buffer = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
    guard getnameinfo(address, socklen_t(MemoryLayout<sockaddr_in>.size), &buffer,
                      socklen_t(buffer.count), nil, 0, NI_NUMERICHOST) == 0 else { return nil }
    return String(cString: buffer)
  }

  static func png(code: String) -> String? {
    guard let filter = CIFilter(name: "CIQRCodeGenerator") else { return nil }
    filter.setValue(Data(code.utf8), forKey: "inputMessage")
    filter.setValue("M", forKey: "inputCorrectionLevel")
    guard let output = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 8, y: 8)),
          let image = CIContext().createCGImage(output, from: output.extent) else { return nil }
    return UIImage(cgImage: image).pngData()?.base64EncodedString()
  }
}

final class LanBackupScanner: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
  private let session = AVCaptureSession()
  private let cameraQueue = DispatchQueue(label: "diehao.lan-backup.camera")
  private var preview: AVCaptureVideoPreviewLayer?
  var result: ((String?) -> Void)?

  override func viewDidLoad() {
    super.viewDidLoad()
    view.backgroundColor = .black
    let cancel = UIButton(type: .system)
    cancel.setTitle("取消扫描", for: .normal)
    cancel.setTitleColor(.black, for: .normal)
    cancel.backgroundColor = .white
    cancel.layer.cornerRadius = 8
    cancel.translatesAutoresizingMaskIntoConstraints = false
    cancel.addTarget(self, action: #selector(cancelScan), for: .touchUpInside)
    view.addSubview(cancel)
    NSLayoutConstraint.activate([
      cancel.centerXAnchor.constraint(equalTo: view.centerXAnchor),
      cancel.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -25),
      cancel.widthAnchor.constraint(equalToConstant: 132), cancel.heightAnchor.constraint(equalToConstant: 44),
    ])
    let output = AVCaptureMetadataOutput()
    guard let camera = AVCaptureDevice.default(for: .video), let input = try? AVCaptureDeviceInput(device: camera),
          session.canAddInput(input), session.canAddOutput(output) else {
      DispatchQueue.main.async { [weak self] in self?.finish(nil) }
      return
    }
    session.addInput(input)
    session.addOutput(output)
    guard output.availableMetadataObjectTypes.contains(.qr) else {
      DispatchQueue.main.async { [weak self] in self?.finish(nil) }
      return
    }
    output.setMetadataObjectsDelegate(self, queue: .main)
    output.metadataObjectTypes = [.qr]
    let layer = AVCaptureVideoPreviewLayer(session: session)
    layer.videoGravity = .resizeAspectFill
    view.layer.insertSublayer(layer, at: 0)
    preview = layer
    cameraQueue.async { [session] in session.startRunning() }
  }

  override func viewDidLayoutSubviews() { super.viewDidLayoutSubviews(); preview?.frame = view.bounds }

  override func viewDidDisappear(_ animated: Bool) {
    super.viewDidDisappear(animated)
    cameraQueue.async { [session] in session.stopRunning() }
    result?(nil)
    result = nil
  }

  func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput metadataObjects: [AVMetadataObject], from connection: AVCaptureConnection) {
    guard let code = (metadataObjects.first as? AVMetadataMachineReadableCodeObject)?.stringValue,
          code.hasPrefix("diehao-lan:v1:"), code.utf8.count <= 100 else { return }
    finish(code)
  }

  @objc private func cancelScan() { finish(nil) }

  private func finish(_ code: String?) {
    let callback = result
    result = nil
    dismiss(animated: true) { callback?(code) }
  }
}
