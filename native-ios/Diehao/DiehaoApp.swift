import SwiftUI

@main
@MainActor
struct DiehaoApp: App {
    @State private var check = IsolationCheck()

    var body: some Scene {
        WindowGroup {
            NavigationStack {
                List {
                    if let registry = check.registry {
                        NavigationLink("账号资料、代理与 Cookie") { AccountTools(registry: registry) }
                    }
                    Section("隔离自检 A 组") {
                        Text(check.status)
                            .accessibilityIdentifier("isolationA.status")
                        Button("写入并检查 A / B") {
                            Task { await check.run() }
                        }
                        .disabled(!check.canSeed)
                        .accessibilityIdentifier("isolationA.seed")
                        Button("验证重启后隔离") {
                            Task { await check.verifyAfterRestart() }
                        }
                        .disabled(!check.canVerify)
                        .accessibilityIdentifier("isolationA.verify")
                        Button("清理测试资料") {
                            Task { await check.clean() }
                        }
                        .disabled(!check.canClean)
                        .accessibilityIdentifier("isolationA.clean")
                    }
                    Section("隔离自检 B / C 组") {
                        Button("验证删除与恢复") { Task { await check.runBC() } }
                            .disabled(!check.canSeed)
                            .accessibilityIdentifier("isolationBC.run")
                    }
                    if check.referencedAccountCount > 0 {
                        Section("旧资料") {
                            Text("已引用 \(check.referencedAccountCount) 份 Muse 账号资料。")
                        }
                    }
                }
                .navigationTitle("叠号")
                .task { await check.start() }
            }
        }
    }
}
