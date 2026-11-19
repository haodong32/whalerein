// Diagnostics preview and confirmed sharing, plus the destructive forget flow.

import XCTest

final class DiagnosticsSettingsUITests: WhaleReinUITestCase {
    func testDiagnosticsShowsStagesAndCancelDoesNotExport() {
        launchAndOpenConversation()
        tap(waitFor("conversation.diagnostics"))

        let preview = waitFor("diagnostics.preview")
        XCTAssertTrue(preview.label.contains("corr-https-0001"))
        XCTAssertFalse(preview.label.contains("Do not export this sentence"))
        XCTAssertFalse(preview.label.contains("inv_SENSITIVE_VALUE"))

        let mqttStage = waitFor("diagnostics.stage.studio-mac.mqtt")
        XCTAssertTrue(mqttStage.label.contains("Failed"))
        XCTAssertTrue(mqttStage.label.contains("mqtt_subscription_failed"))

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
        launchAndOpenConversation(["-ui-staging"])
        tap(waitFor("conversation.settings"))
        XCTAssertTrue(waitFor("settings.staging").exists)
    }
}
