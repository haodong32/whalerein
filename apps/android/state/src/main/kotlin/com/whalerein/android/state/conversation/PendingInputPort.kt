package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.SessionRef

/**
 * Pending approval/question contract.
 *
 * Every decision carries the reviewed [Guard]. The adapter must reject a stale
 * guard instead of applying a decision to newer Host state.
 */
data class PendingCollection(val items: List<PendingItem>)

/** A fixed-entry collection result; "current empty" is not "unavailable". */
sealed interface PendingListResult {
    data class Current(val items: List<PendingItem>) : PendingListResult

    data object Unsupported : PendingListResult

    data class Stale(val failure: CoreFailure) : PendingListResult

    data class Unavailable(val failure: CoreFailure) : PendingListResult
}

sealed interface PendingDetailResult {
    data class Found(val item: PendingItem) : PendingDetailResult

    data object Missing : PendingDetailResult

    data class Changed(val failure: CoreFailure) : PendingDetailResult

    data class Unavailable(val failure: CoreFailure) : PendingDetailResult
}

enum class ApprovalDecision { Approve, Reject }

sealed interface PendingActionOutcome {
    data class Applied(val itemId: String) : PendingActionOutcome

    /** The reviewed guard no longer matches; callers must refresh. */
    data class GuardChanged(val currentGuard: Guard?) : PendingActionOutcome

    data object Expired : PendingActionOutcome

    /** Another client resolved the item first. */
    data object ResolvedElsewhere : PendingActionOutcome

    data object Unsupported : PendingActionOutcome

    data class Unconfirmed(val failure: CoreFailure) : PendingActionOutcome

    data class Failed(val failure: CoreFailure) : PendingActionOutcome
}

/** Identity of an explicitly cancelled in-flight answer. */
data class AnswerRef(val answerId: String)

interface PendingInputPort {
    suspend fun list(scope: SessionRef): PendingListResult

    suspend fun get(scope: SessionRef, itemId: String): PendingDetailResult

    suspend fun resolveApproval(
        scope: SessionRef,
        approvalId: String,
        decision: ApprovalDecision,
        guard: Guard,
    ): PendingActionOutcome

    suspend fun answerQuestion(
        scope: SessionRef,
        batchId: String,
        answers: List<QuestionAnswer>,
        guard: Guard,
        answerRef: AnswerRef,
    ): PendingActionOutcome

    /** Explicit cancellation of an in-flight answer; never implicit. */
    suspend fun cancelAnswer(
        scope: SessionRef,
        answerRef: AnswerRef,
    ): PendingActionOutcome
}
