package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
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
class ProfilesViewModelCoverageTest : AccessTest() {

    private val first = InstanceId("instance-a")
    private val second = InstanceId("instance-b")

    @Test
    @DisplayName("AC-07.switch validation and save failures are typed and do not replace state")
    fun ac07SwitchValidationAndSaveFailuresAreTypedAndDoNotReplaceState() = runTest(dispatcher) {
        val profiles = FakeProfilePort()
        profiles.listedProfiles += profile("instance-a", label = "A", active = true)
        profiles.activeProfile = profiles.listedProfiles.first()
        val viewModel = newViewModel(profiles)
        advanceUntilIdle()

        profiles.validateResult = { Outcome.Success(ProfileValidation.Invalid("profile_schema")) }
        viewModel.validateAndSave(profile("instance-c", label = "C"))
        advanceUntilIdle()
        assertEquals("profile_schema", viewModel.state.value.error?.code)
        assertTrue(profiles.listedProfiles.none { it.instance == InstanceId("instance-c") })

        profiles.validateResult = { Outcome.Failure(CoreFailure("validate_io", "io", true)) }
        viewModel.validateAndSave(profile("instance-d", label = "D"))
        advanceUntilIdle()
        assertEquals("profile_invalid", viewModel.state.value.error?.code)

        profiles.validateResult = { Outcome.Success(ProfileValidation.Valid(it)) }
        profiles.saveResult = Outcome.Failure(CoreFailure("save_io", "io", true))
        viewModel.validateAndSave(profile("instance-e", label = "E"))
        advanceUntilIdle()
        assertEquals("save_io", viewModel.state.value.error?.code)

        profiles.saveResult = null
        viewModel.validateAndSave(profile("instance-f", label = "F"))
        advanceUntilIdle()
        assertTrue(viewModel.state.value.profiles.any { it.instance == InstanceId("instance-f") })
        assertNull(viewModel.state.value.error)
    }

    @Test
    @DisplayName("AC-07.switch removing active and inactive profiles keeps the instance boundary")
    fun ac07SwitchRemovingActiveAndInactiveProfilesKeepsTheInstanceBoundary() = runTest(dispatcher) {
        val profiles = FakeProfilePort()
        profiles.listedProfiles += profile("instance-a", label = "A", active = true)
        profiles.listedProfiles += profile("instance-b", label = "B")
        profiles.activeProfile = profiles.listedProfiles.first()
        val viewModel = newViewModel(profiles)
        advanceUntilIdle()

        viewModel.remove(second)
        advanceUntilIdle()
        assertEquals(1, viewModel.state.value.profiles.size)
        assertEquals(first, viewModel.state.value.activeProfile?.instance)
        assertEquals(0, viewModel.state.value.generation)

        viewModel.remove(first)
        advanceUntilIdle()
        assertNull(viewModel.state.value.activeProfile)
        assertEquals(1, viewModel.state.value.generation)
        assertTrue(viewModel.state.value.newInstall)
        assertFalse(viewModel.isCurrent(first, 0))

        val failure = FakeProfilePort()
        failure.listedProfiles += profile("instance-a", label = "A", active = true)
        failure.activeProfile = failure.listedProfiles.first()
        failure.removeResult = Outcome.Failure(CoreFailure("remove_io", "io", true))
        val failureModel = newViewModel(failure)
        advanceUntilIdle()
        failureModel.remove(first)
        advanceUntilIdle()
        assertEquals("remove_io", failureModel.state.value.error?.code)
        assertEquals(first, failureModel.state.value.activeProfile?.instance)

        val refreshed = FakeProfilePort()
        refreshed.listedProfiles += profile("instance-a", label = "A", active = true)
        refreshed.activeProfile = refreshed.listedProfiles.first()
        val refreshedModel = newViewModel(refreshed)
        advanceUntilIdle()
        refreshedModel.refresh()
        advanceUntilIdle()
        assertEquals(1, refreshedModel.state.value.profiles.size)
    }

    @Test
    @DisplayName("AC-07.keystore-unavailable storage scope and credential exceptions stay typed")
    fun ac07KeystoreUnavailableStorageScopeAndCredentialExceptionsStayTyped() = runTest(dispatcher) {
        val sink = RecordingDiagnosticSink()
        val profiles = FakeProfilePort()
        profiles.listedProfiles += profile("instance-a", label = "A", active = true)
        profiles.activeProfile = profiles.listedProfiles.first()
        profiles.storageScopeResult = Outcome.Success(ProfileStorageScope(first, backupExcluded = true))
        val viewModel = newViewModel(profiles, sink)
        advanceUntilIdle()

        viewModel.refreshStorageScope(first)
        advanceUntilIdle()
        assertEquals(ProfileStorageScope(first, true), viewModel.state.value.storageScopes[first])

        profiles.storageScopeResult = Outcome.Failure(CoreFailure("scope_io", "io", true))
        viewModel.refreshStorageScope(first)
        advanceUntilIdle()
        assertEquals("scope_io", viewModel.state.value.error?.code)

        profiles.credentialException = IllegalStateException("keystore unavailable")
        viewModel.refreshCredentials(first)
        advanceUntilIdle()
        assertEquals(CredentialState.Unavailable, viewModel.state.value.credentials[first])
        assertTrue(sink.events.any { it.event == "profile_credential_unavailable" })
    }

    @Test
    @DisplayName("AC-07.switch activation scope mismatch and missing profile rows are fenced")
    fun ac07SwitchActivationScopeMismatchAndMissingProfileRowsAreFenced() = runTest(dispatcher) {
        val mismatch = FakeProfilePort()
        mismatch.listedProfiles += profile("instance-a", label = "A", active = true)
        mismatch.activeProfile = mismatch.listedProfiles.first()
        mismatch.activationResults.addLast(
            Outcome.Success(
                ProfileActivation(
                    active = profile("instance-c", label = "C", active = true),
                    previous = mismatch.activeProfile,
                ),
            ),
        )
        val mismatchModel = newViewModel(mismatch)
        advanceUntilIdle()
        mismatchModel.activate(second)
        advanceUntilIdle()
        assertEquals(first, mismatchModel.state.value.activeProfile?.instance)
        assertEquals("profile_activation_scope", mismatchModel.state.value.error?.code)
        assertTrue(mismatchModel.state.value.activation is ProfileActivationState.Failed)

        val appended = FakeProfilePort()
        appended.listedProfiles += profile("instance-a", label = "A", active = true)
        appended.activeProfile = appended.listedProfiles.first()
        appended.activationResults.addLast(
            Outcome.Success(
                ProfileActivation(
                    active = profile("instance-c", label = "C", active = true),
                    previous = appended.activeProfile,
                ),
            ),
        )
        val appendedModel = newViewModel(appended)
        advanceUntilIdle()
        appendedModel.activate(InstanceId("instance-c"))
        advanceUntilIdle()
        assertEquals(2, appendedModel.state.value.profiles.size)
        assertEquals(InstanceId("instance-c"), appendedModel.state.value.activeProfile?.instance)

        val listFailure = FakeProfilePort()
        listFailure.listResult = Outcome.Failure(CoreFailure("profile_list_io", "io", true))
        val listFailureModel = newViewModel(listFailure)
        advanceUntilIdle()
        assertEquals("profile_list_io", listFailureModel.state.value.error?.code)
        assertFalse(listFailureModel.state.value.newInstall)

        val activeFailure = FakeProfilePort()
        activeFailure.listedProfiles += profile("instance-a", label = "A")
        activeFailure.activeResult = Outcome.Failure(CoreFailure("profile_active_io", "io", true))
        val activeFailureModel = newViewModel(activeFailure)
        advanceUntilIdle()
        assertEquals("profile_active_io", activeFailureModel.state.value.error?.code)
        assertFalse(activeFailureModel.state.value.newInstall)
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
