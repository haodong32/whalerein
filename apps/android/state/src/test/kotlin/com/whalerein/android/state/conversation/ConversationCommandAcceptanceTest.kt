package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SendId
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import com.whalerein.android.state.fixtures.conversation.ConversationFixtures
import com.whalerein.android.state.fixtures.conversation.FakeClock
import com.whalerein.android.state.fixtures.conversation.FakeCommandPort
import com.whalerein.android.state.fixtures.conversation.FakeComposerPort
import com.whalerein.android.state.fixtures.conversation.FakeDraftPort
import com.whalerein.android.state.fixtures.conversation.FakeHistoryPort
import com.whalerein.android.state.fixtures.conversation.FakeModelsQueuePort
import com.whalerein.android.state.fixtures.conversation.FakeObservationPort
import com.whalerein.android.state.fixtures.conversation.FakePendingInputPort
import com.whalerein.android.state.fixtures.conversation.FakeSessionListPort
import com.whalerein.android.state.fixtures.conversation.FakeWaitPort
import com.whalerein.android.state.fixtures.conversation.RecordingDiagnosticSink
import com.whalerein.android.state.fixtures.conversation.SequentialIdGenerator
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/**
 * AC-27 conversation command correctness through the typed ports.
 *
 * The fakes enforce identity and receipt semantics: the composer keeps the
 * original send identity, the pending-input fake records the exact command
 * reference, and the queue fake distinguishes confirmed from ambiguous.
 */
@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class ConversationCommandAcceptanceTest : ConversationTestBase() {
    @Test
    @DisplayName("AC-27.send-unknown unknown receipt offers recovery and never resends")
    fun sendUnknownReceipt() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        h.composer.submitOutcome = Outcome.Unconfirmed(
            CoreFailure("uncertain", "receipt lost", retryable = true),
        )
        h.model.selectFollowMode(FollowMode.ReceiptOnly)
        h.model.updateDraft("send exactly once")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()

        val unconfirmed = assertInstanceOf(
            SendUiState.Unconfirmed::class.java,
            h.model.state.value.send,
        )
        val originalSendId: SendId = unconfirmed.sendId
        assertEquals(1, h.composer.submits.size)
        // The text stays recoverable but is never resubmitted automatically.
        assertEquals("send exactly once", h.model.state.value.composer.text)

        h.composer.recoveryResult = RecoveryResult.Confirmed(
            receipt = SendReceipt(originalSendId, null),
            reply = ConversationFixtures.textMessage("reply-1", "recovered", sequence = 1),
        )
        h.model.recoverSend()
        advanceUntilIdle()

        assertInstanceOf(SendUiState.Recovered::class.java, h.model.state.value.send)
        assertEquals(originalSendId, h.composer.recoveryRequests.single().sendId)
        assertEquals(1, h.composer.submits.size)
        assertEquals(
            "reply-1",
            h.model.state.value.items.filterIsInstance<CommittedMessageItem>()
                .single().message.messageId,
        )
        cleanup(h)
    }

    @Test
    @DisplayName("AC-27.send-rejected a definite rejection restores the draft without recovery")
    fun sendDefiniteRejection() = runTest(dispatcher) {
        val h = Harness()
        val session = ConversationFixtures.session()
        h.model.open(session)
        advanceUntilIdle()
        h.composer.submitOutcome = Outcome.Failure(
            CoreFailure("rejected", "The Host rejected the send.", retryable = false),
        )
        h.model.updateDraft("retry me")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()

        assertInstanceOf(SendUiState.DefiniteFailure::class.java, h.model.state.value.send)
        assertEquals("retry me", h.model.state.value.composer.text)
        assertEquals("retry me", h.drafts.drafts[session])
        assertEquals(1, h.composer.submits.size)
        // Definite failure is not a recoverable unknown operation.
        h.model.recoverSend()
        advanceUntilIdle()
        assertTrue(h.composer.recoveryRequests.isEmpty())
        cleanup(h)
    }

    @Test
    @DisplayName("AC-27.draft-restart a rejected send survives a process restart")
    fun draftSurvivesRestart() = runTest(dispatcher) {
        val drafts = FakeDraftPort()
        val session = ConversationFixtures.session()
        val first = Harness(drafts = drafts)
        first.model.open(session)
        advanceUntilIdle()
        first.composer.submitOutcome = Outcome.Failure(
            CoreFailure("host_offline", "offline", retryable = true),
        )
        first.model.updateDraft("survive the kill")
        advanceUntilIdle()
        first.model.submit()
        advanceUntilIdle()
        assertEquals("survive the kill", drafts.drafts[session])
        first.model.onHidden()
        advanceUntilIdle()

        // Process restart: a fresh model over the same persisted draft store.
        val restarted = Harness(drafts = drafts)
        restarted.model.open(session)
        advanceUntilIdle()
        assertEquals("survive the kill", restarted.model.state.value.composer.text)
        cleanup(restarted)
    }

    @Test
    @DisplayName("AC-27.draft-newer an older accepted send never clears a newer draft")
    fun newerDraftSurvivesOlderSettlement() = runTest(dispatcher) {
        val h = Harness()
        val session = ConversationFixtures.session()
        h.model.open(session)
        advanceUntilIdle()
        val gate = CompletableDeferred<Unit>()
        h.composer.submitGate = gate
        h.model.selectFollowMode(FollowMode.ReceiptOnly)
        h.model.updateDraft("submitted text")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()

        h.model.updateDraft("newer text")
        advanceUntilIdle()
        assertEquals("newer text", h.drafts.drafts[session])

        gate.complete(Unit)
        advanceUntilIdle()
        assertInstanceOf(SendUiState.ReceiptOnly::class.java, h.model.state.value.send)
        assertEquals("newer text", h.model.state.value.composer.text)
        assertEquals("newer text", h.drafts.drafts[session])
        cleanup(h)
    }

    @Test
    @DisplayName("AC-27.draft-accepted an accepted send clears only the sent draft")
    fun acceptedSendClearsSentDraft() = runTest(dispatcher) {
        val h = Harness()
        val session = ConversationFixtures.session()
        h.model.open(session)
        advanceUntilIdle()
        h.model.selectFollowMode(FollowMode.ReceiptOnly)
        h.model.updateDraft("clear me")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()

        assertInstanceOf(SendUiState.ReceiptOnly::class.java, h.model.state.value.send)
        assertEquals("", h.model.state.value.composer.text)
        assertNull(h.drafts.drafts[session])
        assertEquals(1, h.composer.submits.size)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-27.draft-clear-order an accepted send clears only after the pending save settles")
    fun acceptedSendWaitsForDraftSave() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        val gate = CompletableDeferred<Unit>()
        h.drafts.saveGate = gate
        h.model.updateDraft("send immediately")
        advanceUntilIdle()
        assertEquals(listOf("save-start"), h.drafts.writeOrder)

        h.model.submit()
        advanceUntilIdle()
        assertEquals(
            listOf("save-start"),
            h.drafts.writeOrder,
            "the clear must wait for the in-flight save",
        )

        gate.complete(Unit)
        advanceUntilIdle()
        assertEquals(listOf("save-start", "save-finish", "clear"), h.drafts.writeOrder)
        assertTrue(h.drafts.drafts.isEmpty())
        cleanup(h)
    }

    @Test
    @DisplayName("AC-27.send-restart an unknown send restores its exact operation after restart")
    fun unknownSendRestoresAfterRestart() = runTest(dispatcher) {
        val drafts = FakeDraftPort()
        val session = ConversationFixtures.session()
        val first = Harness(drafts = drafts)
        first.model.open(session)
        advanceUntilIdle()
        first.composer.submitOutcome = Outcome.Unconfirmed(
            CoreFailure("uncertain", "receipt lost", retryable = true),
        )
        first.model.selectFollowMode(FollowMode.ReceiptOnly)
        first.model.updateDraft("survive and recover")
        advanceUntilIdle()
        first.model.submit()
        advanceUntilIdle()
        val originalSendId = (first.model.state.value.send as SendUiState.Unconfirmed).sendId
        assertEquals(originalSendId.value, drafts.pendingSends[session]?.sendId)
        first.model.onHidden()
        advanceUntilIdle()

        // Process restart: the pending operation identity comes from the store.
        val restarted = Harness(drafts = drafts)
        restarted.composer.recoveryResult = RecoveryResult.Confirmed(
            receipt = SendReceipt(originalSendId, null),
            reply = ConversationFixtures.textMessage("restored-reply", "recovered", sequence = 1),
        )
        restarted.model.open(session)
        advanceUntilIdle()
        val restored = assertInstanceOf(
            SendUiState.Unconfirmed::class.java,
            restarted.model.state.value.send,
        )
        assertEquals(originalSendId, restored.sendId)

        restarted.model.recoverSend()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.Recovered::class.java, restarted.model.state.value.send)
        assertEquals(originalSendId, restarted.composer.recoveryRequests.single().sendId)
        assertTrue(restarted.composer.submits.isEmpty(), "a restored send is never resubmitted")
        assertEquals(
            1,
            restarted.model.state.value.items.filterIsInstance<CommittedMessageItem>().size,
        )
        assertNull(drafts.pendingSends[session])
        cleanup(restarted)
    }

    @Test
    @DisplayName("AC-27.send-restart-confirmed a confirmed send leaves no restart operation")
    fun confirmedSendIsNotRestored() = runTest(dispatcher) {
        val drafts = FakeDraftPort()
        val session = ConversationFixtures.session()
        val first = Harness(drafts = drafts)
        first.model.open(session)
        advanceUntilIdle()
        first.model.selectFollowMode(FollowMode.ReceiptOnly)
        first.model.updateDraft("already accepted")
        advanceUntilIdle()
        first.model.submit()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.ReceiptOnly::class.java, first.model.state.value.send)
        assertNull(drafts.pendingSends[session])

        val restarted = Harness(drafts = drafts)
        restarted.model.open(session)
        advanceUntilIdle()
        assertEquals(SendUiState.Idle, restarted.model.state.value.send)
        assertTrue(restarted.composer.submits.isEmpty())
        cleanup(restarted)
    }

    @Test
    @DisplayName("AC-27.send-restart-newer a newer draft outlives an older pending send")
    fun newerDraftOutlivesPendingSend() = runTest(dispatcher) {
        val drafts = FakeDraftPort()
        val session = ConversationFixtures.session()
        val first = Harness(drafts = drafts)
        first.model.open(session)
        advanceUntilIdle()
        first.composer.submitOutcome = Outcome.Unconfirmed(
            CoreFailure("uncertain", "receipt lost", retryable = true),
        )
        first.model.updateDraft("older submitted text")
        advanceUntilIdle()
        first.model.submit()
        advanceUntilIdle()
        first.model.updateDraft("newer draft")
        advanceUntilIdle()
        first.model.onHidden()
        advanceUntilIdle()

        val restarted = Harness(drafts = drafts)
        restarted.model.open(session)
        advanceUntilIdle()
        assertEquals("newer draft", restarted.model.state.value.composer.text)
        assertInstanceOf(SendUiState.Unconfirmed::class.java, restarted.model.state.value.send)

        // Dismissing the restored send restores only when no newer draft exists.
        restarted.model.discardUnconfirmedSend()
        advanceUntilIdle()
        assertEquals(SendUiState.Idle, restarted.model.state.value.send)
        assertEquals("newer draft", restarted.model.state.value.composer.text)
        assertEquals("newer draft", drafts.drafts[session])
        assertNull(drafts.pendingSends[session])
        cleanup(restarted)
    }

    @Test
    @DisplayName("AC-27.send-rejected-restart a rejected send restores text and clears identity")
    fun rejectedSendClearsIdentityForRestart() = runTest(dispatcher) {
        val drafts = FakeDraftPort()
        val session = ConversationFixtures.session()
        val first = Harness(drafts = drafts)
        first.model.open(session)
        advanceUntilIdle()
        first.composer.submitOutcome = Outcome.Failure(
            CoreFailure("rejected", "The Host rejected the send.", retryable = false),
        )
        first.model.updateDraft("retry after rejection")
        advanceUntilIdle()
        first.model.submit()
        advanceUntilIdle()
        assertNull(drafts.pendingSends[session])
        assertEquals("retry after rejection", drafts.drafts[session])
        first.model.onHidden()
        advanceUntilIdle()

        val restarted = Harness(drafts = drafts)
        restarted.model.open(session)
        advanceUntilIdle()
        assertEquals(SendUiState.Idle, restarted.model.state.value.send)
        assertEquals("retry after rejection", restarted.model.state.value.composer.text)
        cleanup(restarted)
    }

    @Test
    @DisplayName("AC-27.wait-cancel a cancelled wait cannot later settle and keeps observation")
    fun cancelledWaitCannotSettle() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        val gate = CompletableDeferred<Unit>()
        h.wait.gate = gate
        h.model.waitForQuiescence()
        advanceUntilIdle()
        val appWaitId = (h.model.state.value.wait as WaitUiState.Waiting).waitId
        val watchCount = h.observation.started.size

        h.model.cancelWait()
        advanceUntilIdle()
        assertEquals(listOf(appWaitId), h.wait.cancels)
        assertEquals(WaitUiState.Cancelled, h.model.state.value.wait)

        // The native attempt settles later with quiescence; the local cancelled
        // outcome must not be replaced, and Host observation keeps running.
        gate.complete(Unit)
        advanceUntilIdle()
        assertEquals(WaitUiState.Cancelled, h.model.state.value.wait)
        assertEquals(watchCount, h.observation.started.size)
        assertTrue(h.commands.stops.isEmpty(), "cancelling a wait never stops Host work")

        // A new wait gets its own identity and settles independently.
        h.wait.gate = null
        h.wait.result = WaitResult.Quiescent
        h.model.waitForQuiescence()
        advanceUntilIdle()
        assertEquals(WaitUiState.Quiescent, h.model.state.value.wait)
        assertEquals(2, h.wait.requests.size)
        assertTrue(h.wait.requests[1].waitId != appWaitId)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-27.wait-question a question-only result asks the user to respond")
    fun questionOnlyWaitAsks() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        val question = ConversationFixtures.questionItem(batchId = "batch-only")
        h.wait.result = WaitResult.QuestionInput(question)
        h.model.waitForPendingInput()
        advanceUntilIdle()

        assertEquals(WaitUiState.QuestionInput(question), h.model.state.value.wait)
        val entry = assertInstanceOf(
            PendingEntryUiState.Current::class.java,
            h.model.state.value.pendingEntry,
        )
        assertTrue(entry.items.contains(question))
        cleanup(h)
    }

    @Test
    @DisplayName("AC-27.queue-unconfirmed an ambiguous queue edit stays recoverable")
    fun ambiguousQueueEditStaysRecoverable() = runTest(dispatcher) {
        val h = ToolsHarness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        h.port.editOutcome = Outcome.Unconfirmed(
            CoreFailure("uncertain", "receipt pending", retryable = true),
        )
        h.model.beginEdit("item-1")
        h.model.updateEditDraft("edited text")
        val queueCallsBefore = h.port.queueCalls.size
        h.model.saveEdit()
        advanceUntilIdle()

        assertInstanceOf(
            ToolsOperationState.Unconfirmed::class.java,
            h.model.state.value.operation,
        )
        val queue = assertInstanceOf(QueueUiState.Ready::class.java, h.model.state.value.queue)
        assertEquals("queued message", queue.items.single().text)
        assertEquals("item-1", h.model.state.value.editingItemId)
        assertEquals("edited text", h.model.state.value.editDraft)
        assertTrue(
            h.port.queueCalls.size > queueCallsBefore,
            "an ambiguous change refreshes the authoritative Host queue",
        )
    }

    @Test
    @DisplayName("AC-27.queue-rejected a rejected queue edit leaves the queue and draft intact")
    fun rejectedQueueEditLeavesStateIntact() = runTest(dispatcher) {
        val h = ToolsHarness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        h.port.removeOutcome = Outcome.Failure(
            CoreFailure("queue_rejected", "The Host rejected the change.", retryable = false),
        )
        h.model.beginEdit("item-1")
        h.model.updateEditDraft("keep my edit")
        h.model.removeItem("item-1")
        advanceUntilIdle()

        assertInstanceOf(ToolsOperationState.Failed::class.java, h.model.state.value.operation)
        val queue = assertInstanceOf(QueueUiState.Ready::class.java, h.model.state.value.queue)
        assertEquals(
            listOf("item-1"),
            queue.items.map { it.itemId },
            "a rejected removal never drops the local row",
        )
        assertEquals("keep my edit", h.model.state.value.editDraft)
        assertEquals("item-1", h.model.state.value.editingItemId)
    }

    @Test
    @DisplayName("AC-27.queue-confirmed a confirmed queue edit applies and clears its draft")
    fun confirmedQueueEditApplies() = runTest(dispatcher) {
        val h = ToolsHarness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        h.model.beginEdit("item-1")
        h.model.updateEditDraft("confirmed edit")
        h.model.saveEdit()
        advanceUntilIdle()

        assertInstanceOf(ToolsOperationState.Applied::class.java, h.model.state.value.operation)
        val queue = assertInstanceOf(QueueUiState.Ready::class.java, h.model.state.value.queue)
        assertEquals("confirmed edit", queue.items.single().text)
        assertNull(h.model.state.value.editingItemId)
    }

    @Test
    @DisplayName("AC-27.approval-only a Host without questions still approves")
    fun approvalOnlyHostStillWorks() = runTest(dispatcher) {
        val h = PendingHarness()
        val approval = ConversationFixtures.approvalItem(guard = "3:${"d".repeat(64)}")
        h.port.listResult = PendingListResult.Current(listOf(approval))
        h.port.detailResult = PendingDetailResult.Found(approval)
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        val entry = assertInstanceOf(
            PendingEntryUiState.Current::class.java,
            h.model.state.value.entry,
        )
        assertEquals(listOf(approval), entry.items)

        h.model.openItem(approval.itemId)
        advanceUntilIdle()
        h.model.approve()
        advanceUntilIdle()
        assertInstanceOf(PendingActionUiState.Applied::class.java, h.model.state.value.action)
        assertEquals(approval.itemId, h.port.resolveCalls.single().first)
        assertEquals(approval.guard, h.port.resolveCalls.single().third)
    }

    @Test
    @DisplayName("AC-27.question-cancel cancellation keeps the original reviewed answer")
    fun questionAnswerAndCancelKeepIdentity() = runTest(dispatcher) {
        val h = PendingHarness()
        val question = ConversationFixtures.questionItem(
            batchId = "batch-7",
            guard = "4:${"e".repeat(64)}",
        )
        h.port.listResult = PendingListResult.Current(listOf(question))
        h.port.detailResult = PendingDetailResult.Found(question)
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        h.model.openItem(question.itemId)
        advanceUntilIdle()
        h.model.setAnswer("question-1", emptyList(), "the answer")
        val gate = CompletableDeferred<Unit>()
        h.port.answerGate = gate
        h.model.submitAnswers()
        advanceUntilIdle()

        val call = h.port.answerCalls.single()
        assertEquals("batch-7", call.batchId)
        assertEquals(question.guard, call.guard)
        val appAnswerId = call.answerRef.answerId

        h.model.cancelAnswer()
        advanceUntilIdle()
        assertEquals(appAnswerId, h.port.cancelCalls.single().answerId)
        gate.complete(Unit)
        advanceUntilIdle()
        // A late answer completion cannot replace the explicit cancellation.
        assertEquals(appAnswerId, h.port.cancelCalls.single().answerId)
    }

    @Test
    @DisplayName("AC-27.question-guard a changed guard requires a fresh review")
    fun changedQuestionGuardRequiresReview() = runTest(dispatcher) {
        val h = PendingHarness()
        val reviewed = ConversationFixtures.questionItem(batchId = "batch-1", guard = "1:old")
        h.port.listResult = PendingListResult.Current(listOf(reviewed))
        h.port.detailResult = PendingDetailResult.Found(reviewed)
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        h.model.openItem(reviewed.itemId)
        advanceUntilIdle()
        h.model.setAnswer("question-1", emptyList(), "answer")

        val changed = ConversationFixtures.questionItem(batchId = "batch-1", guard = "2:new")
        h.model.observeLive(listOf(changed))
        h.model.submitAnswers()
        advanceUntilIdle()

        assertInstanceOf(
            PendingActionUiState.GuardConflict::class.java,
            h.model.state.value.action,
        )
        assertTrue(h.port.answerCalls.isEmpty(), "a stale reviewed answer is never submitted")
    }

    @Test
    @DisplayName("AC-27.create-retry a created session survives a failed details read")
    fun createRetainsConfirmedSession() = runTest(dispatcher) {
        val h = SessionHarness()
        h.model.open(ConversationFixtures.host())
        advanceUntilIdle()
        val created = SessionRef(ConversationFixtures.host(), id("created-1"))
        h.port.createOutcome = Outcome.Success(CreatedSession(created, details = null))
        h.model.createDefault()
        advanceUntilIdle()

        val state = h.model.state.value
        val create = assertInstanceOf(
            CreateSessionUiState.Created::class.java,
            state.create,
        )
        assertEquals(created, create.session)
        assertEquals("created-1", state.selectedSessionId?.value)
        val details = assertInstanceOf(SessionDetailsUiState.Unavailable::class.java, state.details)
        assertTrue(details.failure.retryable)
        assertEquals(1, h.port.createCalls.size)

        // Retry only the details read; the created session is not created again.
        h.port.detailsOutcome = Outcome.Success(ConversationFixtures.sessionDetails(created))
        h.model.refreshDetails()
        advanceUntilIdle()
        assertInstanceOf(SessionDetailsUiState.Loaded::class.java, h.model.state.value.details)
        assertEquals(1, h.port.createCalls.size)
        assertEquals(1, h.port.detailsCalls.size)
    }

    @Test
    @DisplayName("AC-27.fork-completed a completed session forks its latest turn in scope")
    fun forkCompletedSessionOpensChild() = runTest(dispatcher) {
        val h = SessionHarness()
        val host = ConversationFixtures.host()
        h.model.open(host)
        advanceUntilIdle()
        val parent = ConversationFixtures.session(host = host.hostId.value, sessionId = "session-a")
        h.port.detailsOutcome = Outcome.Success(
            ConversationFixtures.sessionDetails(parent, latestCompletedTurnId = "turn-9"),
        )
        h.model.select(parent.sessionId)
        advanceUntilIdle()
        assertEquals(
            "turn-9",
            assertInstanceOf(
                SessionDetailsUiState.Loaded::class.java,
                h.model.state.value.details,
            ).details.latestCompletedTurnId,
        )

        h.model.fork()
        advanceUntilIdle()
        assertEquals("turn-9", h.port.forkCalls.single().second.turnId)
        val forked = assertInstanceOf(
            ForkSessionUiState.Forked::class.java,
            h.model.state.value.fork,
        )
        assertEquals(host, forked.child.host)
        assertEquals("child-1", forked.child.sessionId.value)
        assertEquals("child-1", h.model.state.value.selectedSessionId?.value)
        assertEquals(forked.child, h.model.state.value.details.let {
            (it as SessionDetailsUiState.Loaded).details.session
        })
    }

    @Test
    @DisplayName("AC-27.history-focus a message focus id reaches the exact reread")
    fun focusDispatchesPendingThenExact() {
        val pending = ConversationFixtures.approvalItem(approvalId = "approval-1")
        assertEquals(
            ConversationFocusDispatch.PendingInput,
            dispatchConversationFocus(
                "approval-1",
                "approval-1",
                PendingDetailUiState.Loaded(pending),
            ),
        )
        assertEquals(
            ConversationFocusDispatch.ExactMessage("message-1"),
            dispatchConversationFocus(
                "message-1",
                "message-1",
                PendingDetailUiState.Missing,
            ),
        )
        // A focus id that has not been opened yet, or whose lookup is still
        // pending/unavailable, never guesses an exact read.
        assertEquals(
            ConversationFocusDispatch.None,
            dispatchConversationFocus(
                "message-1",
                "other-id",
                PendingDetailUiState.Missing,
            ),
        )
        assertEquals(
            ConversationFocusDispatch.None,
            dispatchConversationFocus(
                "message-1",
                "message-1",
                PendingDetailUiState.Loading,
            ),
        )
        assertEquals(
            ConversationFocusDispatch.None,
            dispatchConversationFocus(
                "message-1",
                "message-1",
                PendingDetailUiState.Unavailable(CoreFailure("host_offline", "offline")),
            ),
        )
        assertEquals(
            ConversationFocusDispatch.None,
            dispatchConversationFocus(null, null, PendingDetailUiState.Missing),
        )
    }

    @Test
    @DisplayName("AC-27.history-exact an offline cache miss stays distinct from not-found")
    fun exactHistoryFailuresStayDistinct() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()

        h.history.exact = Outcome.Failure(
            CoreFailure("not_found", "message not found", retryable = false),
        )
        h.model.loadExact("missing-message")
        advanceUntilIdle()
        assertEquals("not_found", h.model.state.value.history.failure?.code)

        h.history.exact = Outcome.Failure(
            CoreFailure("host_offline", "offline", retryable = true),
        )
        h.model.loadExact("missing-offline")
        advanceUntilIdle()
        assertEquals("host_offline", h.model.state.value.history.failure?.code)

        val message = ConversationFixtures.textMessage("recovered-1", "found online", sequence = 3)
        h.history.exact = Outcome.Success(
            ConversationFixtures.page(listOf(message), exact = true),
        )
        h.model.loadExact("recovered-1")
        advanceUntilIdle()
        assertNull(h.model.state.value.history.failure)
        assertTrue(
            h.model.state.value.items.filterIsInstance<CommittedMessageItem>()
                .any { it.message.messageId == "recovered-1" },
        )
        assertEquals("recovered-1", h.history.exactCalls.last().second)
        cleanup(h)
    }

    private fun id(value: String) = SessionId(value)

    private fun TestScope.cleanup(h: Harness) {
        h.model.onHidden()
        advanceUntilIdle()
    }

    private class Harness(drafts: FakeDraftPort = FakeDraftPort()) {
        val observation = FakeObservationPort()
        val history = FakeHistoryPort()
        val composer = FakeComposerPort()
        val wait = FakeWaitPort()
        val commands = FakeCommandPort()
        val pending = FakePendingInputPort()
        val drafts = drafts
        val models = FakeModelsQueuePort()
        val clock = FakeClock()
        val diagnostics = RecordingDiagnosticSink()
        val model = ConversationViewModel(
            observation = observation,
            history = history,
            composer = composer,
            wait = wait,
            commands = commands,
            pendingInput = pending,
            drafts = this.drafts,
            clock = clock,
            ids = SequentialIdGenerator("id"),
            diagnostics = diagnostics,
            queue = models,
        )
    }

    private class PendingHarness {
        val port = FakePendingInputPort()
        val diagnostics = RecordingDiagnosticSink()
        val model = PendingInputViewModel(
            port = port,
            clock = FakeClock(),
            ids = SequentialIdGenerator("answer"),
            diagnostics = diagnostics,
        )
    }

    private class ToolsHarness {
        val port = FakeModelsQueuePort()
        val diagnostics = RecordingDiagnosticSink()
        val model = SessionToolsViewModel(
            port = port,
            ids = SequentialIdGenerator("tools"),
            diagnostics = diagnostics,
        )
    }

    private class SessionHarness {
        val port = FakeSessionListPort()
        val diagnostics = RecordingDiagnosticSink()
        val model = SessionListViewModel(port = port, diagnostics = diagnostics)
    }
}
