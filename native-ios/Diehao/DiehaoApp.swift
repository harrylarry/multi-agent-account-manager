import SwiftUI

@main
@MainActor
struct DiehaoApp: App {
    @State private var check = IsolationCheck()

    var body: some Scene {
        WindowGroup {
            NavigationStack {
                if let registry = check.registry, check.ready {
                    Dashboard(registry: registry, check: check)
                } else {
                    Text(check.status)
                        .navigationTitle("叠号")
                }
            }
            .task { await check.start() }
        }
    }
}

@MainActor
struct IsolationDiagnostics: View {
    let check: IsolationCheck
    @AppStorage(SwiftNoiseXX.preference) private var swiftNoise = false
    #if DEBUG && targetEnvironment(simulator) && DIEHAO_NOISE_TESTS
    @State private var noiseFixtureResult: String?
    #endif

    var body: some View {
        List {
            Section("隔离自检 A 组") {
                Text(check.status)
                    .accessibilityIdentifier("isolationA.status")
                Button("写入并检查 A / B") { Task { await check.run() } }
                    .disabled(!check.canSeed)
                    .accessibilityIdentifier("isolationA.seed")
                Button("验证重启后隔离") { Task { await check.verifyAfterRestart() } }
                    .disabled(!check.canVerify)
                    .accessibilityIdentifier("isolationA.verify")
                Button("清理测试资料") { Task { await check.clean() } }
                    .disabled(!check.canClean)
                    .accessibilityIdentifier("isolationA.clean")
            }
            Section("隔离自检 B / C 组") {
                if check.hasTestSlots {
                    Text("验证 A 组重启结果并清理测试资料后，可以运行 B / C 组。")
                        .font(.caption)
                }
                Button("验证删除与恢复") { Task { await check.runBC() } }
                    .disabled(!check.canSeed)
                    .accessibilityIdentifier("isolationBC.run")
            }
            Section("Muse 加密传输") {
                Toggle("试用 Swift Noise", isOn: $swiftNoise)
                Text("切换后请彻底关闭再重开 App。关闭此开关使用 Rust Noise。")
                    .font(.caption)
                #if DEBUG && targetEnvironment(simulator) && DIEHAO_NOISE_TESTS
                if ProcessInfo.processInfo.arguments.contains("-diehao-noise-fixtures") {
                    Button("检查 Noise 向量和 Rust 互通") {
                        do {
                            try SwiftNoiseXX.verifyFixtures()
                            noiseFixtureResult = "Noise 向量与 Rust 互通通过。"
                        } catch {
                            noiseFixtureResult = "Noise 自检失败（\(error)）。"
                        }
                    }
                    .accessibilityIdentifier("noise.fixture")
                    if let noiseFixtureResult {
                        Text(noiseFixtureResult).accessibilityIdentifier("noise.result")
                    }
                }
                #endif
            }
            if check.referencedAccountCount > 0 {
                Section("旧资料") {
                    Text("已引用 \(check.referencedAccountCount) 份 Muse 账号资料。")
                }
            }
        }
        .navigationTitle("隔离自检")
    }
}
