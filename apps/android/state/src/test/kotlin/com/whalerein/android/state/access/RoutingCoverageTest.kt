package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.InstanceKind
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import com.whalerein.android.state.fixtures.access.FakeDirectoryPort
import com.whalerein.android.state.fixtures.access.FakeProfilePort
import com.whalerein.android.state.fixtures.access.FakeStartupPort
import com.whalerein.android.state.fixtures.access.RecordingDiagnosticSink
import com.whalerein.android.state.fixtures.access.cloudSession
import com.whalerein.android.state.fixtures.access.hostView
import com.whalerein.android.state.fixtures.access.profile
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class RoutingCoverageTest : AccessTest() {

    private val instance = InstanceId("instance-1")
    private val host = HostRef(instance, HostId("host-1"))
    private val target = SessionRef(host, SessionId("session-1"))

    @Test
    @DisplayName("AC-05.valid authorized Cloud and Standalone profiles choose the directory")
    fun ac05ValidAuthorizedCloudAndStandaloneProfilesChooseTheDirectory() = runTest(dispatcher) {
        val cloudProfiles = FakeProfilePort()
        cloudProfiles.listedProfiles += profile("instance-1", active = true)
        cloudProfiles.activeProfile = cloudProfiles.listedProfiles.first()
        val cloudStartup = FakeStartupPort()
        cloudStartup.snapshot = Outcome.Success(StartupSnapshot(authorization = StartupAuthorization.Authorized))
        val cloudModel = newWelcome(cloudProfiles, cloudStartup)
        advanceUntilIdle()
        assertEquals(Route.Directory, cloudModel.state.value.defaultRoute)
        cloudModel.continueWithCloud()
        assertEquals(Route.Directory, cloudModel.state.value.destination)

        val nullSnapshot = FakeStartupPort()
        nullSnapshot.snapshot = Outcome.Failure(CoreFailure("restore_io", "io", true))
        val nullModel = newWelcome(cloudProfiles, nullSnapshot)
        advanceUntilIdle()
        assertEquals(Route.Directory, nullModel.state.value.defaultRoute)

        val cloudUnpaired = FakeStartupPort()
        cloudUnpaired.snapshot = Outcome.Success(StartupSnapshot(authorization = StartupAuthorization.Unpaired))
        val cloudUnpairedModel = newWelcome(cloudProfiles, cloudUnpaired)
        advanceUntilIdle()
        assertEquals(Route.StandaloneSetup, cloudUnpairedModel.state.value.defaultRoute)
        cloudUnpairedModel.continueWithCloud()
        assertEquals(Route.StandaloneSetup, cloudUnpairedModel.state.value.destination)

        val standaloneProfiles = FakeProfilePort()
        standaloneProfiles.listedProfiles += profile(
            "instance-s",
            kind = InstanceKind.Standalone,
            active = true,
        )
        standaloneProfiles.activeProfile = standaloneProfiles.listedProfiles.first()
        val standaloneStartup = FakeStartupPort()
        standaloneStartup.snapshot = Outcome.Success(StartupSnapshot(authorization = StartupAuthorization.Authorized))
        val standaloneModel = newWelcome(standaloneProfiles, standaloneStartup)
        advanceUntilIdle()
        assertEquals(Route.Directory, standaloneModel.state.value.defaultRoute)
    }

    @Test
    @DisplayName("AC-05.authorization welcome load failures remain unavailable instead of fresh")
    fun ac05AuthorizationWelcomeLoadFailuresRemainUnavailableInsteadOfFresh() = runTest(dispatcher) {
        val listFailure = FakeProfilePort()
        listFailure.listResult = Outcome.Failure(CoreFailure("welcome_list_io", "io", true))
        val listModel = newWelcome(listFailure, FakeStartupPort())
        advanceUntilIdle()
        assertEquals("welcome_list_io", listModel.state.value.error?.code)
        assertFalse(listModel.state.value.freshInstall)
        assertFalse(listModel.state.value.loading)

        val activeFailure = FakeProfilePort()
        activeFailure.listedProfiles += profile("instance-1")
        activeFailure.activeResult = Outcome.Failure(CoreFailure("welcome_active_io", "io", true))
        val activeModel = newWelcome(activeFailure, FakeStartupPort())
        advanceUntilIdle()
        assertEquals("welcome_active_io", activeModel.state.value.error?.code)
        assertFalse(activeModel.state.value.freshInstall)
    }

    @Test
    @DisplayName("AC-05.valid startup retry recovers after a temporary failure")
    fun ac05ValidStartupRetryRecoversAfterATemporaryFailure() = runTest(dispatcher) {
        val startup = FakeStartupPort()
        startup.snapshot = Outcome.Failure(CoreFailure("startup_io", "io", true))
        val viewModel = newStartup(startup)
        advanceUntilIdle()
        assertEquals("startup_io", viewModel.state.value.error?.code)
        assertFalse(viewModel.state.value.loading)

        startup.snapshot = Outcome.Success(StartupSnapshot(target = target, host = host))
        viewModel.retry()
        advanceUntilIdle()
        assertEquals(
            StartupDestination(Route.Conversation(target), StartupEvidence.ValidPreviousConversation),
            viewModel.state.value.destination,
        )
        assertNull(viewModel.state.value.error)
    }

    @Test
    @DisplayName("AC-05.missing startup without a host falls back to the target host list")
    fun ac05MissingStartupWithoutAHostFallsBackToTheTargetHostList() = runTest(dispatcher) {
        val startup = FakeStartupPort()
        startup.snapshot = Outcome.Success(
            StartupSnapshot(target = target, missingSession = true),
        )
        val viewModel = newStartup(startup)
        advanceUntilIdle()
        assertEquals(
            StartupDestination(Route.HostSessions(host), StartupEvidence.MissingSession),
            viewModel.state.value.destination,
        )

        startup.snapshot = Outcome.Success(StartupSnapshot())
        val emptyModel = newStartup(startup)
        advanceUntilIdle()
        assertEquals(
            StartupDestination(Route.Directory, StartupEvidence.FirstCloudUse),
            emptyModel.state.value.destination,
        )
    }

    @Test
    @DisplayName("AC-04.directory refresh and unknown or ineligible hosts are denied")
    fun ac04DirectoryRefreshAndUnknownOrIneligibleHostsAreDenied() = runTest(dispatcher) {
        val directory = FakeDirectoryPort()
        directory.listedHosts += hostView("host-ready", availability = HostAvailability.Ready)
        directory.listedHosts += hostView(
            "host-offline",
            online = false,
            availability = HostAvailability.Offline,
            connectable = false,
        )
        directory.listedHosts += hostView(
            "host-check",
            availability = HostAvailability.AuthorizationUnavailable,
            connectable = false,
        )
        directory.listedHosts += hostView(
            "host-unknown",
            availability = HostAvailability.Unknown,
            connectable = false,
        )
        val viewModel = newDirectory(directory)
        advanceUntilIdle()

        viewModel.connect(HostId("missing"))
        advanceUntilIdle()
        assertEquals("host_unknown", viewModel.state.value.error?.code)
        assertNull(viewModel.state.value.destination)

        for ((id, code) in listOf(
            HostId("host-offline") to "host_offline",
            HostId("host-check") to "host_authorization_unavailable",
            HostId("host-unknown") to "host_unknown_state",
        )) {
            viewModel.connect(id)
            advanceUntilIdle()
            assertEquals(code, viewModel.state.value.error?.code)
            assertNull(viewModel.state.value.destination)
        }

        directory.listResult = Outcome.Failure(CoreFailure("directory_io", "io", true))
        viewModel.refresh()
        advanceUntilIdle()
        assertEquals("directory_io", viewModel.state.value.error?.code)
        assertFalse(viewModel.state.value.loading)
        assertEquals(4, viewModel.state.value.hosts.size)

        viewModel.clearError()
        assertNull(viewModel.state.value.error)
        assertNull(viewModel.state.value.deniedHostId)
    }

    @Test
    @DisplayName("AC-04.default unknown and failed default selections retain the prior default")
    fun ac04DefaultUnknownAndFailedDefaultSelectionsRetainThePriorDefault() = runTest(dispatcher) {
        val directory = FakeDirectoryPort()
        val readyId = HostId("host-ready")
        directory.listedHosts += hostView("host-ready", availability = HostAvailability.Ready)
        directory.selectResults[readyId] = Outcome.Failure(CoreFailure("default_io", "io", true))
        val viewModel = newDirectory(directory)
        advanceUntilIdle()

        viewModel.selectDefault(HostId("missing"))
        advanceUntilIdle()
        assertEquals("host_unknown", viewModel.state.value.error?.code)

        viewModel.selectDefault(readyId)
        advanceUntilIdle()
        assertEquals("default_io", viewModel.state.value.error?.code)
        assertNull(viewModel.state.value.defaultHostId)
    }

    @Test
    @DisplayName("AC-04.pair non-ready connection results map every availability")
    fun ac04PairNonReadyConnectionResultsMapEveryAvailability() = runTest(dispatcher) {
        val cases = listOf(
            hostView("host-ready", availability = HostAvailability.Ready, connectable = false) to
                "host_unavailable",
            hostView("host-ready", availability = HostAvailability.Offline, connectable = true) to
                "host_offline",
            hostView("host-ready", availability = HostAvailability.AuthorizationUnavailable) to
                "host_authorization_unavailable",
            hostView("host-ready", availability = HostAvailability.Unknown) to
                "host_unknown_state",
        )
        for ((resultHost, code) in cases) {
            val directory = FakeDirectoryPort()
            directory.listedHosts += hostView("host-ready", availability = HostAvailability.Ready)
            directory.connectResults[HostId("host-ready")] = Outcome.Success(
                HostConnection(resultHost, cloudSession(instance)),
            )
            val viewModel = newDirectory(directory)
            advanceUntilIdle()
            viewModel.connect(HostId("host-ready"))
            advanceUntilIdle()
            assertEquals(code, viewModel.state.value.error?.code)
            assertNull(viewModel.state.value.destination)
            assertTrue(viewModel.state.value.activeHost == null)
        }
    }

    private fun newWelcome(
        profiles: FakeProfilePort,
        startup: FakeStartupPort,
    ): WelcomeViewModel = WelcomeViewModel(
        profiles = profiles,
        startup = startup,
        diagnostics = RecordingDiagnosticSink(),
        dispatcher = dispatcher,
    )

    private fun newStartup(startup: FakeStartupPort): StartupViewModel = StartupViewModel(
        instance = instance,
        startup = startup,
        diagnostics = RecordingDiagnosticSink(),
        dispatcher = dispatcher,
    )

    private fun newDirectory(directory: FakeDirectoryPort): DirectoryViewModel = DirectoryViewModel(
        instance = instance,
        directory = directory,
        diagnostics = RecordingDiagnosticSink(),
        dispatcher = dispatcher,
    )
}
