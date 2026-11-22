package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionId

enum class RetainedOperationKind {
    Send,
    Control,
    CloudPairing,
    StandalonePairing,
}

enum class OperationStatus {
    Pending,
    Confirmed,
    Failed,
    Unconfirmed,
}

/**
 * The original authorization scope of a retained operation. Recovery must match
 * every field that Core originally issued; a newer revision or callback
 * generation makes the old operation unrecoverable.
 */
data class OperationScope(
    val instanceId: InstanceId,
    val accountId: String? = null,
    val hostId: HostId? = null,
    val sessionId: SessionId? = null,
    val authorizationRevision: Long = 0,
    val callbackGeneration: Long = 0,
)

data class RetainedOperation(
    val operationId: String,
    val kind: RetainedOperationKind,
    val status: OperationStatus,
    val scope: OperationScope,
    val createdAtEpochMillis: Long,
    val updatedAtEpochMillis: Long,
    /** A redaction-safe label such as a host alias, never content. */
    val label: String,
    val attemptId: String? = null,
)

data class PairingHistoryEntry(
    val pairingId: String,
    val kind: RetainedOperationKind,
    val outcome: OperationStatus,
    val scope: OperationScope,
    val recordedAtEpochMillis: Long,
    val hostLabel: String,
)

data class OperationInventory(
    val operations: List<RetainedOperation> = emptyList(),
    val pairingHistory: List<PairingHistoryEntry> = emptyList(),
)

enum class OperationAuthorizationState {
    Authorized,
    Revoked,
    Superseded,
    ForeignAccount,
    OldCallback,
    Expired,
    Missing,
}

data class OperationAuthorization(
    val state: OperationAuthorizationState,
    val accountId: String? = null,
    val authorizationRevision: Long = 0,
    val callbackGeneration: Long = 0,
) {
    val authorized: Boolean get() = state == OperationAuthorizationState.Authorized
}

data class OperationRecoveryRequest(
    val operation: RetainedOperation,
    val authorization: OperationAuthorization,
)

sealed interface RecoveredOperation {
    data class Confirmed(val operationId: String, val detailCode: String? = null) :
        RecoveredOperation

    data class Unconfirmed(val operationId: String, val failure: CoreFailure) :
        RecoveredOperation
}

/**
 * Core-backed operation inventory and recovery.
 *
 * The port has no "submit" entry point: recovery can only read the original
 * operation identity. Any implementation that submits new intent violates the
 * contract.
 */
interface OperationsPort {
    suspend fun inventory(instance: InstanceId): Outcome<OperationInventory>

    suspend fun authorization(scope: OperationScope): Outcome<OperationAuthorization>

    suspend fun recover(request: OperationRecoveryRequest): Outcome<RecoveredOperation>
}

data class OperationStatusCounts(
    val pending: Int = 0,
    val confirmed: Int = 0,
    val failed: Int = 0,
    val unconfirmed: Int = 0,
)

sealed interface OperationsNotice {
    data class LoadFailed(val failure: CoreFailure) : OperationsNotice

    data class RecoveryRefused(
        val operationId: String,
        val reason: OperationAuthorizationState,
    ) : OperationsNotice

    data class RecoveryFailed(val operationId: String, val failure: CoreFailure) :
        OperationsNotice

    data class RecoveryUnconfirmed(val operationId: String, val failure: CoreFailure) :
        OperationsNotice

    data class RecoveryConfirmed(val operationId: String) : OperationsNotice

    data class RecoveryCancelled(val operationId: String) : OperationsNotice
}

data class OperationsUiState(
    val instanceId: InstanceId,
    val loading: Boolean = false,
    val operations: List<RetainedOperation> = emptyList(),
    val pairingHistory: List<PairingHistoryEntry> = emptyList(),
    val recoveringOperationId: String? = null,
    val notice: OperationsNotice? = null,
) {
    val counts: OperationStatusCounts
        get() = operations.fold(OperationStatusCounts()) { counts, operation ->
            when (operation.status) {
                OperationStatus.Pending -> counts.copy(pending = counts.pending + 1)
                OperationStatus.Confirmed -> counts.copy(confirmed = counts.confirmed + 1)
                OperationStatus.Failed -> counts.copy(failed = counts.failed + 1)
                OperationStatus.Unconfirmed ->
                    counts.copy(unconfirmed = counts.unconfirmed + 1)
            }
        }

    val pendingCloudPairings: List<RetainedOperation>
        get() = operations.filter {
            it.kind == RetainedOperationKind.CloudPairing &&
                it.status == OperationStatus.Pending
        }
}
