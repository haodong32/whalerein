package com.whalerein.android.state.core

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

class RouteTest {
    private val host = HostRef(InstanceId("instance-1"), HostId("host-1"))
    private val session = SessionRef(host, SessionId("session-1"))

    @Test
    @DisplayName("AC-05.routes authorized routes keep their exact scope and focus")
    fun authorizedRoutesKeepScope() {
        assertEquals(
            Route.Conversation(session),
            Route.Conversation(session, focusItemId = null),
        )
        assertEquals(
            Route.PendingInput(session, "item-1"),
            Route.PendingInput(session, focusItemId = "item-1"),
        )
        assertNotEquals(Route.PendingInput(session, "item-1"), Route.PendingInput(session))
        assertEquals(Route.Welcome, Route.Welcome)
        assertEquals(Route.SignIn, Route.SignIn)
        assertEquals(Route.Directory, Route.Directory)
        assertEquals(Route.Profiles, Route.Profiles)
        assertEquals(Route.Operations, Route.Operations)
        assertEquals(Route.Account, Route.Account)
        assertEquals(Route.Diagnostics, Route.Diagnostics)
        assertEquals(host, Route.HostSessions(host).host)
        assertEquals(session, Route.SessionTools(session).session)
        assertEquals("attachment-1", Route.AttachmentSave(session, "attachment-1").attachmentId)
        assertNull(Route.Conversation(session).focusItemId)
    }

    @Test
    @DisplayName("AC-19.entry typed identities do not collide across instances")
    fun typedIdentitiesDoNotCollide() {
        val otherHost = HostRef(InstanceId("instance-2"), HostId("host-1"))
        assertNotEquals(host, otherHost)
        assertNotEquals(SessionRef(otherHost, SessionId("session-1")), session)
        assertEquals(AttemptId("a"), AttemptId("a"))
        assertEquals(SendId("s"), SendId("s"))
        assertEquals(DownloadId("d"), DownloadId("d"))
        assertEquals("host-1", host.hostId.value)
        assertEquals("instance-1", host.instance.value)
        assertEquals("session-1", session.sessionId.value)
    }
}
