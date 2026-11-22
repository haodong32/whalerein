package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.fixtures.conversation.ConversationFixtures
import com.whalerein.android.state.fixtures.conversation.FakeClock
import com.whalerein.android.state.fixtures.conversation.FakePendingInputPort
import com.whalerein.android.state.fixtures.conversation.RecordingDiagnosticSink
import com.whalerein.android.state.fixtures.conversation.SequentialIdGenerator
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/** Additional branch coverage for pending-input review state. */
@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class PendingInputViewModelCoverageTest : ConversationTestBase() {
    @Test
    @DisplayName("AC-18.freshness detail loading resolves missing changed unavailable and throwable")
    fun ac18DetailStates() = runTest(dispatcher) {
        val h = Harness()
        val item = ConversationFixtures.approvalItem()
        h.port.listResult = PendingListResult.Current(listOf(item))
        h.model.open(h.scope)
        advanceUntilIdle()

        h.port.detailResult = PendingDetailResult.Missing
        h.model.openItem(item.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        assertEquals(PendingDetailUiState.Missing, h.model.state.value.detail)

        h.port.detailResult = PendingDetailResult.Changed(CoreFailure("changed", "changed"))
        h.model.openItem(item.itemId, PendingFocusOrigin.Historical)
        advanceUntilIdle()
        assertInstanceOf(PendingDetailUiState.Changed::class.java, h.model.state.value.detail)

        h.port.detailResult = PendingDetailResult.Unavailable(CoreFailure("down", "down"))
        h.model.openItem(item.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        assertInstanceOf(PendingDetailUiState.Unavailable::class.java, h.model.state.value.detail)

        h.port.getThrowable = IllegalStateException("boom")
        h.model.openItem(item.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        assertInstanceOf(PendingDetailUiState.Unavailable::class.java, h.model.state.value.detail)

        h.port.listThrowable = IllegalStateException("boom")
        h.model.refresh()
        advanceUntilIdle()
        assertInstanceOf(PendingEntryUiState.Unavailable::class.java, h.model.state.value.entry)

        // Reopening the same session is a no-op and never reloads the collection.
        val calls = h.port.listCalls.size
        h.model.open(h.scope)
        advanceUntilIdle()
        assertEquals(calls, h.port.listCalls.size)
    }

    @Test
    @DisplayName("AC-18.approvals every decision outcome maps to its current-state result")
    fun ac18DecisionOutcomes() = runTest(dispatcher) {
        val changed = approvalHarness()
        changed.port.approvalOutcome = PendingActionOutcome.GuardChanged(Guard("guard-2"))
        val changedQueueCalls = changed.port.listCalls.size
        changed.model.approve()
        advanceUntilIdle()
        assertInstanceOf(PendingActionUiState.GuardConflict::class.java, changed.model.state.value.action)
        assertTrue(changed.port.listCalls.size > changedQueueCalls)

        val expired = approvalHarness()
        expired.port.approvalOutcome = PendingActionOutcome.Expired
        expired.model.approve()
        advanceUntilIdle()
        assertEquals(PendingActionUiState.Expired, expired.model.state.value.action)

        val unsupported = approvalHarness()
        unsupported.port.approvalOutcome = PendingActionOutcome.Unsupported
        unsupported.model.approve()
        advanceUntilIdle()
        assertEquals(PendingActionUiState.Unsupported, unsupported.model.state.value.action)

        val unconfirmed = approvalHarness()
        unconfirmed.port.approvalOutcome = PendingActionOutcome.Unconfirmed(
            CoreFailure("unknown", "unknown", retryable = true),
        )
        unconfirmed.model.approve()
        advanceUntilIdle()
        assertInstanceOf(PendingActionUiState.Unconfirmed::class.java, unconfirmed.model.state.value.action)

        val failed = approvalHarness()
        failed.port.approvalOutcome = PendingActionOutcome.Failed(CoreFailure("failed", "failed"))
        failed.model.approve()
        advanceUntilIdle()
        assertInstanceOf(PendingActionUiState.Failed::class.java, failed.model.state.value.action)

        val thrown = approvalHarness()
        thrown.port.resolveThrowable = IllegalStateException("boom")
        thrown.model.approve()
        advanceUntilIdle()
        assertInstanceOf(PendingActionUiState.Failed::class.java, thrown.model.state.value.action)
    }

    @Test
    @DisplayName("AC-18.answers guard expiry empty and outcome branches stay explicit")
    fun ac18AnswerBranches() = runTest(dispatcher) {
        val changed = answerHarness()
        changed.model.observeLive(
            listOf(
                PendingItem.Questions(
                    ConversationFixtures.questionBatch(guard = "guard-2"),
                ),
            ),
        )
        changed.model.submitAnswers()
        advanceUntilIdle()
        assertInstanceOf(PendingActionUiState.GuardConflict::class.java, changed.model.state.value.action)
        assertTrue(changed.port.answerCalls.isEmpty())

        val expired = answerHarness(expiresAtMillis = 1_000)
        expired.clock.nowMillis = 2_000
        expired.model.submitAnswers()
        advanceUntilIdle()
        assertEquals(PendingActionUiState.Expired, expired.model.state.value.action)
        assertTrue(expired.port.answerCalls.isEmpty())

        val unsupported = answerHarness()
        unsupported.model.setAnswer("question-1", emptyList(), "answer")
        unsupported.port.answerOutcome = PendingActionOutcome.Unsupported
        unsupported.model.submitAnswers()
        advanceUntilIdle()
        assertEquals(PendingActionUiState.Unsupported, unsupported.model.state.value.action)

        val unconfirmed = answerHarness()
        unconfirmed.model.setAnswer("question-1", emptyList(), "answer")
        unconfirmed.port.answerOutcome = PendingActionOutcome.Unconfirmed(
            CoreFailure("unknown", "unknown"),
        )
        unconfirmed.model.submitAnswers()
        advanceUntilIdle()
        assertInstanceOf(PendingActionUiState.Unconfirmed::class.java, unconfirmed.model.state.value.action)

        val failed = answerHarness()
        failed.model.setAnswer("question-1", emptyList(), "answer")
        failed.port.answerOutcome = PendingActionOutcome.Failed(CoreFailure("failed", "failed"))
        failed.model.submitAnswers()
        advanceUntilIdle()
        assertInstanceOf(PendingActionUiState.Failed::class.java, failed.model.state.value.action)

        val thrown = answerHarness()
        thrown.model.setAnswer("question-1", emptyList(), "answer")
        thrown.port.answerThrowable = IllegalStateException("boom")
        thrown.model.submitAnswers()
        advanceUntilIdle()
        assertInstanceOf(PendingActionUiState.Failed::class.java, thrown.model.state.value.action)
    }

    @Test
    @DisplayName("AC-18.answers cancel without ownership and cancel failure stay explicit")
    fun ac18CancelBranches() = runTest(dispatcher) {
        val h = answerHarness()
        h.model.cancelAnswer()
        advanceUntilIdle()
        assertTrue(h.port.cancelCalls.isEmpty())

        h.model.setAnswer("question-1", emptyList(), "answer")
        h.port.answerGate = kotlinx.coroutines.CompletableDeferred()
        h.model.submitAnswers()
        advanceUntilIdle()
        assertTrue(h.model.state.value.answerInFlight)
        h.port.cancelThrowable = IllegalStateException("boom")
        h.model.cancelAnswer()
        advanceUntilIdle()
        assertInstanceOf(PendingActionUiState.Failed::class.java, h.model.state.value.action)
        assertEquals(1, h.port.cancelCalls.size)

        h.model.clearAction()
        assertEquals(PendingActionUiState.Idle, h.model.state.value.action)
    }

    @Test
    @DisplayName("AC-18.focus observe before open and missing focus are harmless")
    fun ac18FocusBranches() = runTest(dispatcher) {
        val h = Harness()
        h.model.observeLive(listOf(ConversationFixtures.approvalItem()))
        assertNull(h.model.currentItem())
        assertNull(h.model.state.value.focusedItemId)

        val item = ConversationFixtures.approvalItem()
        h.port.listResult = PendingListResult.Current(listOf(item))
        h.port.detailResult = PendingDetailResult.Found(item)
        h.model.open(h.scope)
        advanceUntilIdle()
        h.model.openItem(item.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        h.model.observeLive(emptyList())
        assertEquals(PendingEntryUiState.CurrentEmpty, h.model.state.value.entry)
        assertFalse(h.model.state.value.sheetRequested)
        assertNull(h.model.state.value.reviewedGuard)
        assertNull(h.model.state.value.focusedItemId)
        assertTrue(h.model.state.value.answers.isEmpty())
    }

    @Test
    @DisplayName("AC-18.freshness pending collection and blank answer defaults are explicit")
    fun ac18Defaults() {
        val approval = ConversationFixtures.approvalItem()
        val collection = PendingCollection(listOf(approval))
        assertEquals(1, collection.items.size)
        assertNull(QuestionAnswerDraft().toAnswer("q1").text)
        assertEquals(emptyList<String>(), QuestionAnswerDraft().toAnswer("q1").selectedOptionIds)
        assertEquals(
            "text",
            QuestionAnswerDraft(text = "text").toAnswer("q1").text,
        )
        assertEquals(approval, PendingItem.Approval(approval.approval))
    }

    private fun TestScope.approvalHarness(): Harness {
        val h = Harness()
        val approval = ConversationFixtures.approvalItem()
        h.port.listResult = PendingListResult.Current(listOf(approval))
        h.port.detailResult = PendingDetailResult.Found(approval)
        h.model.open(h.scope)
        advanceUntilIdle()
        h.model.openItem(approval.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        return h
    }

    private fun TestScope.answerHarness(expiresAtMillis: Long? = null): Harness {
        val h = Harness()
        val item = ConversationFixtures.questionItem(expiresAtMillis = expiresAtMillis)
        h.port.listResult = PendingListResult.Current(listOf(item))
        h.port.detailResult = PendingDetailResult.Found(item)
        h.model.open(h.scope)
        advanceUntilIdle()
        h.model.openItem(item.itemId, PendingFocusOrigin.Live)
        advanceUntilIdle()
        return h
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
}
