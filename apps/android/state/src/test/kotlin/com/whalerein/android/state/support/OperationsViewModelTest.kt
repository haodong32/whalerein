package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.fixtures.support.FakeClock
import com.whalerein.android.state.fixtures.support.FakeOperationsPort
import com.whalerein.android.state.fixtures.support.pairingHistory
import com.whalerein.android.state.fixtures.support.retainedOperation
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

class OperationsViewModelTest {
    private val instance = InstanceId("instance-a")
    private val receiver = DiagnosticReceiver()

    private fun model(port: FakeOperationsPort) = OperationsViewModel(
        instanceId = instance,
        port = port,
        diagnostics = receiver,
        clock = FakeClock(),
    )

    @Test
    @DisplayName("AC-21.inventory distinguishes retained operations and multiple Cloud pairings")
    fun AC_21_inventory_distinguishes_retained_operations_and_multiple_Cloud_pairings() = runTest {
        val port = FakeOperationsPort(
            inventoryOutcome = Outcome.Success(
                OperationInventory(
                    operations = listOf(
                        retainedOperation("send-pending", status = OperationStatus.Pending),
                        retainedOperation("send-confirmed", status = OperationStatus.Confirmed),
                        retainedOperation("send-failed", status = OperationStatus.Failed),
                        retainedOperation(
                            "send-unconfirmed",
                            status = OperationStatus.Unconfirmed,
                        ),
                        retainedOperation(
                            "control-1",
                            kind = RetainedOperationKind.Control,
                            status = OperationStatus.Pending,
                        ),
                        retainedOperation(
                            "cloud-a",
                            kind = RetainedOperationKind.CloudPairing,
                            status = OperationStatus.Pending,
                            scope = OperationScope(instance, hostId = com.whalerein.android.state.core.HostId("host-a")),
                        ),
                        retainedOperation(
                            "cloud-b",
                            kind = RetainedOperationKind.CloudPairing,
                            status = OperationStatus.Pending,
                            scope = OperationScope(instance, hostId = com.whalerein.android.state.core.HostId("host-b")),
                        ),
                        retainedOperation(
                            "standalone-1",
                            kind = RetainedOperationKind.StandalonePairing,
                            status = OperationStatus.Pending,
                        ),
                    ),
                    pairingHistory = listOf(
                        pairingHistory("history-cloud", RetainedOperationKind.CloudPairing),
                        pairingHistory("history-standalone"),
                    ),
                ),
            ),
        )
        val viewModel = model(port)

        viewModel.refresh()

        val snapshot = viewModel.state.value
        assertEquals(OperationStatusCounts(pending = 5, confirmed = 1, failed = 1, unconfirmed = 1), snapshot.counts)
        assertEquals(listOf("cloud-a", "cloud-b"), snapshot.pendingCloudPairings.map { it.operationId })
        assertEquals(2, snapshot.pairingHistory.size)
        assertEquals(2, snapshot.pendingCloudPairings.size)
        assertEquals(0, port.authorizationScopes.size)
        assertEquals(0, port.recoveryRequests.size)
        assertTrue(receiver.snapshot().any { it.event == DiagnosticEvents.OPERATIONS_INVENTORY_LOADED })
    }

    @Test
    @DisplayName("AC-21.recover-paths use the original operation id for every operation kind")
    fun AC_21_recover_paths_use_the_original_operation_id_for_every_operation_kind() = runTest {
        val kinds = listOf(
            RetainedOperationKind.Send,
            RetainedOperationKind.Control,
            RetainedOperationKind.CloudPairing,
            RetainedOperationKind.StandalonePairing,
        )
        val operations = kinds.mapIndexed { index, kind ->
            retainedOperation("operation-$index", kind = kind)
        }
        val port = FakeOperationsPort(
            inventoryOutcome = Outcome.Success(OperationInventory(operations = operations)),
        )
        val viewModel = model(port)
        viewModel.refresh()

        for (operation in operations) {
            port.recoverOutcomes.add(
                Outcome.Success(RecoveredOperation.Confirmed(operation.operationId)),
            )
            viewModel.recover(operation.operationId)
            assertEquals(OperationStatus.Confirmed, viewModel.state.value.operations
                .first { it.operationId == operation.operationId }.status)
        }

        assertEquals(operations.map { it.operationId }, port.recoveryRequests.map { it.operation.operationId })
        assertEquals(operations.map { it.kind }, port.recoveryRequests.map { it.operation.kind })
    }

    @Test
    @DisplayName("AC-21.wrong-scope refuses recovery without touching the operation port")
    fun AC_21_wrong_scope_refuses_recovery_without_touching_the_operation_port() = runTest {
        val operation = retainedOperation("send-1")
        val states = listOf(
            OperationAuthorizationState.Revoked,
            OperationAuthorizationState.Superseded,
            OperationAuthorizationState.ForeignAccount,
            OperationAuthorizationState.OldCallback,
            OperationAuthorizationState.Expired,
        )
        for (state in states) {
            val port = FakeOperationsPort(
                inventoryOutcome = Outcome.Success(OperationInventory(operations = listOf(operation))),
                authorizationOutcome = Outcome.Success(OperationAuthorization(state)),
            )
            val viewModel = model(port)
            viewModel.refresh()

            viewModel.recover("send-1")

            assertEquals(0, port.recoveryRequests.size)
            val notice = viewModel.state.value.notice
            assertTrue(notice is OperationsNotice.RecoveryRefused)
            assertEquals(state, (notice as OperationsNotice.RecoveryRefused).reason)
            assertEquals(OperationStatus.Pending, viewModel.state.value.operations.single().status)
        }
    }

    @Test
    @DisplayName("AC-21.retained survives reconnect and restart without a new submission")
    fun AC_21_retained_survives_reconnect_and_restart_without_a_new_submission() = runTest {
        val operation = retainedOperation("send-retained")
        val port = FakeOperationsPort(
            inventoryOutcome = Outcome.Success(OperationInventory(operations = listOf(operation))),
        )
        val firstProcess = model(port)
        firstProcess.refresh()
        assertEquals(1, firstProcess.state.value.operations.size)

        // A new ViewModel over the same Core inventory represents process restart.
        val secondProcess = model(port)
        secondProcess.refresh()
        assertEquals(1, secondProcess.state.value.operations.size)
        assertEquals(0, port.recoveryRequests.size)

        secondProcess.recover("send-retained")
        assertEquals(1, port.recoveryRequests.size)
        assertEquals("send-retained", port.recoveryRequests.single().operation.operationId)
        assertEquals(OperationStatus.Confirmed, secondProcess.state.value.operations.single().status)

        // Reconnect refreshes the same inventory and still performs no recovery.
        firstProcess.refresh()
        assertEquals(3, port.inventoryCalls)
        assertEquals(1, port.recoveryRequests.size)
        assertNull(firstProcess.state.value.notice)
        assertNotNull(receiver.snapshot().firstOrNull())
    }

    @Test
    @DisplayName("AC-21.retained reports a failed recovery without losing the retained identity")
    fun AC_21_retained_reports_a_failed_recovery_without_losing_the_retained_identity() = runTest {
        val operation = retainedOperation("send-failure")
        val port = FakeOperationsPort(
            inventoryOutcome = Outcome.Success(OperationInventory(operations = listOf(operation))),
        )
        port.recoverOutcomes.add(
            Outcome.Failure(CoreFailure("recovery_rejected", "Recovery was rejected.")),
        )
        val viewModel = model(port)
        viewModel.refresh()

        viewModel.recover("send-failure")

        assertEquals(OperationStatus.Failed, viewModel.state.value.operations.single().status)
        assertTrue(viewModel.state.value.notice is OperationsNotice.RecoveryFailed)
    }
}
