package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.fixtures.access.FakeClock
import com.whalerein.android.state.fixtures.access.FakeCloudAuthPort
import com.whalerein.android.state.fixtures.access.RecordingDiagnosticSink
import com.whalerein.android.state.fixtures.access.cloudAttempt
import com.whalerein.android.state.fixtures.access.cloudSession
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class CloudSignInCoverageTest : AccessTest() {

    private val instance = InstanceId("instance-1")

    @Test
    @DisplayName("AC-02.state start failures insecure URLs and binding mismatches are typed")
    fun ac02StateStartFailuresInsecureUrlsAndBindingMismatchesAreTyped() = runTest(dispatcher) {
        val failedAuth = FakeCloudAuthPort()
        failedAuth.startResult = Outcome.Failure(CoreFailure("start_failed", "failed", retryable = true))
        val failedModel = newViewModel(failedAuth)
        advanceUntilIdle()
        failedModel.start()
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Failed, failedModel.state.value.phase)
        assertEquals("start_failed", failedModel.state.value.error?.code)
        assertTrue(failedModel.state.value.canRestart)

        val insecure = FakeCloudAuthPort()
        insecure.nextAttempt = { id, env ->
            cloudAttempt(instance = id, environment = env).copy(authorizationUrl = "http://insecure.test/auth")
        }
        val insecureModel = newViewModel(insecure)
        advanceUntilIdle()
        insecureModel.start()
        advanceUntilIdle()
        assertEquals("authorization_url_insecure", insecureModel.state.value.error?.code)
        assertNull(insecureModel.state.value.attempt)

        val malformedUrl = FakeCloudAuthPort()
        malformedUrl.nextAttempt = { id, env ->
            cloudAttempt(instance = id, environment = env).copy(authorizationUrl = "not a uri")
        }
        val malformedUrlModel = newViewModel(malformedUrl)
        advanceUntilIdle()
        malformedUrlModel.start()
        advanceUntilIdle()
        assertEquals("authorization_url_insecure", malformedUrlModel.state.value.error?.code)

        val wrongEnvironment = FakeCloudAuthPort()
        wrongEnvironment.nextAttempt = { id, _ -> cloudAttempt(instance = id, environment = "production") }
        val wrongEnvironmentModel = newViewModel(wrongEnvironment)
        advanceUntilIdle()
        wrongEnvironmentModel.start()
        advanceUntilIdle()
        assertEquals("attempt_binding_mismatch", wrongEnvironmentModel.state.value.error?.code)

        val wrongRedirect = FakeCloudAuthPort()
        wrongRedirect.nextAttempt = { id, env ->
            cloudAttempt(instance = id, environment = env).copy(
                redirectUri = "https://evil.example/callback",
            )
        }
        val wrongRedirectModel = newViewModel(wrongRedirect)
        advanceUntilIdle()
        wrongRedirectModel.start()
        advanceUntilIdle()
        assertEquals("attempt_binding_mismatch", wrongRedirectModel.state.value.error?.code)

        val invalidExpectedRedirect = FakeCloudAuthPort()
        val invalidExpectedModel = newViewModel(invalidExpectedRedirect, expectedRedirect = "not a uri")
        advanceUntilIdle()
        invalidExpectedModel.start()
        advanceUntilIdle()
        assertEquals("attempt_binding_mismatch", invalidExpectedModel.state.value.error?.code)
    }

    @Test
    @DisplayName("AC-02.late already-active start and cancel without an attempt stay safe")
    fun ac02LateAlreadyActiveStartAndCancelWithoutAnAttemptStaySafe() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        val viewModel = newViewModel(auth)
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()
        assertEquals(1, auth.startCalls.size)
        assertEquals(CloudSignInPhase.AwaitingBrowser, viewModel.state.value.phase)

        val idleAuth = FakeCloudAuthPort()
        val idleModel = newViewModel(idleAuth)
        advanceUntilIdle()
        idleModel.cancel()
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Cancelled, idleModel.state.value.phase)
        assertTrue(idleModel.state.value.canRestart)

        val replacedAuth = FakeCloudAuthPort()
        val replacedModel = newViewModel(replacedAuth)
        advanceUntilIdle()
        replacedModel.replaceAttempt()
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.AwaitingBrowser, replacedModel.state.value.phase)
        assertEquals(1, replacedAuth.startCalls.size)
    }

    @Test
    @DisplayName("AC-02.state malformed callback syntax is rejected before exchange")
    fun ac02StateMalformedCallbackSyntaxIsRejectedBeforeExchange() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        val viewModel = newViewModel(auth)
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()
        val attempt = requireNotNull(viewModel.state.value.attempt)

        val malformed = listOf(
            "not a uri",
            attempt.redirectUri,
            "${attempt.redirectUri}?",
            "${attempt.redirectUri}?state=%ZZ&environment=${attempt.environment}&code=secret",
            "${attempt.redirectUri}?&state&environment=${attempt.environment}&code=secret",
            "${attempt.redirectUri}?state=${attempt.state}" +
                "&environment=${attempt.environment}&environment=${attempt.environment}" +
                "&code=secret",
        )
        for (callback in malformed) {
            viewModel.handleCallback(callback)
            advanceUntilIdle()
            assertEquals(CloudSignInPhase.AwaitingBrowser, viewModel.state.value.phase)
        }
        assertEquals(0, auth.completedCallbacks.size)
        assertNotNull(viewModel.state.value.error)
    }

    @Test
    @DisplayName("AC-02.state callback error and exchange exception stay typed")
    fun ac02StateCallbackErrorAndExchangeExceptionStayTyped() = runTest(dispatcher) {
        val cancelledAuth = FakeCloudAuthPort()
        val cancelledModel = newViewModel(cancelledAuth)
        advanceUntilIdle()
        cancelledModel.start()
        advanceUntilIdle()
        val cancelledAttempt = requireNotNull(cancelledModel.state.value.attempt)
        cancelledModel.handleCallback(
            "${cancelledAttempt.redirectUri}?state=${cancelledAttempt.state}" +
                "&environment=${cancelledAttempt.environment}&error=access_denied",
        )
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Cancelled, cancelledModel.state.value.phase)
        assertTrue(cancelledAuth.completedCallbacks.isEmpty())

        val errorAuth = FakeCloudAuthPort()
        val errorModel = newViewModel(errorAuth)
        advanceUntilIdle()
        errorModel.start()
        advanceUntilIdle()
        val errorAttempt = requireNotNull(errorModel.state.value.attempt)
        errorModel.handleCallback(
            "${errorAttempt.redirectUri}?state=${errorAttempt.state}" +
                "&environment=${errorAttempt.environment}&error=server_error",
        )
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.AwaitingBrowser, errorModel.state.value.phase)
        assertEquals("callback_error", errorModel.state.value.error?.code)
        assertTrue(errorAuth.completedCallbacks.isEmpty())

        val exceptionAuth = FakeCloudAuthPort()
        val session = cloudSession(instance, account = "account-recovered")
        exceptionAuth.completeException = IllegalStateException("exchange failed")
        exceptionAuth.enqueueRecover(CloudAuthResult.Success(session))
        val exceptionModel = newViewModel(exceptionAuth)
        advanceUntilIdle()
        exceptionModel.start()
        advanceUntilIdle()
        val exceptionAttempt = requireNotNull(exceptionModel.state.value.attempt)
        exceptionModel.handleCallback(callback(exceptionAttempt))
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Succeeded, exceptionModel.state.value.phase)
        assertEquals(session, exceptionModel.state.value.session)
        assertEquals(1, exceptionAuth.recoveredAttempts.size)
    }

    @Test
    @DisplayName("AC-02.late rejected cancelled expired and recovery outcomes are distinct")
    fun ac02LateRejectedCancelledExpiredAndRecoveryOutcomesAreDistinct() = runTest(dispatcher) {
        val rejectedExpired = FakeCloudAuthPort()
        rejectedExpired.enqueueComplete(CloudAuthResult.Rejected("expired"))
        val rejectedExpiredModel = startedModel(rejectedExpired)
        val rejectedExpiredAttempt = requireNotNull(rejectedExpiredModel.state.value.attempt)
        rejectedExpiredModel.handleCallback(callback(rejectedExpiredAttempt))
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Expired, rejectedExpiredModel.state.value.phase)

        val cancelled = FakeCloudAuthPort()
        cancelled.enqueueComplete(CloudAuthResult.Cancelled)
        val cancelledModel = startedModel(cancelled)
        cancelledModel.handleCallback(callback(requireNotNull(cancelledModel.state.value.attempt)))
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Cancelled, cancelledModel.state.value.phase)

        val expired = FakeCloudAuthPort()
        expired.enqueueComplete(CloudAuthResult.Expired)
        val expiredModel = startedModel(expired)
        expiredModel.handleCallback(callback(requireNotNull(expiredModel.state.value.attempt)))
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Expired, expiredModel.state.value.phase)
    }

    @Test
    @DisplayName("AC-03.unconfirmed manual recovery after an inconclusive lookup can succeed")
    fun ac03UnconfirmedManualRecoveryAfterAnInconclusiveLookupCanSucceed() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        auth.enqueueComplete(CloudAuthResult.Unconfirmed(CoreFailure("exchange_lost", "lost", true)))
        auth.enqueueRecover(CloudAuthResult.Unconfirmed(CoreFailure("lookup_unavailable", "pending", true)))
        val viewModel = startedModel(auth)
        viewModel.handleCallback(callback(requireNotNull(viewModel.state.value.attempt)))
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Unconfirmed, viewModel.state.value.phase)

        val session = cloudSession(instance, account = "account-recovered")
        auth.enqueueRecover(CloudAuthResult.Success(session))
        viewModel.recover()
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Succeeded, viewModel.state.value.phase)
        assertEquals(session, viewModel.state.value.session)
        assertEquals(2, auth.recoveredAttempts.size)
        assertEquals(1, auth.completedCallbacks.size)
    }

    @Test
    @DisplayName("AC-03.unconfirmed recovery exceptions and non-pending recover calls are typed")
    fun ac03UnconfirmedRecoveryExceptionsAndNonPendingRecoverCallsAreTyped() = runTest(dispatcher) {
        val exceptionAuth = FakeCloudAuthPort()
        exceptionAuth.enqueueComplete(CloudAuthResult.Unconfirmed(CoreFailure("exchange_lost", "lost", true)))
        exceptionAuth.recoverException = IllegalStateException("lookup failed")
        val exceptionModel = startedModel(exceptionAuth)
        exceptionModel.handleCallback(callback(requireNotNull(exceptionModel.state.value.attempt)))
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Unconfirmed, exceptionModel.state.value.phase)
        assertEquals(1, exceptionAuth.recoveredAttempts.size)

        val activeAuth = FakeCloudAuthPort()
        val activeModel = startedModel(activeAuth)
        activeModel.recover()
        advanceUntilIdle()
        assertTrue(activeAuth.recoveredAttempts.isEmpty())
        assertEquals(CloudSignInPhase.AwaitingBrowser, activeModel.state.value.phase)

        val idleModel = newViewModel(FakeCloudAuthPort())
        advanceUntilIdle()
        idleModel.recover()
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Idle, idleModel.state.value.phase)
    }

    @Test
    @DisplayName("AC-02.scope expected account mismatch is rejected without installing")
    fun ac02ScopeExpectedAccountMismatchIsRejectedWithoutInstalling() = runTest(dispatcher) {
        val auth = FakeCloudAuthPort()
        auth.nextAttempt = { id, env ->
            cloudAttempt(
                instance = id,
                environment = env,
                expectedAccountId = "account-old",
            )
        }
        val viewModel = startedModel(auth)
        auth.enqueueComplete(CloudAuthResult.Success(cloudSession(instance, account = "account-new")))
        viewModel.handleCallback(callback(requireNotNull(viewModel.state.value.attempt)))
        advanceUntilIdle()
        assertEquals(CloudSignInPhase.Failed, viewModel.state.value.phase)
        assertEquals("callback_scope", viewModel.state.value.error?.code)
        assertNull(viewModel.state.value.session)
    }

    private suspend fun TestScope.startedModel(
        auth: FakeCloudAuthPort,
        clock: FakeClock = FakeClock(),
    ): CloudSignInViewModel {
        val viewModel = newViewModel(auth, clock)
        advanceUntilIdle()
        viewModel.start()
        advanceUntilIdle()
        return viewModel
    }

    private fun newViewModel(
        auth: FakeCloudAuthPort,
        clock: FakeClock = FakeClock(),
        expectedRedirect: String = "https://app.whalerein.test/client/android/callback",
    ): CloudSignInViewModel = CloudSignInViewModel(
        instance = instance,
        auth = auth,
        clock = clock,
        diagnostics = RecordingDiagnosticSink(),
        expectedRedirect = expectedRedirect,
        dispatcher = dispatcher,
    )

    private fun callback(
        attempt: CloudAttempt,
        state: String = attempt.state,
        environment: String = attempt.environment,
        code: String = "secret-code",
    ): String = "${attempt.redirectUri}?state=$state&environment=$environment&code=$code"
}
