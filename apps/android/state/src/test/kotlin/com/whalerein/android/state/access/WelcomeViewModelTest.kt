package com.whalerein.android.state.access

import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.InstanceKind
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.fixtures.access.FakeProfilePort
import com.whalerein.android.state.fixtures.access.FakeStartupPort
import com.whalerein.android.state.fixtures.access.RecordingDiagnosticSink
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
class WelcomeViewModelTest : AccessTest() {

    @Test
    @DisplayName("AC-07.new-install fresh install defaults to Cloud and creates no identity")
    fun ac07NewInstallFreshInstallDefaultsToCloudAndCreatesNoIdentity() =
        runTest(dispatcher) {
            val profiles = FakeProfilePort()
            val viewModel = newViewModel(profiles, FakeStartupPort())
            advanceUntilIdle()

            assertTrue(viewModel.state.value.freshInstall)
            assertEquals(Route.SignIn, viewModel.state.value.defaultRoute)
            assertTrue(viewModel.state.value.choices.contains(Route.SignIn))
            assertTrue(viewModel.state.value.choices.contains(Route.StandaloneSetup))
            assertNull(viewModel.state.value.activeProfile)
            assertTrue(viewModel.state.value.profiles.isEmpty())

            viewModel.continueWithCloud()
            assertEquals(Route.SignIn, viewModel.state.value.destination)
        }

    @Test
    @DisplayName("AC-05.authorization signed-out unpaired and expired profiles keep an explicit route")
    fun ac05AuthorizationSignedOutUnpairedAndExpiredProfilesKeepAnExplicitRoute() =
        runTest(dispatcher) {
            val signedOut = FakeProfilePort()
            signedOut.listedProfiles += profile("cloud-1")
            signedOut.activeProfile = signedOut.listedProfiles.first()
            val signedOutStartup = FakeStartupPort()
            signedOutStartup.snapshot = Outcome.Success(
                StartupSnapshot(authorization = StartupAuthorization.SignedOut),
            )
            val signedOutModel = newViewModel(signedOut, signedOutStartup)
            advanceUntilIdle()
            assertEquals(Route.SignIn, signedOutModel.state.value.defaultRoute)
            assertTrue(signedOutModel.state.value.choices.contains(Route.Profiles))

            val unpaired = FakeProfilePort()
            unpaired.listedProfiles += profile("standalone-1", kind = InstanceKind.Standalone)
            unpaired.activeProfile = unpaired.listedProfiles.first()
            val unpairedStartup = FakeStartupPort()
            unpairedStartup.snapshot = Outcome.Success(
                StartupSnapshot(authorization = StartupAuthorization.Unpaired),
            )
            val unpairedModel = newViewModel(unpaired, unpairedStartup)
            advanceUntilIdle()
            assertEquals(Route.StandaloneSetup, unpairedModel.state.value.defaultRoute)

            val expired = FakeProfilePort()
            expired.listedProfiles += profile("cloud-2")
            expired.activeProfile = expired.listedProfiles.first()
            val expiredStartup = FakeStartupPort()
            expiredStartup.snapshot = Outcome.Success(
                StartupSnapshot(authorization = StartupAuthorization.Expired),
            )
            val expiredModel = newViewModel(expired, expiredStartup)
            advanceUntilIdle()
            assertEquals(Route.SignIn, expiredModel.state.value.defaultRoute)
            assertFalse(expiredModel.state.value.freshInstall)

            expiredModel.choose(Route.Profiles)
            assertEquals(Route.Profiles, expiredModel.state.value.destination)
            expiredModel.choose(Route.Conversation(com.whalerein.android.state.core.SessionRef(
                com.whalerein.android.state.core.HostRef(InstanceId("cloud-2"), com.whalerein.android.state.core.HostId("h")),
                com.whalerein.android.state.core.SessionId("s"),
            )))
            assertEquals("route_not_available", expiredModel.state.value.error?.code)
        }

    private fun newViewModel(
        profiles: FakeProfilePort,
        startup: FakeStartupPort,
    ): WelcomeViewModel = WelcomeViewModel(
        profiles = profiles,
        startup = startup,
        diagnostics = RecordingDiagnosticSink(),
        dispatcher = dispatcher,
    )
}
