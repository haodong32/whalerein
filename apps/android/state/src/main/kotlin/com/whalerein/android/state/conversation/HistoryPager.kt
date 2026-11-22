package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.SessionRef

/**
 * History paging state machine.
 *
 * It is pure and clock-injected. The pager owns continuation validity, the
 * cached-page marker, the exact-message reference and the reading anchor. An
 * expired, replaced, projection-mismatched or foreign continuation never merges
 * into the current list: it invalidates the pager and requires a new starting
 * page.
 */

data class HistoryCursor(
    val continuation: String,
    val session: SessionRef,
    val projection: String,
    val expiresAtMillis: Long,
) {
    fun isExpired(nowMillis: Long): Boolean = nowMillis > expiresAtMillis
}

data class HistoryPage(
    val session: SessionRef,
    val projection: String,
    val messages: List<Message>,
    val continuation: HistoryCursor? = null,
    val cached: Boolean = false,
    val exact: Boolean = false,
)

enum class HistoryInvalidReason {
    Expired,
    Replaced,
    ProjectionMismatch,
    ForeignSession,
    ExactMissing,
}

sealed interface HistoryPagerStatus {
    data object Idle : HistoryPagerStatus

    data object LoadingLatest : HistoryPagerStatus

    data object LoadingOlder : HistoryPagerStatus

    data class Ready(val atEnd: Boolean) : HistoryPagerStatus

    /** Eligible offline cache; [available] is false for a known-empty cache. */
    data class Cached(val atEnd: Boolean, val available: Boolean = true) : HistoryPagerStatus

    data class CachedUnavailable(val failure: CoreFailure) : HistoryPagerStatus

    data class Invalidated(val reason: HistoryInvalidReason) : HistoryPagerStatus

    data class Failed(val failure: CoreFailure) : HistoryPagerStatus
}

data class HistoryPagerState(
    val session: SessionRef? = null,
    val projection: String = "",
    val messages: List<Message> = emptyList(),
    val continuation: HistoryCursor? = null,
    val status: HistoryPagerStatus = HistoryPagerStatus.Idle,
    val anchor: ReadingAnchor? = null,
    val cached: Boolean = false,
    val exactMessageId: String? = null,
) {
    val hasOlder: Boolean get() = continuation != null

    companion object {
        fun begin(session: SessionRef, projection: String): HistoryPagerState =
            HistoryPagerState(session = session, projection = projection)
    }
}

data class HistoryPagerResult(
    val state: HistoryPagerState,
    val insertion: PageInsertion? = null,
)

sealed interface OlderPageStart {
    data class Request(val cursor: HistoryCursor) : OlderPageStart

    data object AtEnd : OlderPageStart

    data class Invalidated(val result: HistoryPagerResult) : OlderPageStart
}

object HistoryPager {
    fun expectLatest(state: HistoryPagerState): HistoryPagerState =
        state.copy(status = HistoryPagerStatus.LoadingLatest)

    fun expectOlder(state: HistoryPagerState): HistoryPagerState =
        state.copy(status = HistoryPagerStatus.LoadingOlder)

    fun acceptLatest(
        state: HistoryPagerState,
        page: HistoryPage,
        nowMillis: Long,
    ): HistoryPagerResult {
        validationReason(state, page, nowMillis)?.let { reason ->
            return invalidated(state, reason)
        }
        val messages = page.messages
            .distinctBy { it.messageId }
            .sortedWith(compareBy<Message>({ it.sequence }, { it.messageId }))
        val next = state.copy(
            messages = messages,
            continuation = page.continuation,
            status = statusFor(page),
            anchor = null,
            cached = page.cached,
            exactMessageId = null,
        )
        return HistoryPagerResult(next)
    }

    /** Validates a continuation before a network read. */
    fun requestOlder(state: HistoryPagerState, nowMillis: Long): OlderPageStart {
        val status = state.status
        if (status is HistoryPagerStatus.Invalidated) {
            return OlderPageStart.Invalidated(HistoryPagerResult(state))
        }
        val cursor = state.continuation ?: return OlderPageStart.AtEnd
        if (cursor.isExpired(nowMillis)) {
            return OlderPageStart.Invalidated(invalidated(state, HistoryInvalidReason.Expired))
        }
        if (cursor.session != state.session || cursor.projection != state.projection) {
            return OlderPageStart.Invalidated(
                invalidated(state, HistoryInvalidReason.ForeignSession),
            )
        }
        return OlderPageStart.Request(cursor)
    }

    fun acceptOlder(
        state: HistoryPagerState,
        request: HistoryCursor,
        page: HistoryPage,
        nowMillis: Long,
        anchor: ReadingAnchor?,
    ): HistoryPagerResult {
        // The cursor may have been replaced while the read was in flight.
        if (state.continuation?.continuation != request.continuation) {
            return invalidated(state, HistoryInvalidReason.Replaced)
        }
        validationReason(state, page, nowMillis)?.let { reason ->
            return invalidated(state, reason)
        }
        if (nowMillis > request.expiresAtMillis) {
            return invalidated(state, HistoryInvalidReason.Expired)
        }
        val currentItems = state.messages.map(::CommittedMessageItem)
        val insertion = insertOlderMessages(currentItems, page.messages, anchor)
        val messages = insertion.items.committedMessages()
        val next = state.copy(
            messages = messages,
            continuation = page.continuation,
            status = if (page.cached) {
                HistoryPagerStatus.Cached(atEnd = page.continuation == null)
            } else {
                HistoryPagerStatus.Ready(atEnd = page.continuation == null)
            },
            anchor = insertion.anchor ?: anchor,
            cached = page.cached,
        )
        return HistoryPagerResult(next, insertion)
    }

    fun acceptExact(
        state: HistoryPagerState,
        page: HistoryPage,
        nowMillis: Long,
        messageId: String,
        anchor: ReadingAnchor? = null,
    ): HistoryPagerResult {
        validationReason(state, page, nowMillis)?.let { reason ->
            return invalidated(state, reason)
        }
        if (!page.exact || page.messages.none { it.messageId == messageId }) {
            return invalidated(state, HistoryInvalidReason.ExactMissing)
        }
        // An exact read is authoritative for the referenced message, so a
        // repeated canonical id replaces the existing row instead of being
        // ignored like an older page overlap.
        val exactById = page.messages.associateBy { it.messageId }
        val currentItems = state.messages
            .map { exactById[it.messageId] ?: it }
            .map(::CommittedMessageItem)
        val insertion = insertOlderMessages(currentItems, page.messages, anchor)
        val next = state.copy(
            messages = insertion.items.committedMessages(),
            status = statusFor(page),
            anchor = insertion.anchor ?: anchor,
            cached = page.cached,
            exactMessageId = messageId,
        )
        return HistoryPagerResult(next, insertion)
    }

    fun acceptCached(
        state: HistoryPagerState,
        page: HistoryPage,
        nowMillis: Long,
    ): HistoryPagerResult {
        if (!page.cached) {
            return invalidated(state, HistoryInvalidReason.ProjectionMismatch)
        }
        validationReason(state, page, nowMillis)?.let { reason ->
            return invalidated(state, reason)
        }
        val messages = page.messages
            .distinctBy { it.messageId }
            .sortedWith(compareBy<Message>({ it.sequence }, { it.messageId }))
        val next = state.copy(
            messages = messages,
            continuation = page.continuation,
            status = HistoryPagerStatus.Cached(
                atEnd = page.continuation == null,
                available = messages.isNotEmpty(),
            ),
            anchor = null,
            cached = true,
            exactMessageId = null,
        )
        return HistoryPagerResult(next)
    }

    fun cachedUnavailable(
        state: HistoryPagerState,
        failure: CoreFailure,
    ): HistoryPagerResult = HistoryPagerResult(
        state.copy(status = HistoryPagerStatus.CachedUnavailable(failure)),
    )

    fun fail(state: HistoryPagerState, failure: CoreFailure): HistoryPagerResult =
        HistoryPagerResult(state.copy(status = HistoryPagerStatus.Failed(failure)))

    fun reanchor(state: HistoryPagerState, anchor: ReadingAnchor?): HistoryPagerState =
        state.copy(anchor = anchor)

    private fun invalidated(
        state: HistoryPagerState,
        reason: HistoryInvalidReason,
    ): HistoryPagerResult = HistoryPagerResult(
        state.copy(
            messages = emptyList(),
            continuation = null,
            status = HistoryPagerStatus.Invalidated(reason),
            anchor = null,
            cached = false,
            exactMessageId = null,
        ),
    )

    private fun statusFor(page: HistoryPage): HistoryPagerStatus = if (page.cached) {
        HistoryPagerStatus.Cached(
            atEnd = page.continuation == null,
            available = page.messages.isNotEmpty(),
        )
    } else {
        HistoryPagerStatus.Ready(atEnd = page.continuation == null)
    }

    private fun validationReason(
        state: HistoryPagerState,
        page: HistoryPage,
        nowMillis: Long,
    ): HistoryInvalidReason? = when {
        page.session != state.session -> HistoryInvalidReason.ForeignSession
        page.projection != state.projection -> HistoryInvalidReason.ProjectionMismatch
        page.continuation?.isExpired(nowMillis) == true -> HistoryInvalidReason.Expired
        else -> null
    }
}
