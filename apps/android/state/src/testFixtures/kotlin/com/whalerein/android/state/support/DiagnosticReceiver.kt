package com.whalerein.android.state.support

import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticSink

/**
 * Bounded, thread-safe diagnostic capture used by tests and by the composition
 * root when a development build wants an in-memory tail.
 *
 * [emit] never throws and never blocks on the delegate: a throwing delegate is
 * ignored so diagnostics cannot change an operation outcome. The queue drops the
 * oldest record once [capacity] is reached and reports the drop count.
 */
class DiagnosticReceiver(
    private val capacity: Int = 256,
    private val delegate: DiagnosticSink? = null,
) : DiagnosticSink {
    private val lock = Any()
    private val events = ArrayDeque<DiagnosticEvent>()
    private var dropped = 0

    override fun emit(event: DiagnosticEvent) {
        if (capacity <= 0) return
        synchronized(lock) {
            if (events.size >= capacity) {
                events.removeFirst()
                dropped += 1
            }
            events.addLast(event)
        }
        if (delegate != null) {
            try {
                delegate.emit(event)
            } catch (_: Throwable) {
                // Diagnostics must never change an operation outcome.
            }
        }
    }

    fun snapshot(): List<DiagnosticEvent> = synchronized(lock) { events.toList() }

    fun droppedCount(): Int = synchronized(lock) { dropped }

    fun clear() {
        synchronized(lock) {
            events.clear()
            dropped = 0
        }
    }
}
