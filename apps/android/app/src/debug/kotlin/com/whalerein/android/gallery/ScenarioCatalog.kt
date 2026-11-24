package com.whalerein.android.gallery

import androidx.navigation3.runtime.NavKey
import com.whalerein.android.FakeAppDependencies
import com.whalerein.android.FakeSessionListPort
import com.whalerein.android.navigation.ConversationKey
import com.whalerein.android.navigation.SessionsKey
import com.whalerein.android.state.conversation.AttemptEnded
import com.whalerein.android.state.conversation.AttemptRef
import com.whalerein.android.state.conversation.ConversationEvent
import com.whalerein.android.state.conversation.Guard
import com.whalerein.android.state.conversation.Message
import com.whalerein.android.state.conversation.MessageBlock
import com.whalerein.android.state.conversation.MessageBlockKind
import com.whalerein.android.state.conversation.MessageCommitted
import com.whalerein.android.state.conversation.MessageRole
import com.whalerein.android.state.conversation.ObservationBatch
import com.whalerein.android.state.conversation.PendingApproval
import com.whalerein.android.state.conversation.PendingItem
import com.whalerein.android.state.conversation.PreviewAppended
import com.whalerein.android.state.conversation.PreviewReplaced
import com.whalerein.android.state.conversation.Question
import com.whalerein.android.state.conversation.QuestionBatch
import com.whalerein.android.state.conversation.QuestionKind
import com.whalerein.android.state.conversation.QuestionOption
import com.whalerein.android.state.conversation.RunPhase
import com.whalerein.android.state.conversation.RunPhaseChanged
import com.whalerein.android.state.conversation.SessionDetails
import com.whalerein.android.state.conversation.SessionListSnapshot
import com.whalerein.android.state.conversation.SessionSummary
import com.whalerein.android.state.conversation.WatchEndReason
import com.whalerein.android.state.core.AttemptId
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef

/**
 * One reproducible Debug scenario.
 *
 * [create] always builds fresh in-memory dependencies, so opening a scenario
 * again cannot inherit navigation, view-model or observation state from a
 * previous run. [initialBackStack] is the production destination stack the
 * scenario opens on.
 */
data class GalleryScenario(
    val id: String,
    val title: String,
    val notes: String,
    val create: () -> FakeAppDependencies,
    val initialBackStack: () -> List<NavKey>,
)

/**
 * The single scenario catalog shared by the Gallery and instrumentation tests.
 *
 * IDs are stable: UI tests open the same scenarios a developer inspects. The
 * fixtures use fixed clocks, sequential IDs and scripted events only.
 */
object ScenarioCatalog {
    const val EXTRA_SCENARIO_ID = "com.whalerein.android.gallery.SCENARIO_ID"

    val all: List<GalleryScenario> = listOf(
        GalleryScenario(
            id = "sessions.empty",
            title = "Empty session list",
            notes = "Empty state, refresh and the create-session entry point.",
            create = { emptySessionDependencies() },
            initialBackStack = { sessionStack() },
        ),
        GalleryScenario(
            id = "sessions.populated",
            title = "Populated session list",
            notes = "Select a session, then verify narrow-screen back and wide list-detail.",
            create = { populatedSessionDependencies() },
            initialBackStack = { sessionStack() },
        ),
        GalleryScenario(
            id = "conversation.streaming",
            title = "Streaming conversation",
            notes = "Incremental output, final replacement and scroll position.",
            create = { streamingConversationDependencies() },
            initialBackStack = { conversationStack() },
        ),
        GalleryScenario(
            id = "conversation.approval",
            title = "Pending tool approval",
            notes = "Pending indicator, approval sheet and decision feedback.",
            create = { approvalConversationDependencies() },
            initialBackStack = { conversationStack(pendingSheet = true) },
        ),
        GalleryScenario(
            id = "conversation.question",
            title = "Pending user question",
            notes = "Question, options, submission and feedback.",
            create = { questionConversationDependencies() },
            initialBackStack = { conversationStack(pendingSheet = true) },
        ),
    )

    fun find(id: String): GalleryScenario? = all.firstOrNull { it.id == id }

    fun require(id: String): GalleryScenario =
        requireNotNull(find(id)) { "unknown state gallery scenario: $id" }
}

// Fixed identities keep every scenario deterministic and comparable.
private val GALLERY_INSTANCE = InstanceId("gallery-instance")
private val GALLERY_HOST = HostRef(GALLERY_INSTANCE, HostId("gallery-host"))
private val FIRST_SESSION = SessionRef(GALLERY_HOST, SessionId("gallery-release-readiness"))
private val SECOND_SESSION = SessionRef(GALLERY_HOST, SessionId("gallery-flaky-test"))
private val THIRD_SESSION = SessionRef(GALLERY_HOST, SessionId("gallery-architecture-notes"))
private val CONVERSATION_SESSION = FIRST_SESSION

private fun sessionStack(): List<NavKey> =
    listOf(SessionsKey(GALLERY_INSTANCE.value, GALLERY_HOST.hostId.value))

private fun conversationStack(pendingSheet: Boolean = false): List<NavKey> =
    sessionStack() + ConversationKey(
        instanceId = GALLERY_INSTANCE.value,
        hostId = GALLERY_HOST.hostId.value,
        sessionId = CONVERSATION_SESSION.sessionId.value,
        pendingSheet = pendingSheet,
    )

private fun emptySessionDependencies(): FakeAppDependencies {
    val dependencies = FakeAppDependencies(GALLERY_INSTANCE)
    (dependencies.sessionList as FakeSessionListPort).snapshot = SessionListSnapshot(
        host = GALLERY_HOST,
        sessions = emptyList(),
    )
    return dependencies
}

private fun populatedSessionDependencies(): FakeAppDependencies {
    val dependencies = FakeAppDependencies(GALLERY_INSTANCE)
    val port = dependencies.sessionList as FakeSessionListPort
    port.snapshot = SessionListSnapshot(
        host = GALLERY_HOST,
        sessions = listOf(
            SessionSummary(
                sessionId = FIRST_SESSION.sessionId,
                title = "Release readiness",
                updatedAtMillis = 1_700_000_300_000L,
                hasPendingInput = false,
            ),
            SessionSummary(
                sessionId = SECOND_SESSION.sessionId,
                title = "Investigate the flaky device test",
                updatedAtMillis = 1_700_000_200_000L,
                hasPendingInput = true,
            ),
            SessionSummary(
                sessionId = THIRD_SESSION.sessionId,
                title = "Architecture notes",
                updatedAtMillis = 1_700_000_100_000L,
            ),
        ),
    )
    port.detailsBySession = listOf(
        SessionDetails(
            session = FIRST_SESSION,
            title = "Release readiness",
            workingDirectory = "/workspace/whalerein",
            workspace = "release",
            preset = "build",
            latestCompletedTurnId = "turn-42",
            updatedAtMillis = 1_700_000_300_000L,
        ),
        SessionDetails(
            session = SECOND_SESSION,
            title = "Investigate the flaky device test",
            workingDirectory = "/workspace/whalerein",
            workspace = "android",
            preset = "test",
            latestCompletedTurnId = "turn-41",
            updatedAtMillis = 1_700_000_200_000L,
        ),
        SessionDetails(
            session = THIRD_SESSION,
            title = "Architecture notes",
            workingDirectory = "/workspace/whalerein",
            workspace = "docs",
            preset = "review",
            latestCompletedTurnId = "turn-40",
            updatedAtMillis = 1_700_000_100_000L,
        ),
    ).associateBy { it.session.sessionId }
    dependencies.fakeHistory.messages = baseMessages()
    return dependencies
}

private fun streamingConversationDependencies(): FakeAppDependencies {
    val dependencies = populatedSessionDependencies()
    val firstPreview = "Checking the pipelines"
    val finalAnswer = "Release is ready: all checks passed."
    // Slow enough to inspect incremental output and scroll position by hand.
    dependencies.fakeObservation.stepDelayMillis = 700
    dependencies.fakeObservation.script = { watch, step ->
        val scope = watch.scope
        val generation = watch.generation
        val attempt = AttemptRef(generation, AttemptId("gallery-attempt"))
        when (step) {
            0 -> batch(
                scope,
                generation,
                RunPhaseChanged(scope, generation, RunPhase.Running),
            )

            1 -> batch(
                scope,
                generation,
                PreviewAppended(
                    scope = scope,
                    generation = generation,
                    attempt = attempt,
                    blockId = "gallery-answer",
                    kind = MessageBlockKind.Text,
                    text = "$firstPreview and",
                    incomplete = true,
                ),
            )

            2 -> batch(
                scope,
                generation,
                PreviewReplaced(
                    scope = scope,
                    generation = generation,
                    attempt = attempt,
                    blockId = "gallery-answer",
                    kind = MessageBlockKind.Text,
                    text = "$firstPreview. Drafting the summary.",
                    incomplete = true,
                ),
            )

            3 -> batch(
                scope,
                generation,
                PreviewReplaced(
                    scope = scope,
                    generation = generation,
                    attempt = attempt,
                    blockId = "gallery-answer",
                    kind = MessageBlockKind.Text,
                    text = finalAnswer,
                    incomplete = false,
                ),
                MessageCommitted(
                    scope = scope,
                    generation = generation,
                    message = assistantMessage(
                        id = "gallery-final-answer",
                        text = finalAnswer,
                        sequence = 3,
                        origin = attempt,
                    ),
                ),
                AttemptEnded(scope, generation, attempt),
                RunPhaseChanged(scope, generation, RunPhase.Idle),
            ).copy(ended = WatchEndReason.Quiescent)

            else -> null
        }
    }
    return dependencies
}

private fun approvalConversationDependencies(): FakeAppDependencies {
    val dependencies = populatedSessionDependencies()
    dependencies.fakePendingInput.items = listOf(
        PendingItem.Approval(
            PendingApproval(
                approvalId = "gallery-approval",
                title = "Run the deployment script?",
                body = "The Host wants to run ./scripts/deploy.sh and publish the release artifacts.",
                guard = Guard("gallery-approval-guard"),
                expiresAtMillis = null,
            ),
        ),
    )
    return dependencies
}

private fun questionConversationDependencies(): FakeAppDependencies {
    val dependencies = populatedSessionDependencies()
    dependencies.fakePendingInput.items = listOf(
        PendingItem.Questions(
            QuestionBatch(
                batchId = "gallery-question-batch",
                title = "Deployment details",
                guard = Guard("gallery-question-guard"),
                questions = listOf(
                    Question(
                        questionId = "gallery-environment",
                        kind = QuestionKind.SingleChoice,
                        prompt = "Which environment should the Host deploy to?",
                        options = listOf(
                            QuestionOption("staging", "Staging", "Pre-production environment"),
                            QuestionOption("production", "Production", "Public release"),
                        ),
                    ),
                    Question(
                        questionId = "gallery-note",
                        kind = QuestionKind.Open,
                        prompt = "What should the release note mention?",
                        allowOpenAnswer = true,
                        required = false,
                    ),
                ),
            ),
        ),
    )
    return dependencies
}

private fun batch(
    scope: SessionRef,
    generation: Long,
    vararg events: ConversationEvent,
): ObservationBatch = ObservationBatch(
    scope = scope,
    generation = generation,
    events = events.toList(),
)

private fun baseMessages(): List<Message> = listOf(
    userMessage(
        id = "gallery-user-question",
        text = "Summarize the release readiness check.",
        sequence = 1,
    ),
    assistantMessage(
        id = "gallery-assistant-ack",
        text = "I will verify the build matrix and the latest test report.",
        sequence = 2,
    ),
)

private fun userMessage(id: String, text: String, sequence: Long): Message = Message(
    messageId = id,
    role = MessageRole.User,
    blocks = listOf(MessageBlock(blockId = "$id-block", kind = MessageBlockKind.Text, text = text)),
    sequence = sequence,
    createdAtMillis = 1_700_000_000_000L + sequence,
)

private fun assistantMessage(
    id: String,
    text: String,
    sequence: Long,
    origin: AttemptRef? = null,
): Message = Message(
    messageId = id,
    role = MessageRole.Assistant,
    blocks = listOf(MessageBlock(blockId = "$id-block", kind = MessageBlockKind.Text, text = text)),
    sequence = sequence,
    origin = origin,
    createdAtMillis = 1_700_000_000_000L + sequence,
)