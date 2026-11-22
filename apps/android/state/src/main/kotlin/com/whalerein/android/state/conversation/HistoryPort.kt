package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionRef

/**
 * Bounded committed-history contract.
 *
 * A page always echoes the scope and projection it belongs to. The adapter must
 * never translate an invalid continuation into a synthetic page: the pager
 * requires a fresh starting page instead.
 */
interface HistoryPort {
    suspend fun latestPage(scope: SessionRef, projection: String): Outcome<HistoryPage>

    suspend fun olderPage(scope: SessionRef, cursor: HistoryCursor): Outcome<HistoryPage>

    /** Reads one exact committed message reference inside the target scope. */
    suspend fun exactMessage(
        scope: SessionRef,
        projection: String,
        messageId: String,
    ): Outcome<HistoryPage>

    suspend fun cachedPage(scope: SessionRef, projection: String): Outcome<HistoryPage>
}
