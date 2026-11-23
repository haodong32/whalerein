// Pending-question flow over deterministic fixtures: select an option, review
// explicitly, submit, and observe the answered state. No polling timer drives
// discovery; the fixture publishes Core-owned question state with the watch.

import XCTest

final class QuestionsUITests: WhaleReinUITestCase {
    private static let batchID = "question-release-s-001"

    func testQuestionRequiresReviewBeforeAnswering() {
        launchAndOpenConversation(scenario: "questions.pending")

        XCTAssertTrue(
            revealByScrolling("question.card.\(Self.batchID)").exists,
            "the pending question card did not appear"
        )
        let option = revealByScrolling("question.option.question-channel.0")
        XCTAssertEqual(option.value as? String, "Not selected")
        tap(option)
        XCTAssertEqual(option.value as? String, "Selected")

        // An answer is not sent until the explicit review snapshot is confirmed.
        tap(revealByScrolling("question.review.\(Self.batchID)"))
        waitFor("question.review.section.\(Self.batchID)")
        XCTAssertTrue(app.descendants(matching: .any)
            .matching(identifier: "question.submit.\(Self.batchID)").firstMatch.exists)

        tap(waitFor("question.submit.\(Self.batchID)"))
        let status = waitFor("questions.answerState")
        let submitted = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "value == %@", "Answer submitted."),
            object: status
        )
        XCTAssertEqual(XCTWaiter().wait(for: [submitted], timeout: 10), .completed)
        XCTAssertFalse(
            app.descendants(matching: .any)
                .matching(identifier: "question.card.\(Self.batchID)").firstMatch.exists,
            "an answered batch stayed on screen as pending"
        )
    }

    func testSubmittedCardStaysReadOnlyUntilHostRemovesBatch() {
        launchAndOpenConversation(scenario: "questions.pending.slow-removal")

        let option = revealByScrolling("question.option.question-channel.0")
        tap(option)
        tap(revealByScrolling("question.review.\(Self.batchID)"))
        tap(waitFor("question.submit.\(Self.batchID)"))

        // The Host keeps the batch observable briefly. The reviewed card must
        // stay in place, read-only, instead of falling back to an empty review.
        XCTAssertTrue(waitFor("question.submitted.\(Self.batchID)").exists)
        XCTAssertFalse(
            app.descendants(matching: .any)
                .matching(identifier: "question.submit.\(Self.batchID)").firstMatch.exists,
            "a submitted card must not offer Submit again"
        )
        XCTAssertTrue(
            waitForDisappearance("question.card.\(Self.batchID)"),
            "the Host removal should close the answered batch"
        )
    }

    func testQuestionCardExposesAccessibleOptionLabels() {
        launchAndOpenConversation(scenario: "questions.pending")
        let option = revealByScrolling("question.option.question-channel.0")
        XCTAssertTrue(option.label.contains("Which release channel"))
        XCTAssertTrue(option.label.contains("Stable"))
        XCTAssertEqual(option.value as? String, "Not selected")
    }
}
