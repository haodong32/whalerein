package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.fixtures.conversation.ConversationFixtures
import com.whalerein.android.state.fixtures.conversation.FakeSessionListPort
import com.whalerein.android.state.fixtures.conversation.RecordingDiagnosticSink
import kotlinx.coroutines.CompletableDeferred
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

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class SessionListViewModelTest : ConversationTestBase() {
    @Test
    @DisplayName("AC-08.list-details explicit Host refresh cached marker and capabilities")
    fun ac08ListDetails() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(h.host)
        assertEquals(SessionListStatus.Loading, h.model.state.value.status)
        advanceUntilIdle()
        val ready = h.model.state.value
        assertEquals(SessionListStatus.Ready, ready.status)
        assertEquals(h.host, ready.host)
        assertEquals(listOf(h.host), h.port.listCalls)
        assertEquals(2, ready.sessions.size)
        assertEquals(ConversationFixtures.capabilities(), ready.capabilities)
        assertFalse(ready.cached)
        assertTrue(h.diagnostics.events.any {
            it.event == ConversationDiagnosticEventName.SESSION_LIST_LOADED
        })

        h.port.listOutcome = Outcome.Success(
            com.whalerein.android.state.conversation.SessionListSnapshot(
                host = h.host,
                sessions = listOf(ConversationFixtures.sessionSummary("cached-a")),
                cached = true,
                capabilities = ConversationFixtures.capabilities(canFork = false),
            ),
        )
        h.model.refresh()
        advanceUntilIdle()
        assertEquals(SessionListStatus.Cached, h.model.state.value.status)
        assertTrue(h.model.state.value.cached)
        assertFalse(h.model.state.value.capabilities.canFork)
        assertEquals(2, h.port.listCalls.size)

        h.model.open(h.host)
        advanceUntilIdle()
        assertEquals(2, h.port.listCalls.size)
    }

    @Test
    @DisplayName("AC-08.list-details details load refresh and stale response isolation")
    fun ac08Details() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(h.host)
        advanceUntilIdle()

        h.model.select(SessionId("session-a"))
        advanceUntilIdle()
        val loaded = h.model.state.value.details
        assertInstanceOf(SessionDetailsUiState.Loaded::class.java, loaded)
        assertEquals(
            ConversationFixtures.session(sessionId = "session-a"),
            (loaded as SessionDetailsUiState.Loaded).details.session,
        )
        assertEquals("turn-1", loaded.details.latestCompletedTurnId)
        assertEquals(SessionId("session-a"), h.model.state.value.selectedSessionId)

        h.model.refreshDetails()
        advanceUntilIdle()
        assertEquals(2, h.port.detailsCalls.size)

        // A details response for a session that is no longer selected is dropped.
        h.model.select(SessionId("session-a"))
        h.model.select(SessionId("session-b"))
        advanceUntilIdle()
        assertEquals(4, h.port.detailsCalls.size)
        assertEquals(SessionId("session-b"), h.model.state.value.selectedSessionId)
        assertEquals(
            "session-b",
            ((h.model.state.value.details as SessionDetailsUiState.Loaded).details.session.sessionId.value),
        )

        h.model.refreshDetails()
        h.model.refresh()
        assertTrue(h.port.detailsCalls.size >= 4)
    }

    @Test
    @DisplayName("AC-08.list-details list and details failures stay typed")
    fun ac08ListDetailsFailures() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(h.host)
        advanceUntilIdle()

        h.port.listOutcome = Outcome.Failure(CoreFailure("list_failed", "failed"))
        h.model.refresh()
        advanceUntilIdle()
        assertEquals(SessionListStatus.Failed, h.model.state.value.status)
        assertEquals("list_failed", h.model.state.value.failure?.code)

        h.port.listOutcome = Outcome.Unconfirmed(CoreFailure("list_unknown", "unknown"))
        h.model.refresh()
        advanceUntilIdle()
        assertEquals("list_unknown", h.model.state.value.failure?.code)

        h.port.listThrowable = IllegalStateException("boom")
        h.model.refresh()
        advanceUntilIdle()
        assertEquals("unexpected", h.model.state.value.failure?.code)

        h.port.listThrowable = null
        h.port.listOutcome = Outcome.Success(
            com.whalerein.android.state.conversation.SessionListSnapshot(
                host = ConversationFixtures.host(host = "other-host"),
                sessions = emptyList(),
            ),
        )
        h.model.refresh()
        advanceUntilIdle()
        assertEquals(SessionListFailureCodes.FOREIGN_HOST, h.model.state.value.failure?.code)

        h.port.listOutcome = Outcome.Cancelled
        h.model.refresh()
        advanceUntilIdle()
        assertEquals(SessionListStatus.Loading, h.model.state.value.status)

        h.model.select(SessionId("session-a"))
        advanceUntilIdle()
        h.port.detailsOutcome = Outcome.Failure(CoreFailure("details_failed", "failed"))
        h.model.refreshDetails()
        advanceUntilIdle()
        assertInstanceOf(SessionDetailsUiState.Unavailable::class.java, h.model.state.value.details)

        h.port.detailsOutcome = Outcome.Unconfirmed(CoreFailure("details_unknown", "unknown"))
        h.model.refreshDetails()
        advanceUntilIdle()
        assertInstanceOf(SessionDetailsUiState.Unavailable::class.java, h.model.state.value.details)

        h.port.detailsThrowable = IllegalStateException("boom")
        h.model.refreshDetails()
        advanceUntilIdle()
        assertEquals(
            "unexpected",
            (h.model.state.value.details as SessionDetailsUiState.Unavailable).failure.code,
        )

        h.port.detailsThrowable = null
        h.port.detailsOutcome = Outcome.Success(
            ConversationFixtures.sessionDetails(
                session = ConversationFixtures.session(sessionId = "foreign-session"),
            ),
        )
        h.model.refreshDetails()
        advanceUntilIdle()
        assertEquals(
            SessionListFailureCodes.FOREIGN_SESSION,
            (h.model.state.value.details as SessionDetailsUiState.Unavailable).failure.code,
        )
    }

    @Test
    @DisplayName("AC-08.list-details operations before a Host are harmless")
    fun ac08BeforeHost() = runTest(dispatcher) {
        val h = Harness()
        h.model.refresh()
        h.model.refreshDetails()
        h.model.select(SessionId("session-a"))
        h.model.showCreateForm()
        h.model.hideCreateForm()
        h.model.updateCreateInput(ConversationFixtures.createInput(workingDirectory = "/tmp"))
        h.model.clearCreateInput()
        h.model.create()
        h.model.createDefault()
        h.model.fork()
        h.model.clearOperation()
        advanceUntilIdle()
        assertEquals(SessionListStatus.Idle, h.model.state.value.status)
        assertTrue(h.port.listCalls.isEmpty())
        assertTrue(h.port.createCalls.isEmpty())
        assertTrue(h.port.forkCalls.isEmpty())
    }

    @Test
    @DisplayName("AC-08.create default and explicit inputs reach the selected Host")
    fun ac08CreateInputs() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(h.host)
        advanceUntilIdle()

        h.model.showCreateForm()
        assertTrue(h.model.state.value.createFormVisible)
        h.model.createDefault()
        advanceUntilIdle()
        val defaultRequest = h.port.createCalls.single()
        assertEquals(h.host, defaultRequest.first)
        assertNull(defaultRequest.second.workingDirectory)
        assertNull(defaultRequest.second.workspace)
        assertNull(defaultRequest.second.preset)
        assertNull(defaultRequest.second.preferredSessionId)
        assertInstanceOf(CreateSessionUiState.Created::class.java, h.model.state.value.create)
        assertFalse(h.model.state.value.createFormVisible)
        assertTrue(h.model.state.value.sessions.any {
            it.sessionId == SessionId("created-1")
        })
        assertInstanceOf(SessionDetailsUiState.Loaded::class.java, h.model.state.value.details)

        val explicit = ConversationFixtures.createInput(
            workingDirectory = "/workspace/project",
            preset = "default",
            preferredSessionId = "preferred-1",
        )
        h.model.updateCreateInput(explicit)
        h.model.clearOperation()
        h.model.create()
        advanceUntilIdle()
        assertEquals(explicit, h.port.createCalls.last().second)
        assertEquals(h.host, h.port.createCalls.last().first)

        h.model.updateCreateInput(ConversationFixtures.createInput(workspace = "cloud-workspace"))
        h.model.clearOperation()
        h.model.create()
        advanceUntilIdle()
        assertEquals("cloud-workspace", h.port.createCalls.last().second.workspace)
    }

    @Test
    @DisplayName("AC-08.create unsupported invalid and capability-gated inputs stay explicit")
    fun ac08CreateValidation() = runTest(dispatcher) {
        val unsupportedCreate = Harness()
        unsupportedCreate.port.listOutcome = Outcome.Success(
            com.whalerein.android.state.conversation.SessionListSnapshot(
                host = unsupportedCreate.host,
                sessions = emptyList(),
                capabilities = ConversationFixtures.capabilities(canCreate = false),
            ),
        )
        unsupportedCreate.model.open(unsupportedCreate.host)
        advanceUntilIdle()
        unsupportedCreate.model.createDefault()
        advanceUntilIdle()
        assertEquals(CreateSessionUiState.Unsupported, unsupportedCreate.model.state.value.create)
        assertTrue(unsupportedCreate.port.createCalls.isEmpty())

        val fieldCaps = Harness()
        fieldCaps.port.listOutcome = Outcome.Success(
            com.whalerein.android.state.conversation.SessionListSnapshot(
                host = fieldCaps.host,
                sessions = emptyList(),
                capabilities = ConversationFixtures.capabilities(
                    supportsWorkingDirectory = false,
                    supportsWorkspace = false,
                    supportsPreset = false,
                    supportsPreferredSessionId = false,
                ),
            ),
        )
        fieldCaps.model.open(fieldCaps.host)
        advanceUntilIdle()
        fieldCaps.model.create(ConversationFixtures.createInput(workingDirectory = "/tmp"))
        advanceUntilIdle()
        assertEquals(CreateSessionUiState.Unsupported, fieldCaps.model.state.value.create)
        fieldCaps.model.create(ConversationFixtures.createInput(workspace = "ws"))
        advanceUntilIdle()
        assertEquals(CreateSessionUiState.Unsupported, fieldCaps.model.state.value.create)
        fieldCaps.model.create(ConversationFixtures.createInput(preset = "p"))
        advanceUntilIdle()
        assertEquals(CreateSessionUiState.Unsupported, fieldCaps.model.state.value.create)
        fieldCaps.model.create(ConversationFixtures.createInput(preferredSessionId = "preferred"))
        advanceUntilIdle()
        assertEquals(CreateSessionUiState.Unsupported, fieldCaps.model.state.value.create)
        assertTrue(fieldCaps.port.createCalls.isEmpty())

        val invalid = Harness()
        invalid.model.open(invalid.host)
        advanceUntilIdle()
        invalid.model.create(
            ConversationFixtures.createInput(
                workingDirectory = "/tmp",
                workspace = "ws",
            ),
        )
        advanceUntilIdle()
        assertEquals(
            SessionListFailureCodes.INVALID_INPUT,
            (invalid.model.state.value.create as CreateSessionUiState.Failed).failure.code,
        )
        invalid.model.create(ConversationFixtures.createInput(workingDirectory = " "))
        advanceUntilIdle()
        assertEquals(
            SessionListFailureCodes.INVALID_INPUT,
            (invalid.model.state.value.create as CreateSessionUiState.Failed).failure.code,
        )
        invalid.model.create(ConversationFixtures.createInput(workspace = " "))
        advanceUntilIdle()
        assertEquals(
            SessionListFailureCodes.INVALID_INPUT,
            (invalid.model.state.value.create as CreateSessionUiState.Failed).failure.code,
        )
        invalid.model.create(ConversationFixtures.createInput(preset = " "))
        advanceUntilIdle()
        assertEquals(
            SessionListFailureCodes.INVALID_INPUT,
            (invalid.model.state.value.create as CreateSessionUiState.Failed).failure.code,
        )
        assertTrue(invalid.port.createCalls.isEmpty())
    }

    @Test
    @DisplayName("AC-08.create unconfirmed failure foreign and duplicate submissions stay typed")
    fun ac08CreateOutcomes() = runTest(dispatcher) {
        val unconfirmed = Harness()
        unconfirmed.model.open(unconfirmed.host)
        advanceUntilIdle()
        val input = ConversationFixtures.createInput(workingDirectory = "/tmp/kept")
        unconfirmed.model.updateCreateInput(input)
        unconfirmed.port.createOutcome = Outcome.Unconfirmed(CoreFailure("unknown", "unknown"))
        unconfirmed.model.create()
        advanceUntilIdle()
        assertInstanceOf(CreateSessionUiState.Unconfirmed::class.java, unconfirmed.model.state.value.create)
        assertEquals(input, unconfirmed.model.state.value.createInput)

        val failed = Harness()
        failed.model.open(failed.host)
        advanceUntilIdle()
        failed.port.createOutcome = Outcome.Failure(CoreFailure("failed", "failed"))
        failed.model.createDefault()
        advanceUntilIdle()
        assertInstanceOf(CreateSessionUiState.Failed::class.java, failed.model.state.value.create)

        val gated = Harness()
        gated.model.open(gated.host)
        advanceUntilIdle()
        val gate = CompletableDeferred<Unit>()
        gated.port.createGate = gate
        gated.model.createDefault()
        advanceUntilIdle()
        gated.model.createDefault()
        advanceUntilIdle()
        assertEquals(1, gated.port.createCalls.size)
        gate.complete(Unit)
        advanceUntilIdle()
        assertInstanceOf(CreateSessionUiState.Created::class.java, gated.model.state.value.create)
    }

    @Test
    @DisplayName("AC-08.fork latest completed turn displays the confirmed child without execution")
    fun ac08Fork() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(h.host)
        advanceUntilIdle()
        h.model.select(SessionId("session-a"))
        advanceUntilIdle()

        h.model.fork()
        advanceUntilIdle()
        val request = h.port.forkCalls.single()
        assertEquals(h.host, request.first.host)
        assertEquals(SessionId("session-a"), request.first.sessionId)
        assertEquals("turn-1", request.second.turnId)
        val forked = h.model.state.value.fork
        assertInstanceOf(ForkSessionUiState.Forked::class.java, forked)
        forked as ForkSessionUiState.Forked
        assertEquals(h.host, forked.child.host)
        assertEquals("child-1", forked.child.sessionId.value)
        assertTrue(forked.workingDirectoryAvailable)
        assertFalse(forked.autoExecutionStarted)
        assertEquals(SessionId("child-1"), h.model.state.value.selectedSessionId)
        assertInstanceOf(SessionDetailsUiState.Loaded::class.java, h.model.state.value.details)
        assertTrue(h.model.state.value.sessions.any { it.sessionId == SessionId("child-1") })

        val explicit = Harness()
        explicit.model.open(explicit.host)
        advanceUntilIdle()
        explicit.model.select(SessionId("session-a"))
        advanceUntilIdle()
        explicit.model.fork("turn-explicit")
        advanceUntilIdle()
        assertEquals("turn-explicit", explicit.port.forkCalls.single().second.turnId)
        assertEquals(
            SessionId("child-1"),
            (explicit.model.state.value.fork as ForkSessionUiState.Forked).child.sessionId,
        )
    }

    @Test
    @DisplayName("AC-08.fork unavailable working directory still succeeds")
    fun ac08ForkNoWorkingDirectory() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(h.host)
        advanceUntilIdle()
        h.model.select(SessionId("session-a"))
        advanceUntilIdle()
        val child = ConversationFixtures.session(sessionId = "child-2")
        h.port.forkOutcome = Outcome.Success(
            ConversationFixtures.forked(
                child = child,
                details = ConversationFixtures.sessionDetails(child, workingDirectory = null),
                workingDirectoryAvailable = false,
            ),
        )
        h.model.fork()
        advanceUntilIdle()
        val forked = h.model.state.value.fork as ForkSessionUiState.Forked
        assertEquals(child, forked.child)
        assertFalse(forked.workingDirectoryAvailable)
        assertFalse(forked.autoExecutionStarted)
        assertEquals(SessionId("child-2"), h.model.state.value.selectedSessionId)
    }

    @Test
    @DisplayName("AC-08.fork missing turn and details are explicit")
    fun ac08ForkPreconditions() = runTest(dispatcher) {
        val noDetails = Harness()
        noDetails.model.open(noDetails.host)
        advanceUntilIdle()
        noDetails.model.select(SessionId("session-a"))
        noDetails.model.fork()
        advanceUntilIdle()
        // Details are still loading, so there is no completed-turn target yet.
        assertEquals(
            SessionListFailureCodes.DETAILS_REQUIRED,
            (noDetails.model.state.value.fork as ForkSessionUiState.Failed).failure.code,
        )
        assertTrue(noDetails.port.forkCalls.isEmpty())

        val detailsFailed = Harness()
        detailsFailed.port.detailsOutcome = Outcome.Failure(CoreFailure("details", "failed"))
        detailsFailed.model.open(detailsFailed.host)
        advanceUntilIdle()
        detailsFailed.model.select(SessionId("session-a"))
        advanceUntilIdle()
        detailsFailed.model.fork()
        advanceUntilIdle()
        assertEquals(
            SessionListFailureCodes.DETAILS_REQUIRED,
            (detailsFailed.model.state.value.fork as ForkSessionUiState.Failed).failure.code,
        )
        assertTrue(detailsFailed.port.forkCalls.isEmpty())

        val noTurn = Harness()
        noTurn.port.detailsOutcome = Outcome.Success(
            ConversationFixtures.sessionDetails(
                session = ConversationFixtures.session(sessionId = "session-a"),
                latestCompletedTurnId = null,
            ),
        )
        noTurn.model.open(noTurn.host)
        advanceUntilIdle()
        noTurn.model.select(SessionId("session-a"))
        advanceUntilIdle()
        noTurn.model.fork()
        advanceUntilIdle()
        assertEquals(
            SessionListFailureCodes.NO_COMPLETED_TURN,
            (noTurn.model.state.value.fork as ForkSessionUiState.Failed).failure.code,
        )
        assertTrue(noTurn.port.forkCalls.isEmpty())

        val unsupported = Harness()
        unsupported.port.listOutcome = Outcome.Success(
            com.whalerein.android.state.conversation.SessionListSnapshot(
                host = unsupported.host,
                sessions = emptyList(),
                capabilities = ConversationFixtures.capabilities(canFork = false),
            ),
        )
        unsupported.model.open(unsupported.host)
        advanceUntilIdle()
        unsupported.model.select(SessionId("session-a"))
        advanceUntilIdle()
        unsupported.model.fork()
        advanceUntilIdle()
        assertEquals(ForkSessionUiState.Unsupported, unsupported.model.state.value.fork)
        assertTrue(unsupported.port.forkCalls.isEmpty())

        val noSelection = Harness()
        noSelection.model.open(noSelection.host)
        advanceUntilIdle()
        noSelection.model.fork()
        advanceUntilIdle()
        assertEquals(ForkSessionUiState.Idle, noSelection.model.state.value.fork)
        assertTrue(noSelection.port.forkCalls.isEmpty())
    }

    @Test
    @DisplayName("AC-08.unavailable missing capability foreign Host and transport outcomes")
    fun ac08Unavailable() = runTest(dispatcher) {
        val foreignList = Harness()
        foreignList.port.listOutcome = Outcome.Success(
            com.whalerein.android.state.conversation.SessionListSnapshot(
                host = ConversationFixtures.host(instance = "other-instance"),
                sessions = emptyList(),
                cached = true,
            ),
        )
        foreignList.model.open(foreignList.host)
        advanceUntilIdle()
        assertEquals(SessionListStatus.Failed, foreignList.model.state.value.status)
        assertEquals(
            SessionListFailureCodes.FOREIGN_HOST,
            foreignList.model.state.value.failure?.code,
        )
        assertFalse(foreignList.model.state.value.cached)

        val foreignCreate = Harness()
        foreignCreate.model.open(foreignCreate.host)
        advanceUntilIdle()
        val other = ConversationFixtures.session(host = "other-host", sessionId = "foreign-create")
        foreignCreate.port.createOutcome = Outcome.Success(
            ConversationFixtures.created(session = other, details = ConversationFixtures.sessionDetails(other)),
        )
        foreignCreate.model.createDefault()
        advanceUntilIdle()
        assertEquals(
            SessionListFailureCodes.FOREIGN_HOST,
            (foreignCreate.model.state.value.create as CreateSessionUiState.Failed).failure.code,
        )

        val foreignFork = Harness()
        foreignFork.model.open(foreignFork.host)
        advanceUntilIdle()
        foreignFork.model.select(SessionId("session-a"))
        advanceUntilIdle()
        val foreignChild = ConversationFixtures.session(host = "other-host", sessionId = "foreign-child")
        foreignFork.port.forkOutcome = Outcome.Success(
            ConversationFixtures.forked(
                child = foreignChild,
                details = ConversationFixtures.sessionDetails(foreignChild),
            ),
        )
        foreignFork.model.fork()
        advanceUntilIdle()
        assertEquals(
            SessionListFailureCodes.FOREIGN_HOST,
            (foreignFork.model.state.value.fork as ForkSessionUiState.Failed).failure.code,
        )

        val capabilityFailure = Harness()
        capabilityFailure.model.open(capabilityFailure.host)
        advanceUntilIdle()
        capabilityFailure.port.createOutcome = Outcome.Failure(
            CoreFailure(ConversationFailureCodes.CAPABILITY_MISSING, "missing"),
        )
        capabilityFailure.model.createDefault()
        advanceUntilIdle()
        assertEquals(CreateSessionUiState.Unsupported, capabilityFailure.model.state.value.create)
        capabilityFailure.model.select(SessionId("session-a"))
        advanceUntilIdle()
        capabilityFailure.port.forkOutcome = Outcome.Failure(
            CoreFailure(ConversationFailureCodes.CAPABILITY_MISSING, "missing"),
        )
        capabilityFailure.model.fork()
        advanceUntilIdle()
        assertEquals(ForkSessionUiState.Unsupported, capabilityFailure.model.state.value.fork)
    }

    @Test
    @DisplayName("AC-08.unavailable fork and create transport outcomes are typed")
    fun ac08TransportOutcomes() = runTest(dispatcher) {
        val forkFailed = Harness()
        forkFailed.model.open(forkFailed.host)
        advanceUntilIdle()
        forkFailed.model.select(SessionId("session-a"))
        advanceUntilIdle()

        forkFailed.port.forkOutcome = Outcome.Failure(CoreFailure("fork", "failed"))
        forkFailed.model.fork()
        advanceUntilIdle()
        assertInstanceOf(ForkSessionUiState.Failed::class.java, forkFailed.model.state.value.fork)

        forkFailed.port.forkOutcome = Outcome.Unconfirmed(CoreFailure("fork", "unknown"))
        forkFailed.model.fork()
        advanceUntilIdle()
        assertInstanceOf(ForkSessionUiState.Unconfirmed::class.java, forkFailed.model.state.value.fork)

        forkFailed.port.forkThrowable = IllegalStateException("boom")
        forkFailed.model.fork()
        advanceUntilIdle()
        assertInstanceOf(ForkSessionUiState.Failed::class.java, forkFailed.model.state.value.fork)

        forkFailed.port.forkThrowable = null
        forkFailed.port.forkOutcome = Outcome.Cancelled
        forkFailed.model.fork()
        advanceUntilIdle()
        assertEquals(ForkSessionUiState.Idle, forkFailed.model.state.value.fork)

        val createThrown = Harness()
        createThrown.model.open(createThrown.host)
        advanceUntilIdle()
        createThrown.port.createThrowable = IllegalStateException("boom")
        createThrown.model.createDefault()
        advanceUntilIdle()
        assertEquals(
            "unexpected",
            (createThrown.model.state.value.create as CreateSessionUiState.Failed).failure.code,
        )

        createThrown.port.createThrowable = null
        createThrown.port.createOutcome = Outcome.Cancelled
        createThrown.model.createDefault()
        advanceUntilIdle()
        assertEquals(CreateSessionUiState.Idle, createThrown.model.state.value.create)

        val gatedFork = Harness()
        gatedFork.model.open(gatedFork.host)
        advanceUntilIdle()
        gatedFork.model.select(SessionId("session-a"))
        advanceUntilIdle()
        val gate = CompletableDeferred<Unit>()
        gatedFork.port.forkGate = gate
        gatedFork.model.fork()
        advanceUntilIdle()
        gatedFork.model.fork()
        advanceUntilIdle()
        assertEquals(1, gatedFork.port.forkCalls.size)
        gate.complete(Unit)
        advanceUntilIdle()
    }

    private class Harness {
        val host = ConversationFixtures.host()
        val port = FakeSessionListPort()
        val diagnostics = RecordingDiagnosticSink()
        val model = SessionListViewModel(port = port, diagnostics = diagnostics)
    }
}
