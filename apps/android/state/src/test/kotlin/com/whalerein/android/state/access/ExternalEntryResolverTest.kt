package com.whalerein.android.state.access

import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import com.whalerein.android.state.fixtures.access.FakeExternalEntryPort
import com.whalerein.android.state.fixtures.access.RecordingDiagnosticSink
import com.whalerein.android.state.fixtures.access.profile
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class ExternalEntryResolverTest : AccessTest() {

    private val instance = InstanceId("instance-a")
    private val host = HostRef(instance, HostId("host-1"))
    private val session = SessionRef(host, SessionId("session-1"))
    private val target = ExternalTarget(
        profileId = instance,
        hostId = host.hostId,
        sessionId = session.sessionId.value,
        itemId = "item-1",
    )

    @Test
    @DisplayName("AC-19.entry cold and warm hints resolve the same current authorized target")
    fun ac19EntryColdAndWarmHintsResolveTheSameCurrentAuthorizedTarget() =
        runTest(dispatcher) {
            val entry = FakeExternalEntryPort()
            entry.currentProfileResult = Outcome.Success(profile("instance-a", active = true))
            entry.authorizeResult = {
                ExternalAuthorization.Authorized(
                    route = Route.Conversation(session),
                    itemState = ExternalItemState.Pending,
                    pendingItemId = "item-1",
                )
            }
            val resolver = newResolver(entry)

            val cold = resolver.resolve(target)
            val warm = resolver.resolve(target)

            val expected = Route.Conversation(session, focusItemId = "item-1")
            assertEquals(expected, cold.route)
            assertEquals(expected, warm.route)
            assertEquals(ExternalEntryReason.Resolved, cold.reason)
            assertEquals(ExternalItemState.Pending, cold.itemState)
            assertEquals(2, entry.authorizedTargets.size)
        }

    @Test
    @DisplayName("AC-19.denied wrong profile switched session revoked host expired and unauthorized reveal nothing")
    fun ac19DeniedWrongProfileSwitchedSessionRevokedHostExpiredAndUnauthorizedRevealNothing() =
        runTest(dispatcher) {
            val entry = FakeExternalEntryPort()
            entry.currentProfileResult = Outcome.Success(profile("instance-a", active = true))
            val resolver = newResolver(entry)

            val wrongProfile = resolver.resolve(target.copy(profileId = InstanceId("instance-b")))
            assertNull(wrongProfile.route)
            assertEquals(ExternalEntryReason.WrongProfile, wrongProfile.reason)
            assertTrue(wrongProfile.revealNothing)
            assertTrue(entry.authorizedTargets.isEmpty())

            val cases = listOf(
                ExternalAuthorization.SessionNotFound to ExternalEntryReason.SwitchedSession,
                ExternalAuthorization.HostRevoked to ExternalEntryReason.RevokedHost,
                ExternalAuthorization.ExpiredTarget to ExternalEntryReason.ExpiredTarget,
                ExternalAuthorization.Unauthorized to ExternalEntryReason.Unauthorized,
            )
            for ((authorization, reason) in cases) {
                entry.authorizeResult = { authorization }
                val resolution = resolver.resolve(target)
                assertNull(resolution.route)
                assertEquals(reason, resolution.reason)
                assertTrue(resolution.revealNothing)
            }

            val invalid = resolver.resolve(ExternalTarget(profileId = instance))
            assertNull(invalid.route)
            assertEquals(ExternalEntryReason.InvalidTarget, invalid.reason)
        }

    @Test
    @DisplayName("AC-19.resolved a pending item is refocused only while still pending")
    fun ac19ResolvedAPendingItemIsRefocusedOnlyWhileStillPending() = runTest(dispatcher) {
        val entry = FakeExternalEntryPort()
        entry.currentProfileResult = Outcome.Success(profile("instance-a", active = true))
        entry.authorizeResult = {
            ExternalAuthorization.Authorized(
                route = Route.Conversation(session),
                itemState = ExternalItemState.Pending,
                pendingItemId = "item-1",
            )
        }
        val resolver = newResolver(entry)
        val pending = resolver.resolve(target)
        assertEquals(Route.Conversation(session, focusItemId = "item-1"), pending.route)

        entry.authorizeResult = {
            ExternalAuthorization.Authorized(
                route = Route.Conversation(session),
                itemState = ExternalItemState.Resolved,
                pendingItemId = "item-1",
            )
        }
        val resolved = resolver.resolve(target)
        assertEquals(Route.Conversation(session, focusItemId = null), resolved.route)
        assertEquals(ExternalItemState.Resolved, resolved.itemState)
        assertEquals(ExternalEntryReason.Resolved, resolved.reason)

        entry.authorizeResult = {
            ExternalAuthorization.Authorized(
                route = Route.Conversation(session),
                itemState = ExternalItemState.Unavailable,
                pendingItemId = "item-1",
            )
        }
        val unavailable = resolver.resolve(target)
        assertEquals(Route.Conversation(session, focusItemId = null), unavailable.route)
        assertEquals(ExternalEntryReason.PendingItemUnavailable, unavailable.reason)
        assertEquals(ExternalItemState.Unavailable, unavailable.itemState)
    }

    private fun newResolver(entry: FakeExternalEntryPort): ExternalEntryResolver =
        ExternalEntryResolver(entry, RecordingDiagnosticSink())
}
