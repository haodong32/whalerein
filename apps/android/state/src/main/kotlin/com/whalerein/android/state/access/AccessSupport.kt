package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import kotlinx.coroutines.CancellationException

/** Stages owned by the access slice. */
internal object AccessStage {
    const val Startup = "startup"
    const val Login = "login"
    const val Profile = "profile"
    const val Transport = "transport"
}

/**
 * Emits one redacted event.
 *
 * Callers pass only fixed codes and small correlation identities; no callback
 * URL, code, verifier, token, fingerprint or path ever becomes a field.
 */
internal fun DiagnosticSink.accessEvent(
    level: DiagnosticLevel,
    event: String,
    stage: String,
    code: String? = null,
    instanceId: InstanceId? = null,
) {
    emit(
        DiagnosticEvent(
            level = level,
            event = event,
            stage = stage,
            code = code,
            instanceId = instanceId?.value,
        ),
    )
}

/**
 * Runs one port interaction, converting an unexpected throwable into a typed
 * failure with a redacted diagnostic. Cancellation is never swallowed.
 */
internal suspend fun <T> guardedPortCall(
    diagnostics: DiagnosticSink,
    stage: String,
    event: String,
    instanceId: InstanceId? = null,
    block: suspend () -> Outcome<T>,
): Outcome<T> = try {
    block()
} catch (cancelled: CancellationException) {
    throw cancelled
} catch (_: Exception) {
    diagnostics.accessEvent(
        level = DiagnosticLevel.Error,
        event = "${event}_exception",
        stage = stage,
        code = "unexpected_failure",
        instanceId = instanceId,
    )
    Outcome.Failure(CoreFailure("unexpected_failure", "The operation could not be completed.", true))
}

/** Maps a non-success outcome to a typed failure for presentation. */
internal fun <T> Outcome<T>.asFailure(): CoreFailure = when (this) {
    is Outcome.Failure -> failure
    is Outcome.Unconfirmed -> failure
    Outcome.Cancelled -> CoreFailure("cancelled", "The operation was cancelled.", retryable = true)
    is Outcome.Success -> CoreFailure("unexpected_success", "The operation completed.")
}
