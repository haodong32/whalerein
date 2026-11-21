// Queue, wait, exact approval status and guarded open-answer cancellation.
//
// These cases exercise the presentation contract over a recording service:
// authoritative queue replacement only on a current read, targeted mutations
// with a retained receipt when the follow-up refresh fails, interrupt leaving
// the queue intact, typed wait outcomes with independent cancellation, exact
// approval status, and an open-answer cancellation that keeps its identity.

import Foundation
import Testing
@testable import WhaleRein

@MainActor
private func queueItem(
    id: String = "queue-1",
    placement: String = "queued",
    blocks: [QueueBlock] = [.text("first queued instruction")],
    actions: [QueueAction] = [.edit, .remove, .promote]
) -> QueueItem {
    QueueItem(
        itemID: id,
        placement: placement,
        deliveryID: "delivery-\(id)",
        blocks: blocks,
        actions: actions
    )
}

@MainActor
private func currentQueue(_ items: [QueueItem], running: Bool = true) -> QueueSnapshot {
    QueueSnapshot(available: true, running: running, items: items)
}

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

@Suite("Queue and wait input")
@MainActor
struct QueueWaitInputTests {
    // MARK: - Queue states

    @Test("only a current available read replaces the ordered list")
    func queueFreshness() async {
        let service = StubSessionService()
        let item = queueItem()
        service.pendingQueueHandler = { _, _ in currentQueue([item]) }
        let model = await makeStartedConversation(service: service)
        await waitUntil { model.queue.isCurrent }

        #expect(model.queue.items == [item])
        #expect(model.queue.running)

        // An unavailable read keeps the last list but marks it stale.
        service.pendingQueueHandler = { _, _ in
            QueueSnapshot(available: false, running: false, items: [])
        }
        await model.queue.refresh()
        #expect(model.queue.freshness == .stale)
        #expect(model.queue.items == [item])

        // An unsupported Source is explicit and never an authoritative empty.
        service.pendingQueueHandler = { _, _ in
            throw SessionServiceError(.unsupportedCapability)
        }
        await model.queue.refresh()
        #expect(model.queue.freshness == .unsupported)
        #expect(model.queue.items == [item])
        #expect(!model.queue.isEmptyAndCurrent)

        // Successfully reading an empty queue is the only empty-authoritative state.
        service.pendingQueueHandler = { _, _ in currentQueue([], running: false) }
        await model.queue.refresh()
        #expect(model.queue.freshness == .current)
        #expect(model.queue.isEmptyAndCurrent)
        #expect(model.queue.items.isEmpty)

        // A failure with no prior observation is unavailable, not empty.
        let cold = StubSessionService()
        cold.pendingQueueHandler = { _, _ in throw SessionServiceError(.hostOffline) }
        let coldModel = await makeStartedConversation(service: cold)
        #expect(coldModel.queue.freshness == .unavailable)
        #expect(!coldModel.queue.isEmptyAndCurrent)
    }

    @Test("a Host that does not support the queue is explicit")
    func queueUnsupportedCapability() async {
        let service = StubSessionService()
        service.capabilitiesHandler = { _, _ in
            SessionCapabilities(
                running: false,
                supportsSteer: true,
                supportsQueue: true,
                supportsQueueRead: false
            )
        }
        let model = await makeStartedConversation(service: service)
        #expect(model.queue.freshness == .unsupported)
        #expect(service.pendingQueueCalls.isEmpty)
    }

    // MARK: - Targeted mutations

    @Test("mutations are validated against advertised actions")
    func mutationValidation() async {
        let service = StubSessionService()
        let item = queueItem(actions: [.edit, .remove])
        service.pendingQueueHandler = { _, _ in currentQueue([item]) }
        let model = await makeStartedConversation(service: service)
        await waitUntil { model.queue.isCurrent }

        await model.queue.promote(item.itemID)
        #expect(service.updateQueueCalls.isEmpty)
        #expect(model.queue.mutationErrorKey == .queueActionUnavailable)

        // An advertised action reaches Core and keeps its native receipt.
        await model.queue.withdraw(item.itemID)
        #expect(service.updateQueueCalls.count == 1)
        #expect(service.updateQueueCalls[0].3 == .remove)
        #expect(model.queue.lastMutation?.itemID == item.itemID)
        #expect(model.queue.lastMutation?.action == .remove)
        #expect(model.queue.lastMutation?.operationID == model.queue.lastMutation?.receipt?.commandId)
    }

    @Test("a confirmed mutation receipt survives a failed refresh")
    func mutationRefreshFailureKeepsReceipt() async {
        let service = StubSessionService()
        let item = queueItem()
        var reads = 0
        service.pendingQueueHandler = { _, _ in
            reads += 1
            if reads == 1 { return currentQueue([item]) }
            throw SessionServiceError(.unavailable)
        }
        let model = await makeStartedConversation(service: service)
        await waitUntil { model.queue.isCurrent }

        await model.queue.withdraw(item.itemID)

        #expect(model.queue.lastMutation?.receipt?.status == .confirmed)
        #expect(model.queue.freshness == .stale)
        // The old list is not replaced by a failed read, but the receipt stays.
        #expect(model.queue.items == [item])
    }

    @Test("edit replaces whole content with an attachment warning")
    func editReplacesContent() async {
        let service = StubSessionService()
        let attachmentItem = queueItem(
            blocks: [
                .text("original text"),
                .attachment(mediaType: "image/png", name: "shot.png", size: 4, available: true),
            ]
        )
        let replacement = queueItem(blocks: [.text("replacement")])
        var reads = 0
        service.pendingQueueHandler = { _, _ in
            reads += 1
            return reads == 1 ? currentQueue([attachmentItem]) : currentQueue([replacement])
        }
        let model = await makeStartedConversation(service: service)
        await waitUntil { model.queue.isCurrent }

        model.queue.beginEdit(attachmentItem.itemID)
        #expect(model.queue.editWarningVisible)
        #expect(model.queue.editText == "original text")
        #expect(model.queue.editReplacesAttachments)

        model.queue.editText = "replacement"
        await model.queue.submitEdit()

        #expect(service.updateQueueCalls.count == 1)
        #expect(service.updateQueueCalls[0].3 == .edit)
        #expect(service.updateQueueCalls[0].4 == "replacement")
        #expect(model.queue.editItemID == nil)
        #expect(model.queue.items.first?.blocks == [.text("replacement")])
    }

    @Test("empty edit text is rejected before any native call")
    func emptyEditRejected() async {
        let service = StubSessionService()
        let item = queueItem()
        service.pendingQueueHandler = { _, _ in currentQueue([item]) }
        let model = await makeStartedConversation(service: service)
        await waitUntil { model.queue.isCurrent }

        model.queue.beginEdit(item.itemID)
        model.queue.editText = "   "
        await model.queue.submitEdit()

        #expect(service.updateQueueCalls.isEmpty)
        #expect(model.queue.editErrorKey == .queueEditEmpty)
    }

    // MARK: - Interrupt

    @Test("interrupt stays separate and the refreshed queue keeps remaining items")
    func interruptPreservesQueue() async {
        let service = StubSessionService()
        let first = queueItem(id: "queue-1")
        let second = queueItem(id: "queue-2")
        service.capabilitiesHandler = { _, _ in .running }
        service.pendingQueueHandler = { _, _ in currentQueue([first, second]) }
        service.stopHandler = { _, _ in
            CommandReceipt(
                commandId: "cmd-stop",
                requestId: "req-stop",
                status: .confirmed,
                delivery: nil
            )
        }
        let model = await makeStartedConversation(service: service)
        await waitUntil { model.queue.isCurrent }
        #expect(model.queue.items.map(\.itemID) == ["queue-1", "queue-2"])

        await model.stopRun()

        #expect(model.lastStopStatus == .confirmed)
        #expect(model.queue.items.map(\.itemID) == ["queue-1", "queue-2"])
        // Interrupt itself is not a queue mutation; withdrawal stays explicit.
        #expect(service.updateQueueCalls.isEmpty)
        #expect(model.queue.items.allSatisfy { $0.actions.contains(.remove) })
    }

    // MARK: - Wait

    @Test("wait outcomes are typed and cancel releases only the wait")
    func waitOutcomesAndCancellation() async {
        let service = StubSessionService()
        let gate = SuspendedResponse<WaitResult?>()
        service.takeWaitHandler = { _, _ in try await gate.wait() }
        let model = await makeStartedConversation(service: service)
        let streamCount = service.eventStreamCount

        let start = Task { await model.wait.start(timeoutMs: 5_000) }
        await waitUntil { service.beginWaitCalls.count == 1 && model.wait.isWaiting }
        await start.value
        #expect(model.wait.waitID == "wait-1")
        #expect(model.wait.deadline != nil)

        let idle = WaitResult(
            waitID: "wait-1",
            hostID: "studio-mac",
            sessionID: "s-001",
            outcome: .idle,
            state: WaitSnapshot(active: false, running: false, queueAvailable: true, pendingInput: []),
            errorCode: nil
        )
        gate.resume(with: idle)
        await waitUntil { model.wait.phase == .settled }
        #expect(model.wait.result?.outcome == .idle)
        #expect(L10nKey.waitOutcomeKey(for: .idle) == .waitIdle)
        #expect(L10nKey.waitOutcomeKey(for: .inputRequired) == .waitInputRequired)
        #expect(L10nKey.waitOutcomeKey(for: .timeout) == .waitTimeout)
        #expect(L10nKey.waitOutcomeKey(for: .cancelled) == .waitCancelled)
        #expect(L10nKey.waitOutcomeKey(for: .failed) == .waitFailed)
        #expect(service.stopWatchCount == 0, "a wait must never stop the conversation watch")
        #expect(service.eventStreamCount == streamCount)

        // Cancelling a new attempt only releases that attempt.
        let cancelGate = SuspendedResponse<WaitResult?>()
        service.takeWaitHandler = { _, _ in try await cancelGate.wait() }
        let secondStart = Task { await model.wait.start(timeoutMs: 5_000) }
        await waitUntil { model.wait.waitID != nil && model.wait.isWaiting }
        await secondStart.value
        await model.wait.cancel()
        #expect(service.cancelWaitCalls == ["wait-2"])
        cancelGate.resume(with: WaitResult(
            waitID: "wait-2",
            hostID: "studio-mac",
            sessionID: "s-001",
            outcome: .cancelled,
            state: nil,
            errorCode: "cancelled"
        ))
        await waitUntil { model.wait.phase == .settled }
        #expect(model.wait.result?.outcome == .cancelled)
        #expect(service.stopWatchCount == 0)
        #expect(service.eventStreamCount == streamCount)
    }

    @Test("an input-required wait carries the evidenced pending input kinds")
    func waitInputRequired() async {
        let service = StubSessionService()
        service.beginWaitHandler = { _, _, _ in "wait-input" }
        let blocked = WaitResult(
            waitID: "wait-input",
            hostID: "studio-mac",
            sessionID: "s-001",
            outcome: .inputRequired,
            state: WaitSnapshot(
                active: false,
                running: false,
                queueAvailable: true,
                pendingInput: ["approval", "question"]
            ),
            errorCode: nil
        )
        service.takeWaitHandler = { _, _ in blocked }
        let model = await makeStartedConversation(service: service)

        await model.wait.start(timeoutMs: 1_000)
        await waitUntil { model.wait.phase == .settled }

        #expect(model.wait.result?.outcome == .inputRequired)
        #expect(model.wait.result?.state?.pendingInput == ["approval", "question"])
        #expect(L10nKey.waitOutcomeKey(for: .inputRequired) == .waitInputRequired)
    }

    @Test("a failed wait keeps its stable error and does not fail the session")
    func waitFailure() async {
        let service = StubSessionService()
        service.beginWaitHandler = { _, _, _ in "wait-fail" }
        service.takeWaitHandler = { _, _ in
            WaitResult(
                waitID: "wait-fail",
                hostID: "studio-mac",
                sessionID: "s-001",
                outcome: .failed,
                state: nil,
                errorCode: "host_offline"
            )
        }
        let model = await makeStartedConversation(service: service)

        await model.wait.start(timeoutMs: 1_000)
        await waitUntil { model.wait.phase == .settled }

        #expect(model.wait.result?.outcome == .failed)
        #expect(model.wait.result?.errorCode == "host_offline")
        #expect(model.wait.errorKey == .waitFailed)
        #expect(!model.watchEnded)
    }

    // MARK: - Exact approval status

    @Test("exact approval status distinguishes pending, changed, resolved, unavailable and scope")
    func approvalExactStatus() async {
        let service = StubSessionService()
        var current = testApproval(state: .pending, version: 3)
        service.approvalHandler = { _, _ in current }
        let model = await makeStartedConversation(service: service)
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 1,
                event: .reset(messages: [], approvals: [current])
            )
        )

        await model.refreshApprovalStatus(current)
        #expect(model.approvalStatusRefresh[current.approvalId] == .current)
        #expect(model.isApprovalActionable(current))

        // A changed version replaces the reviewed guard and disables the old one.
        current = testApproval(state: .pending, version: 4, digest: "digest-2")
        await model.refreshApprovalStatus(model.approvals[0])
        #expect(model.approvalStatusRefresh[current.approvalId] == .changed)
        #expect(model.approvals[0].version == 4)
        #expect(model.approvals[0].digestHex == "digest-2")

        // A terminal state is rendered exactly and is not actionable.
        current = testApproval(state: .allowed, version: 5, digest: "digest-2")
        await model.refreshApprovalStatus(model.approvals[0])
        #expect(model.approvalStatusRefresh[current.approvalId] == .resolved)
        #expect(model.approvals[0].state == .allowed)
        #expect(!model.isApprovalActionable(model.approvals[0]))

        // An unavailable read marks a still-pending card unavailable instead of
        // clickable. A fresh model avoids the intentionally sticky terminal card.
        let unavailableService = StubSessionService()
        unavailableService.approvalHandler = { _, _ in
            throw SessionServiceError(.unavailable)
        }
        let pending = testApproval(state: .pending, version: 6)
        let unavailableModel = await makeStartedConversation(service: unavailableService)
        unavailableModel.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 1,
                event: .reset(messages: [], approvals: [pending])
            )
        )
        await unavailableModel.refreshApprovalStatus(unavailableModel.approvals[0])
        #expect(unavailableModel.approvalStatusRefresh[pending.approvalId] == .unavailable)
        #expect(unavailableModel.approvals[0].state == .unavailable)
        #expect(!unavailableModel.isApprovalActionable(unavailableModel.approvals[0]))

        // A read for another session never touches the card.
        service.approvalHandler = { _, _ in
            testApproval(id: "approval-1", sessionId: "s-other", state: .pending)
        }
        await model.refreshApprovalStatus(testApproval())
        #expect(model.approvalStatusRefresh["approval-1"] == .outOfScope)
    }

    // MARK: - Open-answer cancellation

    @Test("a confirmed answer is not cancellable")
    func settledAnswerIsNotCancellable() async {
        let service = StubSessionService()
        service.answerQuestionHandler = { _, _, _, _ in
            CommandReceipt(
                commandId: "cmd-answer-ok",
                requestId: "req-answer-ok",
                status: .confirmed,
                delivery: nil
            )
        }
        let model = await makeStartedConversation(service: service)
        service.emit(.reset(messages: [], approvals: []), sequence: 1)
        let batch = questionBatch()
        service.emit(.questions(currentQuestions(batch)), sequence: 2)
        await waitUntil { model.hasPendingQuestions }
        model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Stable"
        )
        _ = model.beginQuestionReview(batchId: batch.batchID)
        await model.submitQuestionReview()
        await pump()

        #expect(!model.canCancelQuestionAnswer)
        #expect(model.questionCancelReceipt == nil)
    }

    @Test("cancellation keeps the original command identity across a replacement observer")
    func questionCancellationRace() async {
        let service = StubSessionService()
        service.answerQuestionHandler = { _, _, _, _ in
            CommandReceipt(
                commandId: "cmd-answer-9",
                requestId: "req-answer-9",
                status: .uncertain,
                delivery: nil
            )
        }
        let model = await makeStartedConversation(service: service)
        service.emit(.reset(messages: [], approvals: []), sequence: 1)
        let batch = questionBatch()
        service.emit(
            .questions(currentQuestions(batch)),
            sequence: 2
        )
        await waitUntil { model.hasPendingQuestions }

        model.toggleQuestionOption(
            batchId: batch.batchID,
            itemId: "question-channel",
            label: "Stable"
        )
        _ = model.beginQuestionReview(batchId: batch.batchID)
        await model.submitQuestionReview()
        await pump()

        #expect(model.questionAnswerState == .uncertain(commandId: "cmd-answer-9"))
        #expect(model.canCancelQuestionAnswer)

        // The cancellation request reports its outcome and keeps the identity.
        service.requestQuestionCancelHandler = { _, _, commandID in
            QuestionCancelReceipt(commandID: commandID, cancellationRequested: true)
        }
        await model.cancelQuestionAnswer()
        #expect(model.questionCancelReceipt?.commandID == "cmd-answer-9")
        #expect(model.questionCancelReceipt?.cancellationRequested == true)
        #expect(model.questionErrorKey == .questionsCancelRequested)
        #expect(model.questionAnswerState == .uncertain(commandId: "cmd-answer-9"))

        // A native acceptance race leaves the request unconfirmed, never a lie.
        service.requestQuestionCancelHandler = { _, _, commandID in
            QuestionCancelReceipt(commandID: commandID, cancellationRequested: false)
        }
        await model.cancelQuestionAnswer()
        #expect(model.questionCancelReceipt?.cancellationRequested == false)
        #expect(model.questionErrorKey == .questionsCancelUnconfirmed)
        #expect(model.questionAnswerState == .uncertain(commandId: "cmd-answer-9"))

        // Replacing the observer keeps the in-flight attempt recoverable.
        await model.reconnectWatch()
        #expect(model.questionAnswerState == .uncertain(commandId: "cmd-answer-9"))
        #expect(model.canCancelQuestionAnswer)
        #expect(model.questionCancelReceipt?.commandID == "cmd-answer-9")
    }
}
