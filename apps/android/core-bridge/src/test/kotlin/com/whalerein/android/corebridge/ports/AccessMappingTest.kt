package com.whalerein.android.corebridge.ports

import com.whalerein.android.state.access.ExternalAuthorization
import com.whalerein.android.state.access.ExternalTarget
import com.whalerein.android.state.access.StartupAuthorization
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.InstanceKind
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import java.io.File
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir

@DisplayName("AC-25 Android access mapping")
class AccessMappingTest {
    @TempDir
    lateinit var directory: File

    private val instance = InstanceId("cloud-production")
    private val otherInstance = InstanceId("standalone")

    @Test
    @DisplayName("AC-25.cloud-attempt-mapping-uses-the-active-environment-and-a-concrete-expiry")
    fun cloudAttemptMappingUsesTheActiveEnvironmentAndAConcreteExpiry() {
        val attempt = cloudAttemptFrom(
            attemptId = "attempt-1",
            authorizationUrl = "https://auth.whalerein.test/oidc/auth?state=state%2Fone&prompt=login",
            redirectUri = "https://app.whalerein.test/client/android/callback",
            environment = "production",
            startedAtEpochMillis = 1_000_000L,
        )

        assertEquals("attempt-1", attempt.attemptId.value)
        assertEquals("production", attempt.environment)
        assertEquals("state/one", attempt.state)
        assertEquals(
            "https://app.whalerein.test/client/android/callback",
            attempt.redirectUri,
        )
        assertEquals(
            1_000_000L + AccessRecordStore.PENDING_ATTEMPT_LIFETIME_MILLIS,
            attempt.expiresAtEpochMillis,
        )
        assertTrue(attempt.expiresAtEpochMillis > 1_000_000L)
    }

    @Test
    @DisplayName("AC-25.cloud-session-mapping-keeps-account-workspace-and-environment")
    fun cloudSessionMappingKeepsAccountWorkspaceAndEnvironment() {
        val session = cloudSessionFrom(
            instance = instance,
            accountId = "account-1",
            workspaceId = "workspace-1",
            environment = "staging",
        )

        assertEquals(instance, session.instance)
        assertEquals("account-1", session.accountId)
        assertEquals("workspace-1", session.workspaceId)
        assertEquals("staging", session.environment)
    }

    @Test
    @DisplayName("AC-25.pending-attempt-survives-a-store-reopen-and-is-consumed-once")
    fun pendingAttemptSurvivesAStoreReopenAndIsConsumedOnce() {
        val store = AccessRecordStore(directory)
        val attempt = cloudAttemptFrom(
            attemptId = "attempt-restored",
            authorizationUrl = "https://auth.whalerein.test/oidc/auth?state=state-restored",
            redirectUri = "https://app.whalerein.test/client/android/callback",
            environment = "production",
            startedAtEpochMillis = 5_000L,
        )
        store.writePending(instance, attempt)

        // A new process opens the same directory and sees the same attempt.
        val reopened = AccessRecordStore(directory)
        assertEquals(attempt, reopened.readPending(instance))

        // Completing consumes it exactly once; a replayed callback sees none.
        reopened.clearPending(instance)
        assertNull(AccessRecordStore(directory).readPending(instance))
    }

    @Test
    @DisplayName("AC-25.pending-attempt-and-target-are-isolated-per-instance")
    fun pendingAttemptAndTargetAreIsolatedPerInstance() {
        val store = AccessRecordStore(directory)
        val attempt = cloudAttemptFrom(
            attemptId = "attempt-a",
            authorizationUrl = "https://auth.whalerein.test/oidc/auth?state=state-a",
            redirectUri = "https://app.whalerein.test/client/android/callback",
            environment = "production",
            startedAtEpochMillis = 1_000L,
        )
        store.writePending(instance, attempt)
        store.writeTarget(instance, session(instance, "host-1", "session-1"))

        store.writeTarget(otherInstance, session(otherInstance, "host-9", "session-9"))

        assertNull(store.readPending(otherInstance))
        assertEquals(
            session(instance, "host-1", "session-1"),
            store.readTarget(instance),
        )
        assertEquals(
            session(otherInstance, "host-9", "session-9"),
            store.readTarget(otherInstance),
        )
        store.clearTarget(instance)
        assertNull(store.readTarget(instance))
        assertEquals(
            session(otherInstance, "host-9", "session-9"),
            store.readTarget(otherInstance),
        )
    }

    @Test
    @DisplayName("AC-25.missing-last-target-uses-the-default-host-session-list")
    fun missingLastTargetUsesTheDefaultHostSessionList() {
        val snapshot = decideStartup(
            StartupRestoreEvidence(
                instance = instance,
                kind = InstanceKind.Cloud,
                signedIn = true,
                pairedHostIds = setOf("host-1", "host-2"),
                defaultHostId = "host-2",
            ),
        )

        assertEquals(StartupAuthorization.Authorized, snapshot.authorization)
        assertNull(snapshot.target)
        assertEquals(HostRef(instance, HostId("host-2")), snapshot.host)
    }

    @Test
    @DisplayName("AC-25.signed-out-launch-never-restores-the-previous-target")
    fun signedOutLaunchNeverRestoresThePreviousTarget() {
        val snapshot = decideStartup(
            StartupRestoreEvidence(
                instance = instance,
                kind = InstanceKind.Cloud,
                signedIn = false,
                pairedHostIds = setOf("host-1"),
                lastTarget = session(instance, "host-1", "session-1"),
                sessionIds = setOf("session-1"),
            ),
        )

        assertEquals(StartupAuthorization.SignedOut, snapshot.authorization)
        assertNull(snapshot.target)
        assertNull(snapshot.host)
    }

    @Test
    @DisplayName("AC-25.revoked-host-launch-never-restores-the-previous-target")
    fun revokedHostLaunchNeverRestoresThePreviousTarget() {
        val snapshot = decideStartup(
            StartupRestoreEvidence(
                instance = instance,
                kind = InstanceKind.Cloud,
                signedIn = true,
                pairedHostIds = setOf("host-2"),
                lastTarget = session(instance, "host-1", "session-1"),
                sessionIds = setOf("session-1"),
            ),
        )

        assertEquals(StartupAuthorization.Revoked, snapshot.authorization)
        assertNull(snapshot.target)
        assertNull(snapshot.host)
    }

    @Test
    @DisplayName("AC-25.online-launch-restores-the-authorized-session")
    fun onlineLaunchRestoresTheAuthorizedSession() {
        val target = session(instance, "host-1", "session-1")
        val snapshot = decideStartup(
            StartupRestoreEvidence(
                instance = instance,
                kind = InstanceKind.Cloud,
                signedIn = true,
                pairedHostIds = setOf("host-1"),
                lastTarget = target,
                sessionIds = setOf("session-1", "session-2"),
            ),
        )

        assertEquals(StartupAuthorization.Authorized, snapshot.authorization)
        assertEquals(target, snapshot.target)
        assertTrue(snapshot.online)
        assertFalse(snapshot.missingSession)
    }

    @Test
    @DisplayName("AC-25.missing-session-falls-back-to-the-correct-host-list")
    fun missingSessionFallsBackToTheCorrectHostList() {
        val snapshot = decideStartup(
            StartupRestoreEvidence(
                instance = instance,
                kind = InstanceKind.Cloud,
                signedIn = true,
                pairedHostIds = setOf("host-1", "host-2"),
                lastTarget = session(instance, "host-1", "removed-session"),
                sessionIds = setOf("session-2"),
            ),
        )

        assertTrue(snapshot.missingSession)
        assertNull(snapshot.target)
        assertEquals(HostRef(instance, HostId("host-1")), snapshot.host)
    }

    @Test
    @DisplayName("AC-25.offline-launch-uses-the-cached-history-when-available")
    fun offlineLaunchUsesTheCachedHistoryWhenAvailable() {
        val target = session(instance, "host-1", "session-1")
        val snapshot = decideStartup(
            StartupRestoreEvidence(
                instance = instance,
                kind = InstanceKind.Cloud,
                signedIn = true,
                pairedHostIds = setOf("host-1"),
                lastTarget = target,
                sessionIds = null,
                cachedSessionIds = setOf("session-1"),
                cachedMessageCount = 3,
            ),
        )

        assertTrue(snapshot.readOnlyCacheAvailable)
        assertFalse(snapshot.online)
        assertEquals(target, snapshot.target)
    }

    @Test
    @DisplayName("AC-25.offline-launch-reports-a-cache-miss-with-the-host-list")
    fun offlineLaunchReportsACacheMissWithTheHostList() {
        val target = session(instance, "host-1", "session-1")
        val snapshot = decideStartup(
            StartupRestoreEvidence(
                instance = instance,
                kind = InstanceKind.Cloud,
                signedIn = true,
                pairedHostIds = setOf("host-1"),
                lastTarget = target,
                sessionIds = null,
                cachedSessionIds = emptySet(),
                cachedMessageCount = 0,
            ),
        )

        assertFalse(snapshot.readOnlyCacheAvailable)
        assertFalse(snapshot.online)
        assertEquals(target, snapshot.target)
        assertEquals(HostRef(instance, HostId("host-1")), snapshot.host)
    }

    @Test
    @DisplayName("AC-25.external-entry-authorizes-a-current-session")
    fun externalEntryAuthorizesACurrentSession() {
        val authorization = decideExternalAuthorization(
            ExternalAuthorizationEvidence(
                target = ExternalTarget(
                    profileId = instance,
                    hostId = HostId("host-1"),
                    sessionId = "session-1",
                ),
                activeInstance = instance,
                kind = InstanceKind.Cloud,
                signedIn = true,
                pairedHostIds = setOf("host-1"),
                sessionIds = setOf("session-1"),
            ),
        )

        assertTrue(authorization is ExternalAuthorization.Authorized)
        assertEquals(
            Route.Conversation(session(instance, "host-1", "session-1")),
            (authorization as ExternalAuthorization.Authorized).route,
        )
    }

    @Test
    @DisplayName("AC-25.external-entry-denies-a-revoked-host-grant")
    fun externalEntryDeniesARevokedHostGrant() {
        val authorization = decideExternalAuthorization(
            ExternalAuthorizationEvidence(
                target = ExternalTarget(hostId = HostId("host-1"), sessionId = "session-1"),
                activeInstance = instance,
                kind = InstanceKind.Cloud,
                signedIn = true,
                pairedHostIds = setOf("host-1"),
                hostGrantRevoked = true,
                sessionIds = setOf("session-1"),
            ),
        )

        assertEquals(ExternalAuthorization.HostRevoked, authorization)
    }

    @Test
    @DisplayName("AC-25.external-entry-denies-a-foreign-instance")
    fun externalEntryDeniesAForeignInstance() {
        val authorization = decideExternalAuthorization(
            ExternalAuthorizationEvidence(
                target = ExternalTarget(
                    profileId = otherInstance,
                    hostId = HostId("host-1"),
                    sessionId = "session-1",
                ),
                activeInstance = instance,
                kind = InstanceKind.Cloud,
                signedIn = true,
                pairedHostIds = setOf("host-1"),
                sessionIds = setOf("session-1"),
            ),
        )

        assertEquals(ExternalAuthorization.WrongProfile, authorization)
    }

    @Test
    @DisplayName("AC-25.external-entry-denies-a-missing-session")
    fun externalEntryDeniesAMissingSession() {
        val authorization = decideExternalAuthorization(
            ExternalAuthorizationEvidence(
                target = ExternalTarget(hostId = HostId("host-1"), sessionId = "removed"),
                activeInstance = instance,
                kind = InstanceKind.Cloud,
                signedIn = true,
                pairedHostIds = setOf("host-1"),
                sessionIds = setOf("session-1"),
            ),
        )

        assertEquals(ExternalAuthorization.SessionNotFound, authorization)
    }

    @Test
    @DisplayName("AC-25.external-entry-denies-when-core-cannot-answer")
    fun externalEntryDeniesWhenCoreCannotAnswer() {
        val base = ExternalTarget(hostId = HostId("host-1"), sessionId = "session-1")

        assertEquals(
            ExternalAuthorization.Unauthorized,
            decideExternalAuthorization(
                ExternalAuthorizationEvidence(
                    target = base,
                    activeInstance = instance,
                    kind = InstanceKind.Cloud,
                    signedIn = true,
                    pairedHostIds = null,
                ),
            ),
        )
        assertEquals(
            ExternalAuthorization.Unauthorized,
            decideExternalAuthorization(
                ExternalAuthorizationEvidence(
                    target = base,
                    activeInstance = instance,
                    kind = InstanceKind.Cloud,
                    signedIn = true,
                    pairedHostIds = setOf("host-1"),
                    sessionIds = null,
                ),
            ),
        )
        assertEquals(
            ExternalAuthorization.Unauthorized,
            decideExternalAuthorization(
                ExternalAuthorizationEvidence(
                    target = base,
                    activeInstance = instance,
                    kind = InstanceKind.Cloud,
                    signedIn = false,
                    pairedHostIds = setOf("host-1"),
                ),
            ),
        )
    }

    @Test
    @DisplayName("AC-25.fingerprint-match-normalizes-the-display-form")
    fun fingerprintMatchNormalizesTheDisplayForm() {
        val canonical = "ab".repeat(32)
        val display = canonical.uppercase().chunked(2).joinToString(":")

        assertTrue(fingerprintMatches(display, listOf(canonical)))
        assertTrue(fingerprintMatches(canonical, listOf(display)))
        assertTrue(fingerprintMatches(" ${display.lowercase()} ", listOf(canonical)))
        assertEquals(canonical, normalizeFingerprint(display))
    }

    @Test
    @DisplayName("AC-25.fingerprint-mismatch-and-empty-input-are-rejected")
    fun fingerprintMismatchAndEmptyInputAreRejected() {
        val canonical = "ab".repeat(32)
        val other = "cd".repeat(32)

        assertFalse(fingerprintMatches(other, listOf(canonical)))
        assertFalse(fingerprintMatches("", listOf(canonical)))
        assertFalse(fingerprintMatches("not a fingerprint", listOf(canonical)))
        assertFalse(fingerprintMatches(canonical, listOf("")))
    }

    private fun session(
        instance: InstanceId,
        hostId: String,
        sessionId: String,
    ): SessionRef = SessionRef(
        host = HostRef(instance, HostId(hostId)),
        sessionId = SessionId(sessionId),
    )
}
