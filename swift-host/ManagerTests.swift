import XCTest

final class ManagerTests: XCTestCase {
    @MainActor
    func testOriginalManagerAndNativeNavigation() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launch()

        // The tab strip and provider control come from the original UIKit source file.
        let tabs = app.segmentedControls["manager-navigation-tabs"]
        XCTAssertTrue(tabs.waitForExistence(timeout: 20))
        XCTAssertTrue(app.webViews.staticTexts["暂无 Muse 账号"].waitForExistence(timeout: 10))
        tabs.buttons["会话"].tap()
        XCTAssertTrue(app.webViews.staticTexts["最近会话"].waitForExistence(timeout: 10))
        tabs.buttons["账号"].tap()
        app.buttons["管理"].tap()
        XCTAssertTrue(app.webViews.buttons["备份与迁移"].waitForExistence(timeout: 10))
        app.buttons["返回"].tap()
        app.buttons["总览"].tap()
        XCTAssertTrue(app.webViews.staticTexts["总览"].waitForExistence(timeout: 10))
        app.buttons["返回"].tap()
        app.buttons["当前服务，Muse"].tap()
        app.buttons["Hark"].tap()
        XCTAssertTrue(app.webViews.staticTexts["暂无 Hark 账号"].waitForExistence(timeout: 10))
    }
}
