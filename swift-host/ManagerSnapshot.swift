import Foundation

struct ManagerSnapshot {
    // Matches Tauri's original iOS app_data_dir. Side-by-side installs have separate sandboxes.
    private static let originalIdentifier = "com.ldjx.accountmanager"
    static let accountFile = URL.applicationSupportDirectory
        .appending(path: originalIdentifier, directoryHint: .isDirectory)
        .appending(path: "accounts.json", directoryHint: .notDirectory)

    private(set) var payload: [String: Any]

    var providers: Set<String> {
        Set((payload["providers"] as? [[String: Any]] ?? []).compactMap { $0["id"] as? String })
    }

    mutating func selectProvider(_ id: String) { payload["selectedProviderId"] = id }
    mutating func setModalActive(_ active: Bool) { payload["modalActive"] = active }

    static func load(from file: URL) throws -> Self {
        guard let bytes = file.path.withCString({ diehao_manager_snapshot($0) }) else {
            throw CocoaError(.fileReadCorruptFile)
        }
        defer { diehao_manager_snapshot_free(bytes) }
        let json = Data(String(cString: bytes).utf8)
        guard let payload = try JSONSerialization.jsonObject(with: json) as? [String: Any] else {
            throw CocoaError(.fileReadCorruptFile)
        }
        return Self(payload: payload)
    }
}
