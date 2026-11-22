package com.whalerein.android.corebridge.ports

import com.whalerein.android.corebridge.BlockView
import com.whalerein.android.corebridge.MessageView
import com.whalerein.android.corebridge.MobileException
import com.whalerein.android.corebridge.ToolActivityView
import com.whalerein.android.state.conversation.Message
import com.whalerein.android.state.conversation.MessageBlock
import com.whalerein.android.state.conversation.MessageBlockKind
import com.whalerein.android.state.conversation.MessageRole
import com.whalerein.android.state.conversation.ToolActivity
import com.whalerein.android.state.conversation.ToolActivityState
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.Outcome

/** Stable, redacted failure for any unexpected bridge throwable. */
internal fun Throwable.toCoreFailure(): CoreFailure = when (this) {
    is MobileException.InvalidInput -> CoreFailure("invalid_input", "The request was invalid.")
    is MobileException.NotPaired -> CoreFailure("not_paired", "The profile is not paired.")
    is MobileException.NotFound -> CoreFailure("not_found", "The requested value was not found.")
    is MobileException.Revoked -> CoreFailure("revoked", "The authorization was revoked.", retryable = false)
    is MobileException.HostOffline -> CoreFailure("host_offline", "The Host is offline.", retryable = true)
    is MobileException.Unavailable -> CoreFailure("unavailable", "The service is unavailable.", retryable = true)
    is MobileException.Conflict -> CoreFailure("conflict", "The state changed; refresh and retry.")
    is MobileException.Timeout -> CoreFailure("timeout", "The operation timed out.", retryable = true)
    is MobileException.Cancelled -> CoreFailure("cancelled", "The operation was cancelled.", retryable = true)
    is MobileException.StaleHistory -> CoreFailure("stale_history", "The history continuation expired.")
    is MobileException.Security -> CoreFailure("security", "The operation was blocked for security.")
    is MobileException.ProtocolVersionIncompatible ->
        CoreFailure("protocol_incompatible", "The Host protocol is incompatible.", retryable = false)
    is MobileException.UnsupportedCapability ->
        CoreFailure("capability_missing", "The Host lacks a required capability.")
    else -> CoreFailure("internal", "The operation could not be completed.", retryable = true)
}

internal inline fun <T> outcomeOf(block: () -> T): Outcome<T> = try {
    Outcome.Success(block())
} catch (cancelled: kotlinx.coroutines.CancellationException) {
    throw cancelled
} catch (error: Throwable) {
    Outcome.Failure(error.asCoreFailure())
}

internal suspend inline fun <T> suspendOutcome(crossinline block: suspend () -> T): Outcome<T> = try {
    Outcome.Success(kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { block() })
} catch (cancelled: kotlinx.coroutines.CancellationException) {
    throw cancelled
} catch (error: Throwable) {
    Outcome.Failure(error.asCoreFailure())
}

internal inline fun <T> unconfirmedOf(block: () -> T): Outcome<T> = try {
    Outcome.Success(block())
} catch (cancelled: kotlinx.coroutines.CancellationException) {
    throw cancelled
} catch (error: Throwable) {
    val failure = error.toCoreFailure()
    if (failure.retryable) Outcome.Unconfirmed(failure) else Outcome.Failure(failure)
}

/** Stable 64-bit epoch identity for the reducer; Core epochs are opaque strings. */
internal fun liveEpochId(value: String): Long {
    if (value.isEmpty()) return 0L
    var hash = -0x340d631b7bdddcdbL
    for (byte in value.toByteArray(Charsets.UTF_8)) {
        hash = hash xor (byte.toLong() and 0xff)
        hash *= 0x100000001b3L
    }
    return hash
}

internal fun coreBlockKind(kind: String): MessageBlockKind = when (kind) {
    "text" -> MessageBlockKind.Text
    "reasoning" -> MessageBlockKind.Reasoning
    "tool" -> MessageBlockKind.Tool
    "notice" -> MessageBlockKind.Notice
    "attachment" -> MessageBlockKind.Attachment
    "code" -> MessageBlockKind.Code
    else -> MessageBlockKind.Unknown
}

internal fun messageRole(role: String): MessageRole = when (role) {
    "user" -> MessageRole.User
    "assistant" -> MessageRole.Assistant
    "tool" -> MessageRole.Tool
    else -> MessageRole.System
}

internal fun BlockView.toMessageBlock(): MessageBlock {
    val fields = buildMap {
        toolActivity?.let { put(MessageBlock.FIELD_CALL_ID, it.callId) }
        notice?.let { put(MessageBlock.FIELD_NOTICE_KIND, it.category) }
        attachment?.let {
            put(MessageBlock.FIELD_ATTACHMENT_ID, it.attachmentRef)
            put(MessageBlock.FIELD_ATTACHMENT_NAME, it.name)
            put(MessageBlock.FIELD_ATTACHMENT_AVAILABLE, it.available.toString())
        }
    }
    return MessageBlock(
        blockId = blockId,
        kind = coreBlockKind(kind),
        text = text,
        fields = fields,
    )
}

internal fun MessageView.toStateMessage(
    origin: com.whalerein.android.state.conversation.AttemptRef? = null,
    sequence: Long = 0,
): Message = Message(
    messageId = messageId,
    role = messageRole(role),
    blocks = blocks.map { it.toMessageBlock() },
    sequence = sequence,
    origin = origin,
    createdAtMillis = createdAtUnixMs,
)

internal fun ToolActivityView.toStateActivity(): ToolActivity = ToolActivity(
    callId = callId,
    name = name,
    state = when (state) {
        "running", "preparing" -> ToolActivityState.Running
        "succeeded" -> ToolActivityState.Completed
        "failed", "interrupted", "abandoned" -> ToolActivityState.Failed
        else -> ToolActivityState.Unknown
    },
    summary = summary ?: progress ?: "",
    fullOutput = result.filterIsInstance<com.whalerein.android.corebridge.ToolResultPartView.Text>()
        .joinToString("\n") { it.text },
    incomplete = state == "preparing" || state == "running",
)
