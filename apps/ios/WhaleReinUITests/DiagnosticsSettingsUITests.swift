// Diagnostics preview and confirmed sharing, plus the destructive forget flow.

import XCTest

final class DiagnosticsSettingsUITests: WhaleReinUITestCase {
    func testDiagnosticsShowsAnonymousHostsAndExpandsTheSameReport() {
        launchAndOpenConversation()
        tap(waitFor("conversation.diagnostics"))

        let firstHost = waitFor("diagnostics.host.host-1")
        XCTAssertTrue(firstHost.label.contains("host-1"))
        XCTAssertTrue(firstHost.label.contains("Connected"))
        let secondHost = waitFor("diagnostics.host.host-2")
        XCTAssertTrue(secondHost.label.contains("Offline"))

        // The fixture has no authorized directory binding, so rows stay
        // anonymous and never gain local decoration.
        XCTAssertFalse(firstHost.label.contains("Studio Mac"))

        tap(waitFor("diagnostics.fullReport"))
        let fullReport = waitFor("diagnostics.preview")
        XCTAssertTrue(fullReport.label.contains("\"hosts\""))
        XCTAssertTrue(fullReport.label.contains("mqtt_subscription_failed"))
        XCTAssertFalse(fullReport.label.contains("Do not export this sentence"))
        XCTAssertFalse(fullReport.label.contains("inv_SENSITIVE_VALUE"))
    }

    func testOneShareTapOpensTheSystemSheetWithoutAnInAppConfirmation() {
        launchAndOpenConversation()
        tap(waitFor("conversation.diagnostics"))

        XCTAssertFalse(app.alerts.firstMatch.exists, "Share has no intervening confirmation")
        tap(waitFor("diagnostics.share"))

        let activity = app.otherElements["ActivityListView"]
        XCTAssertTrue(activity.waitForExistence(timeout: 10))
        if app.buttons["Close"].waitForExistence(timeout: 2) {
            app.buttons["Close"].tap()
        } else {
            app.swipeDown()
        }
        // A cancel sends nothing from the app and the capture stays shareable.
        XCTAssertTrue(waitFor("diagnostics.share").waitForExistence(timeout: 10))
        XCTAssertFalse(app.alerts.firstMatch.exists)
    }

    func testForgetDeviceReturnsToInvitation() {
        launchAndOpenConversation()
        tap(waitFor("conversation.settings"))
        XCTAssertTrue(waitFor("settings.coreRevision").exists)

        tap(waitFor("settings.forget"))
        let alert = app.alerts.firstMatch
        XCTAssertTrue(alert.waitForExistence(timeout: 10))
        let forgetButton = alert.buttons
            .matching(NSPredicate(format: "label == %@", "Forget this device"))
            .firstMatch
        XCTAssertTrue(forgetButton.waitForExistence(timeout: 5))
        forgetButton.tap()

        XCTAssertTrue(waitFor("invitation.field").waitForExistence(timeout: 15))
        XCTAssertTrue(waitFor("invitation.title").exists)
    }

    func testSettingsShowsStagingOnlyWhenRequested() {
        launchAndOpenConversation(scenario: "conversation.staging")
        tap(waitFor("conversation.settings"))
        XCTAssertTrue(waitFor("settings.staging").exists)
    }
}
