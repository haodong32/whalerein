// Approval card behavior: allow, reject, changed and expired variants update the
// original card and never authorize a stale decision.

import XCTest

final class ApprovalUITests: WhaleReinUITestCase {
    private func stateValue() -> String {
        (element("approval.state.\(Self.approvalId)").value as? String) ?? ""
    }

    func testAllowOnceUpdatesOriginalCard() {
        launchAndOpenConversation()
        waitFor("approval.card.\(Self.approvalId)")
        XCTAssertEqual(stateValue(), "Waiting for your decision")

        tap(waitFor("approval.allowOnce"))

        let allowed = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "value == %@", "Allowed once"),
            object: element("approval.state.\(Self.approvalId)")
        )
        XCTAssertEqual(XCTWaiter().wait(for: [allowed], timeout: 10), .completed)
        XCTAssertEqual(element("approval.card.\(Self.approvalId)").identifier, "approval.card.\(Self.approvalId)")
        XCTAssertFalse(app.buttons["approval.allowOnce"].exists)
        XCTAssertFalse(app.buttons["approval.reject"].exists)
    }

    func testRejectUpdatesOriginalCard() {
        launchAndOpenConversation()
        waitFor("approval.card.\(Self.approvalId)")

        tap(waitFor("approval.reject"))

        let rejected = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "value == %@", "Rejected"),
            object: element("approval.state.\(Self.approvalId)")
        )
        XCTAssertEqual(XCTWaiter().wait(for: [rejected], timeout: 10), .completed)
        XCTAssertFalse(app.buttons["approval.allowOnce"].exists)
    }

    func testChangedApprovalRequiresAFreshDecision() {
        launchAndOpenConversation(["-ui-approval-state", "changed"])
        waitFor("approval.card.\(Self.approvalId)")

        tap(waitFor("approval.allowOnce"))

        let stale = waitFor("approval.error.\(Self.approvalId)")
        XCTAssertTrue(stale.label.contains("no longer matches"))
        XCTAssertTrue(app.buttons["approval.allowOnce"].exists, "A changed request stays reviewable")

        tap(waitFor("approval.allowOnce"))
        let allowed = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "value == %@", "Allowed once"),
            object: element("approval.state.\(Self.approvalId)")
        )
        XCTAssertEqual(XCTWaiter().wait(for: [allowed], timeout: 10), .completed)
    }

    func testExpiredApprovalOffersNoDecision() {
        launchAndOpenConversation(["-ui-approval-state", "expired"])
        // The approval card is the last row of a long lazy transcript; scroll to
        // it when the initial bottom pin has not realised it yet.
        revealByScrolling("approval.card.\(Self.approvalId)")

        XCTAssertEqual(stateValue(), "This request expired. Review the latest request.")
        XCTAssertFalse(app.buttons["approval.allowOnce"].exists)
        XCTAssertFalse(app.buttons["approval.reject"].exists)
    }

    func testApprovalLabelsIdentifyToolAndState() {
        launchAndOpenConversation()
        waitFor("approval.card.\(Self.approvalId)")

        let allow = waitFor("approval.allowOnce")
        XCTAssertFalse(allow.label.isEmpty)
        XCTAssertEqual(allow.value as? String, "bash")
        XCTAssertEqual(stateValue(), "Waiting for your decision")
        XCTAssertTrue(element("approval.card.\(Self.approvalId)").label.contains("bash")
            || element("approval.card.\(Self.approvalId)").staticTexts["bash"].exists
            || app.staticTexts["bash"].exists)
    }
}
