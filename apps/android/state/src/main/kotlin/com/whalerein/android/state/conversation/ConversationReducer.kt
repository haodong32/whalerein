package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.SessionRef

/**
 * Ordered, scope-bound conversation events.
 *
 * Every event carries its scope and observation generation. The reducer drops an
 * event whose scope/generation does not match the active state, which is how a
 * late batch already transferred into Kotlin cannot alter a newer view.
 */
sealed interface ConversationEvent {
    val scope: SessionRef
    val generation: Long
}

data class PreviewAppended(
    override val scope: SessionRef,
    override val generation: Long,
    val attempt: AttemptRef,
    val blockId: String,
    val kind: MessageBlockKind,
    val text: String,
    val incomplete: Boolean = false,
) : ConversationEvent

/** A targeted replacement for exactly one (attempt, blockId). */
data class PreviewReplaced(
    override val scope: SessionRef,
    override val generation: Long,
    val attempt: AttemptRef,
    val blockId: String,
    val kind: MessageBlockKind,
    val text: String,
    val incomplete: Boolean = false,
) : ConversationEvent

data class AttemptEnded(
    override val scope: SessionRef,
    override val generation: Long,
    val attempt: AttemptRef,
) : ConversationEvent

/** A committed message with an optional verified Core origin. */
data class MessageCommitted(
    override val scope: SessionRef,
    override val generation: Long,
    val message: Message,
) : ConversationEvent

/** A committed history/replay page. Messages are merged by canonical identity. */
data class MessagesLoaded(
    override val scope: SessionRef,
    override val generation: Long,
    val messages: List<Message>,
) : ConversationEvent

/** A fresh starting page or authoritative snapshot: committed history is replaced. */
data class HistoryReplaced(
    override val scope: SessionRef,
    override val generation: Long,
    val messages: List<Message>,
) : ConversationEvent

data class ToolUpdated(
    override val scope: SessionRef,
    override val generation: Long,
    val activity: ToolActivity,
) : ConversationEvent

data class RunPhaseChanged(
    override val scope: SessionRef,
    override val generation: Long,
    val phase: RunPhase,
) : ConversationEvent

data class PendingInputsChanged(
    override val scope: SessionRef,
    override val generation: Long,
    val items: List<PendingItem>,
) : ConversationEvent

/**
 * Pure reducer state. Preview blocks are ordered independently from committed
 * messages; committed identity is the only merge key.
 */
data class ConversationState(
    val scope: SessionRef? = null,
    val generation: Long = 0,
    val committed: List<Message> = emptyList(),
    val previews: List<PreviewBlock> = emptyList(),
    val toolActivities: List<ToolActivity> = emptyList(),
    val pendingItems: List<PendingItem> = emptyList(),
    val phase: RunPhase = RunPhase.Unknown,
    /** True while a committed Attempt end is still waiting for its message. */
    val syncing: Boolean = false,
    val endedAttempts: Set<String> = emptySet(),
    val nextOrder: Long = 0,
) {
    companion object {
        fun begin(scope: SessionRef, generation: Long): ConversationState =
            ConversationState(scope = scope, generation = generation)
    }
}

/**
 * Deterministic, ordered event application.
 *
 * Invariants implemented here:
 * - only the active scope and generation are accepted;
 * - a preview append never merges reasoning into answer content;
 * - only the same (attempt, block) is replaced;
 * - a committed message supersedes only its own preview by verified origin;
 * - a commit may arrive before or after the Attempt-end event;
 * - paged/live repeats collapse to one canonical message id;
 * - a second tool event updates the existing `call_id` card.
 */
object ConversationReducer {
    fun reduce(state: ConversationState, event: ConversationEvent): ConversationState {
        if (state.scope == null || event.scope != state.scope || event.generation != state.generation) {
            return state
        }
        return when (event) {
            is PreviewAppended -> applyPreview(state, event, replace = false)
            is PreviewReplaced -> applyPreview(state, event, replace = true)
            is AttemptEnded -> applyAttemptEnded(state, event)
            is MessageCommitted -> applyCommitted(state, listOf(event.message))
            is MessagesLoaded -> applyCommitted(state, event.messages)
            is HistoryReplaced -> applyReplaced(state, event.messages)
            is ToolUpdated -> applyTool(state, event.activity)
            is RunPhaseChanged -> state.copy(phase = event.phase)
            is PendingInputsChanged -> state.copy(pendingItems = event.items)
        }
    }

    private fun applyPreview(
        state: ConversationState,
        event: ConversationEvent,
        replace: Boolean,
    ): ConversationState {
        val attempt: AttemptRef
        val blockId: String
        val kind: MessageBlockKind
        val text: String
        val incomplete: Boolean
        when (event) {
            is PreviewAppended -> {
                attempt = event.attempt
                blockId = event.blockId
                kind = event.kind
                text = event.text
                incomplete = event.incomplete
            }

            is PreviewReplaced -> {
                attempt = event.attempt
                blockId = event.blockId
                kind = event.kind
                text = event.text
                incomplete = event.incomplete
            }

            else -> error("not a preview event")
        }

        val index = state.previews.indexOfFirst {
            it.attempt.attemptId == attempt.attemptId && it.blockId == blockId
        }
        if (index < 0) {
            val created = PreviewBlock(
                attempt = attempt,
                blockId = blockId,
                kind = kind,
                text = text,
                replace = replace,
                incomplete = incomplete,
                order = state.nextOrder,
            )
            return state.copy(
                previews = state.previews + created,
                nextOrder = state.nextOrder + 1,
            )
        }

        val previous = state.previews[index]
        val updated = if (!replace && previous.kind == kind) {
            previous.copy(
                text = previous.text + text,
                incomplete = previous.incomplete || incomplete,
                replace = false,
            )
        } else {
            previous.copy(
                kind = kind,
                text = if (replace) text else previous.text + text,
                replace = true,
                incomplete = incomplete,
            )
        }
        val previews = state.previews.toMutableList()
        previews[index] = updated
        return state.copy(previews = previews)
    }

    private fun applyAttemptEnded(
        state: ConversationState,
        event: AttemptEnded,
    ): ConversationState {
        val attemptId = event.attempt.attemptId.value
        val alreadyCommitted = state.committed.any {
            it.origin?.attemptId == event.attempt.attemptId
        }
        if (alreadyCommitted) {
            return state
        }
        val ended = state.endedAttempts + attemptId
        return state.copy(
            endedAttempts = ended,
            syncing = ended.isNotEmpty(),
        )
    }

    private fun applyCommitted(
        state: ConversationState,
        messages: List<Message>,
    ): ConversationState {
        if (messages.isEmpty()) {
            return state
        }
        val merged = LinkedHashMap<String, Message>()
        state.committed.forEach { merged[it.messageId] = it }
        messages.forEach { merged[it.messageId] = it }
        val committed = merged.values.sortedWith(
            compareBy<Message>({ it.sequence }, { it.messageId }),
        )

        // A committed message supersedes only the preview of its own verified
        // origin. Unrelated origins or a missing origin leave other previews.
        val committedAttempts = messages.mapNotNull { it.origin?.attemptId }.toSet()
        val previews = if (committedAttempts.isEmpty()) {
            state.previews
        } else {
            state.previews.filterNot { it.attempt.attemptId in committedAttempts }
        }
        val ended = state.endedAttempts - committedAttempts.map { it.value }.toSet()
        return state.copy(
            committed = committed,
            previews = previews,
            endedAttempts = ended,
            syncing = ended.isNotEmpty(),
        )
    }

    private fun applyReplaced(
        state: ConversationState,
        messages: List<Message>,
    ): ConversationState {
        val committed = messages
            .distinctBy { it.messageId }
            .sortedWith(compareBy<Message>({ it.sequence }, { it.messageId }))
        val committedAttempts = committed.mapNotNull { it.origin?.attemptId }.toSet()
        val previews = state.previews.filterNot { it.attempt.attemptId in committedAttempts }
        val ended = state.endedAttempts - committedAttempts.map { it.value }.toSet()
        return state.copy(
            committed = committed,
            previews = previews,
            endedAttempts = ended,
            syncing = ended.isNotEmpty(),
        )
    }

    private fun applyTool(
        state: ConversationState,
        activity: ToolActivity,
    ): ConversationState {
        val index = state.toolActivities.indexOfFirst { it.callId == activity.callId }
        if (index < 0) {
            return state.copy(toolActivities = state.toolActivities + activity)
        }
        val activities = state.toolActivities.toMutableList()
        activities[index] = activity
        return state.copy(toolActivities = activities)
    }
}
