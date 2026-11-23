package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionRef
import kotlinx.serialization.Serializable

/**
 * One unresolved send identity, persisted so a process restart can recover the
 * exact Host operation instead of resubmitting. The text is message content and
 * must only live in the same encrypted draft storage, never in diagnostics.
 */
@Serializable
data class PendingSendRecord(
    val sendId: String,
    val text: String,
    /** [FollowMode] name at submission time. */
    val follow: String,
    val submittedAtMillis: Long = 0,
)

/** Scoped draft persistence. Draft text is never logged. */
interface DraftPort {
    suspend fun load(scope: SessionRef): Outcome<String?>

    suspend fun save(scope: SessionRef, text: String): Outcome<Unit>

    suspend fun clear(scope: SessionRef): Outcome<Unit>

    /** Persist the one unresolved send for this exact Host/session scope. */
    suspend fun savePendingSend(scope: SessionRef, record: PendingSendRecord): Outcome<Unit>

    /** Load the unresolved send for this exact Host/session scope, if any. */
    suspend fun loadPendingSend(scope: SessionRef): Outcome<PendingSendRecord?>

    /** Remove the unresolved send record for this exact scope. */
    suspend fun clearPendingSend(scope: SessionRef): Outcome<Unit>
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
