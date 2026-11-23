package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.IdGenerator
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.ScreenModel
import com.whalerein.android.state.core.SessionRef
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

/** Session tools state: model catalog/current/select and pending queue. */
data class SessionToolsUiState(
    val session: SessionRef? = null,
    val models: ModelsUiState = ModelsUiState.Loading,
    val queue: QueueUiState = QueueUiState.Loading,
    val operation: ToolsOperationState = ToolsOperationState.Idle,
    val editingItemId: String? = null,
    val editDraft: String = "",
)

sealed interface ModelsUiState {
    data object Loading : ModelsUiState

    data class Ready(
        val models: List<ModelInfo>,
        val currentModelId: String?,
        val guard: Guard? = null,
    ) : ModelsUiState

    data object Unsupported : ModelsUiState

    data class Unavailable(val failure: CoreFailure) : ModelsUiState
}

sealed interface QueueUiState {
    data object Loading : QueueUiState

    data class Ready(val items: List<PendingQueueItem>) : QueueUiState

    data object Unsupported : QueueUiState

    data class Unavailable(val failure: CoreFailure) : QueueUiState
}

enum class ToolsOperation {
    LoadModels,
    LoadQueue,
    SelectModel,
    EditQueue,
    RemoveQueue,
    SteerQueue,
}

/** Guard conflict, missing capability and unconfirmed are distinct states. */
sealed interface ToolsOperationState {
    data object Idle : ToolsOperationState

    data class InFlight(val operation: ToolsOperation) : ToolsOperationState

    data class Applied(val operation: ToolsOperation) : ToolsOperationState

    data class MissingCapability(val operation: ToolsOperation) : ToolsOperationState

    data class GuardConflict(
        val operation: ToolsOperation,
        val currentGuard: Guard? = null,
    ) : ToolsOperationState

    data class Unconfirmed(
        val operation: ToolsOperation,
        val failure: CoreFailure,
    ) : ToolsOperationState

    data class Failed(
        val operation: ToolsOperation,
        val failure: CoreFailure,
    ) : ToolsOperationState
}

class SessionToolsViewModel(
    private val port: ModelsQueuePort,
    private val ids: IdGenerator,
    diagnostics: DiagnosticSink,
) : ScreenModel<SessionToolsUiState>(SessionToolsUiState()) {
    private val diag = ConversationDiagnostics(diagnostics)

    fun open(session: SessionRef) {
        if (state.value.session == session) {
            return
        }
        setState(SessionToolsUiState(session = session))
        refresh()
    }

    fun refresh() {
        loadModels()
        loadQueue()
    }

    fun loadModels() {
        val session = state.value.session ?: return
        edit { copy(models = ModelsUiState.Loading) }
        scope.launch {
            val result = try {
                port.modelCatalog(session)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                Outcome.Failure(unexpectedFailure())
            }
            if (state.value.session != session) {
                return@launch
            }
            when (result) {
                is Outcome.Success -> edit {
                    copy(
                        models = ModelsUiState.Ready(
                            models = result.value.models,
                            currentModelId = result.value.currentModelId,
                            guard = result.value.guard,
                        ),
                    )
                }

                is Outcome.Failure -> edit { copy(models = modelsFailure(result.failure)) }
                is Outcome.Unconfirmed -> edit {
                    copy(models = ModelsUiState.Unavailable(result.failure))
                }

                Outcome.Cancelled -> Unit
            }
        }
    }

    fun loadQueue() {
        val session = state.value.session ?: return
        edit { copy(queue = QueueUiState.Loading) }
        scope.launch {
            val result = try {
                port.pendingQueue(session)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                Outcome.Failure(unexpectedFailure())
            }
            if (state.value.session != session) {
                return@launch
            }
            when (result) {
                is Outcome.Success -> edit {
                    copy(
                        queue = if (result.value.capable) {
                            QueueUiState.Ready(result.value.items)
                        } else {
                            QueueUiState.Unsupported
                        },
                    )
                }

                is Outcome.Failure -> edit { copy(queue = queueFailure(result.failure)) }
                is Outcome.Unconfirmed -> edit {
                    copy(queue = QueueUiState.Unavailable(result.failure))
                }

                Outcome.Cancelled -> Unit
            }
        }
    }

    fun selectModel(modelId: String) {
        val session = state.value.session ?: return
        val models = state.value.models
        if (models is ModelsUiState.Unsupported) {
            edit { copy(operation = ToolsOperationState.MissingCapability(ToolsOperation.SelectModel)) }
            return
        }
        val guard = (models as? ModelsUiState.Ready)?.guard
        val actionId = ids.newId()
        edit { copy(operation = ToolsOperationState.InFlight(ToolsOperation.SelectModel)) }
        scope.launch {
            val result = try {
                port.selectModel(session, modelId, guard)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                Outcome.Failure(unexpectedFailure())
            }
            if (state.value.session != session) {
                return@launch
            }
            handle(ToolsOperation.SelectModel, actionId, result) { selection ->
                edit {
                    copy(
                        models = (models as? ModelsUiState.Ready)?.copy(
                            currentModelId = selection.modelId,
                        ) ?: models,
                        operation = ToolsOperationState.Applied(ToolsOperation.SelectModel),
                    )
                }
            }
        }
    }

    fun beginEdit(itemId: String) {
        val item = queueItems().firstOrNull { it.itemId == itemId } ?: return
        edit { copy(editingItemId = itemId, editDraft = item.text) }
    }

    fun updateEditDraft(text: String) {
        edit { copy(editDraft = text) }
    }

    fun cancelEdit() {
        edit { copy(editingItemId = null, editDraft = "") }
    }

    fun saveEdit() {
        val itemId = state.value.editingItemId ?: return
        val text = state.value.editDraft
        mutateQueue(ToolsOperation.EditQueue, itemId) { session, guard ->
            port.editQueueItem(session, itemId, text, guard)
        }
    }

    fun removeItem(itemId: String) {
        mutateQueue(ToolsOperation.RemoveQueue, itemId) { session, guard ->
            port.removeQueueItem(session, itemId, guard)
        }
    }

    fun steerItem(itemId: String) {
        mutateQueue(ToolsOperation.SteerQueue, itemId) { session, guard ->
            port.steerQueueItem(session, itemId, guard)
        }
    }

    fun clearOperation() {
        edit { copy(operation = ToolsOperationState.Idle) }
    }

    private fun mutateQueue(
        operation: ToolsOperation,
        itemId: String,
        call: suspend (SessionRef, Guard) -> Outcome<QueueMutation>,
    ) {
        val session = state.value.session ?: return
        if (state.value.queue is QueueUiState.Unsupported) {
            edit { copy(operation = ToolsOperationState.MissingCapability(operation)) }
            return
        }
        val item = queueItems().firstOrNull { it.itemId == itemId }
        if (item == null) {
            edit {
                copy(
                    operation = ToolsOperationState.Failed(
                        operation,
                        CoreFailure(
                            ConversationFailureCodes.NOT_FOUND,
                            "Queue item not found",
                            retryable = false,
                        ),
                    ),
                )
            }
            return
        }
        val actionId = ids.newId()
        val submittedEdit = state.value.editDraft
        edit { copy(operation = ToolsOperationState.InFlight(operation)) }
        scope.launch {
            val result = try {
                call(session, item.guard)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                Outcome.Failure(unexpectedFailure())
            }
            if (state.value.session != session) {
                return@launch
            }
            handle(operation, actionId, result) { mutation ->
                edit {
                    val queue = when (queue) {
                        is QueueUiState.Ready -> QueueUiState.Ready(
                            when (mutation) {
                                is QueueMutation.Edited -> queue.items.map {
                                    if (it.itemId == itemId) it.copy(text = submittedEdit) else it
                                }

                                is QueueMutation.Removed -> queue.items.filterNot {
                                    it.itemId == itemId
                                }

                                is QueueMutation.Steered -> queue.items
                            },
                        )

                        else -> queue
                    }
                    copy(
                        queue = queue,
                        operation = ToolsOperationState.Applied(operation),
                        editingItemId = if (operation == ToolsOperation.EditQueue) null else editingItemId,
                        editDraft = if (operation == ToolsOperation.EditQueue) "" else editDraft,
                    )
                }
            }
        }
    }

    private fun <T> handle(
        operation: ToolsOperation,
        actionId: String,
        outcome: Outcome<T>,
        onApplied: (T) -> Unit,
    ) {
        when (outcome) {
            is Outcome.Success -> {
                diag.info(
                    ConversationDiagnosticStage.QUEUE,
                    ConversationDiagnosticEventName.QUEUE_OPERATION,
                    actionId = actionId,
                    code = operation.name.lowercase(),
                )
                onApplied(outcome.value)
            }

            is Outcome.Failure -> failOperation(operation, actionId, outcome.failure)
            is Outcome.Unconfirmed -> {
                diag.warn(
                    ConversationDiagnosticStage.QUEUE,
                    ConversationDiagnosticEventName.QUEUE_OPERATION,
                    actionId = actionId,
                    code = "unconfirmed",
                )
                edit {
                    copy(operation = ToolsOperationState.Unconfirmed(operation, outcome.failure))
                }
                // An ambiguous result is not an applied change. Refresh the
                // authoritative Host queue and keep an edit draft recoverable.
                loadQueue()
            }

            Outcome.Cancelled -> edit { copy(operation = ToolsOperationState.Idle) }
        }
    }

    private fun failOperation(
        operation: ToolsOperation,
        actionId: String,
        failure: CoreFailure,
    ) {
        diag.warn(
            ConversationDiagnosticStage.QUEUE,
            ConversationDiagnosticEventName.QUEUE_OPERATION,
            actionId = actionId,
            code = failure.code,
        )
        when (failure.code) {
            ConversationFailureCodes.CAPABILITY_MISSING -> edit {
                copy(operation = ToolsOperationState.MissingCapability(operation))
            }

            ConversationFailureCodes.GUARD_CONFLICT -> {
                edit {
                    copy(
                        operation = ToolsOperationState.GuardConflict(
                            operation,
                            currentGuard = null,
                        ),
                    )
                }
                loadQueue()
            }

            else -> edit {
                copy(operation = ToolsOperationState.Failed(operation, failure))
            }
        }
    }

    private fun queueItems(): List<PendingQueueItem> =
        (state.value.queue as? QueueUiState.Ready)?.items.orEmpty()

    private fun modelsFailure(failure: CoreFailure): ModelsUiState =
        if (failure.code == ConversationFailureCodes.CAPABILITY_MISSING) {
            ModelsUiState.Unsupported
        } else {
            ModelsUiState.Unavailable(failure)
        }

    private fun queueFailure(failure: CoreFailure): QueueUiState =
        if (failure.code == ConversationFailureCodes.CAPABILITY_MISSING) {
            QueueUiState.Unsupported
        } else {
            QueueUiState.Unavailable(failure)
        }

    private fun unexpectedFailure(): CoreFailure =
        CoreFailure("unexpected", "Unexpected failure", retryable = true)

    private fun edit(transform: SessionToolsUiState.() -> SessionToolsUiState) {
        update { transform(it) }
    }


}
