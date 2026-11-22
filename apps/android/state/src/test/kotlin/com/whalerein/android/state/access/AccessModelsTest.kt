package com.whalerein.android.state.access

import com.whalerein.android.state.core.AttemptId
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.InstanceKind
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import com.whalerein.android.state.fixtures.access.FakeExternalEntryPort
import com.whalerein.android.state.fixtures.access.RecordingDiagnosticSink
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class AccessModelsTest : AccessTest() {

    private val instance = InstanceId("instance-a")
    private val host = HostRef(instance, HostId("host-1"))
    private val session = SessionRef(host, SessionId("session-1"))

    @Test
    @DisplayName("AC-01.app-session models expose stable value identity and copies")
    fun ac01AppSessionModelsExposeStableValueIdentityAndCopies() {
        val profile = Profile(instance, "Profile", InstanceKind.Cloud, true, true)
        assertEquals(profile, profile.copy())
        assertNotEquals(profile, profile.copy(label = "Other"))
        assertEquals(profile.hashCode(), profile.copy().hashCode())
        assertEquals(instance, profile.instance)
        assertEquals("Profile", profile.label)
        assertEquals(InstanceKind.Cloud, profile.kind)
        assertTrue(profile.active)
        assertTrue(profile.backupExcluded)
        assertTrue(profile.toString().contains("Profile"))

        val scope = ProfileStorageScope(instance, true)
        assertEquals(scope, scope.copy())
        assertEquals(instance, scope.instance)
        assertTrue(scope.backupExcluded)

        val sessionValue = CloudSession(instance, "account", "workspace", "local")
        assertEquals(sessionValue, sessionValue.copy())
        assertEquals(instance, sessionValue.instance)
        assertEquals("account", sessionValue.accountId)
        assertEquals("workspace", sessionValue.workspaceId)
        assertEquals("local", sessionValue.environment)

        val attempt = CloudAttempt(
            AttemptId("attempt-1"),
            "https://auth.whalerein.test/oidc/auth",
            "state-1",
            "https://app.whalerein.test/client/android/callback",
            "local",
            Long.MAX_VALUE,
            expectedAccountId = "account",
            expectedWorkspaceId = "workspace",
        )
        assertEquals(attempt, attempt.copy())
        assertEquals("account", attempt.expectedAccountId)
        assertEquals("workspace", attempt.expectedWorkspaceId)
        assertEquals("state-1", attempt.state)

        val hostView = DirectoryHostView(HostId("host-1"), "Host", true, HostAvailability.Ready, true)
        assertEquals(hostView, hostView.copy())
        assertEquals("Host", hostView.label)
        assertEquals(HostAvailability.Ready, hostView.availability)
        assertTrue(hostView.connectable)

        val connection = HostConnection(hostView, sessionValue)
        assertEquals(connection, connection.copy())
        assertEquals(hostView, connection.host)
        assertEquals(sessionValue, connection.session)

        val selection = HostSelection(HostId("host-1"))
        assertEquals(selection, selection.copy())
        assertEquals(HostId("host-1"), selection.hostId)

        val activation = ProfileActivation(profile, null)
        assertEquals(activation, activation.copy())
        assertEquals(profile, activation.active)
        assertNull(activation.previous)

        val snapshot = StartupSnapshot(target = session, host = host)
        assertEquals(snapshot, snapshot.copy())
        assertEquals(session, snapshot.target)
        assertEquals(host, snapshot.host)
        assertTrue(snapshot.online)

        val destination = StartupDestination(Route.Conversation(session), StartupEvidence.ValidPreviousConversation)
        assertEquals(destination, destination.copy())
        assertEquals(Route.Conversation(session), destination.route)
        assertEquals(StartupEvidence.ValidPreviousConversation, destination.evidence)

        val invitation = StandaloneInvitation("Desk", "AA:BB", true)
        assertEquals(invitation, invitation.copy())
        assertEquals("Desk", invitation.hostLabel)
        assertEquals("AA:BB", invitation.fingerprint)
        assertTrue(invitation.caRequired)

        assertEquals(StandaloneTarget("https://lab.test"), StandaloneTarget("https://lab.test"))
        assertEquals(CaImportState.Ready(instance), CaImportState.Ready(instance))
        assertNotEquals(CaImportState.Ready(instance), CaImportState.Ready(InstanceId("other")))
        assertEquals(CredentialState.Invalid("key"), CredentialState.Invalid("key"))
        assertEquals(ProfileValidation.Invalid("bad"), ProfileValidation.Invalid("bad"))
        assertEquals(ProfileValidation.Valid(profile), ProfileValidation.Valid(profile))
        assertEquals(TransportValidation.Rejected("tls"), TransportValidation.Rejected("tls"))
        assertEquals(PairingResult.Rejected("denied"), PairingResult.Rejected("denied"))
        assertEquals(ExternalAuthorization.WrongProfile, ExternalAuthorization.WrongProfile)
        assertEquals(ExternalItemState.Pending, ExternalItemState.Pending)
    }

    @Test
    @DisplayName("AC-19.entry external target and resolution values keep optional scope fields")
    fun ac19EntryExternalTargetAndResolutionValuesKeepOptionalScopeFields() {
        val target = ExternalTarget(instance, host.hostId, session.sessionId.value, "item-1")
        assertEquals(target, target.copy())
        assertEquals(instance, target.profileId)
        assertEquals(host.hostId, target.hostId)
        assertEquals(session.sessionId.value, target.sessionId)
        assertEquals("item-1", target.itemId)
        assertNull(ExternalTarget().profileId)

        val resolution = ExternalEntryResolution(
            route = Route.Conversation(session),
            reason = ExternalEntryReason.Resolved,
            itemState = ExternalItemState.Pending,
            revealNothing = false,
        )
        assertEquals(resolution, resolution.copy())
        assertEquals(Route.Conversation(session), resolution.route)
        assertEquals(ExternalEntryReason.Resolved, resolution.reason)
        assertEquals(ExternalItemState.Pending, resolution.itemState)
        assertFalse(resolution.revealNothing)

        val authorized = ExternalAuthorization.Authorized(Route.Conversation(session))
        assertEquals(authorized, authorized.copy())
        assertEquals(ExternalItemState.NotRequested, authorized.itemState)
        assertNull(authorized.pendingItemId)
    }

    @Test
    @DisplayName("AC-03.unconfirmed guarded calls map unexpected exceptions and preserve cancellation")
    fun ac03UnconfirmedGuardedCallsMapUnexpectedExceptionsAndPreserveCancellation() = runTest(dispatcher) {
        val sink = RecordingDiagnosticSink()
        val failure = guardedPortCall<Unit>(sink, AccessStage.Login, "test_port", instance) {
            throw IllegalStateException("https://callback.example/secret")
        }
        assertTrue(failure is Outcome.Failure)
        assertEquals("unexpected_failure", (failure as Outcome.Failure).failure.code)
        assertTrue(failure.failure.retryable)
        assertTrue(sink.events.any { it.event == "test_port_exception" && it.stage == "login" })
        assertTrue(sink.events.all { it.fields.isEmpty() })
        assertFalse(sink.events.any { it.code?.contains("callback.example") == true })

        val cancelled = try {
            guardedPortCall<Unit>(sink, AccessStage.Login, "test_cancel") {
                throw CancellationException("cancelled")
            }
            false
        } catch (_: CancellationException) {
            true
        }
        assertTrue(cancelled)
    }

    @Test
    @DisplayName("AC-03.unconfirmed outcome presentation maps every result kind")
    fun ac03UnconfirmedOutcomePresentationMapsEveryResultKind() {
        val coreFailure = CoreFailure("core", "core", false)
        assertEquals("core", (Outcome.Failure(coreFailure)).asFailure().code)
        assertEquals("core", (Outcome.Unconfirmed(coreFailure)).asFailure().code)
        val cancelled: Outcome<Unit> = Outcome.Cancelled
        assertEquals("cancelled", cancelled.asFailure().code)
        val success: Outcome<Unit> = Outcome.Success(Unit)
        assertEquals("unexpected_success", success.asFailure().code)
    }

    @Test
    @DisplayName("AC-19.entry external resolver maps unexpected profile and authorization exceptions")
    fun ac19EntryExternalResolverMapsUnexpectedProfileAndAuthorizationExceptions() = runTest(dispatcher) {
        val entry = FakeExternalEntryPort()
        entry.currentProfileResult = Outcome.Success(Profile(instance, "P", InstanceKind.Cloud, true, true))
        entry.currentProfileException = IllegalStateException("current profile failed")
        val resolver = ExternalEntryResolver(entry, RecordingDiagnosticSink())

        val profileFailure = resolver.resolve(ExternalTarget(profileId = instance))
        assertNull(profileFailure.route)
        assertEquals(ExternalEntryReason.Unauthorized, profileFailure.reason)
        assertTrue(profileFailure.revealNothing)

        entry.currentProfileException = null
        entry.authorizeException = IllegalStateException("authorize failed")
        val authorizationFailure = resolver.resolve(
            ExternalTarget(profileId = instance, hostId = host.hostId),
        )
        assertNull(authorizationFailure.route)
        assertEquals(ExternalEntryReason.Unauthorized, authorizationFailure.reason)
        assertTrue(authorizationFailure.revealNothing)
    }
}
