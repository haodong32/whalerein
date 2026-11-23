package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.fixtures.support.FakeAccountPort
import com.whalerein.android.state.fixtures.support.FakeClock
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/** Account screen behavior with Core-shaped Host grants and Standalone devices. */
class AccountScopedRevocationTest {
    private val instance = InstanceId("instance-a")
    private val receiver = DiagnosticReceiver()
    private val hostA = HostId("host-a")
    private val hostB = HostId("host-b")

    private fun model(port: FakeAccountPort) = AccountViewModel(
        instanceId = instance,
        port = port,
        diagnostics = receiver,
        clock = FakeClock(),
    )

    private fun targets() = AccountRevocableTargets(
        cloudHostGrants = listOf(hostA, hostB),
        standaloneDevices = listOf("device-a", "device-b"),
    )

    @Test
    @DisplayName("AC-26.load-populates-grants-and-devices-from-the-account-port")
    fun ac26LoadPopulatesGrantsAndDevicesFromTheAccountPort() = runTest {
        val port = FakeAccountPort(targetsOutcome = Outcome.Success(targets()))
        val viewModel = model(port)

        viewModel.load()

        assertEquals(listOf(hostA, hostB), viewModel.state.value.targets.cloudHostGrants)
        assertEquals(
            listOf("device-a", "device-b"),
            viewModel.state.value.targets.standaloneDevices,
        )
        assertEquals(1, port.listTargetsCalls)
        assertNull(viewModel.state.value.notice)
    }

    @Test
    @DisplayName("AC-26.load-failure-is-reported-without-inventing-targets")
    fun ac26LoadFailureIsReportedWithoutInventingTargets() = runTest {
        val port = FakeAccountPort(
            targetsOutcome = Outcome.Failure(
                CoreFailure("targets_unavailable", "Targets are unavailable."),
            ),
        )
        val viewModel = model(port)

        viewModel.load()

        assertTrue(viewModel.state.value.targets.cloudHostGrants.isEmpty())
        assertTrue(viewModel.state.value.targets.standaloneDevices.isEmpty())
        val notice = viewModel.state.value.notice
        assertTrue(notice is AccountNotice.TargetsFailed)
        assertEquals("targets_unavailable", (notice as AccountNotice.TargetsFailed).failure.code)
    }

    @Test
    @DisplayName("AC-26.revoke-selected-host-leaves-another-host-and-device-usable")
    fun ac26RevokeSelectedHostLeavesAnotherHostAndDeviceUsable() = runTest {
        val port = FakeAccountPort(targetsOutcome = Outcome.Success(targets()))
        val viewModel = model(port)
        viewModel.load()

        viewModel.requestRevokeCloudHostGrant(hostA)
        assertEquals(AccountScopeKind.CloudHostGrant, viewModel.state.value.confirmation?.scopeKind)
        assertEquals("host-a", viewModel.state.value.confirmation?.targetLabel)
        viewModel.confirm()

        assertEquals(listOf(hostA), port.revokedHostIds)
        assertEquals(0, port.currentDeviceRevocations)
        assertTrue(port.revokedDeviceIds.isEmpty())
        assertEquals(
            RevocationScope.CloudHostGrant(hostA),
            (viewModel.state.value.report as AccountReport.Revoked).report.scope,
        )
        assertEquals(listOf(hostB), viewModel.state.value.targets.cloudHostGrants)
        assertEquals(
            listOf("device-a", "device-b"),
            viewModel.state.value.targets.standaloneDevices,
        )
    }

    @Test
    @DisplayName("AC-26.revoke-selected-device-leaves-another-device-and-host-usable")
    fun ac26RevokeSelectedDeviceLeavesAnotherDeviceAndHostUsable() = runTest {
        val port = FakeAccountPort(targetsOutcome = Outcome.Success(targets()))
        val viewModel = model(port)
        viewModel.load()

        viewModel.requestRevokeStandaloneDevice("device-a")
        assertEquals(AccountScopeKind.StandaloneDevice, viewModel.state.value.confirmation?.scopeKind)
        viewModel.confirm()

        assertEquals(listOf("device-a"), port.revokedDeviceIds)
        assertEquals(0, port.currentDeviceRevocations)
        assertTrue(port.revokedHostIds.isEmpty())
        assertEquals(
            RevocationScope.StandaloneDevice("device-a"),
            (viewModel.state.value.report as AccountReport.Revoked).report.scope,
        )
        assertEquals(listOf("device-b"), viewModel.state.value.targets.standaloneDevices)
        assertEquals(listOf(hostA, hostB), viewModel.state.value.targets.cloudHostGrants)
    }

    @Test
    @DisplayName("AC-26.current-device-revocation-is-not-a-selected-target")
    fun ac26CurrentDeviceRevocationIsNotASelectedTarget() = runTest {
        val port = FakeAccountPort(targetsOutcome = Outcome.Success(targets()))
        val viewModel = model(port)
        viewModel.load()

        viewModel.requestRevokeCurrentDevice()
        assertEquals(AccountScopeKind.CurrentDevice, viewModel.state.value.confirmation?.scopeKind)
        viewModel.confirm()

        assertEquals(1, port.currentDeviceRevocations)
        assertTrue(port.revokedHostIds.isEmpty())
        assertTrue(port.revokedDeviceIds.isEmpty())
        assertEquals(
            RevocationScope.CurrentDevice,
            (viewModel.state.value.report as AccountReport.Revoked).report.scope,
        )
        assertTrue(viewModel.state.value.targets.cloudHostGrants.isEmpty())
        assertTrue(viewModel.state.value.targets.standaloneDevices.isEmpty())
    }

    @Test
    @DisplayName("AC-26.local-only-forget-keeps-the-selected-core-mode")
    fun ac26LocalOnlyForgetKeepsTheSelectedCoreMode() = runTest {
        val port = FakeAccountPort(
            targetsOutcome = Outcome.Success(targets()),
            forgetOutcome = Outcome.Success(
                ForgetReport(ForgetMode.LocalOnly, forgotten = true, remoteAuthorityRetained = true),
            ),
        )
        val viewModel = model(port)
        viewModel.load()

        viewModel.requestForget(ForgetMode.LocalOnly)
        assertEquals(
            AccountScopeKind.LocalIdentityWithRemoteAuthority,
            viewModel.state.value.confirmation?.scopeKind,
        )
        viewModel.confirm()

        assertEquals(listOf(ForgetMode.LocalOnly), port.forgetModes)
        assertTrue(viewModel.state.value.targets.cloudHostGrants.isEmpty())
        assertEquals(
            AccountReport.Forgot(
                ForgetReport(ForgetMode.LocalOnly, forgotten = true, remoteAuthorityRetained = true),
            ),
            viewModel.state.value.report,
        )
    }

    @Test
    @DisplayName("AC-26.cancelled-target-revocation-keeps-every-target-usable")
    fun ac26CancelledTargetRevocationKeepsEveryTargetUsable() = runTest {
        val port = FakeAccountPort(targetsOutcome = Outcome.Success(targets()))
        val viewModel = model(port)
        viewModel.load()

        viewModel.requestRevokeCloudHostGrant(hostA)
        viewModel.cancelConfirmation()
        viewModel.confirm()

        assertTrue(port.revokedHostIds.isEmpty())
        assertEquals(listOf(hostA, hostB), viewModel.state.value.targets.cloudHostGrants)
        assertEquals(
            listOf("device-a", "device-b"),
            viewModel.state.value.targets.standaloneDevices,
        )
        assertTrue(viewModel.state.value.notice is AccountNotice.ActionCancelled)
    }
}
