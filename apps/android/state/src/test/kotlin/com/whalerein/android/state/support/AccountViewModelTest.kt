package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.fixtures.support.FakeAccountPort
import com.whalerein.android.state.fixtures.support.FakeClock
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

class AccountViewModelTest {
    private val instance = InstanceId("instance-a")
    private val receiver = DiagnosticReceiver()

    private fun model(port: FakeAccountPort) = AccountViewModel(
        instanceId = instance,
        port = port,
        diagnostics = receiver,
        clock = FakeClock(),
    )

    @Test
    @DisplayName("AC-22.sign-out online and offline report local and pending remote outcomes")
    fun AC_22_sign_out_online_and_offline_report_local_and_pending_remote_outcomes() = runTest {
        val onlinePort = FakeAccountPort()
        val online = model(onlinePort)
        online.load()
        online.requestSignOut()
        assertEquals(AccountScopeKind.Account, online.state.value.confirmation?.scopeKind)
        online.confirm()

        assertEquals(1, onlinePort.signOutCalls)
        assertEquals(AccountConnection.Online, onlinePort.signOutConnections.single())
        assertEquals(CloudAccountState.SignedOut, online.state.value.status?.state)
        assertNull(online.state.value.notice)
        assertEquals(
            AccountReport.SignedOut(
                SignOutReport(localSignedOut = true, remotePending = false, connection = AccountConnection.Online),
            ),
            online.state.value.report,
        )

        val offlinePort = FakeAccountPort(
            statusOutcome = Outcome.Success(
                AccountStatus(
                    state = CloudAccountState.SignedIn,
                    accountId = "account-a",
                    workspace = "workspace-a",
                    connection = AccountConnection.Offline,
                ),
            ),
        )
        val offline = model(offlinePort)
        offline.load()
        offline.requestSignOut()
        offline.confirm()

        assertEquals(AccountConnection.Offline, offlinePort.signOutConnections.single())
        assertEquals(CloudAccountState.PendingRemoteSignOut, offline.state.value.status?.state)
        assertTrue(offline.state.value.pendingRemoteSignOut)
        assertTrue(offline.state.value.notice is AccountNotice.SignOutPendingRemote)
    }

    @Test
    @DisplayName("AC-22.app-sessions revoke one and all with exact scope")
    fun AC_22_app_sessions_revoke_one_and_all_with_exact_scope() = runTest {
        val port = FakeAccountPort(
            sessionsOutcome = Outcome.Success(
                listOf(
                    AppSessionInfo("session-1", "Pixel", 10L, 20L, current = true),
                    AppSessionInfo("session-2", "Tablet", 11L, 21L),
                ),
            ),
        )
        val viewModel = model(port)
        viewModel.load()
        assertEquals(2, viewModel.state.value.appSessions.size)

        viewModel.requestRevokeAppSession("session-2")
        assertEquals(AccountScopeKind.AppSession, viewModel.state.value.confirmation?.scopeKind)
        assertEquals("Tablet", viewModel.state.value.confirmation?.targetLabel)
        viewModel.confirm()

        assertEquals(listOf("session-2"), port.revokeSessionIds)
        assertEquals(listOf("session-1"), viewModel.state.value.appSessions.map { it.sessionId })
        assertEquals(
            RevocationScope.AppSession("session-2"),
            (viewModel.state.value.report as AccountReport.Revoked).report.scope,
        )

        viewModel.requestRevokeAllAppSessions()
        assertEquals(AccountScopeKind.AllAppSessions, viewModel.state.value.confirmation?.scopeKind)
        viewModel.confirm()

        assertEquals(listOf("session-2", null), port.revokeSessionIds)
        assertTrue(viewModel.state.value.appSessions.isEmpty())
        assertEquals(
            RevocationScope.AllAppSessions,
            (viewModel.state.value.report as AccountReport.Revoked).report.scope,
        )
    }

    @Test
    @DisplayName("AC-22.device revocation names the grant or device and reports retention uncertainty")
    fun AC_22_device_revocation_names_the_grant_or_device_and_reports_retention_uncertainty() = runTest {
        val port = FakeAccountPort(
            revokeOutcome = Outcome.Success(
                RevocationReport(
                    scope = RevocationScope.CloudHostGrant(HostId("host-a")),
                    retention = RetentionCertainty.Uncertain,
                    retentionCode = "grant_cache_pending",
                ),
            ),
        )
        val viewModel = model(port)
        viewModel.load()

        viewModel.requestRevokeCloudHostGrant(HostId("host-a"))
        assertEquals(AccountScopeKind.CloudHostGrant, viewModel.state.value.confirmation?.scopeKind)
        viewModel.confirm()
        assertEquals(listOf(HostId("host-a")), port.revokedHostIds)
        val grantNotice = viewModel.state.value.notice
        assertTrue(grantNotice is AccountNotice.RevocationRetentionUncertain)
        assertEquals(
            RevocationScope.CloudHostGrant(HostId("host-a")),
            (grantNotice as AccountNotice.RevocationRetentionUncertain).scope,
        )
        assertEquals("grant_cache_pending", grantNotice.code)

        val devicePort = FakeAccountPort(
            revokeOutcome = Outcome.Success(
                RevocationReport(
                    scope = RevocationScope.StandaloneDevice("device-1"),
                    retention = RetentionCertainty.Uncertain,
                    retentionCode = "device_retention_pending",
                ),
            ),
        )
        val deviceModel = model(devicePort)
        deviceModel.load()
        deviceModel.requestRevokeStandaloneDevice("device-1")
        assertEquals(AccountScopeKind.StandaloneDevice, deviceModel.state.value.confirmation?.scopeKind)
        deviceModel.confirm()
        assertEquals(listOf("device-1"), devicePort.revokedDeviceIds)
        assertTrue(deviceModel.state.value.notice is AccountNotice.RevocationRetentionUncertain)
    }

    @Test
    @DisplayName("AC-22.forget ordinary local-only and cancelled actions are distinct")
    fun AC_22_forget_ordinary_local_only_and_cancelled_actions_are_distinct() = runTest {
        val port = FakeAccountPort()
        val viewModel = model(port)
        viewModel.load()

        viewModel.requestForget(ForgetMode.Ordinary)
        assertEquals(AccountScopeKind.LocalIdentity, viewModel.state.value.confirmation?.scopeKind)
        viewModel.confirm()

        assertEquals(listOf(ForgetMode.Ordinary), port.forgetModes)
        assertEquals(CloudAccountState.SignedOut, viewModel.state.value.status?.state)
        assertEquals(
            AccountReport.Forgot(
                ForgetReport(ForgetMode.Ordinary, forgotten = true, remoteAuthorityRetained = false),
            ),
            viewModel.state.value.report,
        )

        val localOnlyPort = FakeAccountPort(
            forgetOutcome = Outcome.Success(
                ForgetReport(ForgetMode.LocalOnly, forgotten = true, remoteAuthorityRetained = true),
            ),
        )
        val localOnly = model(localOnlyPort)
        localOnly.load()
        localOnly.requestForget(ForgetMode.LocalOnly)
        assertEquals(
            AccountScopeKind.LocalIdentityWithRemoteAuthority,
            localOnly.state.value.confirmation?.scopeKind,
        )
        localOnly.confirm()

        assertEquals(listOf(ForgetMode.LocalOnly), localOnlyPort.forgetModes)
        assertTrue(localOnly.state.value.notice is AccountNotice.ForgottenLocalOnly)
        assertTrue(
            (localOnly.state.value.notice as AccountNotice.ForgottenLocalOnly)
                .remoteAuthorityRetained,
        )

        val cancelPort = FakeAccountPort()
        val cancelled = model(cancelPort)
        cancelled.load()
        cancelled.requestRevokeAllAppSessions()
        cancelled.cancelConfirmation()
        assertNull(cancelled.state.value.confirmation)
        assertTrue(cancelled.state.value.notice is AccountNotice.ActionCancelled)
        assertTrue(cancelPort.revokeSessionIds.isEmpty())
        cancelled.confirm()
        assertTrue(cancelPort.revokeSessionIds.isEmpty())
    }

    @Test
    @DisplayName("AC-22.forget ordinary precondition failure leaves local identity intact")
    fun AC_22_forget_ordinary_precondition_failure_leaves_local_identity_intact() = runTest {
        val port = FakeAccountPort(
            forgetOutcome = Outcome.Failure(
                CoreFailure("authorization_required", "Current authorization is required."),
            ),
        )
        val viewModel = model(port)
        viewModel.load()
        viewModel.requestForget(ForgetMode.Ordinary)
        viewModel.confirm()

        val notice = viewModel.state.value.notice
        assertTrue(notice is AccountNotice.ActionFailed)
        assertEquals(
            "authorization_required",
            (notice as AccountNotice.ActionFailed).failure.code,
        )
        assertEquals(CloudAccountState.SignedIn, viewModel.state.value.status?.state)
        assertFalse(viewModel.state.value.pendingRemoteSignOut)
    }
}
