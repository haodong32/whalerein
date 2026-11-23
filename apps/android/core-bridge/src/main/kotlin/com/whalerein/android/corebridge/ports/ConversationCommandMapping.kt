package com.whalerein.android.corebridge.ports

import com.whalerein.android.corebridge.WaitOutcome
import com.whalerein.android.state.conversation.CONVERSATION_PROJECTION_ANDROID_V1
import com.whalerein.android.state.conversation.ConversationFailureCodes
import com.whalerein.android.state.conversation.Guard
import com.whalerein.android.state.conversation.PendingItem
import com.whalerein.android.state.conversation.PendingListResult
import com.whalerein.android.state.conversation.PendingSendRecord
import com.whalerein.android.state.core.CoreFailure
import kotlinx.serialization.json.Json

/**
 * Pure conversation-command mappings.
 *
 * The binding reports receipts, wait outcomes and pending-input batches as
 * independent fields. These functions keep their classification in one place so
 * an unconfirmed result can never be presented as an applied one, and so the
 * native attempt identity owns cancellation.
 */

/** Outcome of one submitted send, derived from the Core receipt status. */
internal enum class SendDisposition { Accepted, Unconfirmed, Rejected }

/**
 * A missing receipt means Core cannot prove the outcome: that is `Unconfirmed`,
 * never a definite failure, because the original operation may still settle.
 */
internal fun sendDisposition(receiptStatus: String?): SendDisposition = when (receiptStatus) {
    null -> SendDisposition.Unconfirmed
    "succeeded", "accepted", "host_accepted", "received" -> SendDisposition.Accepted
    "host_offline", "unknown", "uncertain" -> SendDisposition.Unconfirmed
    else -> SendDisposition.Rejected
}

/** Outcome of one command receipt; only `Applied` may mutate local state. */
internal enum class CommandDisposition { Applied, Unconfirmed, Rejected }

internal fun commandDisposition(status: String): CommandDisposition = when (status) {
    "succeeded" -> CommandDisposition.Applied
    // A Host-accepted/received command is not terminal: the change may still
    // fail or be rejected, so it must not mutate local state as applied.
    "accepted", "host_accepted", "received", "host_offline", "unknown", "uncertain" ->
        CommandDisposition.Unconfirmed

    else -> CommandDisposition.Rejected
}

/** Which pending-input kind a native wait reported, in Core's evidenced order. */
internal enum class PendingInputKind { Approval, Question, None }

internal fun pendingInputKind(pending: List<String>): PendingInputKind = when {
    pending.any { it == "approval" } -> PendingInputKind.Approval
    pending.any { it == "question" } -> PendingInputKind.Question
    else -> PendingInputKind.None
}

/** Settled meaning of a native wait result. */
internal enum class WaitSettlement {
    Quiescent,
    Approval,
    Question,
    TimedOut,
    Cancelled,
    Failed,
    InputUnavailable,
}

/**
 * Classifies a wait result. An `INPUT_REQUIRED` outcome is only quiescent when
 * an actual readable approval/question exists; a question-only state still asks
 * the user to respond even though there is no approval.
 */
internal fun waitSettlement(
    outcome: WaitOutcome?,
    pendingInput: List<String>,
    hasApproval: Boolean,
    hasQuestion: Boolean,
): WaitSettlement = when (outcome) {
    null -> WaitSettlement.Failed
    WaitOutcome.IDLE -> WaitSettlement.Quiescent
    WaitOutcome.INPUT_REQUIRED -> when (pendingInputKind(pendingInput)) {
        PendingInputKind.Approval -> when {
            hasApproval -> WaitSettlement.Approval
            hasQuestion -> WaitSettlement.Question
            else -> WaitSettlement.InputUnavailable
        }

        PendingInputKind.Question -> when {
            hasQuestion -> WaitSettlement.Question
            hasApproval -> WaitSettlement.Approval
            else -> WaitSettlement.InputUnavailable
        }

        PendingInputKind.None -> when {
            hasApproval -> WaitSettlement.Approval
            hasQuestion -> WaitSettlement.Question
            else -> WaitSettlement.InputUnavailable
        }
    }

    WaitOutcome.TIMEOUT -> WaitSettlement.TimedOut
    WaitOutcome.CANCELLED -> WaitSettlement.Cancelled
    WaitOutcome.FAILED -> WaitSettlement.Failed
}

/**
 * App wait identity to native attempt identity.
 *
 * The model owns an app-level id; the binding owns the native attempt id. Cancel
 * must reach the native id, and a settled/cancelled attempt is removed so a
 * later settle cannot be attributed to a new wait.
 */
internal class WaitIdentityRegistry {
    private val nativeByApp = mutableMapOf<String, String>()

    fun bind(appWaitId: String, nativeWaitId: String) {
        nativeByApp[appWaitId] = nativeWaitId
    }

    fun nativeFor(appWaitId: String): String? = nativeByApp[appWaitId]

    /** Removes and returns the native id only when this attempt is still open. */
    fun release(appWaitId: String): String? = nativeByApp.remove(appWaitId)
}

/**
 * Approvals survive a Host that cannot answer questions. Only an empty approval
 * set turns a question failure into the whole list's result.
 */
internal fun pendingListAfterQuestionFailure(
    approvals: List<PendingItem>,
    failure: CoreFailure,
): PendingListResult = if (approvals.isNotEmpty()) {
    PendingListResult.Current(approvals)
} else {
    when (failure.code) {
        ConversationFailureCodes.CAPABILITY_MISSING,
        ConversationFailureCodes.NOT_FOUND,
        -> PendingListResult.Unsupported

        else -> if (failure.retryable) {
            PendingListResult.Unavailable(failure)
        } else {
            PendingListResult.Stale(failure)
        }
    }
}

/** The native command identity of one in-flight answer, keyed by app answer id. */
internal data class AnswerCommand(
    val commandId: String,
    val batchId: String,
)

internal class AnswerCommandRegistry {
    private val commands = mutableMapOf<String, AnswerCommand>()

    fun bind(answerId: String, command: AnswerCommand) {
        if (command.commandId.isNotEmpty()) commands[answerId] = command
    }

    /** The exact original Core command id, or null when nothing is in flight. */
    fun commandFor(answerId: String): String? = commands[answerId]?.commandId

    fun clear(answerId: String) {
        commands.remove(answerId)
    }
}

/** A reviewed guard matches this exact batch version and digest. */
internal fun reviewedGuardMatches(
    guard: Guard,
    version: ULong,
    digestHex: String,
): Boolean {
    val (reviewedVersion, reviewedDigest) = guardParts(guard) ?: return false
    return reviewedVersion == version.toLong() && reviewedDigest == digestHex
}

/** The current batch guard, used when a reviewed guard is stale. */
internal fun batchGuard(version: ULong, digestHex: String): Guard =
    Guard("$version:$digestHex")

/** Only the live conversation projection may satisfy a cache-only exact read. */
internal fun cacheExactProjectionEligible(projection: String): Boolean =
    projection == CONVERSATION_PROJECTION_ANDROID_V1

/** An online page belongs to the requested projection. */
internal fun historyProjectionMatches(pageProjection: String, requested: String): Boolean =
    pageProjection.isEmpty() || pageProjection == requested

/** Session details expose the latest committed history locator as fork target. */
internal fun latestCompletedTurn(historyCursor: String, blank: Boolean): String? =
    if (blank) null else historyCursor.ifEmpty { null }

/**
 * Reserved draft row for one unresolved send. The prefix keeps it inside the
 * same encrypted, Host-scoped draft storage without colliding with a real
 * session id; message text never reaches diagnostics.
 */
internal const val PENDING_SEND_KEY_PREFIX = "whalerein.pending-send.v1/"

internal fun pendingSendStorageKey(sessionId: String): String =
    PENDING_SEND_KEY_PREFIX + sessionId

internal fun encodePendingSend(record: PendingSendRecord): String =
    Json.encodeToString(PendingSendRecord.serializer(), record)

internal fun decodePendingSend(encoded: String): PendingSendRecord =
    Json.decodeFromString(PendingSendRecord.serializer(), encoded)
