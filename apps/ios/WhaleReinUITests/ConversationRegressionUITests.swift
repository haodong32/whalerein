import XCTest

final class ConversationRegressionUITests: WhaleReinUITestCase {
    func testOutgoingBubbleRemainsVisibleThroughReplyWithoutEcho() {
        launchAndOpenConversation(scenario: "conversation.reply-without-echo")
        let field = waitFor("composer.field")
        tap(field)
        let prompt = "Outgoing fixture prompt"
        field.typeText(prompt)
        tap(waitFor("composer.send"))
        _ = waitFor("conversation.sendStatus")

        continueAfterFailure = true
        XCTAssertEqual(countElements(label: prompt), 1, "After acceptance")
        _ = waitFor("conversation.preview")
        XCTAssertEqual(countElements(label: prompt), 1, "While streaming")
        let reply = app.staticTexts["Completed fixture reply"]
        XCTAssertTrue(reply.waitForExistence(timeout: 15))
        XCTAssertEqual(countElements(label: prompt), 1, "After the final reply")
        let outgoing = app.staticTexts[prompt]
        if outgoing.exists, reply.exists {
            XCTAssertLessThan(outgoing.frame.minY, reply.frame.minY)
        }
    }

    func testShortHistoryOffersRetryThenContinueUntilExhausted() {
        launchAndOpenConversation(scenario: "conversation.short-history-retry")
        let original = waitFor("message.short-3")
        let field = waitFor("composer.field")
        tap(field)
        field.typeText("Fixture draft")
        let viewport = waitFor("conversation.scroll")
        let retry = waitFor("history.continueLoading")
        _ = waitFor("history.error")
        XCTAssertTrue(original.exists)
        let initialOffset = original.frame.minY - viewport.frame.minY

        tap(retry)
        _ = waitFor("message.short-2")
        XCTAssertTrue(waitForDisappearance("history.error"))
        XCTAssertTrue(original.exists)
        XCTAssertEqual(original.frame.minY - viewport.frame.minY, initialOffset, accuracy: 1)
        tap(waitFor("history.continueLoading"))
        _ = waitFor("message.short-1")
        XCTAssertTrue(waitForDisappearance("history.continueLoading"))
        XCTAssertTrue(original.exists)
        XCTAssertEqual(original.frame.minY - viewport.frame.minY, initialOffset, accuracy: 1)
        XCTAssertEqual(field.value as? String, "Fixture draft")
    }
}
