package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.fixtures.access.FakeProfilePort
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
class ProfilesViewModelTest : AccessTest() {

    private val first = InstanceId("instance-a")
    private val second = InstanceId("instance-b")

    @Test
    @DisplayName("AC-07.switch failed activation retains the source and success fences stale callbacks")
    fun ac07SwitchFailedActivationRetainsTheSourceAndSuccessFencesStaleCallbacks() =
        runTest(dispatcher) {
            val profiles = FakeProfilePort()
            profiles.listedProfiles += profile("instance-a", label = "A", active = true)
            profiles.listedProfiles += profile("instance-b", label = "B")
            profiles.activeProfile = profiles.listedProfiles.first()
            val viewModel = newViewModel(profiles)
            advanceUntilIdle()
            assertEquals(first, viewModel.state.value.activeProfile?.instance)
            assertEquals(0, viewModel.state.value.generation)

            profiles.activationResults.addLast(
                Outcome.Failure(CoreFailure("activation_failed", "cannot activate", true)),
            )
            viewModel.activate(second)
            advanceUntilIdle()
            assertEquals(first, viewModel.state.value.activeProfile?.instance)
            assertEquals(0, viewModel.state.value.generation)
            assertTrue(viewModel.state.value.activation is ProfileActivationState.Failed)

            viewModel.activate(second)
            advanceUntilIdle()
            assertEquals(second, viewModel.state.value.activeProfile?.instance)
            assertEquals(1, viewModel.state.value.generation)
            assertTrue(viewModel.state.value.activation is ProfileActivationState.Active)

            assertFalse(viewModel.acceptCallback(first, 0))
            assertFalse(viewModel.applyScopedData(first, 0, ProfileScopedData.Draft))
            assertTrue(viewModel.acceptCallback(second, 1))
            assertTrue(viewModel.applyScopedData(second, 1, ProfileScopedData.Draft))
        }

    @Test
    @DisplayName("AC-07.cross-instance drafts trust and credentials never reach another profile")
    fun ac07CrossInstanceDraftsTrustAndCredentialsNeverReachAnotherProfile() =
        runTest(dispatcher) {
            val profiles = FakeProfilePort()
            profiles.listedProfiles += profile("instance-a", label = "A", active = true)
            profiles.listedProfiles += profile("instance-b", label = "B")
            profiles.activeProfile = profiles.listedProfiles.first()
            profiles.scopes[first] = ProfileStorageScope(first, backupExcluded = true)
            profiles.scopes[second] = ProfileStorageScope(second, backupExcluded = true)
            val viewModel = newViewModel(profiles)
            advanceUntilIdle()

            assertTrue(viewModel.applyScopedData(first, 0, ProfileScopedData.Draft))
            assertTrue(viewModel.applyScopedData(first, 0, ProfileScopedData.Trust))
            assertTrue(viewModel.applyScopedData(first, 0, ProfileScopedData.Credential))
            assertTrue(viewModel.applyScopedData(first, 0, ProfileScopedData.RecoveryRecord))

            viewModel.activate(second)
            advanceUntilIdle()
            assertEquals(second, viewModel.state.value.activeProfile?.instance)
            assertFalse(viewModel.applyScopedData(first, 1, ProfileScopedData.Draft))
            assertFalse(viewModel.applyScopedData(first, 1, ProfileScopedData.Credential))
            assertTrue(viewModel.state.value.appliedScopedData[first].isNullOrEmpty())
            assertTrue(viewModel.state.value.appliedScopedData[second].isNullOrEmpty())
            assertTrue(viewModel.applyScopedData(second, 1, ProfileScopedData.Draft))
            assertEquals(2, viewModel.state.value.storageScopes.size)
            assertEquals(
                ProfileStorageScope(first, backupExcluded = true),
                viewModel.state.value.storageScopes[first],
            )
            assertEquals(
                ProfileStorageScope(second, backupExcluded = true),
                viewModel.state.value.storageScopes[second],
            )
            assertTrue(viewModel.state.value.profiles.all { it.backupExcluded })
        }

    @Test
    @DisplayName("AC-07.new-install a new installation starts empty with no replacement identity")
    fun ac07NewInstallANewInstallationStartsEmptyWithNoReplacementIdentity() =
        runTest(dispatcher) {
            val profiles = FakeProfilePort()
            profiles.credentialStates[first] = CredentialState.Absent
            val viewModel = newViewModel(profiles)
            advanceUntilIdle()

            assertTrue(viewModel.state.value.newInstall)
            assertNull(viewModel.state.value.activeProfile)
            assertTrue(viewModel.state.value.profiles.isEmpty())

            viewModel.refreshCredentials(first)
            advanceUntilIdle()
            assertEquals(CredentialState.Absent, viewModel.state.value.credentials[first])
            assertTrue(viewModel.state.value.profiles.isEmpty())
            assertNull(viewModel.state.value.activeProfile)
        }

    @Test
    @DisplayName("AC-07.keystore-absent a missing record does not create or replace an identity")
    fun ac07KeystoreAbsentAMissingRecordDoesNotCreateOrReplaceAnIdentity() =
        runTest(dispatcher) {
            val profiles = FakeProfilePort()
            profiles.listedProfiles += profile("instance-a", label = "A", active = true)
            profiles.activeProfile = profiles.listedProfiles.first()
            profiles.credentialStates[first] = CredentialState.Absent
            val viewModel = newViewModel(profiles)
            advanceUntilIdle()

            viewModel.refreshCredentials(first)
            advanceUntilIdle()

            assertEquals(CredentialState.Absent, viewModel.state.value.credentials[first])
            assertEquals(first, viewModel.state.value.activeProfile?.instance)
            assertEquals(1, viewModel.state.value.profiles.size)
            assertEquals(0, viewModel.state.value.generation)
        }

    @Test
    @DisplayName("AC-07.keystore-unavailable a temporary failure retains the previous identity")
    fun ac07KeystoreUnavailableATemporaryFailureRetainsThePreviousIdentity() =
        runTest(dispatcher) {
            val profiles = FakeProfilePort()
            profiles.listedProfiles += profile("instance-a", label = "A", active = true)
            profiles.activeProfile = profiles.listedProfiles.first()
            profiles.credentialStates[first] = CredentialState.Unavailable
            val viewModel = newViewModel(profiles)
            advanceUntilIdle()

            viewModel.refreshCredentials(first)
            advanceUntilIdle()

            assertEquals(CredentialState.Unavailable, viewModel.state.value.credentials[first])
            assertEquals(first, viewModel.state.value.activeProfile?.instance)
            assertEquals(1, viewModel.state.value.profiles.size)
            assertEquals(0, viewModel.state.value.generation)
        }

    @Test
    @DisplayName("AC-07.keystore-invalid an invalidated key preserves scope with no new identity")
    fun ac07KeystoreInvalidAnInvalidatedKeyPreservesScopeWithNoNewIdentity() =
        runTest(dispatcher) {
            val sink = RecordingDiagnosticSink()
            val profiles = FakeProfilePort()
            profiles.listedProfiles += profile("instance-a", label = "A", active = true)
            profiles.activeProfile = profiles.listedProfiles.first()
            profiles.credentialStates[first] = CredentialState.Invalid("key_invalidated")
            val viewModel = newViewModel(profiles, sink)
            advanceUntilIdle()

            viewModel.refreshCredentials(first)
            advanceUntilIdle()

            assertEquals(CredentialState.Invalid("key_invalidated"), viewModel.state.value.credentials[first])
            assertEquals(first, viewModel.state.value.activeProfile?.instance)
            assertEquals(1, viewModel.state.value.profiles.size)
            assertEquals(0, viewModel.state.value.generation)
            assertTrue(
                sink.events.any {
                    it.event == "profile_credential_invalid" &&
                        it.level == DiagnosticLevel.Error &&
                        it.code == "key_invalidated"
                },
            )
        }

    private fun newViewModel(
        profiles: FakeProfilePort,
        sink: RecordingDiagnosticSink = RecordingDiagnosticSink(),
    ): ProfilesViewModel = ProfilesViewModel(
        profiles = profiles,
        diagnostics = sink,
        dispatcher = dispatcher,
    )
}
