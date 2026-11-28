import XCTest

final class IsolationTests: XCTestCase {
    @MainActor
    func testNewAccountCanOpenAndCloseMuse() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launch()

        let accounts = app.buttons["管理账号、代理与 Cookie"]
        XCTAssertTrue(accounts.waitForExistence(timeout: 20))
        accounts.tap()
        app.buttons["新增独立账号"].tap()

        let open = app.buttons["account.openMuse"]
        XCTAssertTrue(open.waitForExistence(timeout: 10))
        open.tap()
        let close = app.buttons["account.closeMuse"]
        XCTAssertTrue(close.waitForExistence(timeout: 10))
        close.tap()
        XCTAssertTrue(open.waitForExistence(timeout: 10))
    }

    @MainActor
    func testTwoIdentitiesShareNothingAcrossLaunch() throws {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launch()
        openDiagnostics(app)

        let status = app.staticTexts["isolationA.status"]
        let seed = app.buttons["isolationA.seed"]
        let verify = app.buttons["isolationA.verify"]
        let cleanup = app.buttons["isolationA.clean"]
        XCTAssertTrue(status.waitForExistence(timeout: 10))
        let ready = NSPredicate(format: "NOT (label CONTAINS %@)", "正在启动")
        XCTAssertEqual(
            XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: ready, object: status)], timeout: 10),
            .completed
        )
        if cleanup.isEnabled {
            clean(app: app, button: cleanup, status: status)
        }
        XCTAssertTrue(seed.isEnabled, status.label)
        seed.tap()
        wait(for: status, toContain: "双向隔离通过")

        verify.tap()
        wait(for: status, toContain: "请彻底退出并重新打开")
        app.terminate()
        app.launch()
        openDiagnostics(app)

        let verifyReady = NSPredicate(format: "enabled == true")
        XCTAssertEqual(
            XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: verifyReady, object: verify)], timeout: 10),
            .completed
        )
        verify.tap()
        wait(for: status, toContain: "A 组通过")
        clean(app: app, button: cleanup, status: status)
        let bc = app.buttons["isolationBC.run"]
        bc.tap()
        let bcProgress = NSPredicate(format: "NOT (label BEGINSWITH %@ OR label CONTAINS %@)", "正在", "A 组通过")
        XCTAssertEqual(
            XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: bcProgress, object: status)], timeout: 90),
            .completed, status.label
        )
        if status.label.contains("请彻底退出并重开叠号") {
            app.terminate()
            app.launch()
            openDiagnostics(app)
            XCTAssertEqual(
                XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: verifyReady, object: bc)], timeout: 10),
                .completed
            )
            bc.tap()
        }
        XCTAssertEqual(
            XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: bcProgress, object: status)], timeout: 90),
            .completed, status.label
        )
        XCTAssertTrue(status.label.contains("B / C 组通过"), status.label)
    }

    @MainActor
    private func clean(app: XCUIApplication, button: XCUIElement, status: XCUIElement) {
        button.tap()
        let progress = NSPredicate(format: "label CONTAINS %@ OR label CONTAINS %@", "测试资料已清理", "请彻底退出并重开叠号")
        XCTAssertEqual(
            XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: progress, object: status)], timeout: 90),
            .completed, status.label
        )
        if status.label.contains("请彻底退出并重开叠号") {
            app.terminate()
            app.launch()
            openDiagnostics(app)
            let enabled = NSPredicate(format: "enabled == true")
            XCTAssertEqual(
                XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: enabled, object: button)], timeout: 10),
                .completed
            )
            button.tap()
        }
        wait(for: status, toContain: "测试资料已清理")
    }

    @MainActor
    private func openDiagnostics(_ app: XCUIApplication) {
        let link = app.buttons["dashboard.isolation"]
        XCTAssertTrue(link.waitForExistence(timeout: 20))
        if !link.isHittable { app.swipeUp() }
        link.tap()
    }

    @MainActor
    private func wait(for element: XCUIElement, toContain text: String) {
        let predicate = NSPredicate(format: "label CONTAINS %@", text)
        let result = XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: predicate, object: element)], timeout: 90)
        XCTAssertEqual(result, .completed, "Expected \(text), current: \(element.label)")
    }
}

final class DashboardTests: XCTestCase {
    @MainActor
    func testUnknownSendReconcilesWithoutSendingAgain() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-diehao-send-fixtures", UUID().uuidString]
        app.launch()

        let send = composeSend(in: app)
        send.tap()
        let check = app.buttons["compose.checkSend"]
        XCTAssertTrue(check.waitForExistence(timeout: 10))
        // Another normal send with the same unresolved message must hit the ledger.
        if !send.isHittable { app.swipeUp() }
        send.tap()
        XCTAssertTrue(check.waitForExistence(timeout: 10))
        if !check.isHittable { app.swipeUp() }
        check.tap()
        XCTAssertTrue(app.buttons["account.openMuse"].waitForExistence(timeout: 15))
    }

    @MainActor
    func testRestartReusesRecoveredSendInsteadOfSendingAgain() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-diehao-send-fixtures", UUID().uuidString]
        app.launch()
        let send = composeSend(in: app)
        send.tap()
        XCTAssertTrue(app.buttons["compose.checkSend"].waitForExistence(timeout: 10))
        app.terminate()
        app.launch()
        composeSend(in: app).tap()
        XCTAssertTrue(app.buttons["account.openMuse"].waitForExistence(timeout: 15))
    }

    @MainActor
    private func composeSend(in app: XCUIApplication) -> XCUIElement {
        let compose = app.buttons["dashboard.compose"]
        XCTAssertTrue(compose.waitForExistence(timeout: 20))
        compose.tap()
        let picker = app.buttons["compose.account"]
        XCTAssertTrue(picker.waitForExistence(timeout: 10))
        picker.tap()
        app.buttons["用 测试账号 A 发送"].tap()
        app.textViews["compose.message"].tap()
        app.typeText("Simulator send")
        let send = app.buttons["compose.send"]
        if !send.isHittable { app.swipeUp() }
        return send
    }

    @MainActor
    func testApprovalQueueScopesActionsAndConfirmsOneTimeGrant() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-diehao-approval-fixtures"]
        app.launch()

        let approvals = app.descendants(matching: .any)["dashboard.approvals"]
        XCTAssertTrue(approvals.waitForExistence(timeout: 20))
        let onePending = NSPredicate(format: "value == %@", "1")
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: onePending, object: approvals)],
                                      timeout: 20), .completed)
        app.buttons["测试账号 A"].tap()
        let queue = app.buttons["account.approvals"]
        XCTAssertTrue(queue.waitForExistence(timeout: 10))
        queue.tap()
        let allow = app.buttons["approval.once.safe"]
        XCTAssertTrue(allow.waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["approval.once.blocked"].exists)
        allow.tap()
        XCTAssertTrue(app.staticTexts["Muse 已确认审批结果。"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["暂无待审批请求。"].exists)
    }

    @MainActor
    func testActivityReconcilesReconnectWithoutDuplicateUnread() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-diehao-activity-fixtures"]
        app.launch()

        let unread = app.descendants(matching: .any)["dashboard.unread"]
        XCTAssertTrue(unread.waitForExistence(timeout: 20))
        let two = NSPredicate(format: "value == %@", "2")
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: two, object: unread)],
                                      timeout: 20), .completed)
        XCTAssertTrue((app.buttons["测试账号 A"].value as? String)?.contains("有待处理事项") == true)
        XCTAssertFalse((app.buttons["测试账号 B"].value as? String)?.contains("有待处理事项") == true)
    }

    @MainActor
    func testQuotaSeparatesZeroUseUnknownAndExhaustion() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-diehao-quota-fixtures"]
        app.launch()

        let available = app.buttons["测试账号 A"]
        let unknown = app.buttons["测试账号 B"]
        XCTAssertTrue(available.waitForExistence(timeout: 20))
        let hasValue = NSPredicate(format: "value CONTAINS %@", "额度剩余 100%")
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: hasValue, object: available)],
                                      timeout: 10), .completed)
        XCTAssertTrue((unknown.value as? String)?.contains("额度未观测") == true)
        let refresh = app.buttons["dashboard.refreshQuota"]
        XCTAssertTrue(refresh.waitForExistence(timeout: 5))
        refresh.tap()
        let depleted = NSPredicate(format: "value CONTAINS %@", "额度已用尽")
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: depleted, object: unknown)],
                                      timeout: 10), .completed, String(describing: unknown.value))
        XCTAssertTrue((available.value as? String)?.contains("额度剩余 100%") == true)
    }

    @MainActor
    func testActivityPollingAndComposeInterface() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-diehao-dashboard-fixtures"]
        app.launch()

        let approvals = app.descendants(matching: .any)["dashboard.approvals"]
        XCTAssertTrue(approvals.waitForExistence(timeout: 20))
        let ready = NSPredicate(format: "value == %@", "2")
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: ready, object: approvals)], timeout: 10), .completed)
        XCTAssertEqual(app.descendants(matching: .any)["dashboard.unread"].value as? String, "4")
        XCTAssertEqual(app.descendants(matching: .any)["dashboard.running"].value as? String, "1")
        let poll = app.buttons["dashboard.poll"]
        if !poll.isHittable { app.swipeUp() }
        XCTAssertEqual(poll.value as? String, "20 秒")
        poll.tap()
        XCTAssertTrue(app.buttons["10 秒"].exists)
        XCTAssertTrue(app.buttons["20 秒"].exists)
        app.buttons["30 秒"].tap()
        XCTAssertEqual(poll.value as? String, "30 秒")
        app.swipeDown()
        XCTAssertTrue(app.staticTexts["暂无待处理事项"].waitForExistence(timeout: 10))

        app.buttons["dashboard.compose"].tap()
        let account = app.buttons["compose.account"]
        XCTAssertTrue(account.waitForExistence(timeout: 10))
        account.tap()
        XCTAssertFalse(app.buttons["用 测试账号 B 发送"].exists)
        let signedIn = app.buttons["用 测试账号 A 发送"]
        XCTAssertTrue(signedIn.waitForExistence(timeout: 5))
        signedIn.tap()
        let send = app.buttons["compose.send"]
        XCTAssertFalse(send.isEnabled)
        let message = app.textViews["compose.message"]
        message.tap()
        app.typeText("Simulator compose")
        if !send.isHittable { app.swipeUp() }
        send.tap()
        XCTAssertTrue(app.staticTexts["compose.status"].waitForExistence(timeout: 10))
    }
}
