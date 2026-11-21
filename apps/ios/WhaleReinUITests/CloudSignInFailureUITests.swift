// Sign-in error and cancel flows over deterministic fixtures.

import XCTest

final class CloudSignInFailureUITests: WhaleReinUITestCase {
    func testStartFailureShowsAnErrorAndKeepsTheLoginAvailable() {
        let app = launchApp(["-ui-cloud-start-fail"])
        waitFor("cloud.signIn").tap()

        let error = waitFor("cloud.error")
        XCTAssertTrue(error.exists)
        XCTAssertTrue(app.buttons["cloud.signIn"].isEnabled)
        XCTAssertTrue(element("cloud.welcome.title").exists)
    }

    func testExchangeFailureReturnsToTheWelcomePageWithAnError() {
        let app = launchApp(["-ui-cloud-complete-fail"])
        waitFor("cloud.signIn").tap()

        XCTAssertTrue(waitFor("cloud.error").exists)
        XCTAssertTrue(waitFor("cloud.signIn").exists)
        XCTAssertFalse(element("cloud.account").exists)
    }

    func testCancellingTheSheetShowsNoError() {
        let app = launchApp(["-ui-cloud-cancel-sheet"])
        waitFor("cloud.signIn").tap()

        XCTAssertTrue(waitFor("cloud.signIn").exists)
        XCTAssertFalse(element("cloud.error").exists)
        XCTAssertTrue(element("cloud.welcome.title").exists)
    }

    func testRevokedRestoreReturnsToWelcomeWithAnError() {
        let app = launchApp(["-ui-cloud-revoked"])
        XCTAssertTrue(waitFor("cloud.error").exists)
        XCTAssertTrue(waitFor("cloud.signIn").exists)
        XCTAssertFalse(element("cloud.account").exists)
    }
}
