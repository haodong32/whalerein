package com.whalerein.android.state.access

import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import com.whalerein.android.state.fixtures.access.FakeStartupPort
import com.whalerein.android.state.fixtures.access.RecordingDiagnosticSink
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class StartupViewModelTest : AccessTest() {

    private val instance = InstanceId("instance-1")
    private val host = HostRef(instance, HostId("host-1"))
    private val target = SessionRef(host, SessionId("session-1"))

    @Test
    @DisplayName("AC-05.valid restores the exact previous host and conversation")
    fun ac05ValidRestoresTheExactPreviousHostAndConversation() = runTest(dispatcher) {
        val startup = FakeStartupPort()
        startup.snapshot = Outcome.Success(StartupSnapshot(target = target, host = host))
        val viewModel = newViewModel(startup)
        advanceUntilIdle()

        assertEquals(
            StartupDestination(Route.Conversation(target), StartupEvidence.ValidPreviousConversation),
            viewModel.state.value.destination,
        )
        assertEquals(target, viewModel.state.value.target)
        assertFalse(viewModel.state.value.offline)
        assertEquals(ReadOnlyCacheState.NotApplicable, viewModel.state.value.readOnlyCache)
    }

    @Test
    @DisplayName("AC-05.missing a missing session falls back to the same host list")
    fun ac05MissingAMissingSessionFallsBackToTheSameHostList() = runTest(dispatcher) {
        val startup = FakeStartupPort()
        startup.snapshot = Outcome.Success(
            StartupSnapshot(target = target, host = host, missingSession = true),
        )
        val viewModel = newViewModel(startup)
        advanceUntilIdle()

        assertEquals(
            StartupDestination(Route.HostSessions(host), StartupEvidence.MissingSession),
            viewModel.state.value.destination,
        )
        assertEquals(target, viewModel.state.value.target)
    }

    @Test
    @DisplayName("AC-05.authorization signed out unpaired revoked and expired land on the directory")
    fun ac05AuthorizationSignedOutUnpairedRevokedAndExpiredLandOnTheDirectory() =
        runTest(dispatcher) {
            val cases = listOf(
                StartupAuthorization.SignedOut to StartupEvidence.SignedOut,
                StartupAuthorization.Unpaired to StartupEvidence.Unpaired,
                StartupAuthorization.Revoked to StartupEvidence.Revoked,
                StartupAuthorization.Expired to StartupEvidence.Expired,
                StartupAuthorization.Unavailable to StartupEvidence.AuthorizationUnavailable,
            )
            for ((authorization, evidence) in cases) {
                val startup = FakeStartupPort()
                startup.snapshot = Outcome.Success(
                    StartupSnapshot(target = target, host = host, authorization = authorization),
                )
                val viewModel = newViewModel(startup)
                advanceUntilIdle()
                assertEquals(
                    StartupDestination(Route.Directory, evidence),
                    viewModel.state.value.destination,
                )
                assertNull(viewModel.state.value.error)
            }
        }

    @Test
    @DisplayName("AC-05.offline retains the target and offers only an eligible read-only cache")
    fun ac05OfflineRetainsTheTargetAndOffersOnlyAnEligibleReadOnlyCache() =
        runTest(dispatcher) {
            val cached = FakeStartupPort()
            cached.snapshot = Outcome.Success(
                StartupSnapshot(
                    target = target,
                    host = host,
                    online = false,
                    readOnlyCacheAvailable = true,
                ),
            )
            val cachedModel = newViewModel(cached)
            advanceUntilIdle()
            assertEquals(
                StartupDestination(Route.Conversation(target), StartupEvidence.OfflineCacheAvailable),
                cachedModel.state.value.destination,
            )
            assertEquals(ReadOnlyCacheState.Available, cachedModel.state.value.readOnlyCache)
            assertEquals(target, cachedModel.state.value.target)

            val uncached = FakeStartupPort()
            uncached.snapshot = Outcome.Success(
                StartupSnapshot(
                    target = target,
                    host = host,
                    online = false,
                    readOnlyCacheAvailable = false,
                ),
            )
            val uncachedModel = newViewModel(uncached)
            advanceUntilIdle()
            assertEquals(
                StartupDestination(Route.HostSessions(host), StartupEvidence.OfflineCacheUnavailable),
                uncachedModel.state.value.destination,
            )
            assertEquals(ReadOnlyCacheState.Unavailable, uncachedModel.state.value.readOnlyCache)
            assertEquals(target, uncachedModel.state.value.target)
        }

    private fun newViewModel(startup: FakeStartupPort): StartupViewModel =
        StartupViewModel(
            instance = instance,
            startup = startup,
            diagnostics = RecordingDiagnosticSink(),
            dispatcher = dispatcher,
        )
}
