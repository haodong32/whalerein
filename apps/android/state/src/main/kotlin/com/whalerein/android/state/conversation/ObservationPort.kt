package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionRef

/** Foreground conversation observation contract, implemented by `:core-bridge`. */
enum class WatchMode {
    /** Long-lived live events for the visible conversation. */
    Live,

    /** Ends when visible answer content (never hidden reasoning) appears. */
    FirstVisibleOutput,

    /** Ends when the Host reports quiescence or pending input. */
    UntilQuiescent,
}

enum class WatchEndReason {
    Requested,
    Timeout,
    Quiescent,
    FirstVisibleOutput,
    DroppedEvents,
    ScopeChanged,
    Backgrounded,
    Failed,
}

data class WatchRequest(
    val scope: SessionRef,
    val generation: Long,
    val mode: WatchMode = WatchMode.Live,
    val attempt: AttemptRef? = null,
    val timeoutMillis: Long? = null,
)

data class WatchSession(
    val watchId: String,
    val scope: SessionRef,
    val generation: Long,
    val mode: WatchMode,
)

/**
 * One drained poll result. Every [events] item already carries its scope and
 * generation, so a batch transferred before a scope switch is harmless.
 */
data class ObservationBatch(
    val scope: SessionRef,
    val generation: Long,
    val events: List<ConversationEvent> = emptyList(),
    val droppedCount: Int = 0,
    val ended: WatchEndReason? = null,
)

interface ObservationPort {
    /** Starts exactly one scoped watch or returns the existing scoped session. */
    suspend fun startWatch(request: WatchRequest): Outcome<WatchSession>

    /** Bounded long poll. An empty batch is valid; [ObservationBatch.ended] ends it. */
    suspend fun pollEvents(session: WatchSession): Outcome<ObservationBatch>

    suspend fun stopWatch(session: WatchSession): Outcome<Unit>
}
