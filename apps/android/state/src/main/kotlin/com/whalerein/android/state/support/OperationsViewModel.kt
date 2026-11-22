package com.whalerein.android.state.support

import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.ScreenModel

/**
 * Operations screen contract.
 *
 * Listing never recovers or submits anything. Recovery is explicit, checks the
 * operation's original authorization scope first, and is refused without a
 * port call when that scope is revoked, superseded, foreign or obsolete.
 */
class OperationsViewModel(
    instanceId: InstanceId,
    private val port: OperationsPort,
    private val diagnostics: DiagnosticSink,
    private val clock: Clock,
) : ScreenModel<OperationsUiState>(OperationsUiState(instanceId = instanceId)) {
    suspend fun refresh() {
        setState(state.value.copy(loading = true))
        when (val outcome = port.inventory(state.value.instanceId)) {
            is Outcome.Success -> {
                val inventory = outcome.value
                setState(
                    state.value.copy(
                        loading = false,
                        operations = inventory.operations,
                        pairingHistory = inventory.pairingHistory,
                        notice = null,
                    ),
                )
                emit(
                    DiagnosticLevel.Info,
                    DiagnosticEvents.OPERATIONS_INVENTORY_LOADED,
                    DiagnosticStages.COMMAND,
                    fields = mapOf("count" to inventory.operations.size.toString()),
                )
            }
            is Outcome.Failure -> {
                setState(
                    state.value.copy(
                        loading = false,
                        notice = OperationsNotice.LoadFailed(outcome.failure),
                    ),
                )
                emit(
                    DiagnosticLevel.Warn,
                    DiagnosticEvents.COMMAND_RECOVERY_REFUSED,
                    DiagnosticStages.COMMAND,
                    code = outcome.failure.code,
                )
            }
            is Outcome.Unconfirmed -> {
                setState(
                    state.value.copy(
                        loading = false,
                        notice = OperationsNotice.LoadFailed(outcome.failure),
                    ),
                )
            }
            is Outcome.Cancelled -> setState(state.value.copy(loading = false))
        }
    }

    suspend fun recover(operationId: String) {
        val operation = state.value.operations.firstOrNull { it.operationId == operationId }
        if (operation == null) {
            setState(
                state.value.copy(
                    notice = OperationsNotice.RecoveryFailed(
                        operationId,
                        CoreFailure(
                            code = "unknown_operation",
                            message = "The operation is no longer retained.",
                        ),
                    ),
                ),
            )
            return
        }
        if (state.value.recoveringOperationId != null) return

        setState(state.value.copy(recoveringOperationId = operationId))
        try {
            when (val authorization = port.authorization(operation.scope)) {
                is Outcome.Success -> {
                    if (!authorization.value.authorized) {
                        refuse(operationId, authorization.value.state)
                        return
                    }
                    val request = OperationRecoveryRequest(operation, authorization.value)
                    when (val outcome = port.recover(request)) {
                        is Outcome.Success -> when (val recovered = outcome.value) {
                            is RecoveredOperation.Confirmed -> {
                                updateStatus(operationId, OperationStatus.Confirmed)
                                setState(
                                    state.value.copy(
                                        notice = OperationsNotice.RecoveryConfirmed(operationId),
                                    ),
                                )
                                emit(
                                    DiagnosticLevel.Info,
                                    DiagnosticEvents.COMMAND_RECOVERED,
                                    DiagnosticStages.COMMAND,
                                    fields = mapOf("operation_id" to operationId),
                                )
                            }
                            is RecoveredOperation.Unconfirmed -> {
                                updateStatus(operationId, OperationStatus.Unconfirmed)
                                setState(
                                    state.value.copy(
                                        notice = OperationsNotice.RecoveryUnconfirmed(
                                            operationId,
                                            recovered.failure,
                                        ),
                                    ),
                                )
                            }
                        }
                        is Outcome.Failure -> {
                            updateStatus(operationId, OperationStatus.Failed)
                            setState(
                                state.value.copy(
                                    notice = OperationsNotice.RecoveryFailed(
                                        operationId,
                                        outcome.failure,
                                    ),
                                ),
                            )
                            emit(
                                DiagnosticLevel.Warn,
                                DiagnosticEvents.COMMAND_RECOVERY_REFUSED,
                                DiagnosticStages.COMMAND,
                                code = outcome.failure.code,
                                fields = mapOf("operation_id" to operationId),
                            )
                        }
                        is Outcome.Unconfirmed -> {
                            updateStatus(operationId, OperationStatus.Unconfirmed)
                            setState(
                                state.value.copy(
                                    notice = OperationsNotice.RecoveryUnconfirmed(
                                        operationId,
                                        outcome.failure,
                                    ),
                                ),
                            )
                        }
                        is Outcome.Cancelled -> {
                            setState(
                                state.value.copy(
                                    notice = OperationsNotice.RecoveryCancelled(operationId),
                                ),
                            )
                        }
                    }
                }
                is Outcome.Failure -> {
                    setState(
                        state.value.copy(
                            notice = OperationsNotice.RecoveryFailed(
                                operationId,
                                authorization.failure,
                            ),
                        ),
                    )
                }
                is Outcome.Unconfirmed -> {
                    setState(
                        state.value.copy(
                            notice = OperationsNotice.RecoveryFailed(
                                operationId,
                                authorization.failure,
                            ),
                        ),
                    )
                }
                is Outcome.Cancelled -> {
                    setState(
                        state.value.copy(
                            notice = OperationsNotice.RecoveryCancelled(operationId),
                        ),
                    )
                }
            }
        } finally {
            setState(state.value.copy(recoveringOperationId = null))
        }
    }

    fun clearNotice() {
        setState(state.value.copy(notice = null))
    }

    private fun refuse(operationId: String, reason: OperationAuthorizationState) {
        setState(
            state.value.copy(
                notice = OperationsNotice.RecoveryRefused(operationId, reason),
            ),
        )
        emit(
            DiagnosticLevel.Warn,
            DiagnosticEvents.COMMAND_RECOVERY_REFUSED,
            DiagnosticStages.COMMAND,
            code = reason.name.lowercase(),
            fields = mapOf("operation_id" to operationId),
        )
    }

    private fun updateStatus(operationId: String, status: OperationStatus) {
        val now = clock.epochMillis()
        setState(
            state.value.copy(
                operations = state.value.operations.map { operation ->
                    if (operation.operationId == operationId) {
                        operation.copy(status = status, updatedAtEpochMillis = now)
                    } else {
                        operation
                    }
                },
            ),
        )
    }

    private fun emit(
        level: DiagnosticLevel,
        event: String,
        stage: String,
        code: String? = null,
        fields: Map<String, String> = emptyMap(),
    ) {
        DiagnosticDelivery.deliver(
            diagnostics,
            DiagnosticEvent(
                level = level,
                event = event,
                stage = stage,
                code = code,
                instanceId = state.value.instanceId.value,
                fields = fields,
            ),
        )
    }
}
