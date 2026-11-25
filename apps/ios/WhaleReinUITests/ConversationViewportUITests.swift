import XCTest

final class ConversationViewportUITests: WhaleReinUITestCase {
    private let scenario = "conversation.delayed-viewport"

    func testSendAcrossFirstPageBoundaryKeepsReadingPosition() throws {
        launchAndOpenConversation(scenario: scenario)
        _ = waitFor("message.msg-0320")
        scrollConversationUp(times: 1)
        try sendWhileReading("Viewport fixture send", evidencePrefix: "first-page-send")
    }

    func testAutomaticPrependPreservesVisibleRow() throws {
        launchAndOpenConversation(scenario: scenario)
        _ = waitFor("message.msg-0320")
        let loading = element("history.loadingEarlier")
        for _ in 0..<15 where !loading.exists {
            scrollConversationUp(times: 1)
        }
        XCTAssertTrue(loading.waitForExistence(timeout: 5))
        let anchor = try visibleAnchor()
        capture("prepend-in-flight")
        // Accessibility can briefly omit the loading indicator and the
        // transcript rows while rendering a new snapshot.
        waitForRetainedAnchor(anchor, after: loading)
        capture("prepend-settled")
        scrollConversationUp(times: 1)
        let older = try visibleAnchor()
        XCTAssertLessThan(older.identifier, "message.msg-0271")
        try sendWhileReading("Long transcript send", evidencePrefix: "long-transcript-send")
    }

    func testAnswerAndAuthoritativeRemovalKeepCommittedRowsVisible() throws {
        launchAndOpenConversation(scenario: scenario)
        tap(revealByScrolling("question.option.question-channel.0"))
        tap(revealByScrolling("question.review.question-release-s-001"))
        let submit = waitFor("question.submit.question-release-s-001")
        XCTAssertTrue(submit.isEnabled)
        let scroll = waitFor("conversation.scroll")
        let from = scroll.coordinate(withNormalizedOffset: CGVector(dx: 0.8, dy: 0.41))
        let to = scroll.coordinate(withNormalizedOffset: CGVector(dx: 0.8, dy: 0.52))
        from.press(forDuration: 0.05, thenDragTo: to, withVelocity: .slow, thenHoldForDuration: 0.15)
        // Keep the reviewed answer reachable while reading the rows above it.
        XCTAssertTrue(submit.isHittable)
        let anchor = try visibleAnchor()
        capture("answer-before")
        submit.tap()
        _ = waitFor("question.submitting.question-release-s-001", timeout: 3)
        assertRetained(anchor)
        capture("answer-pending")
        XCTAssertTrue(waitForDisappearance("question.card.question-release-s-001"))
        assertRetained(anchor)
        capture("answer-settled")
        XCTAssertTrue(waitFor("composer.field").isEnabled)
    }

    private func sendWhileReading(_ text: String, evidencePrefix: String) throws {
        let anchor = try visibleAnchor()
        capture("\(evidencePrefix)-before-keyboard")
        let field = waitFor("composer.field")
        tap(field)
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5))
        assertRetained(anchor)
        field.typeText(text)
        assertRetained(anchor)
        capture("\(evidencePrefix)-before-receipt")
        tap(waitFor("composer.send"))
        assertRetained(anchor)
        capture("\(evidencePrefix)-pending")

        // The committed echo can be below the realised accessibility subtree.
        assertRetained(anchor, settledFor: 5)
        capture("\(evidencePrefix)-settled")
        tap(waitFor("conversation.jumpToLatest"))
        _ = waitFor("message.msg-echo-cmd-0001")
        XCTAssertTrue(waitForDisappearance("conversation.jumpToLatest"))
    }

    private struct ReadingAnchor {
        let identifier: String
        let offset: CGFloat
    }

    private func visibleAnchor() throws -> ReadingAnchor {
        let viewport = waitFor("conversation.scroll").frame
        let rows = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "message.")
        )
        let readableRegion = CGRect(
            x: viewport.minX, y: viewport.minY + 64,
            width: viewport.width, height: viewport.height * 0.45 - 64
        )
        let candidates = rows.allElementsBoundByIndex.compactMap { row -> ReadingAnchor? in
            let frame = row.frame
            guard frame.intersection(readableRegion).height > 8 else { return nil }
            return ReadingAnchor(identifier: row.identifier, offset: frame.minY - viewport.minY)
        }
        return try XCTUnwrap(candidates.min { $0.offset < $1.offset })
    }

    private func waitForRetainedAnchor(
        _ anchor: ReadingAnchor,
        after loading: XCUIElement,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        let viewport = element("conversation.scroll")
        let row = element(anchor.identifier)
        let deadline = Date().addingTimeInterval(25)
        var settledSamples = 0
        repeat {
            var settled = false
            if !loading.exists, row.exists, viewport.exists {
                let frame = row.frame
                let viewportFrame = viewport.frame
                settled = frame.intersection(viewportFrame).height > 8
                    && abs(frame.minY - viewportFrame.minY - anchor.offset) <= 1
            }
            settledSamples = settled ? settledSamples + 1 : 0
            if settledSamples == 2 { return }
            RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        } while Date() < deadline
        capture("prepend-did-not-settle")
        XCTAssertFalse(loading.exists, "Older history did not finish loading", file: file, line: line)
        assertRetained(anchor, file: file, line: line)
    }

    private func assertRetained(
        _ anchor: ReadingAnchor,
        settledFor duration: TimeInterval = 0,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        let viewport = element("conversation.scroll")
        let row = element(anchor.identifier)
        let deadline = Date().addingTimeInterval(duration)
        repeat {
            XCTAssertTrue(row.exists, "Expected the reading anchor to stay realised", file: file, line: line)
            let frame = row.frame
            XCTAssertGreaterThan(frame.intersection(viewport.frame).height, 8, file: file, line: line)
            XCTAssertEqual(frame.minY - viewport.frame.minY, anchor.offset, accuracy: 1, file: file, line: line)
            if Date() < deadline {
                RunLoop.current.run(until: Date().addingTimeInterval(0.15))
            }
        } while Date() < deadline
    }

    private func capture(_ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
