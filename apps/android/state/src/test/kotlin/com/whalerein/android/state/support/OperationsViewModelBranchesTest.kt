package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.fixtures.support.FakeClock
import com.whalerein.android.state.fixtures.support.FakeOperationsPort
import com.whalerein.android.state.fixtures.support.pairingHistory
import com.whalerein.android.state.fixtures.support.retainedOperation
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/** Additional branch coverage for operations inventory and recovery. */
@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class OperationsViewModelBranchesTest {
    private val instance = InstanceId("instance-a")
    private val operation = retainedOperation("send-1")

    private fun model(port: FakeOperationsPort) = OperationsViewModel(
        instanceId = instance,
        port = port,
        diagnostics = DiagnosticReceiver(),
        clock = FakeClock(),
    )

    @Test
    @DisplayName("AC-21.inventory load failures keep prior retained operations")
    fun AC_21_inventory_load_failures_keep_prior_retained_operations() = runTest {
        for (outcome in listOf(
            Outcome.Failure(CoreFailure("inventory_failed", "no")),
            Outcome.Unconfirmed(CoreFailure("inventory_unknown", "no")),
        )) {
            val port = FakeOperationsPort(
                inventoryOutcome = Outcome.Success(OperationInventory(operations = listOf(operation))),
            )
            val viewModel = model(port)
            viewModel.refresh()
            port.inventoryOutcome = outcome
            viewModel.refresh()
            assertEquals(1, viewModel.state.value.operations.size)
            assertTrue(viewModel.state.value.notice is OperationsNotice.LoadFailed)
        }

        val cancelledPort = FakeOperationsPort(
            inventoryOutcome = Outcome.Success(OperationInventory(operations = listOf(operation))),
        )
        val cancelled = model(cancelledPort)
        cancelled.refresh()
        cancelledPort.inventoryOutcome = Outcome.Cancelled
        cancelled.refresh()
        assertEquals(1, cancelled.state.value.operations.size)
        assertNull(cancelled.state.value.notice)
    }

    @Test
    @DisplayName("AC-21.recover-paths unknown and unavailable authorization outcomes are refused")
    fun AC_21_recover_paths_unknown_and_unavailable_authorization_outcomes_are_refused() = runTest {
        val port = FakeOperationsPort(
            inventoryOutcome = Outcome.Success(OperationInventory(operations = listOf(operation))),
            authorizationOutcome = Outcome.Success(OperationAuthorization(OperationAuthorizationState.Authorized)),
        )
        val viewModel = model(port)
        viewModel.refresh()

        viewModel.recover("missing")
        assertTrue(viewModel.state.value.notice is OperationsNotice.RecoveryFailed)
        assertEquals(0, port.recoveryRequests.size)

        for (outcome in listOf(
            Outcome.Failure(CoreFailure("auth_unavailable", "no")),
            Outcome.Unconfirmed(CoreFailure("auth_unknown", "no")),
            Outcome.Cancelled,
        )) {
            port.authorizationOutcome = outcome
            viewModel.recover("send-1")
            assertTrue(
                viewModel.state.value.notice is OperationsNotice.RecoveryFailed ||
                    viewModel.state.value.notice is OperationsNotice.RecoveryCancelled,
            )
        }
        assertEquals(0, port.recoveryRequests.size)
        assertEquals(OperationStatus.Pending, viewModel.state.value.operations.single().status)

        viewModel.clearNotice()
        assertNull(viewModel.state.value.notice)
    }

    @Test
    @DisplayName("AC-21.retained recovery outcomes map to unconfirmed and cancelled states")
    fun AC_21_retained_recovery_outcomes_map_to_unconfirmed_and_cancelled_states() = runTest {
        val port = FakeOperationsPort(
            inventoryOutcome = Outcome.Success(OperationInventory(operations = listOf(operation))),
        )
        val viewModel = model(port)
        viewModel.refresh()

        port.recoverOutcomes.add(
            Outcome.Success(
                RecoveredOperation.Unconfirmed(
                    "send-1",
                    CoreFailure("still_unknown", "no"),
                ),
            ),
        )
        viewModel.recover("send-1")
        assertEquals(OperationStatus.Unconfirmed, viewModel.state.value.operations.single().status)
        assertTrue(viewModel.state.value.notice is OperationsNotice.RecoveryUnconfirmed)

        port.recoverOutcomes.add(Outcome.Unconfirmed(CoreFailure("unknown", "no")))
        viewModel.recover("send-1")
        assertEquals(OperationStatus.Unconfirmed, viewModel.state.value.operations.single().status)

        port.recoverOutcomes.add(Outcome.Cancelled)
        viewModel.recover("send-1")
        assertTrue(viewModel.state.value.notice is OperationsNotice.RecoveryCancelled)
    }

    @Test
    @DisplayName("AC-21.retained concurrent recovery ignores a second request for the same operation")
    fun AC_21_retained_concurrent_recovery_ignores_a_second_request_for_the_same_operation() = runTest {
        val port = FakeOperationsPort(
            inventoryOutcome = Outcome.Success(OperationInventory(operations = listOf(operation))),
        )
        val gate = CompletableDeferred<Unit>()
        port.duringRecover = { gate.await() }
        port.recoverOutcomes.add(Outcome.Success(RecoveredOperation.Confirmed("send-1")))
        val viewModel = model(port)
        viewModel.refresh()

        val first = launch { viewModel.recover("send-1") }
        runCurrent()
        viewModel.recover("send-1")
        assertEquals(1, port.recoveryRequests.size)
        assertEquals("send-1", viewModel.state.value.recoveringOperationId)

        gate.complete(Unit)
        first.join()
        assertNull(viewModel.state.value.recoveringOperationId)
        assertEquals(OperationStatus.Confirmed, viewModel.state.value.operations.single().status)
    }

    @Test
    @DisplayName("AC-21.inventory operation models expose scope status counts and history")
    fun AC_21_inventory_operation_models_expose_scope_status_counts_and_history() {
        val scope = OperationScope(
            instanceId = instance,
            accountId = "account-a",
            hostId = HostId("host-a"),
            sessionId = SessionId("session-a"),
            authorizationRevision = 3L,
            callbackGeneration = 4L,
        )
        assertEquals(instance, scope.instanceId)
        assertEquals("account-a", scope.accountId)
        assertEquals(HostId("host-a"), scope.hostId)
        assertEquals(SessionId("session-a"), scope.sessionId)
        assertEquals(3L, scope.authorizationRevision)
        assertEquals(4L, scope.callbackGeneration)
        assertEquals(OperationScope(instance), scope.copy(callbackGeneration = 0L, accountId = null, hostId = null, sessionId = null, authorizationRevision = 0L))

        val retained = retainedOperation("op", scope = scope, label = "label")
        assertEquals("op", retained.operationId)
        assertEquals(RetainedOperationKind.Send, retained.kind)
        assertEquals(OperationStatus.Pending, retained.status)
        assertEquals(scope, retained.scope)
        assertEquals(1_000L, retained.createdAtEpochMillis)
        assertEquals(1_000L, retained.updatedAtEpochMillis)
        assertEquals("label", retained.label)
        assertNull(retained.attemptId)
        assertEquals(OperationStatus.Confirmed, retained.copy(status = OperationStatus.Confirmed).status)

        val history = pairingHistory("pair", RetainedOperationKind.CloudPairing)
        assertEquals("pair", history.pairingId)
        assertEquals(RetainedOperationKind.CloudPairing, history.kind)
        assertEquals(OperationStatus.Confirmed, history.outcome)
        assertEquals(scope.instanceId, history.scope.instanceId)
        assertEquals(900L, history.recordedAtEpochMillis)
        assertEquals("host-a", history.hostLabel)

        val empty = OperationInventory()
        assertTrue(empty.operations.isEmpty())
        assertTrue(empty.pairingHistory.isEmpty())
        assertEquals(1, OperationInventory(operations = listOf(retained)).operations.size)

        val authorization = OperationAuthorization(OperationAuthorizationState.Authorized)
        assertTrue(authorization.authorized)
        assertFalseAuthorization(OperationAuthorization(OperationAuthorizationState.Revoked))
        assertEquals("account-a", OperationAuthorization(OperationAuthorizationState.Authorized, "account-a").accountId)
        assertEquals(3L, OperationAuthorization(OperationAuthorizationState.Authorized, authorizationRevision = 3L).authorizationRevision)
        assertEquals(4L, OperationAuthorization(OperationAuthorizationState.Authorized, callbackGeneration = 4L).callbackGeneration)

        val request = OperationRecoveryRequest(retained, authorization)
        assertEquals(retained, request.operation)
        assertEquals(authorization, request.authorization)
        assertEquals("op", RecoveredOperation.Confirmed("op").operationId)
        assertEquals("detail", RecoveredOperation.Confirmed("op", "detail").detailCode)
        assertEquals(
            "op",
            RecoveredOperation.Unconfirmed("op", CoreFailure("code", "message")).operationId,
        )

        val counts = OperationStatusCounts(pending = 1, confirmed = 2, failed = 3, unconfirmed = 4)
        assertEquals(1, counts.pending)
        assertEquals(2, counts.confirmed)
        assertEquals(3, counts.failed)
        assertEquals(4, counts.unconfirmed)

        val notices: List<Any> = listOf(
            OperationsNotice.LoadFailed(CoreFailure("code", "message")),
            OperationsNotice.RecoveryRefused("op", OperationAuthorizationState.Revoked),
            OperationsNotice.RecoveryFailed("op", CoreFailure("code", "message")),
            OperationsNotice.RecoveryUnconfirmed("op", CoreFailure("code", "message")),
            OperationsNotice.RecoveryConfirmed("op"),
            OperationsNotice.RecoveryCancelled("op"),
        )
        notices.forEach { assertEquals(it, it) }

        val state = OperationsUiState(
            instanceId = instance,
            loading = false,
            operations = listOf(
                retained.copy(status = OperationStatus.Pending),
                retained.copy(operationId = "confirmed", status = OperationStatus.Confirmed),
                retained.copy(operationId = "failed", status = OperationStatus.Failed),
                retained.copy(operationId = "unconfirmed", status = OperationStatus.Unconfirmed),
                retained.copy(
                    operationId = "cloud",
                    kind = RetainedOperationKind.CloudPairing,
                    status = OperationStatus.Pending,
                ),
            ),
            pairingHistory = listOf(history),
            notice = notices.first() as OperationsNotice,
        )
        assertEquals(OperationStatusCounts(2, 1, 1, 1), state.counts)
        assertEquals(listOf("cloud"), state.pendingCloudPairings.map { it.operationId })
        assertTrue(state.copy(pairingHistory = emptyList()).pairingHistory.isEmpty())
    }

    private fun assertFalseAuthorization(authorization: OperationAuthorization) {
        assertEquals(false, authorization.authorized)
    }
}
