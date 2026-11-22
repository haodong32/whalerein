package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionRef

enum class WaitTrigger { Quiescence, PendingInput, FirstVisibleOutput }

data class WaitRequest(
    val scope: SessionRef,
    val waitId: String,
    val attempt: AttemptRef?,
    val trigger: WaitTrigger = WaitTrigger.Quiescence,
    val timeoutMillis: Long,
)

/** Typed bounded-wait result. Cancelling a wait never interrupts the Host run. */
sealed interface WaitResult {
    data object Quiescent : WaitResult

    data class ApprovalInput(val item: PendingItem.Approval) : WaitResult

    data class QuestionInput(val item: PendingItem.Questions) : WaitResult

    data object TimedOut : WaitResult

    data object Cancelled : WaitResult

    data class Failed(val failure: CoreFailure) : WaitResult
}

interface WaitPort {
    suspend fun wait(request: WaitRequest): WaitResult

    /** Explicit cancellation of exactly [waitId]; no Host interruption. */
    suspend fun cancel(waitId: String): Outcome<Unit>
}
