package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.fixtures.conversation.ConversationFixtures
import com.whalerein.android.state.fixtures.conversation.FakeClock
import com.whalerein.android.state.fixtures.conversation.FakePendingInputPort
import com.whalerein.android.state.fixtures.conversation.RecordingDiagnosticSink
import com.whalerein.android.state.fixtures.conversation.SequentialIdGenerator
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class PendingInputViewModelTest : ConversationTestBase() {
    @Test
    @DisplayName("AC-18.discovery live and historical items appear in the fixed entry without focus theft")
    fun ac18Discovery() = runTest(dispatcher) {
        val h = Harness()
        h.port.listResult = PendingListResult.Current(emptyList())
        h.model.open(h.scope)
        advanceUntilIdle()
        assertEquals(PendingEntryUiState.CurrentEmpty, h.model.state.value.entry)

        val question = ConversationFixtures.questionItem()
        h.model.observeLive(listOf(question))
        advanceUntilIdle()
        assertInstanceOf(PendingEntryUiState.Current::class.java, h.model.state.value.entry)
        assertFalse(h.model.state.value.sheetRequested)
        assertEquals(PendingFocusOrigin.None, h.model.state.value.focusOrigin)
        assertNull(h.model.state.value.focusedItemId)

        h.port.detailResult = PendingDetailResult.Found(question)
        h.model.openItem(question.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        assertTrue(h.model.state.value.sheetRequested)
        assertEquals(PendingFocusOrigin.Live, h.model.state.value.focusOrigin)
        assertEquals(question.itemId, h.model.state.value.focusedItemId)
        assertEquals(Guard("guard-1"), h.model.state.value.reviewedGuard)

        h.model.setAnswer("question-1", emptyList(), "draft answer")
        advanceUntilIdle()
        assertTrue(h.port.answerCalls.isEmpty())

        // A question outside committed history still appears, and a live update
        // does not move focus, close the sheet or discard the typed answer.
        h.model.observeLive(listOf(question, ConversationFixtures.approvalItem()))
        advanceUntilIdle()
        assertEquals(question.itemId, h.model.state.value.focusedItemId)
        assertEquals("draft answer", h.model.state.value.answers["question-1"]?.text)
        assertTrue(h.model.state.value.sheetRequested)
    }

    @Test
    @DisplayName("AC-18.approvals approve and reject use the reviewed guard")
    fun ac18Approvals() = runTest(dispatcher) {
        val h = Harness()
        val approval = ConversationFixtures.approvalItem()
        h.port.listResult = PendingListResult.Current(listOf(approval))
        h.port.detailResult = PendingDetailResult.Found(approval)
        h.model.open(h.scope)
        advanceUntilIdle()
        h.model.openItem(approval.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        h.model.approve()
        advanceUntilIdle()
        assertEquals(
            listOf(Triple("approval-1", ApprovalDecision.Approve, Guard("guard-1"))),
            h.port.resolveCalls,
        )
        assertEquals(
            PendingActionUiState.Applied(PendingAction.Approve, "approval-1"),
            h.model.state.value.action,
        )
        assertEquals(PendingEntryUiState.CurrentEmpty, h.model.state.value.entry)
        assertEquals(PendingDetailUiState.Missing, h.model.state.value.detail)

        val rejecting = Harness()
        rejecting.port.listResult = PendingListResult.Current(listOf(approval))
        rejecting.port.detailResult = PendingDetailResult.Found(approval)
        rejecting.model.open(rejecting.scope)
        advanceUntilIdle()
        rejecting.model.openItem(approval.itemId, PendingFocusOrigin.Historical)
        advanceUntilIdle()
        rejecting.model.reject()
        advanceUntilIdle()
        assertEquals(
            ApprovalDecision.Reject,
            rejecting.port.resolveCalls.single().second,
        )
    }

    @Test
    @DisplayName("AC-18.approvals a repeated decision while in flight issues one command")
    fun repeatedApprovalWhileInFlightIsIgnored() = runTest(dispatcher) {
        val h = Harness()
        val approval = ConversationFixtures.approvalItem()
        h.port.listResult = PendingListResult.Current(listOf(approval))
        h.port.detailResult = PendingDetailResult.Found(approval)
        h.model.open(h.scope)
        advanceUntilIdle()
        h.model.openItem(approval.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        val gate = CompletableDeferred<Unit>()
        h.port.resolveGate = gate
        h.model.approve()
        advanceUntilIdle()
        assertEquals(1, h.port.resolveCalls.size)
        assertInstanceOf(
            PendingActionUiState.InFlight::class.java,
            h.model.state.value.action,
        )

        h.model.approve()
        advanceUntilIdle()
        assertEquals(1, h.port.resolveCalls.size)

        gate.complete(Unit)
        advanceUntilIdle()
        assertEquals(
            PendingActionUiState.Applied(PendingAction.Approve, approval.itemId),
            h.model.state.value.action,
        )
    }

    @Test
    @DisplayName("AC-18.answers structured open and continued answers and explicit cancel")
    fun ac18Answers() = runTest(dispatcher) {
        val h = Harness()
        val batch = ConversationFixtures.questionBatch(
            questions = listOf(
                ConversationFixtures.questionBatch().questions.single(),
                ConversationFixtures.multiChoiceQuestion(),
            ),
        )
        val item = PendingItem.Questions(batch)
        h.port.listResult = PendingListResult.Current(listOf(item))
        h.port.detailResult = PendingDetailResult.Found(item)
        h.model.open(h.scope)
        advanceUntilIdle()
        h.model.openItem(item.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()

        h.model.setAnswer("question-1", emptyList(), "continue with plan")
        h.model.setAnswer("question-2", listOf("a", "b"), "both are fine")
        advanceUntilIdle()
        assertTrue(h.port.answerCalls.isEmpty())

        h.model.submitAnswers()
        advanceUntilIdle()
        val call = h.port.answerCalls.single()
        assertEquals("batch-1", call.batchId)
        assertEquals(Guard("guard-1"), call.guard)
        assertEquals(2, call.answers.size)
        assertEquals("continue with plan", call.answers.first { it.questionId == "question-1" }.text)
        assertEquals(
            listOf("a", "b"),
            call.answers.first { it.questionId == "question-2" }.selectedOptionIds,
        )
        assertEquals(
            PendingActionUiState.Applied(PendingAction.Answer, "batch-1"),
            h.model.state.value.action,
        )

        val cancelling = Harness()
        cancelling.port.listResult = PendingListResult.Current(listOf(item))
        cancelling.port.detailResult = PendingDetailResult.Found(item)
        val gate = CompletableDeferred<Unit>()
        cancelling.port.answerGate = gate
        cancelling.model.open(cancelling.scope)
        advanceUntilIdle()
        cancelling.model.openItem(item.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        cancelling.model.setAnswer("question-1", emptyList(), "will be cancelled")
        cancelling.model.submitAnswers()
        advanceUntilIdle()
        assertTrue(cancelling.model.state.value.answerInFlight)
        assertNotNull(cancelling.model.state.value.answerId)

        cancelling.model.cancelAnswer()
        advanceUntilIdle()
        assertEquals(1, cancelling.port.cancelCalls.size)
        assertFalse(cancelling.model.state.value.answerInFlight)
        assertEquals(
            PendingActionUiState.Applied(PendingAction.CancelAnswer),
            cancelling.model.state.value.action,
        )
        gate.complete(Unit)
        advanceUntilIdle()
    }

    @Test
    @DisplayName("AC-18.freshness stale unavailable unsupported expired changed and external outcomes")
    fun ac18Freshness() = runTest(dispatcher) {
        val loading = Harness()
        loading.model.open(loading.scope)
        assertEquals(PendingEntryUiState.Loading, loading.model.state.value.entry)
        advanceUntilIdle()

        loading.port.listResult = PendingListResult.Stale(
            CoreFailure("stale", "stale collection"),
        )
        loading.model.refresh()
        advanceUntilIdle()
        assertInstanceOf(PendingEntryUiState.Stale::class.java, loading.model.state.value.entry)

        loading.port.listResult = PendingListResult.Unavailable(
            CoreFailure("unavailable", "unavailable"),
        )
        loading.model.refresh()
        advanceUntilIdle()
        assertInstanceOf(PendingEntryUiState.Unavailable::class.java, loading.model.state.value.entry)

        loading.port.listResult = PendingListResult.Unsupported
        loading.model.refresh()
        advanceUntilIdle()
        assertEquals(PendingEntryUiState.Unsupported, loading.model.state.value.entry)

        loading.port.listResult = PendingListResult.Current(emptyList())
        loading.model.refresh()
        advanceUntilIdle()
        assertEquals(PendingEntryUiState.CurrentEmpty, loading.model.state.value.entry)

        val changed = Harness()
        val firstGuard = ConversationFixtures.approvalItem(guard = "guard-1")
        changed.port.listResult = PendingListResult.Current(listOf(firstGuard))
        changed.port.detailResult = PendingDetailResult.Found(firstGuard)
        changed.model.open(changed.scope)
        advanceUntilIdle()
        changed.model.openItem(firstGuard.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        changed.model.observeLive(
            listOf(ConversationFixtures.approvalItem(guard = "guard-2")),
        )
        changed.model.approve()
        advanceUntilIdle()
        assertInstanceOf(
            PendingActionUiState.GuardConflict::class.java,
            changed.model.state.value.action,
        )
        assertTrue(changed.port.resolveCalls.isEmpty())

        val expired = Harness()
        val expiredApproval = ConversationFixtures.approvalItem(expiresAtMillis = 1_000)
        expired.clock.nowMillis = 2_000
        expired.port.listResult = PendingListResult.Current(listOf(expiredApproval))
        expired.port.detailResult = PendingDetailResult.Found(expiredApproval)
        expired.model.open(expired.scope)
        advanceUntilIdle()
        expired.model.openItem(expiredApproval.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        expired.model.approve()
        advanceUntilIdle()
        assertEquals(PendingActionUiState.Expired, expired.model.state.value.action)
        assertTrue(expired.port.resolveCalls.isEmpty())

        val external = Harness()
        val resolved = ConversationFixtures.approvalItem()
        external.port.listResult = PendingListResult.Current(listOf(resolved))
        external.port.detailResult = PendingDetailResult.Found(resolved)
        external.port.approvalOutcome = PendingActionOutcome.ResolvedElsewhere
        external.model.open(external.scope)
        advanceUntilIdle()
        external.model.openItem(resolved.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        external.model.approve()
        advanceUntilIdle()
        assertEquals(PendingActionUiState.ResolvedElsewhere, external.model.state.value.action)
        assertEquals(PendingEntryUiState.CurrentEmpty, external.model.state.value.entry)
    }

    @Test
    @DisplayName("AC-18.focus focus changes are explicit and never move reading position")
    fun ac18Focus() = runTest(dispatcher) {
        val h = Harness()
        val first = ConversationFixtures.approvalItem("approval-1", "guard-1")
        val second = ConversationFixtures.approvalItem("approval-2", "guard-2")
        h.port.listResult = PendingListResult.Current(listOf(first, second))
        h.port.detailResult = PendingDetailResult.Found(second)
        h.model.open(h.scope)
        advanceUntilIdle()

        h.model.observeLive(listOf(first, second))
        advanceUntilIdle()
        assertNull(h.model.state.value.focusedItemId)
        assertFalse(h.model.state.value.sheetRequested)

        h.model.openItem(second.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        assertEquals(second.itemId, h.model.state.value.focusedItemId)
        assertEquals(PendingFocusOrigin.Live, h.model.state.value.focusOrigin)
        assertTrue(h.model.state.value.sheetRequested)
        assertEquals(Guard("guard-2"), h.model.state.value.reviewedGuard)

        h.model.dismissSheet()
        advanceUntilIdle()
        assertFalse(h.model.state.value.sheetRequested)
        assertEquals(second.itemId, h.model.state.value.focusedItemId)

        h.model.observeLive(listOf(first, second))
        advanceUntilIdle()
        assertEquals(second.itemId, h.model.state.value.focusedItemId)
        assertEquals(Guard("guard-2"), h.model.state.value.reviewedGuard)

        h.port.detailResult = PendingDetailResult.Found(first)
        h.model.openItem(first.itemId, PendingFocusOrigin.Historical)
        advanceUntilIdle()
        assertEquals(first.itemId, h.model.state.value.focusedItemId)
        assertEquals(PendingFocusOrigin.Historical, h.model.state.value.focusOrigin)
        assertEquals(Guard("guard-1"), h.model.state.value.reviewedGuard)

        // Reading position is owned by the conversation screen; this fixed entry
        // never requests focus, opens a dialog or calls a port from an edit.
        val getsBefore = h.port.getCalls.size
        h.model.setAnswer("question-1", emptyList(), "typing")
        advanceUntilIdle()
        assertEquals(getsBefore, h.port.getCalls.size)
        assertTrue(h.port.answerCalls.isEmpty())
    }

    private class Harness {
        val scope = ConversationFixtures.session()
        val port = FakePendingInputPort()
        val clock = FakeClock()
        val diagnostics = RecordingDiagnosticSink()
        val model = PendingInputViewModel(
            port = port,
            clock = clock,
            ids = SequentialIdGenerator("answer"),
            diagnostics = diagnostics,
        )
    }

    @Test
    @DisplayName("AC-18.approvals a decided approval stays hidden until the authoritative list drops it")
    fun decidedApprovalDoesNotReappearFromStaleLiveSnapshot() = runTest(dispatcher) {
        val h = Harness()
        val approval = ConversationFixtures.approvalItem()
        h.port.listResult = PendingListResult.Current(listOf(approval))
        h.port.detailResult = PendingDetailResult.Found(approval)
        h.model.open(h.scope)
        advanceUntilIdle()
        h.model.openItem(approval.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        h.model.approve()
        advanceUntilIdle()
        assertEquals(PendingEntryUiState.CurrentEmpty, h.model.state.value.entry)

        h.model.observeLive(listOf(approval))
        advanceUntilIdle()
        assertEquals(PendingEntryUiState.CurrentEmpty, h.model.state.value.entry)

        h.model.observeLive(emptyList())
        advanceUntilIdle()
        h.model.observeLive(listOf(approval))
        advanceUntilIdle()
        val visible = h.model.state.value.entry as PendingEntryUiState.Current
        assertEquals(listOf(approval), visible.items)
    }

    @Test
    @DisplayName("AC-18.answers repeated submit while in flight is ignored")
    fun repeatedSubmitWhileInFlightIsIgnored() = runTest(dispatcher) {
        val h = Harness()
        val item = PendingItem.Questions(ConversationFixtures.questionBatch())
        h.port.listResult = PendingListResult.Current(listOf(item))
        h.port.detailResult = PendingDetailResult.Found(item)
        h.model.open(h.scope)
        advanceUntilIdle()
        h.model.openItem(item.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        h.model.setAnswer("question-1", emptyList(), "only once")
        val gate = CompletableDeferred<Unit>()
        h.port.answerGate = gate
        h.model.submitAnswers()
        advanceUntilIdle()
        assertEquals(1, h.port.answerCalls.size)
        assertTrue(h.model.state.value.answerInFlight)

        // A second tap must not issue another command or clear the draft.
        h.model.submitAnswers()
        advanceUntilIdle()
        assertEquals(1, h.port.answerCalls.size)
        assertTrue(h.model.state.value.answerInFlight)
        assertEquals("only once", h.model.state.value.answers["question-1"]?.text)

        gate.complete(Unit)
        advanceUntilIdle()
        assertEquals(
            PendingActionUiState.Applied(PendingAction.Answer, item.itemId),
            h.model.state.value.action,
        )
    }

    @Test
    @DisplayName("AC-18.answers an answered item stays hidden until the authoritative list drops it")
    fun answeredItemDoesNotReappearFromStaleLiveSnapshot() = runTest(dispatcher) {
        val h = Harness()
        val item = PendingItem.Questions(ConversationFixtures.questionBatch())
        h.port.listResult = PendingListResult.Current(listOf(item))
        h.port.detailResult = PendingDetailResult.Found(item)
        h.model.open(h.scope)
        advanceUntilIdle()
        h.model.openItem(item.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        h.model.setAnswer("question-1", emptyList(), "answered")
        h.model.submitAnswers()
        advanceUntilIdle()
        assertEquals(PendingEntryUiState.CurrentEmpty, h.model.state.value.entry)

        // The Host observation still carries the answered item for a moment.
        h.model.observeLive(listOf(item))
        advanceUntilIdle()
        assertEquals(PendingEntryUiState.CurrentEmpty, h.model.state.value.entry)

        // Once the authoritative list drops it, a genuinely new request with
        // the same id is allowed to appear again.
        h.model.observeLive(emptyList())
        advanceUntilIdle()
        assertEquals(PendingEntryUiState.CurrentEmpty, h.model.state.value.entry)
        h.model.observeLive(listOf(item))
        advanceUntilIdle()
        val visible = h.model.state.value.entry as PendingEntryUiState.Current
        assertEquals(listOf(item), visible.items)
    }

    @Test
    @DisplayName("AC-18.answers a historical entry cannot reopen a locally answered item")
    fun historicalEntryCannotReopenAnsweredItem() = runTest(dispatcher) {
        val h = Harness()
        val item = PendingItem.Questions(ConversationFixtures.questionBatch())
        h.port.listResult = PendingListResult.Current(listOf(item))
        h.port.detailResult = PendingDetailResult.Found(item)
        h.model.open(h.scope)
        advanceUntilIdle()
        h.model.openItem(item.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        h.model.setAnswer("question-1", emptyList(), "answered")
        h.model.submitAnswers()
        advanceUntilIdle()
        val getsBefore = h.port.getCalls.size

        // A deep link or history entry points at the same answered item while
        // the Host list has not caught up yet.
        h.model.openItem(item.itemId, PendingFocusOrigin.Historical)
        advanceUntilIdle()
        assertEquals(getsBefore, h.port.getCalls.size)
        assertEquals(PendingDetailUiState.Missing, h.model.state.value.detail)
        assertFalse(h.model.state.value.sheetRequested)
    }
}
