package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.fixtures.access.FakeDirectoryPort
import com.whalerein.android.state.fixtures.access.RecordingDiagnosticSink
import com.whalerein.android.state.fixtures.access.hostView
import com.whalerein.android.state.fixtures.access.cloudSession
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class DirectoryViewModelTest : AccessTest() {

    private val instance = InstanceId("instance-1")
    private val readyId = HostId("host-ready")
    private val noGrantId = HostId("host-no-grant")
    private val offlineId = HostId("host-offline")
    private val unknownId = HostId("host-unknown")
    private val unavailableId = HostId("host-check-unavailable")

    @Test
    @DisplayName("AC-05.authorization a signed-out directory asks for sign-in instead of a security error")
    fun ac05AuthorizationSignedOutDirectoryAsksForSignIn() = runTest(dispatcher) {
        val directory = FakeDirectoryPort()
        directory.listResult = Outcome.Failure(
            CoreFailure("security", "The operation was blocked for security."),
        )
        directory.currentSessionResult = Outcome.Success(null)
        val viewModel = newViewModel(directory)
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(state.needsSignIn)
        assertNull(state.error)
        assertEquals(emptyList<DirectoryHostView>(), state.hosts)
        assertFalse(state.loading)
    }

    @Test
    @DisplayName("AC-04.directory visible ready no-grant offline and unknown states are distinguishable")
    fun ac04DirectoryVisibleReadyNoGrantOfflineAndUnknownStatesAreDistinguishable() =
        runTest(dispatcher) {
            val directory = FakeDirectoryPort()
            directory.listedHosts += listOf(
                hostView("host-ready", label = "Ready", availability = HostAvailability.Ready),
                hostView(
                    "host-no-grant",
                    label = "Eligible",
                    availability = HostAvailability.NoGrant,
                    connectable = true,
                ),
                hostView("host-offline", label = "Offline", online = false, availability = HostAvailability.Offline),
                hostView(
                    "host-check-unavailable",
                    label = "Check unavailable",
                    availability = HostAvailability.AuthorizationUnavailable,
                ),
                hostView("host-unknown", label = "Unknown", availability = HostAvailability.Unknown),
            )
            val viewModel = newViewModel(directory)
            advanceUntilIdle()

            val byId = viewModel.state.value.hosts.associateBy { it.hostId }
            assertEquals(HostAvailability.Ready, byId.getValue(readyId).availability)
            assertEquals(HostAvailability.NoGrant, byId.getValue(noGrantId).availability)
            assertEquals(HostAvailability.Offline, byId.getValue(offlineId).availability)
            assertEquals(
                HostAvailability.AuthorizationUnavailable,
                byId.getValue(unavailableId).availability,
            )
            assertEquals(HostAvailability.Unknown, byId.getValue(unknownId).availability)
            assertTrue(byId.getValue(readyId).connectable)
            assertTrue(byId.getValue(noGrantId).connectable)
            assertFalse(byId.getValue(offlineId).connectable)
            assertFalse(byId.getValue(unavailableId).connectable)
            assertFalse(byId.getValue(unknownId).connectable)
        }

    @Test
    @DisplayName("AC-04.pair only a confirmed usable grant opens a host")
    fun ac04PairOnlyAConfirmedUsableGrantOpensAHost() = runTest(dispatcher) {
        val directory = FakeDirectoryPort()
        directory.listedHosts += hostView(
            "host-no-grant",
            availability = HostAvailability.NoGrant,
            connectable = true,
        )
        directory.connectResults[noGrantId] = Outcome.Success(
            HostConnection(
                hostView("host-no-grant", availability = HostAvailability.Ready),
                cloudSession(instance),
            ),
        )
        val viewModel = newViewModel(directory)
        advanceUntilIdle()

        viewModel.connect(noGrantId)
        advanceUntilIdle()
        assertEquals(
            Route.HostSessions(HostRef(instance, noGrantId)),
            viewModel.state.value.destination,
        )
        assertEquals("account-1", viewModel.state.value.session?.accountId)
        assertEquals(HostRef(instance, noGrantId), viewModel.state.value.activeHost)

        val deniedDirectory = FakeDirectoryPort()
        deniedDirectory.listedHosts += hostView(
            "host-no-grant",
            availability = HostAvailability.NoGrant,
            connectable = true,
        )
        deniedDirectory.connectResults[noGrantId] = Outcome.Success(
            HostConnection(
                hostView("host-no-grant", availability = HostAvailability.NoGrant),
                cloudSession(instance),
            ),
        )
        val deniedModel = newViewModel(deniedDirectory)
        advanceUntilIdle()
        deniedModel.connect(noGrantId)
        advanceUntilIdle()
        assertNull(deniedModel.state.value.destination)
        assertEquals("host_no_grant", deniedModel.state.value.error?.code)
    }

    @Test
    @DisplayName("AC-04.default explicit default persists and unauthorized selection is refused")
    fun ac04DefaultExplicitDefaultPersistsAndUnauthorizedSelectionIsRefused() =
        runTest(dispatcher) {
            val directory = FakeDirectoryPort()
            directory.listedHosts += hostView("host-ready", availability = HostAvailability.Ready)
            directory.listedHosts += hostView("host-no-grant", availability = HostAvailability.NoGrant)
            val viewModel = newViewModel(directory)
            advanceUntilIdle()

            viewModel.selectDefault(readyId)
            advanceUntilIdle()
            assertEquals(readyId, viewModel.state.value.defaultHostId)

            viewModel.selectDefault(noGrantId)
            advanceUntilIdle()
            assertEquals(readyId, viewModel.state.value.defaultHostId)
            assertEquals("default_unauthorized", viewModel.state.value.error?.code)
            assertEquals(listOf(readyId), directory.selectCalls)
        }

    @Test
    @DisplayName("AC-04.denied one host cannot implicitly open another host")
    fun ac04DeniedOneHostCannotImplicitlyOpenAnotherHost() = runTest(dispatcher) {
        val directory = FakeDirectoryPort()
        directory.listedHosts += hostView("host-ready", availability = HostAvailability.Ready)
        directory.listedHosts += hostView("host-second", availability = HostAvailability.Ready)
        directory.connectResults[readyId] = Outcome.Failure(
            CoreFailure("host_denied", "access denied", retryable = true),
        )
        val viewModel = newViewModel(directory)
        advanceUntilIdle()

        viewModel.connect(readyId)
        advanceUntilIdle()

        assertEquals(listOf(readyId), directory.connectCalls)
        assertNull(viewModel.state.value.destination)
        assertNull(viewModel.state.value.activeHost)
        assertEquals(readyId, viewModel.state.value.deniedHostId)

        viewModel.selectDefault(HostId("host-second"))
        advanceUntilIdle()
        assertEquals(HostId("host-second"), viewModel.state.value.defaultHostId)
        assertEquals(listOf(readyId), directory.connectCalls)
        assertNull(viewModel.state.value.destination)
    }

    @Test
    @DisplayName("AC-26.signed-out-directory-offers-standalone-setup")
    fun ac26SignedOutDirectoryOffersStandaloneSetup() = runTest(dispatcher) {
        val directory = FakeDirectoryPort()
        directory.listResult = Outcome.Failure(
            CoreFailure("security", "The operation was blocked for security."),
        )
        directory.currentSessionResult = Outcome.Success(null)
        val viewModel = newViewModel(directory)
        advanceUntilIdle()

        assertTrue(viewModel.state.value.needsSignIn)
        assertTrue(viewModel.state.value.standaloneSetupAvailable)
    }

    @Test
    @DisplayName("AC-26.empty-first-use-directory-keeps-standalone-setup-reachable")
    fun ac26EmptyFirstUseDirectoryKeepsStandaloneSetupReachable() = runTest(dispatcher) {
        val directory = FakeDirectoryPort()
        directory.listResult = Outcome.Success(emptyList())
        val viewModel = newViewModel(directory)
        advanceUntilIdle()

        assertTrue(viewModel.state.value.hosts.isEmpty())
        assertTrue(viewModel.state.value.standaloneSetupAvailable)
    }

    private fun newViewModel(directory: FakeDirectoryPort): DirectoryViewModel =
        DirectoryViewModel(
            instance = instance,
            directory = directory,
            diagnostics = RecordingDiagnosticSink(),
            dispatcher = dispatcher,
        )
}
