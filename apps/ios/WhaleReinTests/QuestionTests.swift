// Question presentation over Core-owned observation state: distinct freshness,
// edits that survive unchanged guards, an explicit review step, exact guard
// submission, and an answer task independent of the visible observation.

import Foundation
import Testing
@testable import WhaleRein

@MainActor
private func questionBatch(
    version: UInt64 = 2,
    digestHex: String = String(repeating: "a1", count: 32)
) -> QuestionBatch {
    let base = FixtureCatalog.questionBatch(sessionId: "s-001")
    return QuestionBatch(
        sessionID: base.sessionID,
        batchID: base.batchID,
        sourceEpoch: base.sourceEpoch,
        version: version,
        digestHex: digestHex,
        phase: base.phase,
        questions: base.questions,
        answerable: base.answerable,
        contentUnavailable: base.contentUnavailable
    )
}

@MainActor
private func questionsState(
    _ freshness: QuestionFreshness,
    batches: [QuestionBatch],
    revision: UInt64 = 1
) -> QuestionsState {
    QuestionsState(
        freshness: freshness,
        stamp: QuestionStamp(observationEpoch: "epoch-1", revision: revision),
        batches: batches,
        error: freshness == .unavailable || freshness == .stale ? "unavailable" : nil
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
}

@Suite("Question presentation")
@MainActor
struct QuestionTests {
    @Test("current questions arrive from the observation and edits survive an unchanged guard")
    func currentQuestionsAndEdits() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await waitUntil { harness.model.hasPendingQuestions }

        #expect(harness.model.questionsFreshness == .current)
        #expect(harness.model.questionBatches.map(\.batchID) == [batch.batchID])
        #expect(harness.model.questionEmptyIsCurrent == false)

        harness.model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Stable"
        )
        harness.model.setQuestionCustomText(
            batchId: batch.batchID,
            itemId: "question-channel",
            text: "after the freeze"
        )
        #expect(
            harness.model.questionSelection(batchId: batch.batchID, itemId: "question-channel")
                == ["Stable"]
        )

        // The same guard re-delivered by Core keeps the edit.
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()
        #expect(
            harness.model.questionSelection(batchId: batch.batchID, itemId: "question-channel")
                == ["Stable"]
        )
        #expect(
            harness.model.questionCustomText(batchId: batch.batchID, itemId: "question-channel")
                == "after the freeze"
        )

        // A changed guard drops the stale edit instead of carrying it forward.
        harness.emitQuestions(questionsState(.current, batches: [questionBatch(version: 3)]))
        await pump()
        #expect(
            harness.model.questionSelection(batchId: batch.batchID, itemId: "question-channel")
                .isEmpty
        )
        #expect(harness.model.beginQuestionReview(batchId: batch.batchID) == nil)
        #expect(harness.model.questionErrorKey == .questionsSelectAnswer)
    }

    @Test("loading, current-empty, stale, unavailable and unsupported stay distinct")
    func freshnessStates() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()

        harness.emitQuestions(questionsState(.loading, batches: []))
        await pump()
        #expect(harness.model.questionsFreshness == .loading)
        #expect(!harness.model.questionEmptyIsCurrent)

        harness.emitQuestions(questionsState(.current, batches: []))
        await pump()
        #expect(harness.model.questionsFreshness == .current)
        #expect(harness.model.questionEmptyIsCurrent)

        harness.emitQuestions(questionsState(.stale, batches: [batch], revision: 2))
        await pump()
        #expect(harness.model.questionsFreshness == .stale)
        #expect(!harness.model.questionEmptyIsCurrent)
        #expect(harness.model.questionBatches.count == 1)

        harness.emitQuestions(questionsState(.unavailable, batches: []))
        await pump()
        #expect(harness.model.questionsFreshness == .unavailable)
        #expect(!harness.model.questionEmptyIsCurrent)

        harness.emitQuestions(questionsState(.unsupported, batches: []))
        await pump()
        #expect(harness.model.questionsFreshness == .unsupported)
        #expect(!harness.model.questionEmptyIsCurrent)
        #expect(!harness.model.hasPendingQuestions)
    }

    @Test("review snapshots the exact guard and submission sends that snapshot")
    func reviewAndSubmit() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()

        harness.model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Beta"
        )
        harness.model.setQuestionCustomText(
            batchId: batch.batchID,
            itemId: "question-channel",
            text: " ship it "
        )
        let review = harness.model.beginQuestionReview(batchId: batch.batchID)
        #expect(review?.guardValue == batch.guardValue)
        #expect(review?.answers == [
            QuestionAnswer(id: "question-channel", selected: ["Beta"], custom: "ship it")
        ])

        await harness.model.submitQuestionReview()
        #expect(harness.service.answerQuestionCalls.count == 1)
        let call = harness.service.answerQuestionCalls.first
        #expect(call?.2.guardValue == batch.guardValue)
        #expect(call?.3 == review?.answers)
        #expect(harness.model.questionAnswerState == .submitted(.confirmed))
        // The answered card stays as reviewed until the Host observation
        // removes or replaces the batch; clearing it here made the card fall
        // back to an empty editable state.
        #expect(harness.model.settledQuestionReviews[batch.batchID] == review)
        #expect(harness.model.questionReview == nil)
        #expect(
            harness.model.questionSelection(batchId: batch.batchID, itemId: "question-channel")
                == ["Beta"]
        )
        #expect(
            harness.model.questionCustomText(batchId: batch.batchID, itemId: "question-channel")
                == " ship it "
        )
    }

    @Test("the submitted card survives an unchanged observation and clears when the batch leaves")
    func submittedCardWaitsForObservation() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()
        harness.model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Beta"
        )
        _ = harness.model.beginQuestionReview(batchId: batch.batchID)
        await harness.model.submitQuestionReview()
        #expect(harness.model.questionAnswerState == .submitted(.confirmed))

        // The same batch is re-delivered before the Host removes it.
        harness.emitQuestions(questionsState(.current, batches: [batch], revision: 2))
        await pump()
        #expect(harness.model.settledQuestionReviews[batch.batchID] != nil)
        #expect(harness.model.questionBatches.count == 1)

        // The Host finally removes the answered batch.
        harness.emitQuestions(questionsState(.current, batches: [], revision: 3))
        await pump()
        #expect(harness.model.questionReview == nil)
        #expect(harness.model.questionAnswerState == .submitted(.confirmed))
        #expect(harness.model.questionErrorKey == nil)
        #expect(harness.model.questionEmptyIsCurrent)
    }

    @Test("an uncertain answer blocks a second batch until it settles")
    func uncertainAnswerBlocksAnotherBatch() async -> Void {
        var harness = await QuestionHarness()
        let first = questionBatch()
        let second = QuestionBatch(
            sessionID: first.sessionID,
            batchID: "question-release-s-002",
            sourceEpoch: first.sourceEpoch,
            version: first.version + 1,
            digestHex: String(repeating: "b2", count: 32),
            phase: first.phase,
            questions: first.questions,
            answerable: first.answerable,
            contentUnavailable: first.contentUnavailable
        )
        harness.emitQuestions(questionsState(.current, batches: [first, second]))
        await pump()
        harness.model.toggleQuestionOption(
            batchId: first.batchID,
            itemId: "question-channel",
            label: "Beta"
        )
        _ = harness.model.beginQuestionReview(batchId: first.batchID)
        harness.service.answerQuestionHandler = { _, _, _, _ in
            CommandReceipt(
                commandId: "cmd-uncertain-batch",
                requestId: "req-uncertain-batch",
                status: .uncertain,
                delivery: nil
            )
        }
        await harness.model.submitQuestionReview()
        #expect(harness.model.hasUncertainQuestionAnswer)

        #expect(harness.model.beginQuestionReview(batchId: second.batchID) == nil)
        #expect(harness.model.questionErrorKey == .questionsAnswerUncertain)

        // The answered batch leaves the observation; the old uncertain state
        // must not keep blocking the remaining card.
        harness.emitQuestions(questionsState(.current, batches: [second], revision: 2))
        await pump()
        #expect(!harness.model.hasUncertainQuestionAnswer)
        #expect(harness.model.questionAnswerState == .idle)
        harness.model.toggleQuestionOption(
            batchId: second.batchID,
            itemId: "question-channel",
            label: "Stable"
        )
        #expect(harness.model.beginQuestionReview(batchId: second.batchID) != nil)
    }

    @Test("two pending batches keep independent submitted cards")
    func multipleBatchesKeepIndependentSettledCards() async -> Void {
        var harness = await QuestionHarness()
        let first = questionBatch()
        let second = QuestionBatch(
            sessionID: first.sessionID,
            batchID: "question-release-s-002",
            sourceEpoch: first.sourceEpoch,
            version: first.version + 1,
            digestHex: String(repeating: "b2", count: 32),
            phase: first.phase,
            questions: first.questions,
            answerable: first.answerable,
            contentUnavailable: first.contentUnavailable
        )
        harness.emitQuestions(questionsState(.current, batches: [first, second]))
        await pump()

        harness.model.toggleQuestionOption(
            batchId: first.batchID,
            itemId: "question-channel",
            label: "Beta"
        )
        _ = harness.model.beginQuestionReview(batchId: first.batchID)
        await harness.model.submitQuestionReview()
        #expect(harness.model.settledQuestionReviews[first.batchID] != nil)

        // Reviewing the second batch must not release the first settled card.
        harness.model.toggleQuestionOption(
            batchId: second.batchID,
            itemId: "question-channel",
            label: "Stable"
        )
        _ = harness.model.beginQuestionReview(batchId: second.batchID)
        await harness.model.submitQuestionReview()
        #expect(harness.model.settledQuestionReviews[first.batchID] != nil)
        #expect(harness.model.settledQuestionReviews[second.batchID] != nil)
        #expect(harness.model.questionReview == nil)

        // Removing only the first batch leaves the second card settled.
        harness.emitQuestions(questionsState(.current, batches: [second], revision: 2))
        await pump()
        #expect(harness.model.settledQuestionReviews[first.batchID] == nil)
        #expect(harness.model.settledQuestionReviews[second.batchID] != nil)
    }

    @Test("a batch removed during submission settles without a stale guard error")
    func removalDuringSubmissionSettlesCleanly() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()
        harness.model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Beta"
        )
        _ = harness.model.beginQuestionReview(batchId: batch.batchID)

        let gate = SuspendedResponse<CommandReceipt>()
        harness.service.answerQuestionHandler = { _, _, _, _ in
            try await gate.wait()
        }
        let task = Task { await harness.model.submitQuestionReview() }
        await gate.waitUntilStarted()

        // The Host observation removes the answered batch before the command
        // response returns.
        harness.emitQuestions(questionsState(.current, batches: [], revision: 2))
        await pump()

        gate.resume(
            with: CommandReceipt(
                commandId: "cmd-removed",
                requestId: "req-removed",
                status: .confirmed,
                delivery: nil
            )
        )
        await task.value
        #expect(harness.model.questionAnswerState == .submitted(.confirmed))
        #expect(harness.model.questionErrorKey == nil)
        #expect(harness.model.questionReview == nil)
    }

    @Test("a submitted review stays locked across a watch replacement")
    func submittedSurvivesWatchReplacement() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()
        harness.model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Beta"
        )
        _ = harness.model.beginQuestionReview(batchId: batch.batchID)
        await harness.model.submitQuestionReview()
        #expect(harness.model.questionAnswerState == .submitted(.confirmed))

        harness.model.preserveQuestionCancellation()
        #expect(harness.model.questionAnswerState == .submitted(.confirmed))
        #expect(harness.model.settledQuestionReviews[batch.batchID] != nil)
    }

    @Test("a new pending batch clears the previous submitted banner")
    func newBatchClearsSubmittedStatus() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()
        harness.model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Beta"
        )
        _ = harness.model.beginQuestionReview(batchId: batch.batchID)
        await harness.model.submitQuestionReview()
        #expect(harness.model.questionAnswerState == .submitted(.confirmed))

        harness.emitQuestions(questionsState(.current, batches: [], revision: 2))
        await pump()
        #expect(harness.model.questionAnswerState == .submitted(.confirmed))

        harness.emitQuestions(questionsState(.current, batches: [questionBatch(version: 3)], revision: 3))
        await pump()
        #expect(harness.model.questionAnswerState == .idle)
        #expect(harness.model.questionErrorKey == nil)
    }

    @Test("a submitted review is dropped quietly when its guard changes")
    func submittedCardGuardChangeIsQuiet() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()
        harness.model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Beta"
        )
        _ = harness.model.beginQuestionReview(batchId: batch.batchID)
        await harness.model.submitQuestionReview()
        #expect(harness.model.questionAnswerState == .submitted(.confirmed))

        harness.emitQuestions(questionsState(.current, batches: [questionBatch(version: 3)]))
        await pump()
        #expect(harness.model.questionReview == nil)
        #expect(harness.model.questionAnswerState == .idle)
        #expect(harness.model.questionErrorKey == nil)
    }

    @Test("a repeated submit while the first is in flight is ignored")
    func repeatedSubmitIsIgnored() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()
        harness.model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Beta"
        )
        _ = harness.model.beginQuestionReview(batchId: batch.batchID)

        let gate = SuspendedResponse<CommandReceipt>()
        harness.service.answerQuestionHandler = { _, _, _, _ in
            try await gate.wait()
        }
        let first = Task { await harness.model.submitQuestionReview() }
        await gate.waitUntilStarted()

        // A second tap while the first request is still open must not clear the
        // review, report a guard change, or issue another answer command.
        await harness.model.submitQuestionReview()
        #expect(harness.service.answerQuestionCalls.count == 1)
        #expect(harness.model.questionAnswerState == .submitting)
        #expect(harness.model.questionReview != nil)
        #expect(harness.model.questionErrorKey == nil)

        gate.resume(
            with: CommandReceipt(
                commandId: "cmd-repeat",
                requestId: "req-repeat",
                status: .confirmed,
                delivery: nil
            )
        )
        await first.value
        #expect(harness.model.questionAnswerState == .submitted(.confirmed))
    }

    @Test("a replacement history window preserves independent question state")
    func historyReplacementPreservesQuestions() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()
        harness.model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Beta"
        )
        _ = harness.model.beginQuestionReview(batchId: batch.batchID)
        await harness.model.submitQuestionReview()
        #expect(harness.model.settledQuestionReviews[batch.batchID] != nil)

        harness.emit(
            .historySnapshot(
                ConversationHistoryPage(
                    source: .online, projection: "projection-next", complete: true,
                    messages: [], activities: [], continuation: nil
                ),
                replacing: true
            )
        )
        await pump()
        #expect(harness.model.questionsFreshness == .current)
        #expect(harness.model.questionBatches.map(\.batchID) == [batch.batchID])
        #expect(harness.model.settledQuestionReviews[batch.batchID] != nil)
        #expect(harness.model.questionAnswerState == .submitted(.confirmed))
    }

    @Test("a changed guard is rejected before any submission")
    func changedGuardRejected() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()
        harness.model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Stable"
        )
        #expect(harness.model.beginQuestionReview(batchId: batch.batchID) != nil)

        harness.emitQuestions(questionsState(.current, batches: [questionBatch(version: 3)]))
        await pump()
        #expect(harness.model.questionReview == nil)
        #expect(harness.model.questionErrorKey == .questionsGuardChanged)

        await harness.model.submitQuestionReview()
        #expect(harness.service.answerQuestionCalls.isEmpty)
    }

    @Test("a stale reviewed answer requires a fresh review and keeps the draft")
    func answerFailureKeepsDraft() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()
        harness.model.draft = "composer draft survives"
        harness.model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Stable"
        )
        _ = harness.model.beginQuestionReview(batchId: batch.batchID)
        harness.service.answerQuestionHandler = { _, _, _, _ in
            throw SessionServiceError(.conflict)
        }

        await harness.model.submitQuestionReview()
        #expect(harness.model.questionAnswerState == .failed)
        #expect(harness.model.questionErrorKey == .questionsGuardChanged)
        // The rejected review snapshot is gone, so the same stale answer cannot
        // be retried; the compositor draft is untouched.
        #expect(harness.model.questionReview == nil)
        #expect(harness.model.draft == "composer draft survives")
    }

    @Test("an uncertain answer is cancelled by its original command identity")
    func cancelUncertainAnswer() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()
        harness.model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Stable"
        )
        _ = harness.model.beginQuestionReview(batchId: batch.batchID)
        harness.service.answerQuestionHandler = { _, _, _, _ in
            CommandReceipt(
                commandId: "cmd-uncertain",
                requestId: "req-uncertain",
                status: .uncertain,
                delivery: nil
            )
        }

        await harness.model.submitQuestionReview()
        #expect(harness.model.questionAnswerState == .uncertain(commandId: "cmd-uncertain"))
        await harness.model.cancelQuestionAnswer()
        #expect(harness.service.cancelQuestionCalls.count == 1)
        #expect(harness.service.cancelQuestionCalls.first?.2 == "cmd-uncertain")
        #expect(harness.model.questionErrorKey == .questionsCancelRequested)
    }

    @Test("closing the observation does not cancel an in-flight answer")
    func answerSurvivesObservationClose() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()
        harness.model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Stable"
        )
        _ = harness.model.beginQuestionReview(batchId: batch.batchID)

        let gate = SuspendedResponse<CommandReceipt>()
        harness.service.answerQuestionHandler = { _, _, _, _ in
            try await gate.wait()
        }
        let task = Task { await harness.model.submitQuestionReview() }
        await gate.waitUntilStarted()

        // Closing the visible observation must not cancel the independent
        // answer task or its review.
        harness.model.close()
        #expect(harness.service.stopWatchCount >= 1)
        gate.resume(
            with: CommandReceipt(
                commandId: "cmd-late",
                requestId: "req-late",
                status: .confirmed,
                delivery: nil
            )
        )
        await task.value
        #expect(harness.model.questionAnswerState == .submitted(.confirmed))
    }

    @Test("the model performs no discovery polling or one-shot question reads")
    func noPollingTimer() async -> Void {
        var harness = await QuestionHarness()
        let batch = questionBatch()
        harness.emitQuestions(questionsState(.current, batches: [batch]))
        await pump()
        let subscriptions = harness.service.eventStreamCount
        try? await Task.sleep(for: .milliseconds(350))
        #expect(harness.service.listQuestionsCalls.isEmpty)
        #expect(harness.service.questionCalls.isEmpty)
        #expect(harness.service.eventStreamCount == subscriptions)
        #expect(harness.model.questionBatches.count == 1)
    }
}
