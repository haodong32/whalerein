package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.fixtures.access.FakeClock
import com.whalerein.android.state.fixtures.access.FakeCloudAuthPort
import com.whalerein.android.state.fixtures.access.RecordingDiagnosticSink
import com.whalerein.android.state.fixtures.access.cloudAttempt
import com.whalerein.android.state.fixtures.access.cloudSession
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/**
 * Behavior at the Cloud access boundary. The fake port is shaped exactly like
 * the bridge mapping: the attempt carries the active profile environment, a
 * concrete 600-second deadline, the pinned redirect, and the returned session
 * keeps the same account, workspace and environment.
 */
@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class AccessGapClosureTest : AccessTest() {
    private val instance = InstanceId("cloud-production")
    private val redirect = "https://app.whalerein.test/client/android/callback"

    @Test
    @DisplayName("AC-25.cloud-success-installs-the-active-environment-session-and-opens-the-directory")
    fun cloudSuccessInstallsTheActiveEnvironmentSessionAndOpensTheDirectory() =
        runTest(dispatcher) {
            val clock = FakeClock()
            val auth = FakeCloudAuthPort()
            auth.nextAttempt = { instanceId, environment ->
                cloudAttempt(
                    instance = instanceId,
                    environment = environment,
                    redirect = redirect,
                    state = "state-production",
                    id = "attempt-production",
                    expiresAt = clock.nowMillis + 600_000L,
                )
            }
            val viewModel = newViewModel(auth, clock, environment = "production")
            advanceUntilIdle()
            viewModel.start()
            advanceUntilIdle()
            val attempt = requireNotNull(viewModel.state.value.attempt)
            assertEquals("production", attempt.environment)
            assertEquals(clock.nowMillis + 600_000L, attempt.expiresAtEpochMillis)
            assertEquals(redirect, attempt.redirectUri)

            auth.enqueueComplete(
                CloudAuthResult.Success(
                    cloudSession(
                        instance = instance,
                        account = "account-production",
                        workspace = "workspace-production",
                        environment = "production",
                    ),
                ),
            )
            viewModel.handleCallback(callback(attempt))
            advanceUntilIdle()

            assertEquals(CloudSignInPhase.Succeeded, viewModel.state.value.phase)
            assertEquals(Route.Directory, viewModel.state.value.destination)
            assertEquals("account-production", viewModel.state.value.session?.accountId)
            assertEquals("workspace-production", viewModel.state.value.session?.workspaceId)
            assertEquals("production", viewModel.state.value.session?.environment)
            assertEquals(1, auth.completedCallbacks.size)
        }

    @Test
    @DisplayName("AC-25.cloud-environment-mismatch-is-rejected-before-the-directory")
    fun cloudEnvironmentMismatchIsRejectedBeforeTheDirectory() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        val viewModel = newViewModel(auth, environment = "production")
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()
        val attempt = requireNotNull(viewModel.state.value.attempt)
        auth.enqueueComplete(
            CloudAuthResult.Success(
                cloudSession(instance = instance, environment = "local"),
            ),
        )

        viewModel.handleCallback(callback(attempt))
        advanceUntilIdle()

        assertEquals(CloudSignInPhase.Failed, viewModel.state.value.phase)
        assertEquals("callback_scope", viewModel.state.value.error?.code)
        assertNull(viewModel.state.value.destination)
        assertNull(viewModel.state.value.session)
    }

    @Test
    @DisplayName("AC-25.cloud-account-mismatch-is-rejected")
    fun cloudAccountMismatchIsRejected() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        auth.nextAttempt = { instanceId, environment ->
            cloudAttempt(
                instance = instanceId,
                environment = environment,
                redirect = redirect,
                state = "state-account",
                id = "attempt-account",
                expectedAccountId = "account-expected",
                expectedWorkspaceId = "workspace-expected",
            )
        }
        val viewModel = newViewModel(auth, environment = "production")
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()
        val attempt = requireNotNull(viewModel.state.value.attempt)
        auth.enqueueComplete(
            CloudAuthResult.Success(
                cloudSession(
                    instance = instance,
                    account = "account-other",
                    workspace = "workspace-expected",
                    environment = "production",
                ),
            ),
        )

        viewModel.handleCallback(callback(attempt))
        advanceUntilIdle()

        assertEquals(CloudSignInPhase.Failed, viewModel.state.value.phase)
        assertEquals("callback_scope", viewModel.state.value.error?.code)
        assertNull(viewModel.state.value.destination)
    }

    @Test
    @DisplayName("AC-25.process-restart-restores-the-same-attempt-and-completes-exactly-once")
    fun processRestartRestoresTheSameAttemptAndCompletesExactlyOnce() = runTest(dispatcher) {
        val clock = FakeClock()
        val firstLaunch = FakeCloudAuthPort()
        firstLaunch.nextAttempt = { instanceId, environment ->
            cloudAttempt(
                instance = instanceId,
                environment = environment,
                redirect = redirect,
                state = "state-survives",
                id = "attempt-survives",
                expiresAt = clock.nowMillis + 600_000L,
            )
        }
        val beforeKill = newViewModel(firstLaunch, clock, environment = "production")
        advanceUntilIdle()
        beforeKill.start()
        advanceUntilIdle()
        val attempt = requireNotNull(beforeKill.state.value.attempt)

        // The OS killed the process while the browser was open. The fresh
        // process sees the same durable attempt and has not started another.
        val afterRestart = FakeCloudAuthPort()
        afterRestart.pending = attempt
        afterRestart.enqueueComplete(
            CloudAuthResult.Success(
                cloudSession(
                    instance = instance,
                    account = "account-restored",
                    workspace = "workspace-restored",
                    environment = "production",
                ),
            ),
        )
        val restarted = newViewModel(afterRestart, clock, environment = "production")
        advanceUntilIdle()

        assertEquals(CloudSignInPhase.AwaitingBrowser, restarted.state.value.phase)
        assertEquals(attempt.attemptId, restarted.state.value.attempt?.attemptId)
        assertTrue(afterRestart.startCalls.isEmpty())

        restarted.handleCallback(callback(attempt))
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Succeeded, restarted.state.value.phase)
        assertEquals(Route.Directory, restarted.state.value.destination)
        assertEquals(1, afterRestart.completedCallbacks.size)

        // A replayed App Link callback must not exchange a second time.
        restarted.handleCallback(callback(attempt))
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Succeeded, restarted.state.value.phase)
        assertEquals(1, afterRestart.completedCallbacks.size)
    }

    @Test
    @DisplayName("AC-25.expired-restored-attempt-requires-a-new-attempt")
    fun expiredRestoredAttemptRequiresANewAttempt() = runTest(dispatcher) {
        val clock = FakeClock()
        val expired = cloudAttempt(
            instance = instance,
            environment = "production",
            redirect = redirect,
            state = "state-expired",
            id = "attempt-expired",
            expiresAt = clock.nowMillis - 1,
        )
        val auth = FakeCloudAuthPort()
        auth.pending = expired
        auth.enqueueComplete(CloudAuthResult.Success(cloudSession(instance)))
        val viewModel = newViewModel(auth, clock, environment = "production")
        advanceUntilIdle()

        assertEquals(CloudSignInPhase.Expired, viewModel.state.value.phase)
        assertTrue(viewModel.state.value.canRestart)

        // The stale callback cannot exchange the expired attempt.
        viewModel.handleCallback(callback(expired))
        advanceUntilIdle()
        assertTrue(auth.completedCallbacks.isEmpty())

        viewModel.start()
        advanceUntilIdle()
        val replacement = requireNotNull(viewModel.state.value.attempt)
        assertTrue(replacement.attemptId != expired.attemptId)
        assertEquals(1, auth.startCalls.size)
    }

    @Test
    @DisplayName("AC-25.start-fails-when-the-attempt-environment-does-not-match-the-profile")
    fun startFailsWhenTheAttemptEnvironmentDoesNotMatchTheProfile() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        auth.startResult = Outcome.Failure(
            CoreFailure("environment_mismatch", "The sign-in attempt did not match the active profile."),
        )
        val viewModel = newViewModel(auth, environment = "production")
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()

        assertEquals(CloudSignInPhase.Failed, viewModel.state.value.phase)
        assertEquals("environment_mismatch", viewModel.state.value.error?.code)
        assertNull(viewModel.state.value.attempt)
    }

    private fun newViewModel(
        auth: FakeCloudAuthPort,
        clock: FakeClock = FakeClock(),
        environment: String,
    ): CloudSignInViewModel = CloudSignInViewModel(
        instance = instance,
        auth = auth,
        clock = clock,
        diagnostics = RecordingDiagnosticSink(),
        environment = environment,
        expectedRedirect = redirect,
        dispatcher = dispatcher,
    )

    private fun callback(
        attempt: CloudAttempt,
        state: String = attempt.state,
        environment: String = attempt.environment,
        code: String = "secret-code",
    ): String = "${attempt.redirectUri}?state=$state&environment=$environment&code=$code"
}
