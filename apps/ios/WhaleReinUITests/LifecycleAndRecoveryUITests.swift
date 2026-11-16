// Lifecycle, recovery, revocation and approval-refresh journeys against the
// deterministic fixture app. No external stack is required.

import XCTest

final class LifecycleAndRecoveryUITests: WhaleReinUITestCase {
    /// Pairing that lands on the revoked blocking state instead of a composer,
    /// so the helper must not wait for `composer.field`.
    private func pairWithoutWaitingForConversation() {
        let field = waitFor("invitation.field")
        tap(field)
        field.typeText(Self.invitationLink + "\n")
        tap(waitFor("pairing.confirm"))
    }

    private func typeAndSend(_ text: String) {
        let field = waitFor("composer.field")
        tap(field)
        field.typeText(text)
        tap(waitFor("composer.send"))
    }

    func testBackgroundResumeKeepsDraftAndRefreshesConversation() {
        launchApp()
        completePairing()
        let field = waitFor("composer.field")
        tap(field)
        field.typeText("resume draft")

        XCUIDevice.shared.press(.home)
        Thread.sleep(forTimeInterval: 1.5)
        app.activate()

        let restored = waitFor("composer.field", timeout: 20)
        XCTAssertEqual(restored.value as? String, "resume draft")
        XCTAssertFalse(element("privacy.shield").exists)
        XCTAssertTrue(waitFor("message.msg-0320").waitForExistence(timeout: 20))
    }

    func testRemoteRevocationBlocksConversationAndPairsAgain() {
        launchApp(["-ui-revoked-on-open"])
        pairWithoutWaitingForConversation()

        XCTAssertTrue(waitFor("revoked.title", timeout: 20).waitForExistence(timeout: 20))
        XCTAssertFalse(element("composer.field").exists)

        tap(waitFor("revoked.pair"))
        XCTAssertTrue(waitFor("invitation.field", timeout: 15).exists)
        XCTAssertFalse(element("revoked.title").exists)
    }

    func testRemoteRevocationForgetReturnsToOnboarding() {
        launchApp(["-ui-revoked-on-open"])
        pairWithoutWaitingForConversation()
        _ = waitFor("revoked.title", timeout: 20)

        tap(waitFor("revoked.forget"))
        XCTAssertTrue(waitFor("invitation.field", timeout: 15).exists)
        XCTAssertFalse(element("revoked.title").exists)
    }

    func testWatchEndedDisablesSendAndReconnects() {
        launchApp(["-ui-watch-ended"])
        completePairing()
        _ = waitFor("conversation.watchEnded")

        let field = waitFor("composer.field")
        tap(field)
        field.typeText("blocked while ended")
        let send = waitFor("composer.send")
        XCTAssertFalse(send.isEnabled, "Send must be disabled while the watch has ended")

        tap(waitFor("composer.reconnect"))
        XCTAssertTrue(waitForDisappearance("conversation.watchEnded", timeout: 15))
        XCTAssertTrue(waitForEnabled("composer.send", timeout: 15).isEnabled)
    }

    func testUncertainSendJourneyShowsUncertainThenSettles() {
        launchApp(["-ui-send-uncertain"])
        completePairing()

        typeAndSend("uncertain journey")
        let status = waitFor("conversation.sendStatus")
        XCTAssertEqual(
            status.label,
            "Delivery is uncertain. The original request will be recovered."
        )

        let accepted = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "label == %@", "Sent"),
            object: element("conversation.sendStatus")
        )
        XCTAssertEqual(XCTWaiter().wait(for: [accepted], timeout: 15), .completed)
        XCTAssertTrue(app.staticTexts["uncertain journey"].waitForExistence(timeout: 10))
    }

    func testEchoedSendDoesNotDuplicateTheBubble() {
        launchApp(["-ui-send-echo"])
        completePairing()

        typeAndSend("single bubble")
        XCTAssertTrue(app.staticTexts["single bubble"].waitForExistence(timeout: 10))
        Thread.sleep(forTimeInterval: 1.5)

        XCTAssertEqual(
            countElements(label: "single bubble"),
            1,
            "A Core echo must replace the optimistic bubble instead of adding a duplicate"
        )
    }

    func testOfflineApprovalDisablesDecisionWithReason() {
        launchApp(["-ui-offline"])
        completePairing()
        _ = waitFor("approval.card.\(Self.approvalId)")

        let reason = waitFor("approval.disabledReason.\(Self.approvalId)")
        XCTAssertEqual(reason.label, "Reconnect to decide")
        XCTAssertFalse(waitFor("approval.allowOnce").isEnabled)
        XCTAssertFalse(waitFor("approval.reject").isEnabled)
    }

    func testShortLivedApprovalExpiresOnTheTimer() {
        launchApp(["-ui-approval-state", "short-lived"])
        completePairing()
        _ = waitFor("approval.card.\(Self.approvalId)")

        let state = element("approval.state.\(Self.approvalId)")
        let expired = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "value == %@", "This request expired. Review the latest request."),
            object: state
        )
        XCTAssertEqual(
            XCTWaiter().wait(for: [expired], timeout: 12),
            .completed,
            "The approval timer must advance expiry without a manual redraw"
        )
        XCTAssertFalse(app.buttons["approval.allowOnce"].exists)
    }

    func testFastClockApprovalStaysActionable() {
        launchApp(["-ui-approval-state", "fast-clock"])
        completePairing()
        _ = waitFor("approval.card.\(Self.approvalId)")

        let allow = waitForEnabled("approval.allowOnce", timeout: 15)
        XCTAssertEqual(
            element("approval.state.\(Self.approvalId)").value as? String,
            "Waiting for your decision"
        )
        tap(allow)

        let allowed = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "value == %@", "Allowed once"),
            object: element("approval.state.\(Self.approvalId)")
        )
        XCTAssertEqual(XCTWaiter().wait(for: [allowed], timeout: 10), .completed)
    }

    func testUnavailableApprovalShowsSessionUnavailable() {
        launchApp(["-ui-approval-state", "unavailable"])
        completePairing()
        revealByScrolling("approval.card.\(Self.approvalId)")

        XCTAssertEqual(
            element("approval.state.\(Self.approvalId)").value as? String,
            "Session unavailable"
        )
        XCTAssertTrue(waitFor("approval.unavailable.\(Self.approvalId)").exists)
        XCTAssertFalse(app.buttons["approval.allowOnce"].exists)
        XCTAssertFalse(app.buttons["approval.reject"].exists)
    }

    func testDecidedApprovalSurvivesAPendingOnlyCoreRefresh() {
        launchApp(["-ui-approval-refresh-after-decision"])
        completePairing()
        _ = waitFor("approval.card.\(Self.approvalId)")

        tap(waitFor("approval.allowOnce"))
        let allowed = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "value == %@", "Allowed once"),
            object: element("approval.state.\(Self.approvalId)")
        )
        XCTAssertEqual(XCTWaiter().wait(for: [allowed], timeout: 10), .completed)

        Thread.sleep(forTimeInterval: 1.2)
        XCTAssertTrue(element("approval.card.\(Self.approvalId)").exists)
        XCTAssertEqual(
            element("approval.state.\(Self.approvalId)").value as? String,
            "Allowed once"
        )
    }

    func testIncompatibleProtocolShowsUpgradeGuidance() {
        launchApp(["-ui-incompatible-protocol"])
        let field = waitFor("invitation.field")
        tap(field)
        field.typeText(Self.invitationLink + "\n")
        tap(waitFor("pairing.confirm"))

        let guidance = waitFor("conversation.incompatible", timeout: 20)
        XCTAssertTrue(guidance.label.contains("protocol version 2"))
        XCTAssertTrue(guidance.label.contains("supports version 1"))
        XCTAssertFalse(element("composer.field").exists)
    }

    func testLegacyHostWithoutProtocolVersionShowsUpgradeGuidance() {
        launchApp(["-ui-legacy-protocol"])
        let field = waitFor("invitation.field")
        tap(field)
        field.typeText(Self.invitationLink + "\n")
        tap(waitFor("pairing.confirm"))

        let guidance = waitFor("conversation.incompatible", timeout: 20)
        XCTAssertTrue(guidance.label.contains("older peer"), "Got: \(guidance.label)")
        XCTAssertTrue(waitFor("conversation.incompatibleGuidance").exists)
        XCTAssertFalse(element("composer.field").exists)
    }

    func testAddHostEntryPointFromSettings() {
        launchApp()
        completePairing()
        tap(waitFor("conversation.settings"))
        tap(waitFor("settings.addHost"))

        XCTAssertTrue(waitFor("invitation.field", timeout: 20).exists)
        XCTAssertTrue(waitFor("invitation.close").exists)
        tap(waitFor("invitation.close"))
        XCTAssertTrue(waitFor("composer.field", timeout: 15).exists)
    }

    func testRevokeCancelKeepsDevicePaired() {
        launchApp()
        completePairing()
        tap(waitFor("conversation.settings"))
        tap(waitFor("settings.revoke"))

        let alert = app.alerts.firstMatch
        XCTAssertTrue(alert.waitForExistence(timeout: 10))
        alert.buttons
            .matching(NSPredicate(format: "label == %@", "Cancel"))
            .firstMatch
            .tap()

        XCTAssertTrue(waitFor("settings.revoke").exists)
        XCTAssertFalse(element("invitation.field").exists)
        tap(waitFor("settings.done"))
        XCTAssertTrue(waitFor("composer.field").exists)
    }

    func testRevokeConfirmReturnsToInvitation() {
        launchApp()
        completePairing()
        tap(waitFor("conversation.settings"))
        tap(waitFor("settings.revoke"))

        let alert = app.alerts.firstMatch
        XCTAssertTrue(alert.waitForExistence(timeout: 10))
        let confirm = alert.buttons
            .matching(NSPredicate(format: "label == %@", "Revoke this device"))
            .firstMatch
        XCTAssertTrue(confirm.waitForExistence(timeout: 5))
        confirm.tap()

        XCTAssertTrue(waitFor("invitation.field", timeout: 20).exists)
        XCTAssertFalse(element("composer.field").exists)
    }

    func testDiagnosticsFailureShowsRetryBranch() {
        launchApp(["-ui-diagnostics-failure"])
        completePairing()
        tap(waitFor("conversation.diagnostics"))

        XCTAssertTrue(waitFor("diagnostics.error", timeout: 15).exists)
        XCTAssertTrue(waitFor("diagnostics.retry").exists)
        tap(waitFor("diagnostics.retry"))
        XCTAssertTrue(waitFor("diagnostics.error", timeout: 15).exists)
    }

    func testLockedKeychainShowsRecoverableGuidance() {
        launchApp(["-ui-keychain-locked"])

        XCTAssertTrue(waitFor("keychain.locked.title", timeout: 20).exists)
        XCTAssertTrue(waitFor("keychain.locked.error").exists)
        XCTAssertFalse(element("invitation.field").exists, "The pairing must be preserved")

        tap(waitFor("keychain.retry"))
        XCTAssertTrue(waitFor("keychain.locked.title", timeout: 20).exists)
        XCTAssertFalse(element("composer.field").exists)
    }

    func testChineseInvalidInvitationErrorIsLocalized() {
        launchApp(["-AppleLanguages", "(zh-Hans)", "-AppleLocale", "zh_Hans"])
        let field = waitFor("invitation.field")
        tap(field)
        field.typeText("not a whalerein link")
        tap(waitFor("invitation.continue"))

        XCTAssertEqual(waitFor("invitation.error").label, "此邀请链接无效。")
    }
}
