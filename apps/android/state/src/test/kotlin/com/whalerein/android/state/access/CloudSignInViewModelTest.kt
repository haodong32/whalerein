package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.fixtures.access.FakeClock
import com.whalerein.android.state.fixtures.access.FakeCloudAuthPort
import com.whalerein.android.state.fixtures.access.RecordingDiagnosticSink
import com.whalerein.android.state.fixtures.access.cloudAttempt
import com.whalerein.android.state.fixtures.access.cloudSession
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class CloudSignInViewModelTest : AccessTest() {

    private val instance = InstanceId("instance-1")

    @Test
    @DisplayName("AC-01.hosted browser return opens the host directory")
    fun ac01HostedBrowserReturnOpensTheHostDirectory() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        val viewModel = newViewModel(auth)
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()

        assertEquals(CloudSignInPhase.AwaitingBrowser, viewModel.state.value.phase)
        val attempt = requireNotNull(viewModel.state.value.attempt)
        assertTrue(attempt.authorizationUrl.startsWith("https://"))
        auth.enqueueComplete(CloudAuthResult.Success(cloudSession(instance)))

        viewModel.handleCallback(callback(attempt))
        advanceUntilIdle()

        assertEquals(CloudSignInPhase.Succeeded, viewModel.state.value.phase)
        assertEquals(Route.Directory, viewModel.state.value.destination)
        assertEquals("account-1", viewModel.state.value.session?.accountId)
        assertEquals(1, auth.completedCallbacks.size)
    }

    @Test
    @DisplayName("AC-01.recreated pending attempt survives process recreation and returns")
    fun ac01RecreatedPendingAttemptSurvivesProcessRecreationAndReturns() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        auth.pending = cloudAttempt(instance = instance, state = "state-restored", id = "attempt-restored")
        auth.enqueueComplete(CloudAuthResult.Success(cloudSession(instance)))
        val viewModel = newViewModel(auth)
        advanceUntilIdle()

        assertEquals(CloudSignInPhase.AwaitingBrowser, viewModel.state.value.phase)
        assertEquals("attempt-restored", viewModel.state.value.attempt?.attemptId?.value)
        assertTrue(auth.startCalls.isEmpty())

        viewModel.handleCallback(callback(requireNotNull(viewModel.state.value.attempt)))
        advanceUntilIdle()

        assertEquals(CloudSignInPhase.Succeeded, viewModel.state.value.phase)
        assertEquals(Route.Directory, viewModel.state.value.destination)
    }

    @Test
    @DisplayName("AC-01.app-session verifies workspace and opens the directory without host access")
    fun ac01AppSessionVerifiesWorkspaceAndOpensTheDirectoryWithoutHostAccess() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        val viewModel = newViewModel(auth)
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()
        val attempt = requireNotNull(viewModel.state.value.attempt)
        auth.enqueueComplete(
            CloudAuthResult.Success(
                cloudSession(instance, account = "account-9", workspace = "workspace-9"),
            ),
        )

        viewModel.handleCallback(callback(attempt))
        advanceUntilIdle()

        assertEquals("workspace-9", viewModel.state.value.session?.workspaceId)
        assertEquals(Route.Directory, viewModel.state.value.destination)
        assertFalse(viewModel.state.value.destination is Route.HostSessions)
        assertEquals(1, auth.completedCallbacks.size)
    }

    @Test
    @DisplayName("AC-02.redirect wrong scheme host path or environment never exchanges")
    fun ac02RedirectWrongSchemeHostPathOrEnvironmentNeverExchanges() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        val viewModel = newViewModel(auth)
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()
        val attempt = requireNotNull(viewModel.state.value.attempt)

        val invalidCallbacks = listOf(
            callback(attempt, base = attempt.redirectUri.replaceFirst("https://", "http://")),
            callback(attempt, base = attempt.redirectUri.replace("app.whalerein.test", "app.example.test")),
            callback(attempt, base = attempt.redirectUri.replace("/client/android/callback", "/other/callback")),
            callback(attempt, environment = "production"),
        )
        for (invalid in invalidCallbacks) {
            viewModel.handleCallback(invalid)
            advanceUntilIdle()
            assertEquals(CloudSignInPhase.AwaitingBrowser, viewModel.state.value.phase)
            assertNull(viewModel.state.value.destination)
        }

        assertEquals("callback_environment", viewModel.state.value.error?.code)
        assertTrue(auth.completedCallbacks.isEmpty())
    }

    @Test
    @DisplayName("AC-02.state missing duplicate or incorrect state and code never exchanges")
    fun ac02StateMissingDuplicateOrIncorrectStateAndCodeNeverExchanges() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        val viewModel = newViewModel(auth)
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()
        val attempt = requireNotNull(viewModel.state.value.attempt)

        val invalidCallbacks = listOf(
            "${attempt.redirectUri}?environment=${attempt.environment}&code=secret-code",
            "${attempt.redirectUri}?state=${attempt.state}&state=${attempt.state}" +
                "&environment=${attempt.environment}&code=secret-code",
            callback(attempt, state = "wrong-state"),
            "${attempt.redirectUri}?state=${attempt.state}&environment=${attempt.environment}" +
                "&code=secret-code&code=secret-code",
            "${attempt.redirectUri}?state=${attempt.state}&environment=${attempt.environment}",
        )
        for (invalid in invalidCallbacks) {
            viewModel.handleCallback(invalid)
            advanceUntilIdle()
            assertEquals(CloudSignInPhase.AwaitingBrowser, viewModel.state.value.phase)
        }

        assertTrue(auth.completedCallbacks.isEmpty())
        assertNotNull(viewModel.state.value.error)
    }

    @Test
    @DisplayName("AC-02.scope cross account workspace or environment cannot replace a valid session")
    fun ac02ScopeCrossAccountWorkspaceOrEnvironmentCannotReplaceAValidSession() =
        runTest(dispatcher) {
            val auth = FakeCloudAuthPort()
            val previous = cloudSession(instance, account = "account-old", workspace = "workspace-old")
            auth.currentSessionResult = com.whalerein.android.state.core.Outcome.Success(previous)
            val viewModel = newViewModel(auth)
            advanceUntilIdle()
            viewModel.start()
            advanceUntilIdle()
            val attempt = requireNotNull(viewModel.state.value.attempt)
            auth.enqueueComplete(
                CloudAuthResult.Success(
                    cloudSession(instance, account = "account-new", workspace = "workspace-new", environment = "production"),
                ),
            )

            viewModel.handleCallback(callback(attempt))
            advanceUntilIdle()

            assertEquals(CloudSignInPhase.Failed, viewModel.state.value.phase)
            assertEquals("callback_scope", viewModel.state.value.error?.code)
            assertEquals(previous, viewModel.state.value.previousSession)
            assertEquals(previous, viewModel.state.value.session)
            assertNull(viewModel.state.value.destination)

            val workspaceAuth = FakeCloudAuthPort()
            workspaceAuth.nextAttempt = { instanceId, environment ->
                cloudAttempt(
                    instance = instanceId,
                    environment = environment,
                    state = "state-workspace",
                    id = "attempt-workspace",
                    expectedWorkspaceId = "workspace-old",
                )
            }
            val workspaceModel = newViewModel(workspaceAuth)
            advanceUntilIdle()
            workspaceModel.start()
            advanceUntilIdle()
            val workspaceAttempt = requireNotNull(workspaceModel.state.value.attempt)
            workspaceAuth.enqueueComplete(
                CloudAuthResult.Success(cloudSession(instance, workspace = "workspace-other")),
            )
            workspaceModel.handleCallback(callback(workspaceAttempt))
            advanceUntilIdle()
            assertEquals(CloudSignInPhase.Failed, workspaceModel.state.value.phase)
            assertEquals("callback_scope", workspaceModel.state.value.error?.code)
            assertNull(workspaceModel.state.value.destination)
        }

    @Test
    @DisplayName("AC-02.late duplicate expired cancelled and replaced callbacks leave state intact")
    fun ac02LateDuplicateExpiredCancelledAndReplacedCallbacksLeaveStateIntact() =
        runTest(dispatcher) {
            val clock = FakeClock()
            val replayAuth = FakeCloudAuthPort()
            val replayModel = newViewModel(replayAuth, clock)
            advanceUntilIdle()
            replayModel.start()
            advanceUntilIdle()
            val replayAttempt = requireNotNull(replayModel.state.value.attempt)
            replayAuth.enqueueComplete(CloudAuthResult.Success(cloudSession(instance)))
            replayModel.handleCallback(callback(replayAttempt))
            advanceUntilIdle()
            replayModel.handleCallback(callback(replayAttempt))
            advanceUntilIdle()
            assertEquals(CloudSignInPhase.Succeeded, replayModel.state.value.phase)
            assertEquals(Route.Directory, replayModel.state.value.destination)
            assertEquals(1, replayAuth.completedCallbacks.size)

            val cancelledAuth = FakeCloudAuthPort()
            val cancelledModel = newViewModel(cancelledAuth, clock)
            advanceUntilIdle()
            cancelledModel.start()
            advanceUntilIdle()
            val cancelledAttempt = requireNotNull(cancelledModel.state.value.attempt)
            cancelledModel.cancel()
            advanceUntilIdle()
            cancelledModel.handleCallback(callback(cancelledAttempt))
            advanceUntilIdle()
            assertEquals(CloudSignInPhase.Cancelled, cancelledModel.state.value.phase)
            assertTrue(cancelledAuth.completedCallbacks.isEmpty())

            val replacedAuth = FakeCloudAuthPort()
            val replacedModel = newViewModel(replacedAuth, clock)
            advanceUntilIdle()
            replacedModel.start()
            advanceUntilIdle()
            val firstAttempt = requireNotNull(replacedModel.state.value.attempt)
            replacedModel.replaceAttempt()
            advanceUntilIdle()
            val secondAttempt = requireNotNull(replacedModel.state.value.attempt)
            assertTrue(firstAttempt.attemptId != secondAttempt.attemptId)
            replacedModel.handleCallback(callback(firstAttempt))
            advanceUntilIdle()
            assertEquals(CloudSignInPhase.AwaitingBrowser, replacedModel.state.value.phase)
            assertEquals(secondAttempt.attemptId, replacedModel.state.value.attempt?.attemptId)
            assertEquals("attempt_obsolete", replacedModel.state.value.error?.code)
            assertTrue(replacedAuth.completedCallbacks.isEmpty())

            val expiredAuth = FakeCloudAuthPort()
            expiredAuth.pending = cloudAttempt(
                instance = instance,
                id = "attempt-expired",
                expiresAt = clock.nowMillis - 1,
            )
            val expiredModel = newViewModel(expiredAuth, clock)
            advanceUntilIdle()
            assertEquals(CloudSignInPhase.Expired, expiredModel.state.value.phase)
            assertTrue(expiredModel.state.value.canRestart)
            assertNull(expiredModel.state.value.destination)

            val expiredCallbackAuth = FakeCloudAuthPort()
            val expiredClock = FakeClock()
            expiredCallbackAuth.nextAttempt = { instanceId, environment ->
                cloudAttempt(
                    instance = instanceId,
                    environment = environment,
                    state = "state-expired-callback",
                    id = "attempt-expired-callback",
                    expiresAt = expiredClock.nowMillis + 100,
                )
            }
            val expiredCallbackModel = newViewModel(expiredCallbackAuth, expiredClock)
            advanceUntilIdle()
            expiredCallbackModel.start()
            advanceUntilIdle()
            val expiredCallbackAttempt = requireNotNull(expiredCallbackModel.state.value.attempt)
            expiredClock.nowMillis += 500
            expiredCallbackModel.handleCallback(callback(expiredCallbackAttempt))
            advanceUntilIdle()
            assertEquals(CloudSignInPhase.Expired, expiredCallbackModel.state.value.phase)
            assertTrue(expiredCallbackAuth.completedCallbacks.isEmpty())
        }

    @Test
    @DisplayName("AC-02.cross-client mismatch cannot consume or replace the local attempt")
    fun ac02CrossClientMismatchCannotConsumeOrReplaceTheLocalAttempt() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        val previous = cloudSession(instance, account = "account-old", workspace = "workspace-old")
        auth.currentSessionResult = com.whalerein.android.state.core.Outcome.Success(previous)
        val viewModel = newViewModel(auth)
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()
        val attempt = requireNotNull(viewModel.state.value.attempt)
        auth.enqueueComplete(CloudAuthResult.Rejected("cross_client"))

        viewModel.handleCallback(callback(attempt))
        advanceUntilIdle()

        assertEquals(CloudSignInPhase.Failed, viewModel.state.value.phase)
        assertEquals("cross_client", viewModel.state.value.error?.code)
        assertEquals(previous, viewModel.state.value.previousSession)
        assertEquals(previous, viewModel.state.value.session)
        assertNull(viewModel.state.value.destination)
    }

    @Test
    @DisplayName("AC-03.accepted-lost recovers the original session without exchanging again")
    fun ac03AcceptedLostRecoversTheOriginalSessionWithoutExchangingAgain() =
        runTest(dispatcher) {
            val auth = FakeCloudAuthPort()
            val viewModel = newViewModel(auth)
            advanceUntilIdle()
            viewModel.start()
            advanceUntilIdle()
            val attempt = requireNotNull(viewModel.state.value.attempt)
            val recovered = cloudSession(instance, account = "account-recovered", workspace = "workspace-recovered")
            auth.enqueueComplete(CloudAuthResult.Unconfirmed(CoreFailure("exchange_lost", "lost", true)))
            auth.enqueueRecover(CloudAuthResult.Success(recovered))

            viewModel.handleCallback(callback(attempt))
            advanceUntilIdle()

            assertEquals(CloudSignInPhase.Succeeded, viewModel.state.value.phase)
            assertEquals(recovered, viewModel.state.value.session)
            assertEquals(Route.Directory, viewModel.state.value.destination)
            assertEquals(1, auth.completedCallbacks.size)
            assertEquals(listOf(attempt.attemptId), auth.recoveredAttempts)
        }

    @Test
    @DisplayName("AC-03.unconfirmed inconclusive lookup retains the pending outcome")
    fun ac03UnconfirmedInconclusiveLookupRetainsThePendingOutcome() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        val previous = cloudSession(instance, account = "account-old", workspace = "workspace-old")
        auth.currentSessionResult = com.whalerein.android.state.core.Outcome.Success(previous)
        val viewModel = newViewModel(auth)
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()
        val attempt = requireNotNull(viewModel.state.value.attempt)
        auth.enqueueComplete(CloudAuthResult.Unconfirmed(CoreFailure("exchange_lost", "lost", true)))
        auth.enqueueRecover(CloudAuthResult.Unconfirmed(CoreFailure("lookup_unavailable", "pending", true)))

        viewModel.handleCallback(callback(attempt))
        advanceUntilIdle()

        assertEquals(CloudSignInPhase.Unconfirmed, viewModel.state.value.phase)
        assertEquals(attempt.attemptId, viewModel.state.value.attempt?.attemptId)
        assertFalse(viewModel.state.value.canRestart)
        assertEquals(previous, viewModel.state.value.previousSession)
        assertNull(viewModel.state.value.destination)

        viewModel.start()
        advanceUntilIdle()
        assertEquals(1, auth.startCalls.size)
        assertEquals(1, auth.completedCallbacks.size)
    }

    @Test
    @DisplayName("AC-03.expiry expiry or cancel allows a new attempt without code replay")
    fun ac03ExpiryExpiryOrCancelAllowsANewAttemptWithoutCodeReplay() = runTest(dispatcher) {
        val clock = FakeClock()
        val auth = FakeCloudAuthPort()
        auth.nextAttempt = { instanceId, environment ->
            cloudAttempt(
                instance = instanceId,
                environment = environment,
                state = "state-${clock.nowMillis}",
                id = "attempt-${clock.nowMillis}",
                expiresAt = clock.nowMillis + 100,
            )
        }
        val viewModel = newViewModel(auth, clock)
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()
        val attempt = requireNotNull(viewModel.state.value.attempt)
        auth.enqueueComplete(CloudAuthResult.Unconfirmed(CoreFailure("exchange_lost", "lost", true)))
        auth.enqueueRecover(CloudAuthResult.Unconfirmed(CoreFailure("lookup_unavailable", "pending", true)))
        viewModel.handleCallback(callback(attempt))
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Unconfirmed, viewModel.state.value.phase)

        clock.nowMillis += 500
        viewModel.recover()
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Expired, viewModel.state.value.phase)
        assertTrue(viewModel.state.value.canRestart)

        viewModel.start()
        advanceUntilIdle()
        val secondAttempt = requireNotNull(viewModel.state.value.attempt)
        assertEquals(2, auth.startCalls.size)
        assertEquals(1, auth.completedCallbacks.size)

        viewModel.cancel()
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Cancelled, viewModel.state.value.phase)
        clock.nowMillis += 10
        viewModel.start()
        advanceUntilIdle()
        assertEquals(3, auth.startCalls.size)
        assertEquals(1, auth.completedCallbacks.size)
        assertTrue(secondAttempt.attemptId != viewModel.state.value.attempt?.attemptId)
    }

    @Test
    @DisplayName("AC-02.state diagnostics stay redacted and allowlisted")
    fun ac02StateDiagnosticsStayRedactedAndAllowlisted() = runTest(dispatcher) {
        val sink = RecordingDiagnosticSink()
        val auth = FakeCloudAuthPort()
        val viewModel = newViewModel(auth, sink = sink)
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()
        val attempt = requireNotNull(viewModel.state.value.attempt)
        viewModel.handleCallback(callback(attempt, code = "secret-code"))
        advanceUntilIdle()
        viewModel.handleCallback(callback(attempt, code = "secret-code-2"))
        advanceUntilIdle()

        val events = sink.events
        assertTrue(events.isNotEmpty())
        assertTrue(events.all { it.stage in setOf("login", "startup", "transport", "profile") })
        assertTrue(events.none { it.level == DiagnosticLevel.Debug })
        assertTrue(events.all { it.fields.isEmpty() })
        val rendered = events.joinToString(" ") { "${it.event} ${it.stage} ${it.code}" }
        assertFalse(rendered.contains("secret-code"))
        assertFalse(rendered.contains("http://"))
        assertFalse(rendered.contains("https://"))
        assertFalse(rendered.contains("state-1"))
        assertFalse(rendered.contains("verifier"))
        assertFalse(rendered.contains("fingerprint"))
    }

    private fun newViewModel(
        auth: FakeCloudAuthPort,
        clock: FakeClock = FakeClock(),
        sink: RecordingDiagnosticSink = RecordingDiagnosticSink(),
    ): CloudSignInViewModel = CloudSignInViewModel(
        instance = instance,
        auth = auth,
        clock = clock,
        diagnostics = sink,
        dispatcher = dispatcher,
    )

    private fun callback(
        attempt: CloudAttempt,
        base: String = attempt.redirectUri,
        state: String = attempt.state,
        environment: String = attempt.environment,
        code: String = "secret-code",
    ): String = "$base?state=$state&environment=$environment&code=$code"
}
