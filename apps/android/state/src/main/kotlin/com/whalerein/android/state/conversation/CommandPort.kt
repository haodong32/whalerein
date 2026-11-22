package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.SessionRef

/** Typed stop/interruption result. "Stopped" is reported only on [Confirmed]. */
sealed interface StopOutcome {
    data object Accepted : StopOutcome

    data object Confirmed : StopOutcome

    data class Failed(val failure: CoreFailure) : StopOutcome

    data class Unconfirmed(val failure: CoreFailure) : StopOutcome
}

interface CommandPort {
    suspend fun stop(scope: SessionRef, attempt: AttemptRef?): StopOutcome
}
