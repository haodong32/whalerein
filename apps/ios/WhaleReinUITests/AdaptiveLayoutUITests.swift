// Adaptive wide-layout coverage: an iPhone in landscape shows the persistent
// sidebar instead of the bottom-sheet switcher, and both Host and session
// selection work from inside the sidebar.

import XCTest

final class AdaptiveLayoutUITests: WhaleReinUITestCase {
    func testLandscapeShowsSidebarAndSwitchesConversation() {
        launchApp()
        completePairing()
        waitFor("composer.field")

        XCUIDevice.shared.orientation = .landscapeLeft

        let sidebar = waitFor("session.sidebar", timeout: 20)
        XCTAssertTrue(sidebar.exists)
        XCTAssertFalse(
            element("conversation.switcher").exists,
            "The compact switcher toolbar control must be hidden while the sidebar is visible"
        )

        let pickerBefore = waitFor("host.picker")
        XCTAssertTrue((pickerBefore.value as? String ?? "").contains("studio-mac"))
        chooseHost("travel-laptop")

        let selectedHost = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "value CONTAINS %@", "travel-laptop"),
            object: pickerBefore
        )
        XCTAssertEqual(XCTWaiter().wait(for: [selectedHost], timeout: 10), .completed)

        // The session section sits below the Host section in a short landscape
        // sidebar, so scroll the list until the target row is realized.
        let session = reveal("session.row.s-101", in: sidebar)
        tap(session)
        waitFor("composer.field")
        // Let the replacement transcript finish its initial bottom pin before
        // querying rows; the lazy stack realises the final rows during that pin.
        sleep(3)

        // The offline Host opens its cached session, so this banner and the
        // short travel transcript prove the root conversation was replaced.
        XCTAssertTrue(waitFor("conversation.cached").exists)
        XCTAssertTrue(element("message.msg-0014").waitForExistence(timeout: 10))
        let currentSession = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "label CONTAINS %@", "Selected"),
            object: session
        )
        XCTAssertEqual(XCTWaiter().wait(for: [currentSession], timeout: 10), .completed)

        // The composer must stay above the landscape keyboard with the sidebar
        // visible, so the draft field is never hidden by the keyboard.
        let field = waitFor("composer.field")
        tap(field)
        field.typeText("Landscape draft")
        let keyboard = app.keyboards.firstMatch
        XCTAssertTrue(keyboard.waitForExistence(timeout: 10))
        XCTAssertLessThanOrEqual(field.frame.maxY, keyboard.frame.minY + 2)
        XCTAssertTrue(field.isHittable)

        XCUIDevice.shared.orientation = .portrait
        let sidebarGone = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == false"),
            object: sidebar
        )
        XCTAssertEqual(XCTWaiter().wait(for: [sidebarGone], timeout: 20), .completed)
        XCTAssertTrue(waitFor("conversation.switcher", timeout: 20).exists)
    }

    /// Scrolls the sidebar list until the row exists. The landscape sidebar is
    /// shorter than its Host and session sections, and the identifier on the
    /// sidebar container resolves to its header text, so target the List's
    /// collection view explicitly.
    private func reveal(_ identifier: String, in container: XCUIElement) -> XCUIElement {
        let list = app.collectionViews.firstMatch
        for _ in 0..<5 {
            let candidate = element(identifier)
            if candidate.exists {
                return candidate
            }
            if list.exists {
                list.swipeUp()
            } else {
                container.swipeUp()
            }
        }
        return waitFor(identifier)
    }
}
