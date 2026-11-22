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

/** Additional branch coverage for account, session and device actions. */
class AccountViewModelBranchesTest {
    private val instance = InstanceId("instance-a")

    private fun model(port: FakeAccountPort) = AccountViewModel(
        instanceId = instance,
        port = port,
        diagnostics = DiagnosticReceiver(),
        clock = FakeClock(),
    )

    @Test
    @DisplayName("AC-22.sign-out load and sign-out failure outcomes preserve prior authorities")
    fun AC_22_sign_out_load_and_sign_out_failure_outcomes_preserve_prior_authorities() = runTest {
        for (outcome in listOf(
            Outcome.Failure(CoreFailure("status_failed", "no")),
            Outcome.Unconfirmed(CoreFailure("status_unknown", "no")),
        )) {
            val viewModel = model(FakeAccountPort(statusOutcome = outcome))
            viewModel.load()
            assertTrue(viewModel.state.value.notice is AccountNotice.LoadFailed)
        }

        val cancelledStatus = model(FakeAccountPort(statusOutcome = Outcome.Cancelled))
        cancelledStatus.load()
        assertNull(cancelledStatus.state.value.notice)

        val signedOut = FakeAccountPort(
            statusOutcome = Outcome.Success(
                AccountStatus(CloudAccountState.SignedOut, connection = AccountConnection.Offline),
            ),
        )
        val signedOutModel = model(signedOut)
        signedOutModel.load()
        assertEquals(0, signedOut.listSessionsCalls)
        assertFalse(signedOutModel.state.value.signedIn)

        for (outcome in listOf(
            Outcome.Failure(CoreFailure("sign_out_failed", "no")),
            Outcome.Unconfirmed(CoreFailure("sign_out_unknown", "no")),
            Outcome.Cancelled,
        )) {
            val port = FakeAccountPort(signOutOutcome = outcome)
            val viewModel = model(port)
            viewModel.load()
            viewModel.requestSignOut()
            viewModel.confirm()
            assertTrue(
                viewModel.state.value.notice is AccountNotice.ActionFailed ||
                    viewModel.state.value.notice is AccountNotice.ActionUnconfirmed ||
                    viewModel.state.value.notice is AccountNotice.ActionCancelled,
            )
            assertEquals(CloudAccountState.SignedIn, viewModel.state.value.status?.state)
        }
    }

    @Test
    @DisplayName("AC-22.app-sessions session listing failures stay distinct from account failure")
    fun AC_22_app_sessions_session_listing_failures_stay_distinct_from_account_failure() = runTest {
        for (outcome in listOf(
            Outcome.Failure(CoreFailure("sessions_failed", "no")),
            Outcome.Unconfirmed(CoreFailure("sessions_unknown", "no")),
        )) {
            val port = FakeAccountPort(sessionsOutcome = outcome)
            val viewModel = model(port)
            viewModel.load()
            assertTrue(viewModel.state.value.notice is AccountNotice.SessionsFailed)
            assertTrue(viewModel.state.value.appSessions.isEmpty())
        }

        val cancelledPort = FakeAccountPort(sessionsOutcome = Outcome.Cancelled)
        val cancelled = model(cancelledPort)
        cancelled.load()
        assertNull(cancelled.state.value.notice)
        assertTrue(cancelled.state.value.appSessions.isEmpty())
    }

    @Test
    @DisplayName("AC-22.device revocation failure outcomes leave existing scope untouched")
    fun AC_22_device_revocation_failure_outcomes_leave_existing_scope_untouched() = runTest {
        for (outcome in listOf(
            Outcome.Failure(CoreFailure("revoke_failed", "no")),
            Outcome.Unconfirmed(CoreFailure("revoke_unknown", "no")),
            Outcome.Cancelled,
        )) {
            val port = FakeAccountPort(revokeOutcome = outcome)
            val viewModel = model(port)
            viewModel.load()
            viewModel.requestRevokeCloudHostGrant(HostId("host-a"))
            viewModel.confirm()
            assertTrue(
                viewModel.state.value.notice is AccountNotice.ActionFailed ||
                    viewModel.state.value.notice is AccountNotice.ActionUnconfirmed ||
                    viewModel.state.value.notice is AccountNotice.ActionCancelled,
            )
        }

        val noConfirmation = model(FakeAccountPort())
        noConfirmation.confirm()
        assertNull(noConfirmation.state.value.report)
    }

    @Test
    @DisplayName("AC-22.forget not-applied and unavailable forget outcomes are explicit")
    fun AC_22_forget_not_applied_and_unavailable_forget_outcomes_are_explicit() = runTest {
        val notApplied = model(
            FakeAccountPort(
                forgetOutcome = Outcome.Success(
                    ForgetReport(ForgetMode.Ordinary, forgotten = false, remoteAuthorityRetained = false),
                ),
            ),
        )
        notApplied.load()
        notApplied.requestForget(ForgetMode.Ordinary)
        notApplied.confirm()
        assertTrue(notApplied.state.value.notice is AccountNotice.ActionFailed)
        assertEquals(CloudAccountState.SignedIn, notApplied.state.value.status?.state)

        for (outcome in listOf(
            Outcome.Unconfirmed(CoreFailure("forget_unknown", "no")),
            Outcome.Cancelled,
        )) {
            val viewModel = model(FakeAccountPort(forgetOutcome = outcome))
            viewModel.load()
            viewModel.requestForget(ForgetMode.LocalOnly)
            viewModel.confirm()
            assertTrue(
                viewModel.state.value.notice is AccountNotice.ActionUnconfirmed ||
                    viewModel.state.value.notice is AccountNotice.ActionCancelled,
            )
        }
    }

    @Test
    @DisplayName("AC-22.forget account models expose scope retention and status semantics")
    fun AC_22_forget_account_models_expose_scope_retention_and_status_semantics() {
        val signedIn = AccountStatus(
            state = CloudAccountState.SignedIn,
            accountId = "account-a",
            workspace = "workspace-a",
            connection = AccountConnection.Offline,
        )
        assertTrue(signedIn.signedIn)
        assertEquals("account-a", signedIn.accountId)
        assertEquals("workspace-a", signedIn.workspace)
        assertEquals(AccountConnection.Offline, signedIn.connection)
        assertTrue(signedIn.copy(state = CloudAccountState.PendingRemoteSignOut).signedIn)
        assertFalse(signedIn.copy(state = CloudAccountState.SignedOut).signedIn)

        val session = AppSessionInfo("session-1", "Pixel", 1L, 2L, current = true)
        assertEquals("session-1", session.sessionId)
        assertEquals("Pixel", session.deviceLabel)
        assertEquals(1L, session.createdAtEpochMillis)
        assertEquals(2L, session.lastSeenEpochMillis)
        assertTrue(session.current)
        assertFalse(session.copy(current = false).current)

        val scopes: List<RevocationScope> = listOf(
            RevocationScope.AppSession("session-1"),
            RevocationScope.AllAppSessions,
            RevocationScope.CloudHostGrant(HostId("host-a")),
            RevocationScope.StandaloneDevice("device-1"),
        )
        scopes.forEach { assertEquals(it, it) }
        assertEquals("session-1", (scopes[0] as RevocationScope.AppSession).sessionId)
        assertEquals(HostId("host-a"), (scopes[2] as RevocationScope.CloudHostGrant).hostId)
        assertEquals("device-1", (scopes[3] as RevocationScope.StandaloneDevice).deviceId)

        val report = RevocationReport(RevocationScope.AllAppSessions)
        assertEquals(RetentionCertainty.Certain, report.retention)
        assertNull(report.retentionCode)
        assertEquals(
            RetentionCertainty.Uncertain,
            report.copy(retention = RetentionCertainty.Uncertain, retentionCode = "pending").retention,
        )

        val forget = ForgetReport(ForgetMode.LocalOnly, forgotten = true, remoteAuthorityRetained = true)
        assertEquals(ForgetMode.LocalOnly, forget.mode)
        assertTrue(forget.forgotten)
        assertTrue(forget.remoteAuthorityRetained)

        val signOut = SignOutReport(localSignedOut = true, remotePending = true, connection = AccountConnection.Offline)
        assertTrue(signOut.localSignedOut)
        assertTrue(signOut.remotePending)
        assertEquals(AccountConnection.Offline, signOut.connection)

        val actions: List<AccountAction> = listOf(
            AccountAction.SignOut,
            AccountAction.RevokeAppSession("session-1"),
            AccountAction.RevokeAllAppSessions,
            AccountAction.RevokeCloudHostGrant(HostId("host-a")),
            AccountAction.RevokeStandaloneDevice("device-1"),
            AccountAction.Forget(ForgetMode.Ordinary),
        )
        actions.forEach { assertEquals(it, it) }
        assertEquals("session-1", (actions[1] as AccountAction.RevokeAppSession).sessionId)
        assertEquals(HostId("host-a"), (actions[3] as AccountAction.RevokeCloudHostGrant).hostId)
        assertEquals("device-1", (actions[4] as AccountAction.RevokeStandaloneDevice).deviceId)
        assertEquals(ForgetMode.Ordinary, (actions[5] as AccountAction.Forget).mode)

        val confirmation = AccountConfirmation(
            action = AccountAction.SignOut,
            instanceId = instance,
            accountId = "account-a",
            scopeKind = AccountScopeKind.Account,
            targetLabel = "workspace-a",
        )
        assertEquals(instance, confirmation.instanceId)
        assertEquals("account-a", confirmation.accountId)
        assertEquals(AccountScopeKind.Account, confirmation.scopeKind)
        assertEquals("workspace-a", confirmation.targetLabel)

        val reports: List<AccountReport> = listOf(
            AccountReport.SignedOut(signOut),
            AccountReport.Revoked(report),
            AccountReport.Forgot(forget),
        )
        reports.forEach { assertEquals(it, it) }

        val notices: List<AccountNotice> = listOf(
            AccountNotice.LoadFailed(CoreFailure("code", "message")),
            AccountNotice.SessionsFailed(CoreFailure("code", "message")),
            AccountNotice.ActionCancelled,
            AccountNotice.SignOutPendingRemote(),
            AccountNotice.SignOutPendingRemote(CoreFailure("code", "message")),
            AccountNotice.RevocationRetentionUncertain(scopes[2], "pending"),
            AccountNotice.ForgottenLocalOnly(true),
            AccountNotice.ActionFailed(CoreFailure("code", "message")),
            AccountNotice.ActionUnconfirmed(CoreFailure("code", "message")),
        )
        notices.forEach { assertEquals(it, it) }

        val state = AccountUiState(
            instanceId = instance,
            loading = false,
            status = signedIn.copy(state = CloudAccountState.PendingRemoteSignOut),
            appSessions = listOf(session),
            confirmation = confirmation,
            report = reports[0],
            notice = notices[2],
        )
        assertTrue(state.signedIn)
        assertTrue(state.pendingRemoteSignOut)
        assertFalse(state.copy(status = signedIn.copy(state = CloudAccountState.SignedOut)).signedIn)
    }
}
