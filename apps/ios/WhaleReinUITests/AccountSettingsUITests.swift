// Settings account sections: typed status, default computer, scoped Cloud app
// sign-in revocation and production diagnostics entry.
//
// The fixture service supplies deterministic account/app-session state, so
// cancel/confirm scopes and instance labels are real UI evidence without a
// live Cloud deployment.

import XCTest

final class AccountSettingsUITests: WhaleReinUITestCase {
    /// Scrolls the Settings form until the element is realized. `waitFor` alone
    /// cannot see rows a lazy Form has not built yet.
    private func revealInSettings(_ identifier: String) -> XCUIElement {
        let element = self.element(identifier)
        for _ in 0..<10 {
            if element.exists { return element }
            app.swipeUp()
        }
        return waitFor(identifier)
    }

    private func openSettings() {
        launchAndOpenConversation(["-ui-cloud-signed-in"])
        tap(waitFor("conversation.settings"))
        XCTAssertTrue(waitFor("settings.coreRevision").exists)
    }

    func testSettingsShowsTypedStatusAndScopedAppSessions() {
        openSettings()

        let service = revealInSettings("settings.status.service")
        XCTAssertTrue(service.label.contains("WhaleRein Cloud"))
        let account = revealInSettings("settings.status.account")
        XCTAssertTrue(account.label.contains("Signed in"))
        let device = revealInSettings("settings.status.deviceAccess")
        XCTAssertTrue(device.label.contains("Ready"))

        let current = revealInSettings("settings.appSessionScope.sess-fixture-ios")
        XCTAssertTrue(current.label.contains("This app"))
        let other = revealInSettings("settings.appSessionScope.sess-fixture-other")
        XCTAssertTrue(other.label.contains("Other app"))

        tap(revealInSettings("settings.appSession.revoke.sess-fixture-other"))
        let alert = app.alerts.firstMatch
        XCTAssertTrue(alert.waitForExistence(timeout: 10))
        XCTAssertTrue(alert.label.contains("Sign out this app sign-in?"))
        alert.buttons
            .matching(NSPredicate(format: "label == %@", "Sign out"))
            .firstMatch
            .tap()

        let state = revealInSettings("settings.appSessionState.sess-fixture-other")
        XCTAssertTrue(state.label.contains("Revoked"))
        // The current app sign-in survives an isolated revocation.
        XCTAssertTrue(
            revealInSettings("settings.appSessionState.sess-fixture-ios")
                .label
                .contains("Active")
        )
    }

    func testRevokeAllShowsPendingAndSignsOutLocally() {
        openSettings()
        tap(revealInSettings("settings.appSession.revokeAll"))

        let alert = app.alerts.firstMatch
        XCTAssertTrue(alert.waitForExistence(timeout: 10))
        XCTAssertTrue(alert.label.contains("Sign out all app sign-ins?"))
        alert.buttons
            .matching(NSPredicate(format: "label == %@", "Sign out all apps"))
            .firstMatch
            .tap()

        // The local app sign-in ends and Core's pending remote sign-out is
        // presented as typed state rather than a confirmed sign-out.
        let pending = revealInSettings("settings.status.pendingSignOut")
        XCTAssertTrue(pending.waitForExistence(timeout: 15))
    }

    func testDefaultComputerSelectionShowsCurrent() {
        openSettings()
        let row = revealInSettings("settings.defaultHost.travel-laptop")
        tap(row)
        XCTAssertTrue(
            waitFor("settings.defaultHost.current.travel-laptop").waitForExistence(timeout: 10)
        )
    }

    func testDiagnosticsAvailableFromProductionSettingsWithoutPrivateMarkers() {
        openSettings()
        tap(revealInSettings("settings.diagnostics.open"))

        let preview = waitFor("diagnostics.preview")
        XCTAssertTrue(preview.label.contains("corr-https-0001"))
        XCTAssertFalse(preview.label.contains("inv_SENSITIVE_VALUE"))
        XCTAssertFalse(preview.label.contains("PRIVATE_KEY_MATERIAL"))

        tap(waitFor("diagnostics.share"))
        let alert = app.alerts.firstMatch
        XCTAssertTrue(alert.waitForExistence(timeout: 10))
        alert.buttons
            .matching(NSPredicate(format: "label == %@", "Cancel"))
            .firstMatch
            .tap()

        let status = waitFor("diagnostics.status")
        XCTAssertTrue(status.label.contains("cancelled"))
    }

    func testRevokeAndForgetHaveDistinctConfirmations() {
        openSettings()
        tap(revealInSettings("settings.revoke"))

        let alert = app.alerts.firstMatch
        XCTAssertTrue(alert.waitForExistence(timeout: 10))
        XCTAssertTrue(alert.label.contains("Revoke this device"))
        alert.buttons
            .matching(NSPredicate(format: "label == %@", "Cancel"))
            .firstMatch
            .tap()

        // Cancel leaves the paired conversation in place.
        XCTAssertTrue(waitFor("conversation.settings").exists)

        tap(revealInSettings("settings.forget"))
        let forgetAlert = app.alerts.firstMatch
        XCTAssertTrue(forgetAlert.waitForExistence(timeout: 10))
        XCTAssertTrue(forgetAlert.label.contains("Forget this device"))
        XCTAssertFalse(forgetAlert.label.contains("Revoke this device"))
        forgetAlert.buttons
            .matching(NSPredicate(format: "label == %@", "Cancel"))
            .firstMatch
            .tap()
        XCTAssertTrue(waitFor("conversation.settings").exists)
    }
}
