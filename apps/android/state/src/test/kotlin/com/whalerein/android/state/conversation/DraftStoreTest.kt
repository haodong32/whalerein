package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.fixtures.conversation.ConversationFixtures
import com.whalerein.android.state.fixtures.conversation.FakeDraftPort
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class DraftStoreTest {
    private val session = ConversationFixtures.session()

    @Test
    @DisplayName("AC-27.draft-store claim flush and replacement fencing")
    fun claimFlushAndReplacement() = runTest {
        val port = FakeDraftPort()
        val store = DraftStore(port)
        val first = (store.claim(session) as Outcome.Success).value
        assertEquals(null, first.text)

        val firstRevision = store.update(first.lease, "first draft")
        assertTrueSuccess(store.setAndFlush(first.lease, "first draft", firstRevision))
        assertEquals("first draft", port.drafts[session])

        val second = (store.claim(session) as Outcome.Success).value
        assertEquals("first draft", second.text)

        // The old lease can no longer overwrite the replacement.
        val staleRevision = store.update(first.lease, "stale")
        assertTrueSuccess(store.setAndFlush(first.lease, "stale", staleRevision))
        assertEquals("first draft", port.drafts[session])

        val nextRevision = store.update(second.lease, "second draft")
        assertTrueSuccess(store.setAndFlush(second.lease, "second draft", nextRevision))
        assertEquals("second draft", port.drafts[session])
    }

    @Test
    @DisplayName("AC-27.draft-store conditional clear keeps newer text")
    fun conditionalClearKeepsNewerText() = runTest {
        val port = FakeDraftPort()
        val store = DraftStore(port)
        val claim = (store.claim(session) as Outcome.Success).value
        val revision = store.update(claim.lease, "newer")
        assertTrueSuccess(store.setAndFlush(claim.lease, "newer", revision))

        assertTrueSuccess(store.clearIfCurrent(claim.lease, matching = "submitted"))
        assertEquals("newer", port.drafts[session])
    }

    @Test
    @DisplayName("AC-27.draft-store a clear waits for the in-flight save")
    fun clearWaitsForInFlightSave() = runTest {
        val port = FakeDraftPort()
        val gate = CompletableDeferred<Unit>()
        port.saveGate = gate
        val store = DraftStore(port)
        val claim = (store.claim(session) as Outcome.Success).value
        val revision = store.update(claim.lease, "submitted")
        val save = launch { store.setAndFlush(claim.lease, "submitted", revision) }
        advanceUntilIdle()
        assertEquals(listOf("save-start"), port.writeOrder)

        val clear = launch { store.clearIfCurrent(claim.lease, matching = "submitted") }
        advanceUntilIdle()
        assertEquals(listOf("save-start"), port.writeOrder)

        gate.complete(Unit)
        advanceUntilIdle()
        save.join()
        clear.join()
        assertEquals(listOf("save-start", "save-finish", "clear"), port.writeOrder)
        assertNull(port.drafts[session])
    }

    private fun assertTrueSuccess(outcome: Outcome<Unit>) {
        check(outcome is Outcome.Success) { "expected success, got $outcome" }
    }
}
