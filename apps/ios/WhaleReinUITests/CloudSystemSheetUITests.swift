// End-to-end system sheet path: the real ASWebAuthenticationSession opens a
// local mock OIDC authorization page that redirects to the app callback.

import XCTest

final class CloudSystemSheetUITests: WhaleReinUITestCase {
    func testRealSystemSheetCompletesThroughAMockProvider() throws {
        let provider = try MockOIDCProvider()
        defer { provider.stop() }

        app = XCUIApplication()
        app.launchEnvironment = ["UI_CLOUD_AUTH_BASE": provider.baseURL]
        launchScenario("cloud.system-sheet")

        waitFor("cloud.signIn").tap()

        // The real sheet may show a system confirmation; accept it if present.
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let confirm = springboard.buttons["Continue"]
        if confirm.waitForExistence(timeout: 3) {
            confirm.tap()
        }

        let account = waitFor("cloud.account", timeout: 20)
        XCTAssertTrue(account.label.contains("fixture@whalerein.test"))
        XCTAssertFalse(element("cloud.welcome.title").exists)
    }
}
