package com.whalerein.android.corebridge.ports

import com.whalerein.android.corebridge.ConversationEvent
import com.whalerein.android.corebridge.ConversationEventEnvelope
import com.whalerein.android.corebridge.WatchEventScope
import com.whalerein.android.state.conversation.WatchMode
import com.whalerein.android.state.conversation.WatchSession
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class ObservationScopeMappingTest {
    private val session = WatchSession(
        watchId = "app-watch",
        generation = 3L,
        scope = SessionRef(HostRef(InstanceId("instance"), HostId("host")), SessionId("session")),
        mode = WatchMode.Live,
    )
    private val scope = WatchEventScope(
        generation = 8UL,
        hostId = "host",
        sessionId = "session",
        observationId = "observation",
    )

    @Test
    fun acceptsOnlyTheCurrentProducerAndIndependentQueueLoss() {
        val end = ConversationEvent.WatchEnded
        assertEquals(end, ConversationEventEnvelope(scope, end).watchPayloadFor(session, 8UL))
        assertNull(ConversationEventEnvelope(scope.copy(generation = 7UL), end).watchPayloadFor(session, 8UL))
        assertNull(ConversationEventEnvelope(scope.copy(hostId = "other-host"), end).watchPayloadFor(session, 8UL))
        assertNull(ConversationEventEnvelope(scope.copy(sessionId = "other-session"), end).watchPayloadFor(session, 8UL))
        assertNull(ConversationEventEnvelope(null, end).watchPayloadFor(session, 8UL))

        val dropped = ConversationEvent.DroppedEvents(2UL)
        assertEquals(dropped, ConversationEventEnvelope(null, dropped).watchPayloadFor(session, 8UL))
        assertEquals(dropped, ConversationEventEnvelope(scope, dropped).watchPayloadFor(session, 8UL))
    }
}
