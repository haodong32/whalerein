// Core navigation and conversation flows: pairing, host/session selection,
// reading while streaming, sending idle/running, tool expansion, copy and jump.

import XCTest

final class NavigationAndConversationUITests: WhaleReinUITestCase {
    func testPairingToConversation() {
        launchApp()
        waitForLabel("Pair your iPhone")
        completePairing()

        // The paired app restores a conversation directly; the toolbar switcher
        // is the only Host/session entry point.
        let connection = waitFor("host.connectionStatus")
        XCTAssertEqual(connection.value as? String, "Connected")
        XCTAssertTrue(element("message.msg-0320").waitForExistence(timeout: 10))
        XCTAssertTrue(element("approval.card.\(Self.approvalId)").exists)

        openSwitcher()
        let picker = waitFor("host.picker")
        XCTAssertTrue((picker.value as? String ?? "").contains("Online"))
        XCTAssertTrue((picker.value as? String ?? "").contains("Selected"))
        tap(waitFor("switcher.close"))
        waitFor("composer.field")
    }

    func testSendWhileIdleAndWhileRunning() {
        launchAndOpenConversation()

        let field = waitFor("composer.field")
        tap(field)
        field.typeText("UI test message one")
        tap(waitFor("composer.send"))

        XCTAssertTrue(app.staticTexts["UI test message one"].waitForExistence(timeout: 10))
        let running = waitFor("conversation.running")
        XCTAssertTrue(running.exists)
        let delivery = waitFor("composer.delivery")
        // Ordinary Send defaults to the native queue, matching the CLI. Steer
        // stays an explicit choice when the Source advertises it.
        XCTAssertEqual(delivery.value as? String, "Queue", "Running work should default to Queue")
        _ = waitFor("conversation.sendStatus")

        field.tap()
        field.typeText("UI test message two")
        tap(waitFor("composer.send"))
        XCTAssertTrue(app.staticTexts["UI test message two"].waitForExistence(timeout: 10))
    }

    func testCopyCodeBlockShowsConfirmation() {
        launchAndOpenConversation()

        let copy = waitFor("code.copy.msg-0320-1")
        if !copy.isHittable {
            scrollConversationUp(times: 1)
        }
        tap(copy)
        XCTAssertTrue(waitFor("code.copied").exists)
    }

    func testToolResultShowsCollapsedSummary() {
        launchAndOpenConversation(scenario: "conversation.baseline")

        // Expansion is covered by the model's scroll-anchor tests; this flow
        // checks the collapsed row's meaning without depending on hit-testing.
        let expand = revealByScrolling("tool.expand.msg-0319-block-0")
        XCTAssertEqual(expand.value as? String, "Show details")
        XCTAssertFalse(element("tool.details.msg-0319-block-0").exists)
    }

    func testReadingOlderRowsWhileStreamingRetainsAnchorAndJumps() {
        // Restoring the conversation starts the scripted stream.
        // Opening the switcher first could let the short stream end.
        launchAndOpenConversation(scenario: "conversation.streaming")
        XCTAssertTrue(waitFor("conversation.preview").exists)

        scrollConversationUp(times: 4)
        guard let anchor = firstVisibleMessageRow() else {
            XCTFail("Expected a visible message row after scrolling up")
            return
        }
        let identifier = anchor.identifier
        let initialFrame = anchor.frame

        Thread.sleep(forTimeInterval: 2.0)

        let restored = element(identifier)
        XCTAssertTrue(restored.exists, "The row being read should remain realised")
        XCTAssertEqual(Double(restored.frame.minY), Double(initialFrame.minY), accuracy: 8, "The reading position should not move while streaming")

        // The jump affordance must appear while output is still streaming.
        XCTAssertTrue(waitFor("conversation.jumpToLatest").exists)

        // Let the scripted stream settle before jumping. Tapping mid-update races
        // the animation on a loaded Simulator; the reading-position assertion
        // above already covers the streaming case.
        XCTAssertTrue(
            waitForDisappearance("conversation.preview", timeout: 40),
            "The scripted stream did not finish"
        )

        let jump = waitFor("conversation.jumpToLatest")
        tap(jump)
        XCTAssertTrue(
            waitForDisappearance("conversation.jumpToLatest", timeout: 15),
            "Jump to latest stayed visible after returning to the end of the transcript"
        )
    }

    func testSwitchingHostsDropsLateEventsFromThePreviousWatch() {
        launchAndOpenConversation(scenario: "conversation.late-event")
        _ = waitFor("approval.card.\(Self.approvalId)")

        openSwitcher()
        chooseHost("travel-laptop")
        let session = waitFor("session.row.s-101")
        tap(session)
        waitFor("composer.field")

        Thread.sleep(forTimeInterval: 2.5)
        assertNoElement(containing: "LATE-EVENT-LEAK")
        XCTAssertTrue(waitFor("message.msg-0014").exists)
    }
}
