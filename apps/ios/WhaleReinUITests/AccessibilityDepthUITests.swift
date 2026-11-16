// VoiceOver depth: code-block header meaning, tool-output announcement, unread
// count meaning, and Host-qualified session rows (QMQ-08).

import XCTest

final class AccessibilityDepthUITests: WhaleReinUITestCase {
    func testCodeHeaderAndToolOutputAnnounceMeaning() {
        launchAndOpenConversation()

        let header = revealByScrolling("code.header.msg-0320-1")
        XCTAssertEqual(header.label, "Code block")
        XCTAssertEqual(header.value as? String, "swift")

        let expand = waitFor("tool.expand.msg-0319-block-0")
        if !expand.isHittable {
            scrollConversationUp(times: 1)
        }
        XCTAssertEqual(expand.value as? String, "Show details")
        tap(expand)
        XCTAssertTrue(waitFor("tool.details.msg-0319-block-0").exists)
        let output = waitFor("tool.output.msg-0319-block-0")
        XCTAssertEqual(output.label, "Tool output")
        XCTAssertEqual(expand.value as? String, "Hide details")
    }

    func testPipeTableRendersAsAGridWithoutLiteralDelimiters() {
        launchAndOpenConversation()

        let table = revealByScrolling("markdown.table.msg-0318-1")
        XCTAssertTrue(table.exists)
        assertNoElement(containing: "| ---")
        XCTAssertTrue(app.staticTexts["Unit suites"].exists)
        XCTAssertTrue(app.staticTexts["Passed"].exists)
    }

    func testUnreadCountMeaningWhileStreaming() {
        launchApp(["-ui-stream-on-open"])
        completePairing()
        _ = waitFor("conversation.preview")

        scrollConversationUp(times: 4)
        let jump = waitFor("conversation.jumpToLatest")
        let unread = jump.value as? String ?? ""
        XCTAssertTrue(unread.contains("unread"), "Expected an unread meaning, got: \(unread)")
        XCTAssertTrue(unread.contains("message"))
    }

    func testHostQualifiedSessionRows() {
        launchApp()
        completePairing()
        openSwitcher()
        chooseHost(Self.hostId)

        let studioSession = revealSessionRow("session.row.\(Self.sessionId)")
        let studioLabel = studioSession.label
        XCTAssertTrue(
            studioLabel.contains("Host studio-mac"),
            "Session rows must name their Host, got: \(studioLabel)"
        )

        chooseHost("travel-laptop")
        let travelSession = revealSessionRow("session.row.s-101")
        let travelLabel = travelSession.label
        XCTAssertTrue(travelLabel.contains("Host travel-laptop"))
        XCTAssertNotEqual(studioLabel, travelLabel)
    }

    /// Scrolls the switcher sheet until a session row is realized. The list is
    /// dense and may lazily create rows below the medium detent.
    private func revealSessionRow(_ identifier: String) -> XCUIElement {
        let anchor = element("host.picker")
        for _ in 0..<6 {
            if element(identifier).waitForExistence(timeout: 1) {
                return element(identifier)
            }
            // Swipe from inside the sheet so the gesture scrolls the switcher
            // list rather than the transcript behind a medium-detent sheet.
            if anchor.exists {
                anchor.swipeUp()
            } else if app.tables.firstMatch.exists {
                app.tables.firstMatch.swipeUp()
            } else if app.collectionViews.firstMatch.exists {
                app.collectionViews.firstMatch.swipeUp()
            } else {
                app.swipeUp()
            }
        }
        return waitFor(identifier)
    }
}
