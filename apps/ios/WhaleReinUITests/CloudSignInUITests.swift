// App-side Cloud sign-in flow over deterministic fixtures: sign out, sign in
// with a clean sheet, and prove the second sign-in really changes accounts
// instead of silently resuming the first one.

import XCTest

final class CloudSignInUITests: WhaleReinUITestCase {
    func testSignOutThenSignInSwitchesAccountWithoutReturningToWelcome() {
        let app = launchScenario("cloud.switch-accounts")

        let signIn = waitFor("cloud.signIn")
        XCTAssertFalse(element("cloud.account").exists)

        signIn.tap()

        // The exchanged account is not ready yet: the flow shows progress,
        // never the welcome page with a login button.
        XCTAssertTrue(waitFor("cloud.signingIn", timeout: 2).exists)
        XCTAssertFalse(element("cloud.welcome.title").exists)

        let account = waitFor("cloud.account")
        XCTAssertTrue(account.label.contains("fixture@whalerein.test"))
        XCTAssertFalse(element("cloud.welcome.title").exists)

        waitFor("cloud.signOut").tap()

        // A local sign-out returns to the single welcome/login entry.
        XCTAssertTrue(waitFor("cloud.signIn").exists)

        waitFor("cloud.signIn").tap()
        let switched = waitFor("cloud.account")
        XCTAssertTrue(switched.label.contains("second@whalerein.test"))
        XCTAssertFalse(switched.label.contains("fixture@whalerein.test"))
        XCTAssertFalse(element("cloud.welcome.title").exists)
    }
}
