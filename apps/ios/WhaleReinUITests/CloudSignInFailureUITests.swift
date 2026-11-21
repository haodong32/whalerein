// Sign-in error and cancel flows over deterministic fixtures.

import XCTest

final class CloudSignInFailureUITests: WhaleReinUITestCase {
    func testStartFailureShowsAnErrorAndKeepsTheLoginAvailable() {
        let app = launchScenario("cloud.start-failure")
        waitFor("cloud.signIn").tap()

        let error = waitFor("cloud.error")
        XCTAssertTrue(error.exists)
        XCTAssertTrue(app.buttons["cloud.signIn"].isEnabled)
        XCTAssertTrue(element("cloud.welcome.title").exists)
    }

    func testExchangeFailureReturnsToTheWelcomePageWithAnError() {
        let app = launchScenario("cloud.complete-failure")
        waitFor("cloud.signIn").tap()

        XCTAssertTrue(waitFor("cloud.error").exists)
        XCTAssertTrue(waitFor("cloud.signIn").exists)
        XCTAssertFalse(element("cloud.account").exists)
    }

    func testCancellingTheSheetShowsNoError() {
        let app = launchScenario("cloud.cancel-sheet")
        waitFor("cloud.signIn").tap()

        XCTAssertTrue(waitFor("cloud.signIn").exists)
        XCTAssertFalse(element("cloud.error").exists)
        XCTAssertTrue(element("cloud.welcome.title").exists)
    }

    func testRevokedRestoreReturnsToWelcomeWithAnError() {
        let app = launchScenario("cloud.revoked")
        XCTAssertTrue(waitFor("cloud.error").exists)
        XCTAssertTrue(waitFor("cloud.signIn").exists)
        XCTAssertFalse(element("cloud.account").exists)
    }
}
