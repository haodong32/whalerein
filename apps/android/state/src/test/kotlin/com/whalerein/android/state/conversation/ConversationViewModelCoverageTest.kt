package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SendId
import com.whalerein.android.state.fixtures.conversation.ConversationFixtures
import com.whalerein.android.state.fixtures.conversation.FakeClock
import com.whalerein.android.state.fixtures.conversation.FakeCommandPort
import com.whalerein.android.state.fixtures.conversation.FakeComposerPort
import com.whalerein.android.state.fixtures.conversation.FakeDraftPort
import com.whalerein.android.state.fixtures.conversation.FakeHistoryPort
import com.whalerein.android.state.fixtures.conversation.FakeModelsQueuePort
import com.whalerein.android.state.fixtures.conversation.FakeObservationPort
import com.whalerein.android.state.fixtures.conversation.FakePendingInputPort
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

/** Additional branch coverage for the conversation screen model. */
@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class ConversationViewModelCoverageTest : ConversationTestBase() {
    @Test
    @DisplayName("AC-09.paging older page loads once and reaches the end")
    fun ac09PagingOlder() = runTest(dispatcher) {
        val h = Harness()
        h.history.latest = Outcome.Success(
            ConversationFixtures.page(
                listOf(ConversationFixtures.textMessage("m3", "three", sequence = 3)),
                continuation = ConversationFixtures.cursor("c2"),
            ),
        )
        h.model.open(h.scope)
        advanceUntilIdle()

        h.history.older = Outcome.Success(
            ConversationFixtures.page(
                listOf(
                    ConversationFixtures.textMessage("m1", "one", sequence = 1),
                    ConversationFixtures.textMessage("m2", "two", sequence = 2),
                ),
            ),
        )
        h.model.setReadingAnchor(ReadingAnchor(ConversationItemKey.Committed("m3")))
        h.model.loadOlder()
        advanceUntilIdle()
        assertEquals(
            listOf("m1", "m2", "m3"),
            h.model.state.value.items.filterIsInstance<CommittedMessageItem>().map { it.message.messageId },
        )
        assertEquals(1, h.history.olderCalls.size)
        assertEquals(ConversationStatus.Ready, h.model.state.value.status)

        h.model.loadOlder()
        advanceUntilIdle()
        assertEquals(1, h.history.olderCalls.size)
        assertEquals(ConversationStatus.Ready, h.model.state.value.status)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-09.paging expired continuation invalidates and reloads a starting page")
    fun ac09PagingExpired() = runTest(dispatcher) {
        val h = Harness()
        h.history.latest = Outcome.Success(
            ConversationFixtures.page(
                listOf(ConversationFixtures.textMessage("m3", "three", sequence = 3)),
                continuation = ConversationFixtures.cursor("c2", expiresAtMillis = 1_000),
            ),
        )
        h.model.open(h.scope)
        advanceUntilIdle()
        h.clock.nowMillis = 2_000
        h.model.loadOlder()
        advanceUntilIdle()
        assertEquals(2, h.history.latestCalls.size)
        assertTrue(h.history.olderCalls.isEmpty())
        assertEquals(ConversationStatus.Invalidated, h.model.state.value.status)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-09.paging projection-mismatched older page invalidates and reloads")
    fun ac09PagingMismatch() = runTest(dispatcher) {
        val h = Harness()
        h.history.latest = Outcome.Success(
            ConversationFixtures.page(
                listOf(ConversationFixtures.textMessage("m3", "three", sequence = 3)),
                continuation = ConversationFixtures.cursor("c2"),
            ),
        )
        h.model.open(h.scope)
        advanceUntilIdle()
        h.history.older = Outcome.Success(
            ConversationFixtures.page(
                listOf(ConversationFixtures.textMessage("m2", "two", sequence = 2)),
                projection = "other-projection",
            ),
        )
        h.model.loadOlder()
        advanceUntilIdle()
        assertEquals(2, h.history.latestCalls.size)
        assertEquals(ConversationStatus.Ready, h.model.state.value.status)
        assertEquals(
            listOf("m3"),
            h.model.state.value.items.filterIsInstance<CommittedMessageItem>()
                .map { it.message.messageId },
        )
        cleanup(h)
    }

    @Test
    @DisplayName("AC-09.exact exact reads replace and missing references reload")
    fun ac09Exact() = runTest(dispatcher) {
        val h = Harness()
        h.history.latest = Outcome.Success(
            ConversationFixtures.page(
                listOf(ConversationFixtures.textMessage("m3", "stale", sequence = 3)),
            ),
        )
        h.model.open(h.scope)
        advanceUntilIdle()
        h.history.exact = Outcome.Success(
            ConversationFixtures.page(
                listOf(
                    ConversationFixtures.textMessage("m1", "one", sequence = 1),
                    ConversationFixtures.textMessage("m3", "fresh", sequence = 3),
                ),
                exact = true,
            ),
        )
        h.model.loadExact("m1")
        advanceUntilIdle()
        assertEquals("m1", h.model.state.value.history.exactMessageId)
        assertEquals(
            "fresh",
            h.model.state.value.items.filterIsInstance<CommittedMessageItem>()
                .first { it.message.messageId == "m3" }.message.blocks.single().text,
        )

        h.history.exact = Outcome.Success(
            ConversationFixtures.page(emptyList(), exact = true),
        )
        h.model.loadExact("does-not-exist")
        advanceUntilIdle()
        assertEquals(2, h.history.latestCalls.size)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-09.cache cached success unavailable and unconfirmed are distinct")
    fun ac09Cache() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(h.scope)
        advanceUntilIdle()
        h.history.cached = Outcome.Success(
            ConversationFixtures.page(
                listOf(ConversationFixtures.textMessage("m1", "cached", sequence = 1)),
                cached = true,
            ),
        )
        h.model.loadCached()
        advanceUntilIdle()
        assertEquals(ConversationStatus.Cached, h.model.state.value.status)
        assertTrue(h.model.state.value.history.cached)

        h.history.cached = Outcome.Failure(CoreFailure("cache", "unavailable"))
        h.model.loadCached()
        advanceUntilIdle()
        assertEquals(ConversationStatus.Cached, h.model.state.value.status)
        assertEquals("cache", h.model.state.value.history.failure?.code)

        h.history.cached = Outcome.Unconfirmed(CoreFailure("cache_unknown", "unknown"))
        h.model.loadCached()
        advanceUntilIdle()
        assertEquals("cache_unknown", h.model.state.value.history.failure?.code)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-09.paging history failures surface and a stale response is dropped")
    fun ac09HistoryFailure() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(h.scope)
        advanceUntilIdle()
        h.history.throwable = IllegalStateException("boom")
        h.model.loadOlder()
        h.model.loadExact("missing")
        h.model.loadCached()
        advanceUntilIdle()
        assertEquals("unexpected", h.model.state.value.history.failure?.code)
        assertTrue(h.diagnostics.events.any { it.event == ConversationDiagnosticEventName.REPLY_UNREADABLE })

        // A response started for the old scope is dropped after a switch.
        val first = ConversationFixtures.session(sessionId = "first")
        val second = ConversationFixtures.session(sessionId = "second")
        val stale = Harness()
        stale.history.throwable = null
        stale.model.open(first)
        stale.model.open(second)
        advanceUntilIdle()
        assertEquals(second, stale.model.state.value.session)
        assertEquals(2, stale.history.latestCalls.size)
        cleanup(stale)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-15.follow pending input failure timeout and unexpected errors are typed")
    fun ac15FollowBranches() = runTest(dispatcher) {
        val pending = followHarness()
        pending.composer.replyResult = ReplyResult.PendingInput(ConversationFixtures.approvalItem())
        sendFollow(pending)
        advanceUntilIdle()
        assertInstanceOf(SendUiState.FollowPendingInput::class.java, pending.model.state.value.send)
        assertInstanceOf(PendingEntryUiState.Current::class.java, pending.model.state.value.pendingEntry)
        cleanup(pending.model)

        val failed = followHarness()
        failed.composer.replyResult = ReplyResult.Failed(CoreFailure("read_failed", "failed"))
        sendFollow(failed)
        advanceUntilIdle()
        assertInstanceOf(SendUiState.FollowFailed::class.java, failed.model.state.value.send)
        cleanup(failed.model)

        val timedOut = followHarness()
        timedOut.composer.replyResult = ReplyResult.TimedOut
        sendFollow(timedOut)
        advanceUntilIdle()
        assertInstanceOf(SendUiState.FollowTimedOut::class.java, timedOut.model.state.value.send)
        cleanup(timedOut.model)

        val thrown = followHarness()
        thrown.composer.replyThrowable = IllegalStateException("boom")
        sendFollow(thrown)
        advanceUntilIdle()
        assertInstanceOf(SendUiState.FollowFailed::class.java, thrown.model.state.value.send)
        cleanup(thrown.model)
    }

    @Test
    @DisplayName("AC-15.lost-receipt cancelled and unexpected submissions restore the draft")
    fun ac15SubmitBranches() = runTest(dispatcher) {
        val cancelled = Harness()
        cancelled.model.open(cancelled.scope)
        advanceUntilIdle()
        cancelled.composer.submitOutcome = Outcome.Cancelled
        cancelled.model.updateDraft("cancelled draft")
        advanceUntilIdle()
        cancelled.model.submit()
        advanceUntilIdle()
        assertEquals("cancelled draft", cancelled.model.state.value.composer.text)
        assertInstanceOf(SendUiState.Cancelled::class.java, cancelled.model.state.value.send)
        cleanup(cancelled)

        val thrown = Harness()
        thrown.model.open(thrown.scope)
        advanceUntilIdle()
        thrown.composer.submitThrowable = IllegalStateException("boom")
        thrown.model.updateDraft("unexpected")
        advanceUntilIdle()
        thrown.model.submit()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.DefiniteFailure::class.java, thrown.model.state.value.send)
        assertEquals("unexpected", thrown.model.state.value.composer.text)
        cleanup(thrown)
    }

    @Test
    @DisplayName("AC-15.follow a late receipt from an older submission is ignored")
    fun ac15LateReceipt() = runTest(dispatcher) {
        val h = followHarness()
        val replyGate = CompletableDeferred<Unit>()
        h.composer.replyGate = replyGate
        sendFollow(h)
        advanceUntilIdle()
        assertInstanceOf(SendUiState.FollowPending::class.java, h.model.state.value.send)

        h.composer.replyGate = null
        h.model.selectFollowMode(FollowMode.ReceiptOnly)
        h.model.updateDraft("second")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.ReceiptOnly::class.java, h.model.state.value.send)

        replyGate.complete(Unit)
        advanceUntilIdle()
        assertInstanceOf(SendUiState.ReceiptOnly::class.java, h.model.state.value.send)
        assertTrue(h.diagnostics.events.any { it.code == "late_receipt_ignored" })
        cleanup(h.model)
    }

    @Test
    @DisplayName("AC-15.lost-receipt recovery distinguishes unread unconfirmed and failure")
    fun ac15RecoveryBranches() = runTest(dispatcher) {
        val unread = recoveryHarness()
        prepareRecovery(unread)
        unread.composer.recoveryResult = RecoveryResult.ConfirmedUnread(
            SendReceipt(SendId("send-1"), null),
            CoreFailure("unread", "unread"),
        )
        unread.model.recoverSend()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.ConfirmedUnread::class.java, unread.model.state.value.send)

        val still = recoveryHarness()
        prepareRecovery(still)
        still.composer.recoveryResult = RecoveryResult.StillUnconfirmed(
            CoreFailure("still", "still unknown"),
        )
        still.model.recoverSend()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.Unconfirmed::class.java, still.model.state.value.send)

        val failed = recoveryHarness()
        prepareRecovery(failed)
        failed.composer.recoveryResult = RecoveryResult.Failed(CoreFailure("gone", "failed"))
        failed.model.recoverSend()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.DefiniteFailure::class.java, failed.model.state.value.send)

        val thrown = recoveryHarness()
        prepareRecovery(thrown)
        thrown.composer.recoveryThrowable = IllegalStateException("boom")
        thrown.model.recoverSend()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.Unconfirmed::class.java, thrown.model.state.value.send)

        val ineligible = Harness()
        ineligible.model.open(ineligible.scope)
        advanceUntilIdle()
        ineligible.model.recoverSend()
        advanceUntilIdle()
        assertTrue(ineligible.composer.recoveryRequests.isEmpty())
        cleanup(ineligible)
        cleanup(thrown.model)
        cleanup(failed.model)
        cleanup(still.model)
        cleanup(unread.model)
    }

    @Test
    @DisplayName("AC-15.confirmed-unread retry keeps confirmation for every follow result")
    fun ac15RetryBranches() = runTest(dispatcher) {
        val pending = unreadHarness()
        prepareUnread(pending)
        pending.composer.replyResult = ReplyResult.PendingInput(ConversationFixtures.approvalItem())
        pending.model.retryContentRead()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.FollowPendingInput::class.java, pending.model.state.value.send)

        val unreadAgain = unreadHarness()
        prepareUnread(unreadAgain)
        unreadAgain.composer.replyResult = ReplyResult.ConfirmedUnread(
            CoreFailure("still_unread", "still unread"),
        )
        unreadAgain.model.retryContentRead()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.ConfirmedUnread::class.java, unreadAgain.model.state.value.send)

        val timedOut = unreadHarness()
        prepareUnread(timedOut)
        timedOut.composer.replyResult = ReplyResult.TimedOut
        timedOut.model.retryContentRead()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.FollowTimedOut::class.java, timedOut.model.state.value.send)

        val failed = unreadHarness()
        prepareUnread(failed)
        failed.composer.replyResult = ReplyResult.Failed(CoreFailure("retry", "failed"))
        failed.model.retryContentRead()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.ConfirmedUnread::class.java, failed.model.state.value.send)

        val thrown = unreadHarness()
        prepareUnread(thrown)
        thrown.composer.replyThrowable = IllegalStateException("boom")
        thrown.model.retryContentRead()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.ConfirmedUnread::class.java, thrown.model.state.value.send)

        val ineligible = Harness()
        ineligible.model.open(ineligible.scope)
        advanceUntilIdle()
        ineligible.model.retryContentRead()
        advanceUntilIdle()
        assertTrue(ineligible.composer.replyRequests.isEmpty())
        cleanup(ineligible)
        cleanup(thrown.model)
        cleanup(failed.model)
        cleanup(timedOut.model)
        cleanup(unreadAgain.model)
        cleanup(pending.model)
    }

    @Test
    @DisplayName("AC-11.watch observation failures and endings become typed watch states")
    fun ac11WatchFailures() = runTest(dispatcher) {
        val startFailed = Harness()
        startFailed.observation.startThrowable = IllegalStateException("boom")
        startFailed.model.open(startFailed.scope)
        advanceUntilIdle()
        assertInstanceOf(WatchUiState.Failed::class.java, startFailed.model.state.value.watch)
        cleanup(startFailed)

        val startFailure = Harness()
        startFailure.observation.startFailure = CoreFailure("watch", "failed")
        startFailure.model.open(startFailure.scope)
        advanceUntilIdle()
        assertInstanceOf(WatchUiState.Failed::class.java, startFailure.model.state.value.watch)
        cleanup(startFailure)

        val pollFailure = Harness()
        pollFailure.observation.pollFailure = CoreFailure("poll", "failed")
        pollFailure.model.open(pollFailure.scope)
        advanceUntilIdle()
        assertInstanceOf(WatchUiState.Failed::class.java, pollFailure.model.state.value.watch)
        cleanup(pollFailure)

        val pollUnconfirmed = Harness()
        pollUnconfirmed.observation.pollUnconfirmed = CoreFailure("poll", "unknown")
        pollUnconfirmed.model.open(pollUnconfirmed.scope)
        advanceUntilIdle()
        assertInstanceOf(WatchUiState.Failed::class.java, pollUnconfirmed.model.state.value.watch)
        cleanup(pollUnconfirmed)

        val pollCancelled = Harness()
        pollCancelled.observation.pollCancelled = true
        pollCancelled.model.open(pollCancelled.scope)
        advanceUntilIdle()
        assertEquals(WatchUiState.Watching, pollCancelled.model.state.value.watch)
        cleanup(pollCancelled)

        val pollThrown = Harness()
        pollThrown.observation.pollThrowable = IllegalStateException("boom")
        pollThrown.model.open(pollThrown.scope)
        advanceUntilIdle()
        assertInstanceOf(WatchUiState.Failed::class.java, pollThrown.model.state.value.watch)
        cleanup(pollThrown)
    }

    @Test
    @DisplayName("AC-11.watch requested quiescent dropped and failed endings are distinct")
    fun ac11WatchEndings() = runTest(dispatcher) {
        val requested = Harness()
        requested.model.open(requested.scope)
        advanceUntilIdle()
        requested.observation.send(
            batch(requested.scope, requested.model.state.value.generation, ended = WatchEndReason.Requested),
        )
        advanceUntilIdle()
        assertEquals(WatchUiState.Ended(WatchEndReason.Requested), requested.model.state.value.watch)
        cleanup(requested)

        val quiescent = Harness()
        quiescent.model.open(quiescent.scope)
        advanceUntilIdle()
        quiescent.observation.send(
            batch(quiescent.scope, quiescent.model.state.value.generation, ended = WatchEndReason.Quiescent),
        )
        advanceUntilIdle()
        assertEquals(WatchUiState.Ended(WatchEndReason.Quiescent), quiescent.model.state.value.watch)
        cleanup(quiescent)

        val failed = Harness()
        failed.model.open(failed.scope)
        advanceUntilIdle()
        failed.observation.send(
            batch(failed.scope, failed.model.state.value.generation, ended = WatchEndReason.Failed),
        )
        advanceUntilIdle()
        assertInstanceOf(WatchUiState.Failed::class.java, failed.model.state.value.watch)
        cleanup(failed)

        val dropped = Harness()
        dropped.model.open(dropped.scope)
        advanceUntilIdle()
        dropped.observation.send(
            batch(dropped.scope, dropped.model.state.value.generation, ended = WatchEndReason.DroppedEvents),
        )
        advanceUntilIdle()
        assertEquals(2, dropped.history.latestCalls.size)
        cleanup(dropped)

        val liveFirstVisible = Harness()
        liveFirstVisible.model.open(liveFirstVisible.scope)
        advanceUntilIdle()
        liveFirstVisible.observation.send(
            batch(
                liveFirstVisible.scope,
                liveFirstVisible.model.state.value.generation,
                ended = WatchEndReason.FirstVisibleOutput,
            ),
        )
        advanceUntilIdle()
        assertEquals(WatchUiState.Watching, liveFirstVisible.model.state.value.watch)
        cleanup(liveFirstVisible)
    }

    @Test
    @DisplayName("AC-16.wait-end wait failure cancellation and duplicate waits are typed")
    fun ac16WaitBranches() = runTest(dispatcher) {
        val failed = Harness()
        failed.model.open(failed.scope)
        advanceUntilIdle()
        failed.wait.result = WaitResult.Failed(CoreFailure("wait", "failed"))
        failed.model.waitForQuiescence()
        advanceUntilIdle()
        assertInstanceOf(WaitUiState.Failed::class.java, failed.model.state.value.wait)
        cleanup(failed)

        val cancelled = Harness()
        cancelled.model.open(cancelled.scope)
        advanceUntilIdle()
        cancelled.wait.result = WaitResult.Cancelled
        cancelled.model.waitForQuiescence()
        advanceUntilIdle()
        assertEquals(WaitUiState.Cancelled, cancelled.model.state.value.wait)
        cleanup(cancelled)

        val thrown = Harness()
        thrown.model.open(thrown.scope)
        advanceUntilIdle()
        thrown.wait.throwable = IllegalStateException("boom")
        thrown.model.waitForQuiescence()
        advanceUntilIdle()
        assertInstanceOf(WaitUiState.Failed::class.java, thrown.model.state.value.wait)
        cleanup(thrown)

        val duplicate = Harness()
        duplicate.model.open(duplicate.scope)
        advanceUntilIdle()
        duplicate.wait.gate = CompletableDeferred()
        duplicate.model.waitForQuiescence()
        advanceUntilIdle()
        duplicate.model.waitForPendingInput()
        advanceUntilIdle()
        assertEquals(1, duplicate.wait.requests.size)
        duplicate.model.cancelWait()
        advanceUntilIdle()
        duplicate.model.cancelWait()
        advanceUntilIdle()
        assertEquals(1, duplicate.wait.cancels.size)
        cleanup(duplicate)

        val noWait = Harness()
        noWait.model.open(noWait.scope)
        advanceUntilIdle()
        noWait.model.cancelWait()
        advanceUntilIdle()
        assertTrue(noWait.wait.cancels.isEmpty())
        cleanup(noWait)
    }

    @Test
    @DisplayName("AC-16.stop a duplicate stop is ignored and an unexpected throw is unconfirmed")
    fun ac16StopBranches() = runTest(dispatcher) {
        val duplicate = Harness()
        duplicate.model.open(duplicate.scope)
        advanceUntilIdle()
        val gate = CompletableDeferred<Unit>()
        duplicate.commands.gate = gate
        duplicate.model.stop()
        advanceUntilIdle()
        duplicate.model.stop()
        advanceUntilIdle()
        assertEquals(1, duplicate.commands.stops.size)
        gate.complete(Unit)
        advanceUntilIdle()
        assertEquals(StopUiState.Confirmed, duplicate.model.state.value.stop)
        cleanup(duplicate)

        val thrown = Harness()
        thrown.model.open(thrown.scope)
        advanceUntilIdle()
        thrown.commands.throwable = IllegalStateException("boom")
        thrown.model.stop()
        advanceUntilIdle()
        assertInstanceOf(StopUiState.Unconfirmed::class.java, thrown.model.state.value.stop)
        cleanup(thrown)
    }

    @Test
    @DisplayName("AC-11.overflow pending and queue refreshes cover stale unavailable and throwable")
    fun ac11PendingQueueBranches() = runTest(dispatcher) {
        val h = Harness()
        h.pending.listResult = PendingListResult.Stale(CoreFailure("stale", "stale"))
        h.model.open(h.scope)
        advanceUntilIdle()
        assertInstanceOf(PendingEntryUiState.Stale::class.java, h.model.state.value.pendingEntry)

        h.pending.listResult = PendingListResult.Unsupported
        h.model.refreshPendingInputs()
        advanceUntilIdle()
        assertEquals(PendingEntryUiState.Unsupported, h.model.state.value.pendingEntry)

        h.pending.listResult = PendingListResult.Unavailable(CoreFailure("down", "down"))
        h.model.refreshPendingInputs()
        advanceUntilIdle()
        assertInstanceOf(PendingEntryUiState.Unavailable::class.java, h.model.state.value.pendingEntry)

        h.pending.listResult = PendingListResult.Current(
            listOf(ConversationFixtures.approvalItem()),
        )
        h.model.refreshPendingInputs()
        advanceUntilIdle()
        assertInstanceOf(PendingEntryUiState.Current::class.java, h.model.state.value.pendingEntry)

        h.pending.listThrowable = IllegalStateException("boom")
        h.model.refreshPendingInputs()
        advanceUntilIdle()
        assertInstanceOf(PendingEntryUiState.Unavailable::class.java, h.model.state.value.pendingEntry)

        h.queue.queue = Outcome.Failure(CoreFailure("queue", "failed"))
        h.model.refreshQueue()
        advanceUntilIdle()
        h.queue.queue = Outcome.Unconfirmed(CoreFailure("queue", "unknown"))
        h.model.refreshQueue()
        advanceUntilIdle()
        h.queue.queue = Outcome.Cancelled
        h.model.refreshQueue()
        advanceUntilIdle()
        h.queue.queueThrowable = IllegalStateException("boom")
        h.model.refreshQueue()
        advanceUntilIdle()
        assertTrue(h.diagnostics.events.any { it.stage == ConversationDiagnosticStage.QUEUE })

        val noQueue = Harness(withQueue = false)
        noQueue.model.open(noQueue.scope)
        advanceUntilIdle()
        noQueue.model.refreshQueue()
        advanceUntilIdle()
        assertTrue(noQueue.model.state.value.pendingQueue.isEmpty())
        cleanup(noQueue)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-14.modes empty drafts and a missing session never submit")
    fun ac14NoSubmit() = runTest(dispatcher) {
        val h = Harness()
        h.model.submit()
        h.model.waitForQuiescence()
        h.model.startFirstVisibleOutput()
        h.model.onVisible()
        h.model.onHidden()
        h.model.refreshPendingInputs()
        h.model.refreshQueue()
        h.model.loadOlder()
        h.model.loadCached()
        h.model.loadExact("missing")
        h.model.setReadingAnchor(null)
        advanceUntilIdle()
        assertTrue(h.composer.submits.isEmpty())

        h.model.open(h.scope)
        advanceUntilIdle()
        h.model.submit()
        h.model.queue()
        h.model.steer()
        h.model.startIfIdle()
        advanceUntilIdle()
        assertTrue(h.composer.submits.isEmpty())
        cleanup(h)
    }

    @Test
    @DisplayName("AC-14.modes draft load failure and announcement consumption keep intent")
    fun ac14DraftAnnouncement() = runTest(dispatcher) {
        val stored = Harness()
        stored.drafts.drafts[stored.scope] = "stored draft"
        stored.model.open(stored.scope)
        advanceUntilIdle()
        assertEquals("stored draft", stored.model.state.value.composer.text)
        cleanup(stored)

        val loadFailed = Harness()
        loadFailed.drafts.loadFailure = CoreFailure("draft", "load failed")
        loadFailed.model.open(loadFailed.scope)
        advanceUntilIdle()
        assertEquals("", loadFailed.model.state.value.composer.text)
        assertTrue(loadFailed.diagnostics.events.any { it.fields["operation"] == "draft_load" })
        cleanup(loadFailed)

        val announced = Harness()
        announced.model.open(announced.scope)
        advanceUntilIdle()
        val generation = announced.model.state.value.generation
        announced.observation.send(
            batch(
                announced.scope,
                generation,
                listOf(
                    RunPhaseChanged(announced.scope, generation, RunPhase.Running),
                    PreviewAppended(
                        announced.scope,
                        generation,
                        ConversationFixtures.attempt(),
                        "answer-1",
                        MessageBlockKind.Text,
                        "visible",
                    ),
                ),
            ),
        )
        advanceUntilIdle()
        assertEquals(AnnouncementMilestone.FirstVisibleOutput, announced.model.state.value.announcement)
        announced.model.consumeAnnouncement()
        assertNull(announced.model.state.value.announcement)
        announced.model.onForeground()
        announced.model.onBackground()
        advanceUntilIdle()
        cleanup(announced)
    }

    @Test
    @DisplayName("AC-11.switch pending input events update the fixed entry")
    fun ac11PendingEvents() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(h.scope)
        advanceUntilIdle()
        var generation = h.model.state.value.generation
        h.observation.send(
            batch(
                h.scope,
                generation,
                listOf(
                    PendingInputsChanged(
                        h.scope,
                        generation,
                        listOf(ConversationFixtures.approvalItem()),
                    ),
                ),
            ),
        )
        advanceUntilIdle()
        assertInstanceOf(PendingEntryUiState.Current::class.java, h.model.state.value.pendingEntry)
        generation = h.model.state.value.generation
        h.observation.send(
            batch(
                h.scope,
                generation,
                listOf(PendingInputsChanged(h.scope, generation, emptyList())),
            ),
        )
        advanceUntilIdle()
        assertEquals(PendingEntryUiState.CurrentEmpty, h.model.state.value.pendingEntry)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-16.stop current attempt comes from committed origin when no preview exists")
    fun ac16CommittedOrigin() = runTest(dispatcher) {
        val h = Harness()
        val origin = ConversationFixtures.attempt("committed-attempt")
        h.history.latest = Outcome.Success(
            ConversationFixtures.page(
                listOf(
                    ConversationFixtures.textMessage(
                        messageId = "m1",
                        text = "committed",
                        sequence = 1,
                        origin = origin,
                    ),
                ),
            ),
        )
        h.model.open(h.scope)
        advanceUntilIdle()
        h.model.stop()
        advanceUntilIdle()
        assertEquals(listOf(origin), h.commands.stops)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-11.lifecycle reopening while a wait is active cancels only the wait")
    fun ac11ReopenWithWait() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(h.scope)
        advanceUntilIdle()
        h.wait.gate = CompletableDeferred()
        h.model.waitForQuiescence()
        advanceUntilIdle()
        val second = ConversationFixtures.session(sessionId = "session-2")
        h.model.open(second)
        advanceUntilIdle()
        assertEquals(second, h.model.state.value.session)
        assertEquals(WaitUiState.Idle, h.model.state.value.wait)
        assertTrue(h.commands.stops.isEmpty())
        cleanup(h)
    }

    private fun followHarness(): Harness {
        val h = Harness()
        h.model.open(h.scope)
        h.model.selectFollowMode(FollowMode.FollowReply)
        // open() launches async work; the caller advances before asserting.
        return h
    }

    private fun TestScope.sendFollow(h: Harness) {
        h.model.updateDraft("follow")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()
    }

    private fun recoveryHarness(): Harness {
        val h = Harness()
        h.model.open(h.scope)
        h.composer.submitOutcome = Outcome.Unconfirmed(CoreFailure("unknown", "unknown"))
        h.model.selectFollowMode(FollowMode.ReceiptOnly)
        h.model.updateDraft("recover")
        return h
    }

    private fun unreadHarness(): Harness {
        val h = Harness()
        h.model.open(h.scope)
        h.composer.submitOutcome = null
        h.model.selectFollowMode(FollowMode.FollowReply)
        h.composer.replyResult = ReplyResult.ConfirmedUnread(CoreFailure("unread", "unread"))
        h.model.updateDraft("unread")
        return h
    }

    private fun TestScope.prepareRecovery(h: Harness) {
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.Unconfirmed::class.java, h.model.state.value.send)
    }

    private fun TestScope.prepareUnread(h: Harness) {
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()
        assertInstanceOf(SendUiState.ConfirmedUnread::class.java, h.model.state.value.send)
    }

    private fun batch(
        scope: com.whalerein.android.state.core.SessionRef,
        generation: Long,
        events: List<ConversationEvent> = emptyList(),
        droppedCount: Int = 0,
        ended: WatchEndReason? = null,
    ): ObservationBatch = ObservationBatch(scope, generation, events, droppedCount, ended)

    private fun TestScope.cleanup(h: Harness) {
        h.model.onHidden()
        advanceUntilIdle()
    }

    private fun TestScope.cleanup(h: ConversationViewModel) {
        h.onHidden()
        advanceUntilIdle()
    }

    private class Harness(withQueue: Boolean = true) {
        val scope = ConversationFixtures.session()
        val observation = FakeObservationPort()
        val history = FakeHistoryPort()
        val composer = FakeComposerPort()
        val wait = FakeWaitPort()
        val commands = FakeCommandPort()
        val pending = FakePendingInputPort()
        val drafts = FakeDraftPort()
        val queue = FakeModelsQueuePort()
        val clock = FakeClock()
        val diagnostics = RecordingDiagnosticSink()
        val model = ConversationViewModel(
            observation = observation,
            history = history,
            composer = composer,
            wait = wait,
            commands = commands,
            pendingInput = pending,
            drafts = drafts,
            clock = clock,
            ids = SequentialIdGenerator("cover"),
            diagnostics = diagnostics,
            queue = if (withQueue) queue else null,
        )
    }
}
