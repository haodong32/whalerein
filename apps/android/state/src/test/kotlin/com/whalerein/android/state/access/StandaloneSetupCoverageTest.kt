package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.fixtures.access.FakePairingPort
import com.whalerein.android.state.fixtures.access.RecordingDiagnosticSink
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class StandaloneSetupCoverageTest : AccessTest() {

    private val instance = InstanceId("instance-1")

    @Test
    @DisplayName("AC-06.public-ca public review skips CA and LAN requirements")
    fun ac06PublicCaPublicReviewSkipsCaAndLanRequirements() = runTest(dispatcher) {
        val pairing = FakePairingPort()
        pairing.lanState = LanPermissionState.Granted
        val viewModel = newViewModel(pairing, lanRequired = false)
        viewModel.loadInvitation("public")
        advanceUntilIdle()
        assertEquals(LanPermissionState.NotRequired, viewModel.state.value.lan)
        assertEquals(0, pairing.lanQueries)

        viewModel.importCa("ignored")
        advanceUntilIdle()
        assertEquals(CaImportState.NotRequired, viewModel.state.value.ca)
        assertTrue(pairing.importedPems.isEmpty())

        viewModel.requestLanPermission()
        advanceUntilIdle()
        assertEquals(LanPermissionState.NotRequired, viewModel.state.value.lan)
        assertEquals(0, pairing.lanRequests)

        viewModel.refreshLanPermission()
        advanceUntilIdle()
        assertEquals(LanPermissionState.NotRequired, viewModel.state.value.lan)
        assertEquals(0, pairing.lanQueries)
    }

    @Test
    @DisplayName("AC-06.invalid review and fingerprint failures remain typed")
    fun ac06InvalidReviewAndFingerprintFailuresRemainTyped() = runTest(dispatcher) {
        val reviewFailure = FakePairingPort()
        reviewFailure.invitationResult = Outcome.Failure(CoreFailure("bad_invitation", "bad", false))
        val reviewModel = newViewModel(reviewFailure)
        reviewModel.loadInvitation("bad")
        advanceUntilIdle()
        assertEquals(StandalonePhase.Idle, reviewModel.state.value.phase)
        assertEquals("bad_invitation", reviewModel.state.value.error?.code)

        val fingerprintFailure = FakePairingPort()
        fingerprintFailure.fingerprintResult = Outcome.Failure(CoreFailure("fingerprint_lookup", "bad", true))
        val fingerprintModel = newViewModel(fingerprintFailure)
        fingerprintModel.loadInvitation("invitation")
        advanceUntilIdle()
        fingerprintModel.checkFingerprint("AA")
        advanceUntilIdle()
        assertNull(fingerprintModel.state.value.fingerprintMatches)
        assertEquals("fingerprint_lookup", fingerprintModel.state.value.error?.code)
    }

    @Test
    @DisplayName("AC-06.private-ca CA scope mismatch and import failures stay scoped")
    fun ac06PrivateCaCaScopeMismatchAndImportFailuresStayScoped() = runTest(dispatcher) {
        val other = InstanceId("instance-2")
        val mismatch = FakePairingPort()
        mismatch.invitationResult = Outcome.Success(StandaloneInvitation("Lab", "AA", caRequired = true))
        mismatch.importCaResult = Outcome.Success(CaImportState.Ready(other))
        val mismatchModel = newViewModel(mismatch)
        mismatchModel.loadInvitation("invitation")
        advanceUntilIdle()
        mismatchModel.importCa("pem")
        advanceUntilIdle()
        assertEquals(CaImportState.Rejected("ca_scope_mismatch"), mismatchModel.state.value.ca)
        assertNull(mismatchModel.state.value.caScope)
        assertEquals("ca_scope_mismatch", mismatchModel.state.value.error?.code)

        val failed = FakePairingPort()
        failed.invitationResult = Outcome.Success(StandaloneInvitation("Lab", "AA", caRequired = true))
        failed.importCaResult = Outcome.Failure(CoreFailure("ca_store", "store failed", true))
        val failedModel = newViewModel(failed)
        failedModel.loadInvitation("invitation")
        advanceUntilIdle()
        failedModel.importCa("pem")
        advanceUntilIdle()
        assertEquals(CaImportState.Rejected("ca_store"), failedModel.state.value.ca)
        assertEquals("ca_store", failedModel.state.value.error?.code)

        val rejected = FakePairingPort()
        rejected.invitationResult = Outcome.Success(StandaloneInvitation("Lab", "AA", caRequired = true))
        rejected.importCaResult = Outcome.Success(CaImportState.Rejected("ca_invalid"))
        val rejectedModel = newViewModel(rejected)
        rejectedModel.loadInvitation("invitation")
        advanceUntilIdle()
        rejectedModel.importCa("pem")
        advanceUntilIdle()
        assertEquals(CaImportState.Rejected("ca_invalid"), rejectedModel.state.value.ca)
        assertNull(rejectedModel.state.value.caScope)
    }

    @Test
    @DisplayName("AC-06.lan request outcomes and permission query failures stay typed")
    fun ac06LanRequestOutcomesAndPermissionQueryFailuresStayTyped() = runTest(dispatcher) {
        val denied = FakePairingPort()
        denied.lanState = LanPermissionState.Granted
        denied.requestLanState = LanPermissionState.Denied
        val deniedModel = newViewModel(denied)
        deniedModel.requestLanPermission()
        advanceUntilIdle()
        assertEquals(LanPermissionState.Denied, deniedModel.state.value.lan)
        assertEquals("lan_permission_denied", deniedModel.state.value.error?.code)

        val notRequired = FakePairingPort()
        notRequired.lanState = LanPermissionState.Granted
        notRequired.requestLanState = LanPermissionState.NotRequired
        val notRequiredModel = newViewModel(notRequired)
        notRequiredModel.requestLanPermission()
        advanceUntilIdle()
        assertEquals(LanPermissionState.NotRequired, notRequiredModel.state.value.lan)
        assertNull(notRequiredModel.state.value.error)

        val queryFailure = FakePairingPort()
        queryFailure.lanPermissionException = IllegalStateException("permission query failed")
        val queryModel = newViewModel(queryFailure)
        queryModel.refreshLanPermission()
        advanceUntilIdle()
        assertEquals(LanPermissionState.Unknown, queryModel.state.value.lan)

        val requestFailure = FakePairingPort()
        requestFailure.requestLanPermissionException = IllegalStateException("permission request failed")
        val requestModel = newViewModel(requestFailure)
        requestModel.requestLanPermission()
        advanceUntilIdle()
        assertEquals(LanPermissionState.Denied, requestModel.state.value.lan)
        assertTrue(requestModel.state.value.error != null)
    }

    @Test
    @DisplayName("AC-06.tls-policy rejected targets development cleartext and port failures are typed")
    fun ac06TlsPolicyRejectedTargetsDevelopmentCleartextAndPortFailuresAreTyped() = runTest(dispatcher) {
        val rejected = FakePairingPort()
        rejected.validateTargetResult = Outcome.Success(TransportValidation.Rejected("tls_hostname"))
        val rejectedModel = newViewModel(rejected)
        rejectedModel.validateTarget("https://wrong.example.test")
        advanceUntilIdle()
        assertEquals("tls_hostname", rejectedModel.state.value.error?.code)

        val failed = FakePairingPort()
        failed.validateTargetResult = Outcome.Failure(CoreFailure("tls_unavailable", "unavailable", true))
        val failedModel = newViewModel(failed)
        failedModel.validateTarget("https://lab.example.test")
        advanceUntilIdle()
        assertEquals("tls_unavailable", failedModel.state.value.error?.code)

        val development = FakePairingPort()
        val developmentModel = newViewModel(development, production = false)
        developmentModel.validateTarget("http://192.0.2.10:8443")
        advanceUntilIdle()
        assertNull(developmentModel.state.value.error)
    }

    @Test
    @DisplayName("AC-06.invalid pairing guards and decide outcomes are distinguishable")
    fun ac06InvalidPairingGuardsAndDecideOutcomesAreDistinguishable() = runTest(dispatcher) {
        val missing = newViewModel(FakePairingPort())
        missing.accept()
        advanceUntilIdle()
        assertEquals("invitation_missing", missing.state.value.error?.code)

        val cancelled = FakePairingPort()
        cancelled.lanState = LanPermissionState.Granted
        val cancelledModel = newViewModel(cancelled)
        cancelledModel.loadInvitation("invitation")
        advanceUntilIdle()
        cancelledModel.reject()
        advanceUntilIdle()
        assertEquals(StandalonePhase.Cancelled, cancelledModel.state.value.phase)
        cancelledModel.accept()
        advanceUntilIdle()
        assertEquals("invitation_cancelled", cancelledModel.state.value.error?.code)

        val hostRejected = FakePairingPort()
        hostRejected.lanState = LanPermissionState.Granted
        hostRejected.decideResult = {
            Outcome.Success(PairingResult.Rejected("host_denied"))
        }
        val hostRejectedModel = newViewModel(hostRejected)
        hostRejectedModel.loadInvitation("invitation")
        advanceUntilIdle()
        hostRejectedModel.checkFingerprint("AA")
        advanceUntilIdle()
        hostRejectedModel.accept()
        advanceUntilIdle()
        assertEquals(StandalonePhase.Reviewing, hostRejectedModel.state.value.phase)
        assertEquals("host_denied", hostRejectedModel.state.value.error?.code)

        val pairFailed = FakePairingPort()
        pairFailed.lanState = LanPermissionState.Granted
        pairFailed.decideResult = {
            Outcome.Failure(CoreFailure("pair_transport", "failed", true))
        }
        val pairFailedModel = newViewModel(pairFailed)
        pairFailedModel.loadInvitation("invitation")
        advanceUntilIdle()
        pairFailedModel.checkFingerprint("AA")
        advanceUntilIdle()
        pairFailedModel.accept()
        advanceUntilIdle()
        assertEquals(StandalonePhase.Reviewing, pairFailedModel.state.value.phase)
        assertEquals("pair_transport", pairFailedModel.state.value.error?.code)

        val paired = FakePairingPort()
        paired.lanState = LanPermissionState.Granted
        val pairedModel = newViewModel(paired)
        pairedModel.loadInvitation("invitation")
        advanceUntilIdle()
        pairedModel.checkFingerprint("AA")
        advanceUntilIdle()
        pairedModel.accept()
        advanceUntilIdle()
        assertEquals(StandalonePhase.Paired, pairedModel.state.value.phase)
        pairedModel.accept()
        advanceUntilIdle()
        assertEquals(listOf(true), paired.decidedAccepts)
    }

    private fun newViewModel(
        pairing: FakePairingPort,
        production: Boolean = true,
        lanRequired: Boolean = true,
    ): StandaloneSetupViewModel = StandaloneSetupViewModel(
        instance = instance,
        pairing = pairing,
        diagnostics = RecordingDiagnosticSink(),
        production = production,
        lanRequired = lanRequired,
        dispatcher = dispatcher,
    )
}
