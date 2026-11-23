package com.whalerein.android.corebridge.ports

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.support.ForgetMode
import com.whalerein.android.state.support.ForgetReport
import com.whalerein.android.state.support.RetentionCertainty
import com.whalerein.android.state.support.RevocationReport
import com.whalerein.android.state.support.RevocationScope
import kotlinx.coroutines.runBlocking
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/**
 * Proves the bridge maps one confirmed target to exactly one Core operation.
 *
 * The recording backend keeps per-target state, so a test fails if revoking
 * Host A also removes Host B or the current device.
 */
class SupportScopeMappingTest {
    private val hostA = HostId("host-a")
    private val hostB = HostId("host-b")

    private class RecordingBackend : ScopedRevocationBackend {
        val cloudGrants = linkedSetOf(HostId("host-a"), HostId("host-b"))
        val standaloneDevices = linkedSetOf("device-a", "device-b")
        val currentDeviceRevocations = mutableListOf<Unit>()
        val forgot = mutableListOf<ForgetMode>()
        var failure: Outcome<Nothing>? = null

        override suspend fun revokeCurrentDevice(): Outcome<RevocationResult> {
            if (failure != null) return failure as Outcome<RevocationResult>
            currentDeviceRevocations += Unit
            cloudGrants.clear()
            standaloneDevices.clear()
            return Outcome.Success(RevocationResult())
        }

        override suspend fun revokeCloudHostGrant(hostId: HostId): Outcome<RevocationResult> {
            if (failure != null) return failure as Outcome<RevocationResult>
            val removed = cloudGrants.remove(hostId)
            return if (removed) {
                Outcome.Success(RevocationResult())
            } else {
                Outcome.Failure(CoreFailure("unknown_grant", "No such grant."))
            }
        }

        override suspend fun revokeStandaloneDevice(deviceId: String): Outcome<RevocationResult> {
            if (failure != null) return failure as Outcome<RevocationResult>
            val removed = standaloneDevices.remove(deviceId)
            return if (removed) {
                Outcome.Success(RevocationResult())
            } else {
                Outcome.Failure(CoreFailure("unknown_device", "No such device."))
            }
        }

        override suspend fun forget(mode: ForgetMode): Outcome<Unit> {
            forgot += mode
            return Outcome.Success(Unit)
        }
    }

    @Test
    @DisplayName("AC-26.scope-cloud-grant-revokes-only-the-selected-host")
    fun ac26ScopeCloudGrantRevokesOnlyTheSelectedHost() = runBlocking {
        val backend = RecordingBackend()
        val revoker = ScopedAccountRevoker(backend)

        val outcome = revoker.revoke(RevocationScope.CloudHostGrant(hostA))

        assertTrue(outcome is Outcome.Success)
        val report = (outcome as Outcome.Success).value
        assertEquals(RevocationScope.CloudHostGrant(hostA), report.scope)
        assertEquals(RetentionCertainty.Certain, report.retention)
        assertEquals(setOf(hostB), backend.cloudGrants)
        assertEquals(emptyList<Unit>(), backend.currentDeviceRevocations)
        assertEquals(setOf("device-a", "device-b"), backend.standaloneDevices)
    }

    @Test
    @DisplayName("AC-26.scope-standalone-device-revokes-only-the-selected-device")
    fun ac26ScopeStandaloneDeviceRevokesOnlyTheSelectedDevice() = runBlocking {
        val backend = RecordingBackend()
        val revoker = ScopedAccountRevoker(backend)

        val outcome = revoker.revoke(RevocationScope.StandaloneDevice("device-a"))

        assertTrue(outcome is Outcome.Success)
        assertEquals(
            RevocationScope.StandaloneDevice("device-a"),
            (outcome as Outcome.Success).value.scope,
        )
        assertEquals(setOf("device-b"), backend.standaloneDevices)
        assertEquals(setOf(hostA, hostB), backend.cloudGrants)
        assertTrue(backend.currentDeviceRevocations.isEmpty())
    }

    @Test
    @DisplayName("AC-26.scope-current-device-is-distinct-from-a-selected-target")
    fun ac26ScopeCurrentDeviceIsDistinctFromASelectedTarget() = runBlocking {
        val backend = RecordingBackend()
        val revoker = ScopedAccountRevoker(backend)

        val outcome = revoker.revoke(RevocationScope.CurrentDevice)

        assertEquals(
            RevocationScope.CurrentDevice,
            (outcome as Outcome.Success).value.scope,
        )
        assertEquals(1, backend.currentDeviceRevocations.size)
        assertTrue(backend.cloudGrants.isEmpty())
        assertTrue(backend.standaloneDevices.isEmpty())
    }

    @Test
    @DisplayName("AC-26.forget-local-only-uses-the-local-core-mode")
    fun ac26ForgetLocalOnlyUsesTheLocalCoreMode() = runBlocking {
        val backend = RecordingBackend()
        val revoker = ScopedAccountRevoker(backend)

        val ordinary = revoker.forget(ForgetMode.Ordinary)
        val localOnly = revoker.forget(ForgetMode.LocalOnly)

        assertEquals(
            ForgetReport(ForgetMode.Ordinary, forgotten = true, remoteAuthorityRetained = false),
            (ordinary as Outcome.Success).value,
        )
        assertEquals(
            ForgetReport(ForgetMode.LocalOnly, forgotten = true, remoteAuthorityRetained = true),
            (localOnly as Outcome.Success).value,
        )
        assertEquals(listOf(ForgetMode.Ordinary, ForgetMode.LocalOnly), backend.forgot)
        assertEquals(ForgetCommand.Ordinal, SupportScopeMapping.forget(ForgetMode.Ordinary))
        assertEquals(ForgetCommand.LocalOnly, SupportScopeMapping.forget(ForgetMode.LocalOnly))
    }

    @Test
    @DisplayName("AC-26.scope-app-session-never-reaches-a-device-operation")
    fun ac26ScopeAppSessionNeverReachesADeviceOperation() = runBlocking {
        val backend = RecordingBackend()
        val revoker = ScopedAccountRevoker(backend)

        val outcome = revoker.revoke(RevocationScope.AppSession("session-a"))

        assertTrue(outcome is Outcome.Failure)
        assertEquals("revocation_scope_mismatch", (outcome as Outcome.Failure).failure.code)
        assertTrue(backend.currentDeviceRevocations.isEmpty())
        assertFalse(backend.cloudGrants.isEmpty())
        assertFalse(backend.standaloneDevices.isEmpty())
    }

    @Test
    @DisplayName("AC-26.failed-target-revocation-preserves-other-authorized-targets")
    fun ac26FailedTargetRevocationPreservesOtherAuthorizedTargets() = runBlocking {
        val backend = RecordingBackend().apply {
            failure = Outcome.Failure(CoreFailure("unavailable", "Core is unavailable."))
        }
        val revoker = ScopedAccountRevoker(backend)

        val outcome = revoker.revoke(RevocationScope.CloudHostGrant(hostA))

        assertTrue(outcome is Outcome.Failure)
        assertEquals(setOf(hostA, hostB), backend.cloudGrants)
        assertEquals(setOf("device-a", "device-b"), backend.standaloneDevices)
        assertTrue(backend.currentDeviceRevocations.isEmpty())
    }

    @Test
    @DisplayName("AC-26.scope-mapping-names-each-target-exactly")
    fun ac26ScopeMappingNamesEachTargetExactly() {
        assertEquals(
            RevocationCommand.CloudHostGrant(hostA),
            SupportScopeMapping.revocation(RevocationScope.CloudHostGrant(hostA)),
        )
        assertEquals(
            RevocationCommand.StandaloneDevice("device-a"),
            SupportScopeMapping.revocation(RevocationScope.StandaloneDevice("device-a")),
        )
        assertEquals(
            RevocationCommand.CurrentDevice,
            SupportScopeMapping.revocation(RevocationScope.CurrentDevice),
        )
        assertEquals(
            RevocationCommand.CloudAppSession("session-a"),
            SupportScopeMapping.revocation(RevocationScope.AppSession("session-a")),
        )
        assertEquals(
            RevocationCommand.AllCloudAppSessions,
            SupportScopeMapping.revocation(RevocationScope.AllAppSessions),
        )
    }

    @Test
    @DisplayName("AC-26.revocable-targets-advertise-only-supported-cloud-grants")
    fun ac26RevocableTargetsAdvertiseOnlySupportedCloudGrants() {
        val targets = RevocableTargetPolicy.targets(
            service = RevocableTargetPolicy.CLOUD_SERVICE,
            hosts = listOf(
                HostGrantCandidate("host-a", "ready"),
                HostGrantCandidate("host-b", "revoked"),
                HostGrantCandidate("host-c", "ready"),
            ),
        )

        assertEquals(listOf(HostId("host-a"), HostId("host-c")), targets.cloudHostGrants)
        assertTrue(targets.standaloneDevices.isEmpty())
    }

    @Test
    @DisplayName("AC-26.standalone-never-advertises-an-unsupported-device-revocation")
    fun ac26StandaloneNeverAdvertisesAnUnsupportedDeviceRevocation() {
        val targets = RevocableTargetPolicy.targets(
            service = "standalone",
            hosts = listOf(HostGrantCandidate("host-a", "ready")),
        )

        assertTrue(targets.cloudHostGrants.isEmpty())
        assertTrue(targets.standaloneDevices.isEmpty())
    }
}
