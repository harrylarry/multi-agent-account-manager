import XCTest

final class ManagerTests: XCTestCase {
    @MainActor
    func testOriginalManagerAndNativeNavigation() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launch()

        // The tab strip and provider control come from the original UIKit source file.
        let museEmpty = app.webViews.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "暂无 Muse 账号")).firstMatch
        XCTAssertTrue(museEmpty.waitForExistence(timeout: 20), app.debugDescription)
        let tabs = app.descendants(matching: .any).matching(identifier: "manager-navigation-tabs").firstMatch
        XCTAssertTrue(tabs.waitForExistence(timeout: 10), app.debugDescription)
        app.buttons["会话"].firstMatch.tap()
        XCTAssertTrue(app.webViews.staticTexts["最近会话"].waitForExistence(timeout: 10))
        app.buttons["账号"].firstMatch.tap()
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
