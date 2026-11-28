import XCTest

final class IsolationTests: XCTestCase {
    @MainActor
    func testTwoIdentitiesShareNothingAcrossLaunch() throws {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launch()

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
            cleanup.tap()
            wait(for: status, toContain: "测试资料已清理")
        }
        XCTAssertTrue(seed.isEnabled, status.label)
        seed.tap()
        wait(for: status, toContain: "双向隔离通过")

        verify.tap()
        wait(for: status, toContain: "请彻底退出并重新打开")
        app.terminate()
        app.launch()

        let verifyReady = NSPredicate(format: "enabled == true")
        XCTAssertEqual(
            XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: verifyReady, object: verify)], timeout: 10),
            .completed
        )
        verify.tap()
        wait(for: status, toContain: "A 组通过")
        cleanup.tap()
        wait(for: status, toContain: "测试资料已清理")
        let bc = app.buttons["isolationBC.run"]
        bc.tap()
        let bcProgress = NSPredicate(format: "label CONTAINS %@ OR label CONTAINS %@", "B / C 组通过", "请彻底退出并重开叠号")
        XCTAssertEqual(
            XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: bcProgress, object: status)], timeout: 90),
            .completed, status.label
        )
        if status.label.contains("请彻底退出并重开叠号") {
            app.terminate()
            app.launch()
            XCTAssertEqual(
                XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: verifyReady, object: bc)], timeout: 10),
                .completed
            )
            bc.tap()
        }
        wait(for: status, toContain: "B / C 组通过")
    }

    @MainActor
    private func wait(for element: XCUIElement, toContain text: String) {
        let predicate = NSPredicate(format: "label CONTAINS %@", text)
        let result = XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: predicate, object: element)], timeout: 90)
        XCTAssertEqual(result, .completed, "Expected \(text), current: \(element.label)")
    }
}
