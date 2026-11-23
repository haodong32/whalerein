package com.whalerein.android.state.support

import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.fixtures.support.FakeClock
import com.whalerein.android.state.fixtures.support.retainedOperation
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/** Retained-operation recovery after the account screen revoked one target. */
class OperationsScopedRecoveryTest {
    private val instance = InstanceId("instance-a")
    private val receiver = DiagnosticReceiver()
    private val hostA = HostId("host-a")
    private val hostB = HostId("host-b")

    private class ScopedPort(
        val inventory: OperationInventory,
        private val authorizedHost: HostId,
    ) : OperationsPort {
        val inventoryCalls = mutableListOf<InstanceId>()
        val authorizationScopes = mutableListOf<OperationScope>()
        val recovered = mutableListOf<String>()

        override suspend fun inventory(instance: InstanceId): Outcome<OperationInventory> {
            inventoryCalls += instance
            return Outcome.Success(inventory)
        }

        override suspend fun authorization(scope: OperationScope): Outcome<OperationAuthorization> =
            Outcome.Success(
                OperationAuthorization(
                    state = if (scope.hostId == authorizedHost) {
                        OperationAuthorizationState.Authorized
                    } else {
                        OperationAuthorizationState.Revoked
                    },
                ),
            ).also { authorizationScopes += scope }

        override suspend fun recover(request: OperationRecoveryRequest): Outcome<RecoveredOperation> {
            recovered += request.operation.operationId
            return Outcome.Success(RecoveredOperation.Confirmed(request.operation.operationId))
        }
    }

    private fun model(port: OperationsPort) = OperationsViewModel(
        instanceId = instance,
        port = port,
        diagnostics = receiver,
        clock = FakeClock(),
    )

    @Test
    @DisplayName("AC-26.operations-surface-only-the-authorized-retained-target")
    fun ac26OperationsSurfaceOnlyTheAuthorizedRetainedTarget() = runTest {
        val operationA = retainedOperation(
            "send-a",
            scope = OperationScope(instance, hostId = hostA),
        )
        val operationB = retainedOperation(
            "send-b",
            scope = OperationScope(instance, hostId = hostB),
        )
        val port = ScopedPort(
            inventory = OperationInventory(operations = listOf(operationA, operationB)),
            authorizedHost = hostA,
        )
        val viewModel = model(port)

        viewModel.refresh()

        assertEquals(listOf("send-a", "send-b"), viewModel.state.value.operations.map { it.operationId })
        assertEquals(2, viewModel.state.value.counts.pending)
        assertTrue(port.recovered.isEmpty())
    }

    @Test
    @DisplayName("AC-26.recovery-after-revocation-refuses-the-revoked-target-and-recovers-the-other")
    fun ac26RecoveryAfterRevocationRefusesTheRevokedTargetAndRecoversTheOther() = runTest {
        val operationA = retainedOperation(
            "send-a",
            scope = OperationScope(instance, hostId = hostA),
        )
        val operationB = retainedOperation(
            "send-b",
            scope = OperationScope(instance, hostId = hostB),
        )
        val port = ScopedPort(
            inventory = OperationInventory(operations = listOf(operationA, operationB)),
            authorizedHost = hostA,
        )
        val viewModel = model(port)
        viewModel.refresh()

        viewModel.recover("send-b")
        val refusal = viewModel.state.value.notice
        assertTrue(refusal is OperationsNotice.RecoveryRefused)
        assertEquals(
            OperationAuthorizationState.Revoked,
            (refusal as OperationsNotice.RecoveryRefused).reason,
        )
        assertEquals(OperationStatus.Pending, viewModel.state.value.operations
            .first { it.operationId == "send-b" }.status)
        assertTrue(port.recovered.isEmpty())

        viewModel.recover("send-a")
        assertEquals(listOf("send-a"), port.recovered)
        assertEquals(
            OperationStatus.Confirmed,
            viewModel.state.value.operations.first { it.operationId == "send-a" }.status,
        )
        assertEquals(
            OperationStatus.Pending,
            viewModel.state.value.operations.first { it.operationId == "send-b" }.status,
        )
        assertEquals(listOf(hostA, hostB).size, port.authorizationScopes.size)
    }
}
