package com.whalerein.android.state.support

import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticSink

/**
 * Delivery guard for diagnostic sinks.
 *
 * [DiagnosticSink] implementations are required not to throw, but a faulty sink
 * must never change an operation outcome. Every ViewModel emission goes through
 * this guard, which also protects against an unexpected runtime failure in a
 * platform writer.
 */
object DiagnosticDelivery {
    fun deliver(sink: DiagnosticSink, event: DiagnosticEvent) {
        try {
            sink.emit(event)
        } catch (_: Throwable) {
            // Diagnostics are observational only.
        }
    }
}
