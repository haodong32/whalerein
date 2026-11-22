package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionRef

/** Scoped draft persistence. Draft text is never logged. */
interface DraftPort {
    suspend fun load(scope: SessionRef): Outcome<String?>

    suspend fun save(scope: SessionRef, text: String): Outcome<Unit>

    suspend fun clear(scope: SessionRef): Outcome<Unit>
}

/** Shared failure codes used to distinguish typed port outcomes. */
object ConversationFailureCodes {
    const val CAPABILITY_MISSING = "capability_missing"
    const val GUARD_CONFLICT = "guard_conflict"
    const val EXPIRED = "expired"
    const val RESOLVED_ELSEWHERE = "resolved_elsewhere"
    const val CONTENT_UNREADABLE = "content_unreadable"
    const val OFFLINE = "offline"
    const val NOT_FOUND = "not_found"
}
