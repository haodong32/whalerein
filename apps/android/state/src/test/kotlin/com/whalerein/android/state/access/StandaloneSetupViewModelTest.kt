package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
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
class StandaloneSetupViewModelTest : AccessTest() {

    private val instance = InstanceId("instance-1")

    @Test
    @DisplayName("AC-06.public-ca public invitation pairs without a private CA import")
    fun ac06PublicCaPublicInvitationPairsWithoutAPrivateCAImport() = runTest(dispatcher) {
        val pairing = FakePairingPort()
        pairing.invitationResult = Outcome.Success(
            StandaloneInvitation("Desk Host", "AA:BB:CC", caRequired = false),
        )
        pairing.lanState = LanPermissionState.Granted
        val viewModel = newViewModel(pairing)
        viewModel.loadInvitation("invitation")
        advanceUntilIdle()
        viewModel.checkFingerprint("AA:BB:CC")
        advanceUntilIdle()
        viewModel.accept()
        advanceUntilIdle()

        assertEquals(StandalonePhase.Paired, viewModel.state.value.phase)
        assertEquals(CaImportState.NotRequired, viewModel.state.value.ca)
        assertEquals(
            Route.HostSessions(HostRef(instance, HostId("standalone-host"))),
            viewModel.state.value.destination,
        )
        assertTrue(pairing.importedPems.isEmpty())
        assertEquals(listOf(true), pairing.decidedAccepts)
    }

    @Test
    @DisplayName("AC-06.private-ca imported CA is scoped to the selected instance")
    fun ac06PrivateCaImportedCAIsScopedToTheSelectedInstance() = runTest(dispatcher) {
        val pairing = FakePairingPort()
        pairing.invitationResult = Outcome.Success(
            StandaloneInvitation("Lab Host", "11:22:33", caRequired = true),
        )
        pairing.lanState = LanPermissionState.Granted
        pairing.importCaResult = Outcome.Success(CaImportState.Ready(instance))
        val viewModel = newViewModel(pairing)
        viewModel.loadInvitation("invitation")
        advanceUntilIdle()
        viewModel.checkFingerprint("11:22:33")
        advanceUntilIdle()
        viewModel.importCa("-----BEGIN CERTIFICATE-----lab")
        advanceUntilIdle()
        assertEquals(CaImportState.Ready(instance), viewModel.state.value.ca)
        assertEquals(instance, viewModel.state.value.caScope)
        viewModel.accept()
        advanceUntilIdle()

        assertEquals(StandalonePhase.Paired, viewModel.state.value.phase)
        assertEquals(listOf("-----BEGIN CERTIFICATE-----lab"), pairing.importedPems)
        assertEquals(instance, viewModel.state.value.caScope)
    }

    @Test
    @DisplayName("AC-06.invalid fingerprint wrong hostname and wrong CA are distinguishable")
    fun ac06InvalidFingerprintWrongHostnameAndWrongCAAreDistinguishable() =
        runTest(dispatcher) {
            val mismatch = FakePairingPort()
            mismatch.invitationResult = Outcome.Success(
                StandaloneInvitation("Desk Host", "AA:BB:CC", caRequired = false),
            )
            mismatch.fingerprintResult = Outcome.Success(FingerprintCheck.Mismatch)
            mismatch.lanState = LanPermissionState.Granted
            val mismatchModel = newViewModel(mismatch)
            mismatchModel.loadInvitation("invitation")
            advanceUntilIdle()
            mismatchModel.checkFingerprint("00:00:00")
            advanceUntilIdle()
            assertEquals(false, mismatchModel.state.value.fingerprintMatches)
            assertEquals("fingerprint_mismatch", mismatchModel.state.value.error?.code)
            mismatchModel.accept()
            advanceUntilIdle()
            assertEquals("fingerprint_unverified", mismatchModel.state.value.error?.code)
            assertTrue(mismatch.decidedAccepts.isEmpty())

            val hostname = FakePairingPort()
            hostname.lanState = LanPermissionState.Granted
            hostname.validateTargetResult = Outcome.Failure(
                CoreFailure("tls_hostname", "certificate name mismatch", retryable = false),
            )
            val hostnameModel = newViewModel(hostname)
            hostnameModel.validateTarget("https://wrong.example.test")
            advanceUntilIdle()
            assertEquals("tls_hostname", hostnameModel.state.value.error?.code)

            val badCa = FakePairingPort()
            badCa.invitationResult = Outcome.Success(
                StandaloneInvitation("Lab Host", "11:22:33", caRequired = true),
            )
            badCa.lanState = LanPermissionState.Granted
            badCa.importCaResult = Outcome.Success(CaImportState.Rejected("ca_invalid"))
            val badCaModel = newViewModel(badCa)
            badCaModel.loadInvitation("invitation")
            advanceUntilIdle()
            badCaModel.checkFingerprint("11:22:33")
            advanceUntilIdle()
            badCaModel.importCa("not-a-certificate")
            advanceUntilIdle()
            assertEquals(CaImportState.Rejected("ca_invalid"), badCaModel.state.value.ca)
            badCaModel.accept()
            advanceUntilIdle()
            assertEquals("ca_required", badCaModel.state.value.error?.code)
            assertTrue(badCa.decidedAccepts.isEmpty())
        }

    @Test
    @DisplayName("AC-06.lan denial allow and later revocation gate pairing")
    fun ac06LanDenialAllowAndLaterRevocationGatePairing() = runTest(dispatcher) {
        val pairing = FakePairingPort()
        pairing.invitationResult = Outcome.Success(
            StandaloneInvitation("Desk Host", "AA:BB:CC", caRequired = false),
        )
        pairing.lanState = LanPermissionState.Denied
        pairing.requestLanState = LanPermissionState.Granted
        val viewModel = newViewModel(pairing)
        viewModel.loadInvitation("invitation")
        advanceUntilIdle()
        assertEquals(LanPermissionState.Denied, viewModel.state.value.lan)
        viewModel.checkFingerprint("AA:BB:CC")
        advanceUntilIdle()
        viewModel.accept()
        advanceUntilIdle()
        assertEquals("lan_permission_denied", viewModel.state.value.error?.code)
        assertTrue(pairing.decidedAccepts.isEmpty())

        viewModel.requestLanPermission()
        advanceUntilIdle()
        assertEquals(LanPermissionState.Granted, viewModel.state.value.lan)
        viewModel.accept()
        advanceUntilIdle()
        assertEquals(StandalonePhase.Paired, viewModel.state.value.phase)
        assertEquals(listOf(true), pairing.decidedAccepts)

        pairing.lanState = LanPermissionState.Revoked
        viewModel.refreshLanPermission()
        advanceUntilIdle()
        assertEquals(LanPermissionState.Revoked, viewModel.state.value.lan)
        assertEquals("lan_permission_revoked", viewModel.state.value.error?.code)
        viewModel.accept()
        advanceUntilIdle()
        assertEquals(listOf(true), pairing.decidedAccepts)
    }

    @Test
    @DisplayName("AC-06.tls-policy production rejects cleartext and public Cloud never asks for LAN")
    fun ac06TlsPolicyProductionRejectsCleartextAndPublicCloudNeverAsksForLAN() =
        runTest(dispatcher) {
            val pairing = FakePairingPort()
            val production = newViewModel(pairing, production = true, lanRequired = true)
            production.validateTarget("http://192.0.2.10:8443")
            advanceUntilIdle()
            assertEquals("cleartext_rejected", production.state.value.error?.code)
            assertEquals(0, pairing.lanRequests)

            val cloudPublic = newViewModel(pairing, production = true, lanRequired = false)
            cloudPublic.requestLanPermission()
            advanceUntilIdle()
            assertEquals(LanPermissionState.NotRequired, cloudPublic.state.value.lan)
            assertEquals(0, pairing.lanRequests)
            assertEquals(0, pairing.lanQueries)

            production.validateTarget("https://lab.example.test")
            advanceUntilIdle()
            assertNull(production.state.value.error)
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
