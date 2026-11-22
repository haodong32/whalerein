package com.whalerein.android.state.conversation

import org.junit.jupiter.api.DisplayName
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SendId
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
import com.whalerein.android.state.fixtures.conversation.FakeWaitPort
import com.whalerein.android.state.fixtures.conversation.RecordingDiagnosticSink
import com.whalerein.android.state.fixtures.conversation.SequentialIdGenerator
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class ConversationViewModelTest : ConversationTestBase() {
    @Test
    @DisplayName("AC-11.switch old scope batch cannot alter the new view")
    fun ac11Switch() = runTest(dispatcher) {
        val h = Harness()
        val first = ConversationFixtures.session(sessionId = "session-1")
        val second = ConversationFixtures.session(sessionId = "session-2")
        h.history.latest = Outcome.Success(
            ConversationFixtures.page(listOf(ConversationFixtures.textMessage("first-1", "one", sequence = 1))),
        )
        h.model.open(first)
        advanceUntilIdle()
        val firstGeneration = h.model.state.value.generation
        assertEquals(1, h.model.state.value.items.filterIsInstance<CommittedMessageItem>().size)

        h.observation.send(
            batch(
                first,
                firstGeneration,
                listOf(
                    PreviewAppended(
                        first,
                        firstGeneration,
                        ConversationFixtures.attempt(),
                        "answer-1",
                        MessageBlockKind.Text,
                        "old",
                    ),
                ),
            ),
        )
        advanceUntilIdle()
        assertTrue(h.model.state.value.items.any { it is StreamingPreviewItem })

        h.history.latest = Outcome.Success(ConversationFixtures.page(emptyList()))
        h.model.open(second)
        advanceUntilIdle()
        assertEquals(second, h.model.state.value.session)
        assertTrue(h.model.state.value.items.isEmpty())

        // This old batch was already transferred into Kotlin before the switch.
        h.observation.send(
            batch(
                first,
                firstGeneration,
                listOf(
                    MessageCommitted(
                        first,
                        firstGeneration,
                        ConversationFixtures.textMessage("old-message", "stale", sequence = 2),
                    ),
                ),
            ),
        )
        advanceUntilIdle()
        assertEquals(second, h.model.state.value.session)
        assertTrue(h.model.state.value.items.isEmpty())

        h.observation.send(
            batch(
                second,
                h.model.state.value.generation,
                listOf(
                    MessageCommitted(
                        second,
                        h.model.state.value.generation,
                        ConversationFixtures.textMessage("second-1", "new", sequence = 1),
                    ),
                ),
            ),
        )
        advanceUntilIdle()
        assertEquals("second-1", (h.model.state.value.items.single() as CommittedMessageItem).message.messageId)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-11.lifecycle rotation and foreground reuse one watch and history copy")
    fun ac11Lifecycle() = runTest(dispatcher) {
        val h = Harness()
        val scope = ConversationFixtures.session()
        h.history.latest = Outcome.Success(
            ConversationFixtures.page(listOf(ConversationFixtures.textMessage("m1", "one", sequence = 1))),
        )
        h.model.open(scope)
        advanceUntilIdle()
        assertEquals(1, h.observation.started.size)
        assertEquals(1, h.history.latestCalls.size)

        // Rotation/pane recreation reopens the same session and asks for visibility.
        h.model.open(scope)
        h.model.onVisible()
        h.model.onVisible()
        advanceUntilIdle()
        assertEquals(1, h.observation.started.size)
        assertEquals(1, h.history.latestCalls.size)

        h.model.updateDraft("draft across rotation")
        advanceUntilIdle()
        h.model.onHidden()
        advanceUntilIdle()
        assertEquals(1, h.observation.stopped.size)
        assertEquals("draft across rotation", h.model.state.value.composer.text)

        h.model.onVisible()
        advanceUntilIdle()
        assertEquals(2, h.observation.started.size)
        // Resume does not copy committed history again.
        assertEquals(1, h.history.latestCalls.size)
        assertEquals(1, h.model.state.value.items.size)
        assertTrue(h.diagnostics.events.any { it.event == ConversationDiagnosticEventName.WATCH_STOPPED })
        cleanup(h)
    }

    @Test
    @DisplayName("AC-11.overflow dropped events resync and late receipts are ignored")
    fun ac11Overflow() = runTest(dispatcher) {
        val h = Harness()
        val scope = ConversationFixtures.session()
        h.history.latest = Outcome.Success(
            ConversationFixtures.page(listOf(ConversationFixtures.textMessage("m1", "one", sequence = 1))),
        )
        h.model.open(scope)
        advanceUntilIdle()
        val oldGeneration = h.model.state.value.generation

        h.observation.send(
            batch(scope, oldGeneration, droppedCount = 3),
        )
        advanceUntilIdle()
        val resynced = h.model.state.value
        assertTrue(resynced.generation > oldGeneration)
        assertEquals(ConversationStatus.Ready, resynced.status)
        assertEquals(2, h.history.latestCalls.size)
        assertEquals(1, resynced.items.size)
        assertTrue(h.diagnostics.events.any { it.event == ConversationDiagnosticEventName.EVENTS_DROPPED })
        assertTrue(h.diagnostics.events.any { it.event == ConversationDiagnosticEventName.RESYNC_STARTED })

        // An old generation event (including a command receipt) is unrelated now.
        h.observation.send(
            batch(
                scope,
                oldGeneration,
                listOf(
                    RunPhaseChanged(scope, oldGeneration, RunPhase.Running),
                    MessageCommitted(
                        scope,
                        oldGeneration,
                        ConversationFixtures.textMessage("late", "late", sequence = 2),
                    ),
                ),
            ),
        )
        advanceUntilIdle()
        assertEquals(resynced.generation, h.model.state.value.generation)
        assertEquals(RunPhase.Unknown, h.model.state.value.phase)
        assertEquals(1, h.model.state.value.items.size)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-11.watch first visible output ignores reasoning and explicit end is separate")
    fun ac11Watch() = runTest(dispatcher) {
        val h = Harness()
        val scope = ConversationFixtures.session()
        h.model.open(scope)
        advanceUntilIdle()

        h.model.startFirstVisibleOutput()
        advanceUntilIdle()
        assertEquals(WatchMode.FirstVisibleOutput, h.observation.started.last().mode)
        assertEquals(WatchUiState.FirstOutputWaiting, h.model.state.value.watch)

        val generation = h.model.state.value.generation
        h.observation.send(
            batch(
                scope,
                generation,
                listOf(
                    PreviewAppended(
                        scope,
                        generation,
                        ConversationFixtures.attempt(),
                        "reason-1",
                        MessageBlockKind.Reasoning,
                        "hidden reasoning",
                    ),
                ),
                ended = WatchEndReason.FirstVisibleOutput,
            ),
        )
        advanceUntilIdle()
        assertEquals(WatchUiState.FirstOutputWaiting, h.model.state.value.watch)

        h.model.startFirstVisibleOutput()
        advanceUntilIdle()
        val nextGeneration = h.model.state.value.generation
        h.observation.send(
            batch(
                scope,
                nextGeneration,
                listOf(
                    PreviewAppended(
                        scope,
                        nextGeneration,
                        ConversationFixtures.attempt(),
                        "answer-1",
                        MessageBlockKind.Text,
                        "visible",
                    ),
                ),
                ended = WatchEndReason.FirstVisibleOutput,
            ),
        )
        advanceUntilIdle()
        assertEquals(WatchUiState.FirstOutputObserved, h.model.state.value.watch)

        h.model.startFirstVisibleOutput()
        advanceUntilIdle()
        val timeoutGeneration = h.model.state.value.generation
        h.observation.send(
            batch(scope, timeoutGeneration, ended = WatchEndReason.Timeout),
        )
        advanceUntilIdle()
        assertEquals(WatchUiState.TimedOut, h.model.state.value.watch)

        // Ending the observation never interrupts the Host run.
        h.model.onHidden()
        advanceUntilIdle()
        assertTrue(h.commands.stops.isEmpty())
        cleanup(h)
    }

    @Test
    @DisplayName("AC-14.modes the selected delivery mode marks exactly one scoped submission")
    fun ac14Modes() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()

        h.model.updateDraft("queue this")
        advanceUntilIdle()
        h.model.queue()
        advanceUntilIdle()
        assertEquals(DeliveryMode.Queue, h.composer.submits.last().mode)

        h.model.updateDraft("steer this")
        advanceUntilIdle()
        h.model.steer()
        advanceUntilIdle()
        assertEquals(DeliveryMode.Steer, h.composer.submits.last().mode)

        h.model.updateDraft("start if idle")
        advanceUntilIdle()
        h.model.startIfIdle()
        advanceUntilIdle()
        assertEquals(DeliveryMode.StartIfIdle, h.composer.submits.last().mode)

        h.model.selectDeliveryMode(DeliveryMode.Send)
        h.model.updateDraft("send now")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()
        assertEquals(DeliveryMode.Send, h.composer.submits.last().mode)
        assertEquals(4, h.composer.submits.size)
        assertEquals(h.model.state.value.session, h.composer.submits.last().scope)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-14.repeated-tap a double tap submits the original draft exactly once")
    fun ac14RepeatedTap() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        val gate = CompletableDeferred<Unit>()
        h.composer.submitGate = gate
        h.model.updateDraft("only once")
        advanceUntilIdle()

        h.model.submit()
        h.model.submit()
        advanceUntilIdle()
        assertEquals(1, h.composer.submits.size)
        assertInstanceOf(SendUiState.Submitting::class.java, h.model.state.value.send)

        gate.complete(Unit)
        advanceUntilIdle()
        assertEquals(1, h.composer.submits.size)
        val send = h.model.state.value.send
        assertInstanceOf(SendUiState.ReceiptOnly::class.java, send)
        assertEquals(
            h.composer.submits.single().sendId,
            (send as SendUiState.ReceiptOnly).sendId,
        )
        cleanup(h)
    }

    @Test
    @DisplayName("AC-14.next-draft newer draft survives receipt and definite failure")
    fun ac14NextDraft() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()

        val gate = CompletableDeferred<Unit>()
        h.composer.submitGate = gate
        h.model.updateDraft("older")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()
        h.model.updateDraft("newer")
        advanceUntilIdle()
        gate.complete(Unit)
        advanceUntilIdle()
        assertEquals("newer", h.model.state.value.composer.text)

        h.composer.submitGate = null
        h.composer.submitOutcome = Outcome.Failure(
            CoreFailure(ConversationFailureCodes.OFFLINE, "offline"),
        )
        h.model.updateDraft("offline draft")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()
        assertEquals("offline draft", h.model.state.value.composer.text)
        assertEquals(ComposerAvailability.Offline, h.model.state.value.composer.availability)
        assertInstanceOf(SendUiState.DefiniteFailure::class.java, h.model.state.value.send)

        h.composer.submitOutcome = null
        val secondGate = CompletableDeferred<Unit>()
        h.composer.submitGate = secondGate
        h.model.updateDraft("will be replaced")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()
        h.model.updateDraft("typed later")
        advanceUntilIdle()
        secondGate.complete(Unit)
        advanceUntilIdle()
        assertEquals("typed later", h.model.state.value.composer.text)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-14.transition run-state race reports the real Source outcome")
    fun ac14Transition() = runTest(dispatcher) {
        val h = Harness()
        val scope = ConversationFixtures.session()
        h.model.open(scope)
        advanceUntilIdle()
        val generation = h.model.state.value.generation
        h.observation.send(
            batch(
                scope,
                generation,
                listOf(RunPhaseChanged(scope, generation, RunPhase.Running)),
            ),
        )
        advanceUntilIdle()
        assertEquals(RunPhase.Running, h.model.state.value.phase)

        h.model.updateDraft("queued while running")
        advanceUntilIdle()
        h.model.queue()
        advanceUntilIdle()
        assertEquals(DeliveryMode.Queue, h.composer.submits.single().mode)
        assertInstanceOf(SendUiState.ReceiptOnly::class.java, h.model.state.value.send)

        h.composer.submitOutcome = Outcome.Failure(
            CoreFailure(ConversationFailureCodes.CAPABILITY_MISSING, "not supported"),
        )
        h.model.updateDraft("unsupported mode")
        advanceUntilIdle()
        h.model.steer()
        advanceUntilIdle()
        assertEquals(ComposerAvailability.Unavailable, h.model.state.value.composer.availability)
        assertEquals("unsupported mode", h.model.state.value.composer.text)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-15.receipt-only reports a receipt without following the reply")
    fun ac15ReceiptOnly() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        h.model.selectFollowMode(FollowMode.ReceiptOnly)
        h.model.updateDraft("receipt only")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()

        val send = h.model.state.value.send
        assertInstanceOf(SendUiState.ReceiptOnly::class.java, send)
        assertTrue(h.composer.replyRequests.isEmpty())
        assertEquals(1, h.composer.submits.size)
        assertEquals(
            h.composer.submits.single().sendId,
            (send as SendUiState.ReceiptOnly).sendId,
        )
        cleanup(h)
    }

    @Test
    @DisplayName("AC-15.follow a matching reply is distinguishable and appears once")
    fun ac15Follow() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        h.model.selectFollowMode(FollowMode.FollowReply)
        val reply = ConversationFixtures.textMessage("reply-1", "answer", sequence = 1)
        h.composer.replyResult = ReplyResult.Reply(reply)
        h.model.updateDraft("follow me")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()

        val send = h.model.state.value.send
        assertInstanceOf(SendUiState.FollowedReply::class.java, send)
        assertEquals(1, h.composer.replyRequests.size)
        assertEquals(1, h.model.state.value.items.filterIsInstance<CommittedMessageItem>().size)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-15.lost-receipt recovery keeps one submission and the original send id")
    fun ac15LostReceipt() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        h.composer.submitOutcome = Outcome.Unconfirmed(
            CoreFailure("network", "receipt lost", retryable = true),
        )
        h.model.selectFollowMode(FollowMode.ReceiptOnly)
        h.model.updateDraft("do not duplicate")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()

        val unconfirmed = h.model.state.value.send
        assertInstanceOf(SendUiState.Unconfirmed::class.java, unconfirmed)
        val originalSendId: SendId = (unconfirmed as SendUiState.Unconfirmed).sendId
        assertEquals(1, h.composer.submits.size)
        assertEquals("do not duplicate", h.model.state.value.composer.text)

        h.composer.recoveryResult = RecoveryResult.Confirmed(
            receipt = SendReceipt(originalSendId, null),
            reply = ConversationFixtures.textMessage("recovered-reply", "recovered", sequence = 1),
        )
        h.model.recoverSend()
        advanceUntilIdle()

        assertInstanceOf(SendUiState.Recovered::class.java, h.model.state.value.send)
        assertEquals(originalSendId, h.composer.recoveryRequests.single().sendId)
        assertEquals(1, h.composer.submits.size)
        assertEquals(1, h.model.state.value.items.filterIsInstance<CommittedMessageItem>().size)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-15.confirmed-unread retry reads content only and keeps the confirmation")
    fun ac15ConfirmedUnread() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        h.model.selectFollowMode(FollowMode.FollowReply)
        h.composer.replyResult = ReplyResult.ConfirmedUnread(
            CoreFailure(
                ConversationFailureCodes.CONTENT_UNREADABLE,
                "content unavailable",
                retryable = true,
            ),
        )
        h.model.updateDraft("one submission")
        advanceUntilIdle()
        h.model.submit()
        advanceUntilIdle()

        val unread = h.model.state.value.send
        assertInstanceOf(SendUiState.ConfirmedUnread::class.java, unread)
        assertTrue((unread as SendUiState.ConfirmedUnread).canRetry)
        assertEquals(1, h.composer.submits.size)

        h.composer.replyResult = ReplyResult.Reply(
            ConversationFixtures.textMessage("late-reply", "now readable", sequence = 1),
        )
        h.model.retryContentRead()
        advanceUntilIdle()

        val followed = h.model.state.value.send
        assertInstanceOf(SendUiState.FollowedReply::class.java, followed)
        assertEquals(unread.sendId, (followed as SendUiState.FollowedReply).sendId)
        assertEquals(1, h.composer.submits.size)
        assertEquals(2, h.composer.replyRequests.size)
        assertEquals(unread.sendId, h.composer.replyRequests.last().sendId)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-16.wait-ready quiescence approval and question input are typed")
    fun ac16WaitReady() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()

        h.wait.result = WaitResult.Quiescent
        h.model.waitForQuiescence()
        advanceUntilIdle()
        assertEquals(WaitUiState.Quiescent, h.model.state.value.wait)

        val approval = ConversationFixtures.approvalItem()
        h.wait.result = WaitResult.ApprovalInput(approval)
        h.model.waitForPendingInput()
        advanceUntilIdle()
        assertEquals(WaitUiState.ApprovalInput(approval), h.model.state.value.wait)
        val entry = h.model.state.value.pendingEntry
        assertInstanceOf(PendingEntryUiState.Current::class.java, entry)
        assertTrue((entry as PendingEntryUiState.Current).items.contains(approval))

        val question = ConversationFixtures.questionItem()
        h.wait.result = WaitResult.QuestionInput(question)
        h.model.waitForPendingInput()
        advanceUntilIdle()
        assertEquals(WaitUiState.QuestionInput(question), h.model.state.value.wait)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-16.wait-end timeout and cancel release only their own wait")
    fun ac16WaitEnd() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()

        h.wait.result = WaitResult.TimedOut
        h.model.waitForQuiescence()
        advanceUntilIdle()
        assertEquals(WaitUiState.TimedOut, h.model.state.value.wait)

        val gate = CompletableDeferred<Unit>()
        h.wait.gate = gate
        h.model.waitForQuiescence()
        advanceUntilIdle()
        val waitId = (h.model.state.value.wait as WaitUiState.Waiting).waitId
        h.model.cancelWait()
        advanceUntilIdle()
        assertEquals(WaitUiState.Cancelled, h.model.state.value.wait)
        assertEquals(listOf(waitId), h.wait.cancels)
        assertTrue(h.commands.stops.isEmpty())
        cleanup(h)
    }

    @Test
    @DisplayName("AC-16.stop only confirmation reports stopped and the four outcomes stay distinct")
    fun ac16Stop() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()

        h.commands.outcome = StopOutcome.Accepted
        h.model.stop()
        advanceUntilIdle()
        assertEquals(StopUiState.Accepted, h.model.state.value.stop)

        h.commands.outcome = StopOutcome.Confirmed
        h.model.stop()
        advanceUntilIdle()
        assertEquals(StopUiState.Confirmed, h.model.state.value.stop)

        h.commands.outcome = StopOutcome.Failed(CoreFailure("stop_failed", "failed"))
        h.model.stop()
        advanceUntilIdle()
        assertTrue(h.model.state.value.stop is StopUiState.Failed)

        h.commands.outcome = StopOutcome.Unconfirmed(CoreFailure("stop_unconfirmed", "unknown"))
        h.model.stop()
        advanceUntilIdle()
        assertTrue(h.model.state.value.stop is StopUiState.Unconfirmed)
        assertEquals(4, h.commands.stops.size)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-16.queue-unchanged wait and observation cancellation never stop or dequeue work")
    fun ac16QueueUnchanged() = runTest(dispatcher) {
        val h = Harness()
        h.queue.queue = Outcome.Success(
            QueueSnapshot(
                items = listOf(
                    ConversationFixtures.queueItem(itemId = "q1", text = "first"),
                    ConversationFixtures.queueItem(itemId = "q2", text = "second"),
                ),
            ),
        )
        h.model.open(ConversationFixtures.session())
        advanceUntilIdle()
        val initialQueue = h.model.state.value.pendingQueue
        assertEquals(2, initialQueue.size)

        val gate = CompletableDeferred<Unit>()
        h.wait.gate = gate
        h.model.waitForQuiescence()
        advanceUntilIdle()
        h.model.cancelWait()
        advanceUntilIdle()
        h.model.onHidden()
        advanceUntilIdle()

        assertEquals(initialQueue, h.model.state.value.pendingQueue)
        assertTrue(h.commands.stops.isEmpty())

        h.commands.outcome = StopOutcome.Confirmed
        h.model.stop()
        advanceUntilIdle()
        assertEquals(initialQueue, h.model.state.value.pendingQueue)
        cleanup(h)
    }

    @Test
    @DisplayName("AC-13.adaptive window changes preserve state projection only")
    fun ac13Adaptive() = runTest(dispatcher) {
        val h = Harness()
        val scope = ConversationFixtures.session()
        h.history.latest = Outcome.Success(
            ConversationFixtures.page(listOf(ConversationFixtures.textMessage("m1", "one", sequence = 1))),
        )
        h.model.open(scope)
        advanceUntilIdle()
        h.model.updateDraft("draft survives")
        advanceUntilIdle()
        val generation = h.model.state.value.generation
        val items = h.model.state.value.items

        h.model.onHidden()
        advanceUntilIdle()
        h.model.onVisible()
        advanceUntilIdle()

        // Device layout/orientation rendering is out of JVM scope; the state that
        // the layout must preserve is asserted here.
        assertEquals("draft survives", h.model.state.value.composer.text)
        assertEquals(generation, h.model.state.value.generation)
        assertEquals(items, h.model.state.value.items)
        assertEquals(scope, h.model.state.value.session)
        cleanup(h)
    }

    private fun batch(
        scope: SessionRef,
        generation: Long,
        events: List<ConversationEvent> = emptyList(),
        droppedCount: Int = 0,
        ended: WatchEndReason? = null,
    ): ObservationBatch = ObservationBatch(
        scope = scope,
        generation = generation,
        events = events,
        droppedCount = droppedCount,
        ended = ended,
    )

    private fun TestScope.cleanup(h: Harness) {
        h.model.onHidden()
        advanceUntilIdle()
    }

    private inner class Harness {
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
            ids = SequentialIdGenerator("send"),
            diagnostics = diagnostics,
            queue = queue,
        )
    }
}
