package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SendId
import com.whalerein.android.state.core.SessionRef

/**
 * Composer submission contract.
 *
 * [SendRequest.sendId] is created by the screen model *before* submission, so a
 * retry or recovery always uses the original scoped operation identity and the
 * Host never receives a duplicate send.
 */
enum class DeliveryMode {
    /** Ordinary send using the native queue where the Source supports it. */
    Queue,

    /** Explicitly ask the Host to start the run when it is currently idle. */
    StartIfIdle,

    /** Queue the draft while a run is active. */
    Steer,

    /** Explicit interruption-free immediate delivery where advertised. */
    Send,
}

enum class FollowMode {
    /** Report the submission receipt only. */
    ReceiptOnly,

    /** Follow the matching reply within a bounded deadline. */
    FollowReply,
}

data class SendRequest(
    val scope: SessionRef,
    val sendId: SendId,
    val text: String,
    val mode: DeliveryMode,
    val follow: FollowMode,
)

data class SendReceipt(
    val sendId: SendId,
    val attempt: AttemptRef?,
    val acceptedAtMillis: Long = 0,
)

data class ReplyRequest(
    val scope: SessionRef,
    val sendId: SendId,
    val attempt: AttemptRef?,
    val timeoutMillis: Long,
)

/** Typed follow result. A confirmed reply can still have unreadable content. */
sealed interface ReplyResult {
    data class Reply(val message: Message) : ReplyResult

    /** The follow reached pending Host input rather than a final reply. */
    data class PendingInput(val item: PendingItem) : ReplyResult

    /** The reply is confirmed but its content cannot be read right now. */
    data class ConfirmedUnread(val failure: CoreFailure) : ReplyResult

    data object TimedOut : ReplyResult

    data class Failed(val failure: CoreFailure) : ReplyResult
}

data class RecoveryRequest(
    val scope: SessionRef,
    val sendId: SendId,
)

/** Recovery by original operation identity; never resubmits. */
sealed interface RecoveryResult {
    data class Confirmed(val receipt: SendReceipt, val reply: Message? = null) : RecoveryResult

    data class ConfirmedUnread(
        val receipt: SendReceipt,
        val failure: CoreFailure,
    ) : RecoveryResult

    data class StillUnconfirmed(val failure: CoreFailure) : RecoveryResult

    data class Failed(val failure: CoreFailure) : RecoveryResult
}

interface ComposerPort {
    suspend fun submit(request: SendRequest): Outcome<SendReceipt>

    suspend fun readReply(request: ReplyRequest): ReplyResult

    suspend fun recover(request: RecoveryRequest): RecoveryResult
}
