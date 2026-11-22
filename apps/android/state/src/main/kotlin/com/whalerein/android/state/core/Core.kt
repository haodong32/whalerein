package com.whalerein.android.state.core

/**
 * Small identity and infrastructure types shared by every screen contract.
 *
 * These types are intentionally free of UniFFI, Android and JNA imports: the
 * JDK-only `:state` build must compile and test without an Android SDK or a
 * native library. `:core-bridge` maps its generated binding values onto them.
 */

@JvmInline
value class InstanceId(val value: String)

@JvmInline
value class HostId(val value: String)

@JvmInline
value class SessionId(val value: String)

@JvmInline
value class AttemptId(val value: String)

@JvmInline
value class SendId(val value: String)

@JvmInline
value class DownloadId(val value: String)

data class HostRef(val instance: InstanceId, val hostId: HostId)

data class SessionRef(val host: HostRef, val sessionId: SessionId)

enum class InstanceKind { Cloud, Standalone }

/** Wall-clock and monotonic time, injected so deadlines are testable. */
interface Clock {
    fun epochMillis(): Long

    fun elapsedMillis(): Long
}

fun interface IdGenerator {
    fun newId(): String
}

enum class DiagnosticLevel { Debug, Info, Warn, Error }

/**
 * One redacted diagnostic event.
 *
 * [fields] carries only allowlisted scalars: stage, code, counts, durations and
 * safe correlation identities. Message content, reasoning, drafts, tool
 * arguments, question text, file paths, callback URLs, credentials, tokens,
 * pairing material and keys must never appear here.
 */
data class DiagnosticEvent(
    val level: DiagnosticLevel,
    val event: String,
    val stage: String,
    val code: String? = null,
    val traceId: String? = null,
    val actionId: String? = null,
    val instanceId: String? = null,
    val fields: Map<String, String> = emptyMap(),
)

fun interface DiagnosticSink {
    /** Emits one event. The sink must never throw or block the caller. */
    fun emit(event: DiagnosticEvent)
}

data class CoreFailure(
    val code: String,
    val message: String,
    val retryable: Boolean = false,
)

/** Typed operation outcome. Expected failures never throw across a port. */
sealed interface Outcome<out T> {
    data class Success<T>(val value: T) : Outcome<T>

    data class Failure(val failure: CoreFailure) : Outcome<Nothing>

    /** The operation may have been accepted; it must be recovered by identity. */
    data class Unconfirmed(val failure: CoreFailure) : Outcome<Nothing>

    data object Cancelled : Outcome<Nothing>

    fun valueOrNull(): T? = (this as? Success)?.value
}

fun <T> Outcome<T>.failureOrNull(): CoreFailure? = when (this) {
    is Outcome.Failure -> failure
    is Outcome.Unconfirmed -> failure
    else -> null
}
