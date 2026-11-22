package com.whalerein.android.state.conversation

import org.junit.jupiter.api.DisplayName
import com.whalerein.android.state.core.SessionRef
import com.whalerein.android.state.fixtures.conversation.ConversationFixtures
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class ConversationReducerTest : ConversationTestBase() {
    @Test
    @DisplayName("AC-10.blocks two live blocks keep independent identity and targeted replacement")
    fun ac10Blocks() {
        val (scope, generation, attempt) = previewScope()
        var state = ConversationState.begin(scope, generation)
        state = append(state, scope, generation, attempt, "reason-1", MessageBlockKind.Reasoning, "Thinking...")
        state = append(state, scope, generation, attempt, "answer-1", MessageBlockKind.Text, "Hel")
        state = append(state, scope, generation, attempt, "answer-1", MessageBlockKind.Text, "lo")

        assertEquals(2, state.previews.size)
        assertEquals("Thinking...", state.previews.first { it.blockId == "reason-1" }.text)
        assertEquals("Hello", state.previews.first { it.blockId == "answer-1" }.text)

        state = ConversationReducer.reduce(
            state,
            PreviewReplaced(
                scope = scope,
                generation = generation,
                attempt = attempt,
                blockId = "answer-1",
                kind = MessageBlockKind.Text,
                text = "Replacement",
            ),
        )

        assertEquals(2, state.previews.size)
        assertEquals("Replacement", state.previews.first { it.blockId == "answer-1" }.text)
        assertEquals("Thinking...", state.previews.first { it.blockId == "reason-1" }.text)
        assertTrue(state.previews.first { it.blockId == "answer-1" }.replace)
        assertEquals(
            listOf(ConversationItemKey.Preview(attempt.attemptId.value, "reason-1"),
                ConversationItemKey.Preview(attempt.attemptId.value, "answer-1")),
            state.toItems().map { it.key },
        )
    }

    @Test
    @DisplayName("AC-10.incomplete reasoning and answer stay separate and incomplete is honored")
    fun ac10Incomplete() {
        val (scope, generation, attempt) = previewScope()
        var state = ConversationState.begin(scope, generation)
        state = ConversationReducer.reduce(
            state,
            PreviewAppended(
                scope,
                generation,
                attempt,
                "answer-1",
                MessageBlockKind.Text,
                "partial answer",
                incomplete = true,
            ),
        )
        state = append(state, scope, generation, attempt, "reason-1", MessageBlockKind.Reasoning, "hidden")

        val answer = state.previews.first { it.blockId == "answer-1" }
        assertTrue(answer.incomplete)
        assertEquals("partial answer", answer.text)
        assertFalse(answer.replace)
        assertEquals("hidden", state.previews.first { it.blockId == "reason-1" }.text)

        val foreign = ConversationReducer.reduce(
            state,
            MessageCommitted(
                scope,
                generation,
                ConversationFixtures.textMessage(
                    messageId = "foreign-message",
                    text = "foreign",
                    sequence = 1,
                    origin = ConversationFixtures.attempt("other-attempt"),
                ),
            ),
        )
        assertEquals(2, foreign.previews.size)
        assertTrue(foreign.previews.first { it.blockId == "answer-1" }.incomplete)
    }

    @Test
    @DisplayName("AC-10.commit-order a commit before or after Attempt end converges to one message")
    fun ac10CommitOrder() {
        val (scope, generation, attempt) = previewScope()
        val previewState = append(
            ConversationState.begin(scope, generation),
            scope,
            generation,
            attempt,
            "answer-1",
            MessageBlockKind.Text,
            "streamed",
        )

        val commit = ConversationFixtures.textMessage("message-1", "final", sequence = 5, origin = attempt)
        val beforeEnd = ConversationReducer.reduce(
            previewState,
            MessageCommitted(scope, generation, commit),
        )
        assertTrue(beforeEnd.previews.isEmpty())
        assertFalse(beforeEnd.syncing)

        val endedAfterCommit = ConversationReducer.reduce(
            beforeEnd,
            AttemptEnded(scope, generation, attempt),
        )
        assertFalse(endedAfterCommit.syncing)
        assertEquals(1, endedAfterCommit.committed.size)

        val endedFirst = ConversationReducer.reduce(
            previewState,
            AttemptEnded(scope, generation, attempt),
        )
        assertTrue(endedFirst.syncing)
        assertEquals(1, endedFirst.previews.size)

        val committedAfterEnd = ConversationReducer.reduce(
            endedFirst,
            MessageCommitted(scope, generation, commit),
        )
        assertFalse(committedAfterEnd.syncing)
        assertTrue(committedAfterEnd.previews.isEmpty())

        val replayed = ConversationReducer.reduce(
            committedAfterEnd,
            MessagesLoaded(
                scope,
                generation,
                listOf(commit.copy(blocks = listOf(ConversationFixtures.block("answer-1", MessageBlockKind.Text, "authoritative")))),
            ),
        )
        assertEquals(1, replayed.committed.size)
        assertEquals("authoritative", replayed.committed.single().blocks.single().text)
        assertEquals(1, replayed.toItems().count { it is CommittedMessageItem })
    }

    @Test
    @DisplayName("AC-10.foreign-origin unrelated or absent origin keeps another preview")
    fun ac10ForeignOrigin() {
        val (scope, generation, attempt) = previewScope()
        val base = append(
            ConversationState.begin(scope, generation),
            scope,
            generation,
            attempt,
            "answer-1",
            MessageBlockKind.Text,
            "streamed",
        )

        val unrelated = ConversationReducer.reduce(
            base,
            MessageCommitted(
                scope,
                generation,
                ConversationFixtures.textMessage(
                    messageId = "other-message",
                    text = "other",
                    sequence = 1,
                    origin = ConversationFixtures.attempt("another-attempt"),
                ),
            ),
        )
        assertEquals(1, unrelated.previews.size)
        assertEquals(1, unrelated.committed.size)

        val absent = ConversationReducer.reduce(
            unrelated,
            MessageCommitted(
                scope,
                generation,
                ConversationFixtures.textMessage("history-message", "history", sequence = 2),
            ),
        )
        assertEquals(1, absent.previews.size)

        val matching = ConversationReducer.reduce(
            absent,
            MessageCommitted(
                scope,
                generation,
                ConversationFixtures.textMessage("matching-message", "matching", sequence = 3, origin = attempt),
            ),
        )
        assertTrue(matching.previews.isEmpty())
    }

    @Test
    @DisplayName("AC-10.tool a second event for the same call updates one card")
    fun ac10Tool() {
        val (scope, generation) = previewScope().let { it.first to it.second }
        var state = ConversationState.begin(scope, generation)
        state = ConversationReducer.reduce(
            state,
            ToolUpdated(
                scope,
                generation,
                ToolActivity("call-1", "shell", ToolActivityState.Running, "running"),
            ),
        )
        state = ConversationReducer.reduce(
            state,
            ToolUpdated(
                scope,
                generation,
                ToolActivity(
                    "call-1",
                    "shell",
                    ToolActivityState.Completed,
                    "done",
                    fullOutput = "full result",
                ),
            ),
        )
        assertEquals(1, state.toolActivities.size)
        val activity = state.toolActivities.single()
        assertEquals(ToolActivityState.Completed, activity.state)
        assertEquals("full result", activity.fullOutput)
        assertEquals(1, state.toItems().count { it is ToolActivityItem })
    }

    @Test
    @DisplayName("AC-09.overlap paged and live repeats yield one canonical row")
    fun ac09Overlap() {
        val scope = ConversationFixtures.session()
        val generation = 1L
        val message = ConversationFixtures.textMessage("message-1", "live", sequence = 4)
        var state = ConversationReducer.reduce(
            ConversationState.begin(scope, generation),
            MessageCommitted(scope, generation, message),
        )
        state = ConversationReducer.reduce(
            state,
            MessagesLoaded(
                scope,
                generation,
                listOf(
                    ConversationFixtures.textMessage("message-0", "older", sequence = 1),
                    message.copy(blocks = listOf(ConversationFixtures.block("answer-1", MessageBlockKind.Text, "paged"))),
                ),
            ),
        )
        val committedItems = state.toItems().filterIsInstance<CommittedMessageItem>()
        assertEquals(2, committedItems.size)
        assertEquals("paged", committedItems.first { it.message.messageId == "message-1" }.message.blocks.single().text)

        val current = state.toItems()
        val insertion = insertOlderMessages(
            current,
            listOf(message, ConversationFixtures.textMessage("message-0", "older", sequence = 1)),
            anchor = ReadingAnchor(ConversationItemKey.Committed("message-1")),
        )
        assertEquals(0, insertion.insertedCount)
        assertEquals(2, insertion.items.filterIsInstance<CommittedMessageItem>().size)
        assertTrue(insertion.anchor != null)
    }

    @Test
    @DisplayName("AC-11.switch old scope and generation events are ignored")
    fun ac11Switch() {
        val first = ConversationFixtures.session(sessionId = "session-1")
        val second = ConversationFixtures.session(sessionId = "session-2")
        val state = ConversationState.begin(second, 2)
        val staleGeneration = ConversationReducer.reduce(
            state,
            RunPhaseChanged(second, 1, RunPhase.Running),
        )
        assertSame(state, staleGeneration)
        val foreignScope = ConversationReducer.reduce(
            state,
            RunPhaseChanged(first, 2, RunPhase.Running),
        )
        assertSame(state, foreignScope)
    }

    private fun previewScope(): Triple<SessionRef, Long, AttemptRef> {
        val scope = ConversationFixtures.session()
        return Triple(scope, 1L, ConversationFixtures.attempt())
    }

    private fun append(
        state: ConversationState,
        scope: SessionRef,
        generation: Long,
        attempt: AttemptRef,
        blockId: String,
        kind: MessageBlockKind,
        text: String,
    ): ConversationState = ConversationReducer.reduce(
        state,
        PreviewAppended(scope, generation, attempt, blockId, kind, text),
    )
}
