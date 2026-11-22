package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.fixtures.conversation.ConversationFixtures
import com.whalerein.android.state.fixtures.conversation.FakeModelsQueuePort
import com.whalerein.android.state.fixtures.conversation.RecordingDiagnosticSink
import com.whalerein.android.state.fixtures.conversation.SequentialIdGenerator
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class SessionToolsViewModelTest : ConversationTestBase() {
    @Test
    @DisplayName("AC-17.models catalog current model and selection stay scoped")
    fun ac17Models() = runTest(dispatcher) {
        val h = Harness()
        h.model.open(h.scope)
        advanceUntilIdle()

        val models = h.model.state.value.models
        assertInstanceOf(ModelsUiState.Ready::class.java, models)
        models as ModelsUiState.Ready
        assertEquals(2, models.models.size)
        assertEquals("model-a", models.currentModelId)

        h.model.selectModel("model-b")
        advanceUntilIdle()

        assertEquals(
            listOf("model-b" to Guard("catalog-guard")),
            h.port.selectCalls,
        )
        assertEquals(listOf(h.scope), h.port.operationScopes)
        assertEquals(
            ToolsOperationState.Applied(ToolsOperation.SelectModel),
            h.model.state.value.operation,
        )
        assertEquals(
            "model-b",
            (h.model.state.value.models as ModelsUiState.Ready).currentModelId,
        )
        assertTrue(h.diagnostics.events.any { it.stage == ConversationDiagnosticStage.QUEUE })
    }

    @Test
    @DisplayName("AC-17.queue list edit remove and steer use the reviewed item guard")
    fun ac17Queue() = runTest(dispatcher) {
        val h = Harness()
        h.port.queue = Outcome.Success(
            QueueSnapshot(
                items = listOf(
                    ConversationFixtures.queueItem("q1", "first", "guard-1"),
                    ConversationFixtures.queueItem("q2", "second", "guard-2"),
                ),
            ),
        )
        h.model.open(h.scope)
        advanceUntilIdle()
        val ready = h.model.state.value.queue
        assertInstanceOf(QueueUiState.Ready::class.java, ready)
        assertEquals(2, (ready as QueueUiState.Ready).items.size)

        h.model.beginEdit("q1")
        h.model.updateEditDraft("edited first")
        h.model.saveEdit()
        advanceUntilIdle()
        assertEquals(
            listOf(Triple("q1", "edited first", Guard("guard-1"))),
            h.port.editCalls,
        )
        assertEquals(
            "edited first",
            (h.model.state.value.queue as QueueUiState.Ready).items.first { it.itemId == "q1" }.text,
        )
        assertEquals(ToolsOperationState.Applied(ToolsOperation.EditQueue), h.model.state.value.operation)
        assertNull(h.model.state.value.editingItemId)

        h.model.removeItem("q2")
        advanceUntilIdle()
        assertEquals(listOf("q2" to Guard("guard-2")), h.port.removeCalls)
        assertTrue((h.model.state.value.queue as QueueUiState.Ready).items.none { it.itemId == "q2" })
        assertEquals(ToolsOperationState.Applied(ToolsOperation.RemoveQueue), h.model.state.value.operation)

        h.model.steerItem("q1")
        advanceUntilIdle()
        assertEquals(listOf("q1" to Guard("guard-1")), h.port.steerCalls)
        assertEquals(ToolsOperationState.Applied(ToolsOperation.SteerQueue), h.model.state.value.operation)
        assertEquals(listOf(h.scope, h.scope, h.scope), h.port.operationScopes)
    }

    @Test
    @DisplayName("AC-17.failures missing capability guard conflict and unconfirmed differ")
    fun ac17Failures() = runTest(dispatcher) {
        val unsupported = Harness()
        unsupported.port.catalog = Outcome.Failure(
            CoreFailure(ConversationFailureCodes.CAPABILITY_MISSING, "no models"),
        )
        unsupported.port.queue = Outcome.Failure(
            CoreFailure(ConversationFailureCodes.CAPABILITY_MISSING, "no queue"),
        )
        unsupported.model.open(unsupported.scope)
        advanceUntilIdle()
        assertEquals(ModelsUiState.Unsupported, unsupported.model.state.value.models)
        assertEquals(QueueUiState.Unsupported, unsupported.model.state.value.queue)

        unsupported.model.selectModel("model-b")
        advanceUntilIdle()
        assertEquals(
            ToolsOperationState.MissingCapability(ToolsOperation.SelectModel),
            unsupported.model.state.value.operation,
        )
        assertTrue(unsupported.port.selectCalls.isEmpty())

        unsupported.model.removeItem("q1")
        advanceUntilIdle()
        assertEquals(
            ToolsOperationState.MissingCapability(ToolsOperation.RemoveQueue),
            unsupported.model.state.value.operation,
        )
        assertTrue(unsupported.port.removeCalls.isEmpty())

        val conflict = Harness()
        conflict.model.open(conflict.scope)
        advanceUntilIdle()
        conflict.port.editOutcome = Outcome.Failure(
            CoreFailure(ConversationFailureCodes.GUARD_CONFLICT, "changed guard"),
        )
        val queueCallsBefore = conflict.port.queueCalls.size
        conflict.model.beginEdit("item-1")
        conflict.model.updateEditDraft("edited")
        conflict.model.saveEdit()
        advanceUntilIdle()
        assertInstanceOf(
            ToolsOperationState.GuardConflict::class.java,
            conflict.model.state.value.operation,
        )
        assertEquals(ToolsOperation.EditQueue, (conflict.model.state.value.operation as ToolsOperationState.GuardConflict).operation)
        assertTrue(conflict.port.queueCalls.size > queueCallsBefore)

        val unconfirmed = Harness()
        unconfirmed.model.open(unconfirmed.scope)
        advanceUntilIdle()
        unconfirmed.port.steerOutcome = Outcome.Unconfirmed(
            CoreFailure("network", "unknown result", retryable = true),
        )
        unconfirmed.model.steerItem("item-1")
        advanceUntilIdle()
        assertInstanceOf(
            ToolsOperationState.Unconfirmed::class.java,
            unconfirmed.model.state.value.operation,
        )
        assertTrue(
            unconfirmed.model.state.value.operation !is ToolsOperationState.MissingCapability,
        )
    }

    private class Harness {
        val scope = ConversationFixtures.session()
        val port = FakeModelsQueuePort()
        val diagnostics = RecordingDiagnosticSink()
        val model = SessionToolsViewModel(
            port = port,
            ids = SequentialIdGenerator("tool"),
            diagnostics = diagnostics,
        )
    }
}
