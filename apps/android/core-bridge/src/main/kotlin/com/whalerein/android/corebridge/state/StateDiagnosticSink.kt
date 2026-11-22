package com.whalerein.android.corebridge.state

import com.whalerein.android.corebridge.CoreDiagnosticSink
import com.whalerein.android.corebridge.DiagnosticEventView
import com.whalerein.android.corebridge.diagnostics.coreDiagnosticStage
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink

/**
 * Implements the existing `:state` diagnostic port over Core's callback.
 *
 * This is the first bridge adapter: it exists because `:state` already owns a
 * `DiagnosticSink`. Access, conversation, support and attachment ports are not
 * defined yet, so this module does not invent them; adapters will be added as
 * those ports land.
 */
class StateDiagnosticSinkAdapter(
    private val downstream: DiagnosticSink,
) : CoreDiagnosticSink {
    override fun onEvent(event: DiagnosticEventView) {
        try {
            downstream.emit(event.toStateEvent())
        } catch (_: Throwable) {
            // The state sink contract must never throw; a broken writer is
            // contained here so Core's callback remains harmless.
        }
    }
}

fun DiagnosticEventView.toStateEvent(): DiagnosticEvent = DiagnosticEvent(
    level = when (level.lowercase()) {
        "debug" -> DiagnosticLevel.Debug
        "info" -> DiagnosticLevel.Info
        "warn" -> DiagnosticLevel.Warn
        else -> DiagnosticLevel.Error
    },
    event = event,
    stage = coreDiagnosticStage(event),
    code = fields["code"],
    traceId = traceId.ifEmpty { null },
    fields = fields,
)
