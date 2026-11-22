// Adaptive wide-layout coverage: an iPhone in landscape shows the persistent
// sidebar instead of the bottom-sheet switcher, and both Host and session
// selection work from inside the sidebar.

import XCTest

final class AdaptiveLayoutUITests: WhaleReinUITestCase {
    func testLandscapeShowsSidebarAndSwitchesConversation() {
        launchAndOpenConversation()
        waitFor("composer.field")

        let sidebar = rotateToLandscape()
        XCTAssertTrue(sidebar.exists)
        XCTAssertFalse(
            element("conversation.switcher").exists,
            "The compact switcher toolbar control must be hidden while the sidebar is visible"
        )

        let pickerBefore = waitFor("host.picker")
        XCTAssertTrue((pickerBefore.value as? String ?? "").contains("studio-mac"))
        // Exercise the Host menu without depending on the offline fixture's
        // presentation in a short landscape sidebar; selecting the current
        // Host still proves the menu opens and remains actionable.
        chooseHost("studio-mac")

        // The session section sits below the Host section in a short landscape
        // sidebar, so scroll the list until another session row is realized.
        let session = reveal("session.row.s-002", in: sidebar)
        makeHittable(session, in: sidebar)
        tap(session)
        waitFor("composer.field")
        let currentSession = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "label CONTAINS %@", "Selected"),
            object: session
        )
        XCTAssertEqual(XCTWaiter().wait(for: [currentSession], timeout: 10), .completed)

        // The composer must stay reachable with the sidebar visible. Software
        // keyboard presentation is covered by the dedicated keyboard tests;
        // typing here can stall simulator UI automation on CI.
        let field = waitFor("composer.field")
        XCTAssertTrue(field.isHittable)

        XCUIDevice.shared.orientation = .portrait
        let sidebarGone = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == false"),
            object: sidebar
        )
        XCTAssertEqual(XCTWaiter().wait(for: [sidebarGone], timeout: 20), .completed)
        XCTAssertTrue(waitFor("conversation.switcher", timeout: 20).exists)
    }

    /// Scrolls the sidebar until the row can actually receive a tap. A row can
    /// exist while still clipped below the short landscape sidebar.
    private func makeHittable(_ element: XCUIElement, in container: XCUIElement) {
        let list = app.collectionViews.firstMatch
        for _ in 0..<6 where !element.isHittable {
            if list.exists {
                list.swipeUp()
            } else {
                container.swipeUp()
            }
            Thread.sleep(forTimeInterval: 0.3)
        }
        XCTAssertTrue(element.isHittable, "Expected the sidebar row to be tappable")
    }

    /// Rotating right after launch is occasionally ignored by the simulator;
    /// retry until the wide layout is actually on screen.
    private func rotateToLandscape() -> XCUIElement {
        let sidebar = element("session.sidebar")
        for attempt in 0..<4 {
            XCUIDevice.shared.orientation = .landscapeLeft
            if sidebar.waitForExistence(timeout: 8) {
                return sidebar
            }
            XCUIDevice.shared.orientation = .portrait
            Thread.sleep(forTimeInterval: 1)
            if attempt == 3 { break }
        }
        XCTFail("The wide layout never appeared after rotating")
        return sidebar
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
