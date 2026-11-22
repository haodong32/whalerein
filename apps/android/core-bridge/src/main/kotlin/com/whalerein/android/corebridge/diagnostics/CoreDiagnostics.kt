package com.whalerein.android.corebridge.diagnostics

import com.whalerein.android.corebridge.CoreDiagnosticSink
import com.whalerein.android.corebridge.DiagnosticContext
import com.whalerein.android.corebridge.DiagnosticEventView

/**
 * Small app-facing receiver for Core's redacted events.
 *
 * The adapter catches receiver failures so a broken journal writer can never
 * affect Core. Delivery already happens on a bounded background thread.
 */
fun interface CoreDiagnosticReceiver {
    fun onDiagnostic(event: DiagnosticEventView)
}

/** Generated-interface adapter exposed to `:app`. */
class CoreDiagnosticSinkAdapter(
    private val receiver: CoreDiagnosticReceiver,
) : CoreDiagnosticSink {
    override fun onEvent(event: DiagnosticEventView) {
        try {
            receiver.onDiagnostic(event)
        } catch (_: Throwable) {
            // A diagnostic consumer must never break the producer.
        }
    }
}

/** Reserve a Core trace before opening the handle. */
fun newDiagnosticContext(receiver: CoreDiagnosticReceiver): DiagnosticContext =
    DiagnosticContext(CoreDiagnosticSinkAdapter(receiver))

/**
 * Map Core's stable event names onto the shared `:state` stage vocabulary.
 * Only a closed set is inferred; unknown events keep an empty stage.
 */
fun coreDiagnosticStage(event: String): String = when {
    event.startsWith("configuration_") -> "configuration"
    event.startsWith("http_") || event.startsWith("mqtt_") -> "transport"
    event.startsWith("access_request_") ||
        event in setOf("request_receipt_mismatch", "request_state_mismatch") -> "command"
    event == "secure_channel_established" -> "observation"
    event.startsWith("attachment_") -> "transfer"
    event == "capability_missing" -> "command"
    event == "diagnostic_save_failed" || event == "diagnostics_dropped" -> "startup"
    else -> ""
}
