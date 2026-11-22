package com.whalerein.android.state.conversation

import org.junit.jupiter.api.DisplayName
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.fixtures.conversation.ConversationFixtures
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class HistoryPagerTest : ConversationTestBase() {
    @Test
    @DisplayName("AC-09.paging older page insertion keeps the reading anchor and order")
    fun ac09Paging() {
        val scope = ConversationFixtures.session()
        val latest = ConversationFixtures.page(
            messages = listOf(
                ConversationFixtures.textMessage("m3", "three", sequence = 3),
                ConversationFixtures.textMessage("m4", "four", sequence = 4),
            ),
            continuation = ConversationFixtures.cursor("cursor-2"),
        )
        val loaded = HistoryPager.acceptLatest(
            HistoryPager.expectLatest(HistoryPagerState.begin(scope, ConversationFixtures.projection)),
            latest,
            nowMillis = 0,
        )
        val anchor = ReadingAnchor(ConversationItemKey.Committed("m3"), offsetFromItemTopPx = 42)
        val anchored = HistoryPager.reanchor(loaded.state, anchor)

        val start = HistoryPager.requestOlder(anchored, nowMillis = 0)
        assertTrue(start is OlderPageStart.Request)
        val request = (start as OlderPageStart.Request).cursor

        val accepted = HistoryPager.acceptOlder(
            anchored,
            request,
            ConversationFixtures.page(
                messages = listOf(
                    ConversationFixtures.textMessage("m1", "one", sequence = 1),
                    ConversationFixtures.textMessage("m2", "two", sequence = 2),
                ),
                continuation = ConversationFixtures.cursor("cursor-3"),
            ),
            nowMillis = 0,
            anchor = anchor,
        )

        assertEquals(listOf("m1", "m2", "m3", "m4"), accepted.state.messages.map { it.messageId })
        val insertion = requireNotNull(accepted.insertion)
        assertEquals(
            listOf(ConversationItemKey.Committed("m1"), ConversationItemKey.Committed("m2")),
            insertion.insertedKeys,
        )
        assertEquals(2, insertion.anchorIndexShift)
        assertEquals(anchor, insertion.anchor)
        assertEquals(
            listOf("m1", "m2", "m3", "m4"),
            insertion.items.map { (it as CommittedMessageItem).message.messageId },
        )
        assertEquals(HistoryPagerStatus.Ready(atEnd = false), accepted.state.status)
        assertTrue(accepted.state.hasOlder)
    }

    @Test
    @DisplayName("AC-09.exact exact reference belongs to the target and merges once")
    fun ac09Exact() {
        val scope = ConversationFixtures.session()
        val state = HistoryPager.acceptLatest(
            HistoryPagerState.begin(scope, ConversationFixtures.projection),
            ConversationFixtures.page(
                listOf(ConversationFixtures.textMessage("m3", "three", sequence = 3)),
            ),
            nowMillis = 0,
        ).state

        val exact = HistoryPager.acceptExact(
            state,
            ConversationFixtures.page(
                listOf(
                    ConversationFixtures.textMessage("m1", "one", sequence = 1),
                    ConversationFixtures.textMessage("m3", "three from exact", sequence = 3),
                ),
                exact = true,
            ),
            nowMillis = 0,
            messageId = "m1",
        )
        assertEquals("m1", exact.state.exactMessageId)
        assertEquals(listOf("m1", "m3"), exact.state.messages.map { it.messageId })
        assertEquals("three from exact", exact.state.messages.first { it.messageId == "m3" }.blocks.single().text)

        val missing = HistoryPager.acceptExact(
            exact.state,
            ConversationFixtures.page(listOf(ConversationFixtures.textMessage("m2", "two")), exact = true),
            nowMillis = 0,
            messageId = "does-not-exist",
        )
        assertEquals(
            HistoryPagerStatus.Invalidated(HistoryInvalidReason.ExactMissing),
            missing.state.status,
        )

        val foreign = HistoryPager.acceptExact(
            state,
            ConversationFixtures.page(
                listOf(ConversationFixtures.textMessage("m1", "one")),
                session = ConversationFixtures.session(sessionId = "other-session"),
                exact = true,
            ),
            nowMillis = 0,
            messageId = "m1",
        )
        assertEquals(
            HistoryPagerStatus.Invalidated(HistoryInvalidReason.ForeignSession),
            foreign.state.status,
        )
    }

    @Test
    @DisplayName("AC-09.cache eligible cached page is marked and empty or unavailable stays distinct")
    fun ac09Cache() {
        val scope = ConversationFixtures.session()
        val state = HistoryPagerState.begin(scope, ConversationFixtures.projection)

        val cached = HistoryPager.acceptCached(
            HistoryPager.expectLatest(state),
            ConversationFixtures.page(
                listOf(ConversationFixtures.textMessage("m1", "cached", sequence = 1)),
                cached = true,
            ),
            nowMillis = 0,
        )
        assertTrue(cached.state.cached)
        assertEquals(HistoryPagerStatus.Cached(atEnd = true, available = true), cached.state.status)

        val empty = HistoryPager.acceptCached(
            state,
            ConversationFixtures.page(emptyList(), cached = true),
            nowMillis = 0,
        )
        assertEquals(HistoryPagerStatus.Cached(atEnd = true, available = false), empty.state.status)
        assertTrue(empty.state.cached)

        val unavailable = HistoryPager.cachedUnavailable(
            cached.state,
            CoreFailure("cache_unavailable", "Cache unavailable"),
        )
        assertEquals(
            HistoryPagerStatus.CachedUnavailable(CoreFailure("cache_unavailable", "Cache unavailable")),
            unavailable.state.status,
        )
        assertEquals(1, unavailable.state.messages.size)

        val notCached = HistoryPager.acceptCached(
            state,
            ConversationFixtures.page(emptyList(), cached = false),
            nowMillis = 0,
        )
        assertEquals(
            HistoryPagerStatus.Invalidated(HistoryInvalidReason.ProjectionMismatch),
            notCached.state.status,
        )
    }

    @Test
    @DisplayName("AC-09.invalidated expired replaced projection or foreign continuation starts a new page")
    fun ac09Invalidated() {
        val scope = ConversationFixtures.session()
        val live = ConversationFixtures.page(
            listOf(ConversationFixtures.textMessage("m3", "three", sequence = 3)),
            continuation = ConversationFixtures.cursor("cursor-2", expiresAtMillis = 1_000),
        )
        val state = HistoryPager.acceptLatest(
            HistoryPagerState.begin(scope, ConversationFixtures.projection),
            live,
            nowMillis = 0,
        ).state

        val expired = HistoryPager.requestOlder(state, nowMillis = 2_000)
        assertTrue(expired is OlderPageStart.Invalidated)
        val expiredState = (expired as OlderPageStart.Invalidated).result.state
        assertEquals(
            HistoryPagerStatus.Invalidated(HistoryInvalidReason.Expired),
            expiredState.status,
        )
        assertTrue(expiredState.messages.isEmpty())
        assertNull(expiredState.continuation)

        // A cursor replaced while the read is in flight is rejected.
        val request = (HistoryPager.requestOlder(state, nowMillis = 0) as OlderPageStart.Request).cursor
        val replacedState = state.copy(continuation = ConversationFixtures.cursor("cursor-9"))
        val replaced = HistoryPager.acceptOlder(
            replacedState,
            request,
            ConversationFixtures.page(listOf(ConversationFixtures.textMessage("m2", "two"))),
            nowMillis = 0,
            anchor = null,
        )
        assertEquals(
            HistoryPagerStatus.Invalidated(HistoryInvalidReason.Replaced),
            replaced.state.status,
        )

        val projectionMismatch = HistoryPager.acceptOlder(
            state,
            request,
            ConversationFixtures.page(
                listOf(ConversationFixtures.textMessage("m2", "two")),
                projection = "other-projection",
            ),
            nowMillis = 0,
            anchor = null,
        )
        assertEquals(
            HistoryPagerStatus.Invalidated(HistoryInvalidReason.ProjectionMismatch),
            projectionMismatch.state.status,
        )

        val foreign = HistoryPager.acceptLatest(
            state,
            ConversationFixtures.page(
                listOf(ConversationFixtures.textMessage("foreign", "foreign")),
                session = ConversationFixtures.session(sessionId = "foreign-session"),
            ),
            nowMillis = 0,
        )
        assertEquals(
            HistoryPagerStatus.Invalidated(HistoryInvalidReason.ForeignSession),
            foreign.state.status,
        )
        assertTrue(foreign.state.messages.isEmpty())

        // A new starting page after invalidation never mixes the old list.
        val fresh = HistoryPager.acceptLatest(
            projectionMismatch.state,
            ConversationFixtures.page(
                listOf(ConversationFixtures.textMessage("fresh", "fresh", sequence = 1)),
                continuation = ConversationFixtures.cursor("cursor-fresh"),
            ),
            nowMillis = 0,
        )
        assertEquals(listOf("fresh"), fresh.state.messages.map { it.messageId })
        assertEquals(HistoryPagerStatus.Ready(atEnd = false), fresh.state.status)
    }
}
