// Question review recovery: a Host rejection or changed guard invalidates the
// reviewed snapshot instead of repairing it locally, so the user must review a
// fresh answer. An uncertain answer stays cancellable by the exact original
// command identity through the live binding surface.

import Foundation
import Testing
@testable import WhaleRein

@MainActor
private func questionBatch(version: UInt64 = 2) -> QuestionBatch {
    let base = FixtureCatalog.questionBatch(sessionId: "s-001")
    return QuestionBatch(
        sessionID: base.sessionID,
        batchID: base.batchID,
        sourceEpoch: base.sourceEpoch,
        version: version,
        digestHex: String(repeating: "a1", count: 32),
        phase: base.phase,
        questions: base.questions,
        answerable: base.answerable,
        contentUnavailable: base.contentUnavailable
    )
}

@MainActor
private func currentQuestions(_ batch: QuestionBatch) -> QuestionsState {
    QuestionsState(
        freshness: .current,
        stamp: QuestionStamp(observationEpoch: "epoch-1", revision: 1),
        batches: [batch],
        error: nil
    )
}

@MainActor
private struct QuestionHarness {
    let service: StubSessionService
    let model: ConversationModel
    var sequence = 0

    init() async {
        service = StubSessionService()
        model = await makeStartedConversation(service: service)
        emit(.reset(messages: [], approvals: []))
    }

    mutating func emit(_ event: MessageEvent) {
        sequence += 1
        service.emit(event, sequence: sequence)
    }

    mutating func emitQuestions(_ state: QuestionsState) {
        emit(.questions(state))
    }

    mutating func startReview(_ batch: QuestionBatch, label: String = "Stable") {
        model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: label
        )
        _ = model.beginQuestionReview(batchId: batch.batchID)
    }
}

@Suite("Question review recovery")
@MainActor
struct QuestionReviewRecoveryTests {
    @Test("a rejected reviewed answer requires a fresh review")
    func rejectedAnswerRequiresFreshReview() async {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(currentQuestions(batch))
        await waitUntil { harness.model.hasPendingQuestions }
        harness.startReview(batch)
        harness.service.answerQuestionHandler = { _, _, _, _ in
            CommandReceipt(
                commandId: "cmd-rejected",
                requestId: "req-rejected",
                status: .rejected,
                delivery: nil
            )
        }

        await harness.model.submitQuestionReview()
        #expect(harness.model.questionAnswerState == .failed)
        #expect(harness.model.questionErrorKey == .questionsAnswerRejected)
        #expect(harness.model.questionReview == nil)
        #expect(harness.service.answerQuestionCalls.count == 1)

        // The same stale snapshot cannot be resubmitted.
        await harness.model.submitQuestionReview()
        #expect(harness.service.answerQuestionCalls.count == 1)

        // A fresh review of the still-current guard is accepted.
        harness.service.answerQuestionHandler = { _, _, _, _ in
            CommandReceipt(
                commandId: "cmd-fresh",
                requestId: "req-fresh",
                status: .confirmed,
                delivery: nil
            )
        }
        harness.startReview(batch)
        await harness.model.submitQuestionReview()
        #expect(harness.service.answerQuestionCalls.count == 2)
        #expect(harness.model.questionAnswerState == .submitted(.confirmed))
    }

    @Test("a changed Host guard invalidates the review without local repair")
    func changedGuardRequiresFreshReview() async {
        var harness = await QuestionHarness()
        let original = questionBatch(version: 2)
        let changed = questionBatch(version: 3)
        harness.emitQuestions(currentQuestions(original))
        await waitUntil { harness.model.hasPendingQuestions }
        harness.startReview(original)
        #expect(harness.model.questionReview?.guardValue == original.guardValue)

        harness.emitQuestions(currentQuestions(changed))
        await waitUntil { harness.model.questions.batches.first?.version == 3 }

        #expect(harness.model.questionReview == nil)
        #expect(harness.model.questionErrorKey == .questionsGuardChanged)
        #expect(harness.model.questions.batches.first?.guardValue == changed.guardValue)
        #expect(
            harness.model.questionSelection(batchId: original.batchID, itemId: "question-channel")
                .isEmpty
        )

        // Submitting the dismissed review performs no native call.
        await harness.model.submitQuestionReview()
        #expect(harness.service.answerQuestionCalls.isEmpty)

        // A fresh review binds the new guard, not the old one.
        harness.service.answerQuestionHandler = { _, _, _, _ in
            CommandReceipt(
                commandId: "cmd-fresh-guard",
                requestId: "req-fresh-guard",
                status: .confirmed,
                delivery: nil
            )
        }
        harness.startReview(changed)
        await harness.model.submitQuestionReview()
        #expect(harness.service.answerQuestionCalls.count == 1)
        #expect(harness.service.answerQuestionCalls.first?.2.guardValue == changed.guardValue)
    }

    @Test("an uncertain answer is cancelled by its original command identity")
    func cancellationUsesOriginalCommandIdentity() async {
        var harness = await QuestionHarness()
        let batch = questionBatch(version: 2)
        harness.emitQuestions(currentQuestions(batch))
        await waitUntil { harness.model.hasPendingQuestions }
        harness.startReview(batch)
        harness.service.answerQuestionHandler = { _, _, _, _ in
            CommandReceipt(
                commandId: "cmd-uncertain-42",
                requestId: "req-uncertain-42",
                status: .uncertain,
                delivery: nil
            )
        }
        await harness.model.submitQuestionReview()
        #expect(harness.model.questionAnswerState == .uncertain(commandId: "cmd-uncertain-42"))
        #expect(harness.model.canCancelQuestionAnswer)

        // The observation changes while the answer is still in flight.
        harness.emitQuestions(currentQuestions(questionBatch(version: 3)))
        await waitUntil { harness.model.questions.batches.first?.version == 3 }
        #expect(harness.model.questionReview == nil)

        harness.service.requestQuestionCancelHandler = { _, _, commandID in
            QuestionCancelReceipt(commandID: commandID, cancellationRequested: true)
        }
        await harness.model.cancelQuestionAnswer()

        // The live binding call receives the original command identity, not a
        // locally repaired id from the new observation.
        #expect(harness.service.requestQuestionCancelCalls.count == 1)
        #expect(harness.service.requestQuestionCancelCalls.first?.2 == "cmd-uncertain-42")
        #expect(harness.model.questionCancelReceipt?.commandID == "cmd-uncertain-42")
        #expect(harness.model.questionCancelReceipt?.cancellationRequested == true)
        #expect(harness.model.questionErrorKey == .questionsCancelRequested)
        #expect(harness.model.questionAnswerState == .uncertain(commandId: "cmd-uncertain-42"))
        #expect(harness.service.answerQuestionCalls.count == 1)
    }

    @Test("an unconfirmed cancellation keeps the original identity recoverable")
    func unconfirmedCancellationKeepsIdentity() async {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(currentQuestions(batch))
        await waitUntil { harness.model.hasPendingQuestions }
        harness.startReview(batch)
        harness.service.answerQuestionHandler = { _, _, _, _ in
            CommandReceipt(
                commandId: "cmd-uncertain-77",
                requestId: "req-uncertain-77",
                status: .uncertain,
                delivery: nil
            )
        }
        await harness.model.submitQuestionReview()
        harness.service.requestQuestionCancelHandler = { _, _, commandID in
            QuestionCancelReceipt(commandID: commandID, cancellationRequested: false)
        }

        await harness.model.cancelQuestionAnswer()
        #expect(harness.model.questionErrorKey == .questionsCancelUnconfirmed)
        #expect(harness.model.questionAnswerState == .uncertain(commandId: "cmd-uncertain-77"))
        #expect(harness.model.canCancelQuestionAnswer)
        #expect(harness.model.questionCancelReceipt?.commandID == "cmd-uncertain-77")
    }
}
