package com.whalerein.android.state.fixtures.conversation

import com.whalerein.android.state.conversation.AttemptRef
import com.whalerein.android.state.conversation.CONVERSATION_PROJECTION_ANDROID_V1
import com.whalerein.android.state.conversation.HistoryCursor
import com.whalerein.android.state.conversation.HistoryPage
import com.whalerein.android.state.conversation.Message
import com.whalerein.android.state.conversation.MessageBlock
import com.whalerein.android.state.conversation.MessageBlockKind
import com.whalerein.android.state.conversation.MessageRole
import com.whalerein.android.state.conversation.PendingApproval
import com.whalerein.android.state.conversation.PendingItem
import com.whalerein.android.state.conversation.PendingQueueItem
import com.whalerein.android.state.conversation.PreviewBlock
import com.whalerein.android.state.conversation.Question
import com.whalerein.android.state.conversation.QuestionBatch
import com.whalerein.android.state.conversation.QuestionKind
import com.whalerein.android.state.conversation.QuestionOption
import com.whalerein.android.state.conversation.CreateSessionInput
import com.whalerein.android.state.conversation.CreatedSession
import com.whalerein.android.state.conversation.ForkedSession
import com.whalerein.android.state.conversation.Guard
import com.whalerein.android.state.conversation.SessionCapabilities
import com.whalerein.android.state.conversation.SessionDetails
import com.whalerein.android.state.conversation.SessionSummary
import com.whalerein.android.state.core.AttemptId
import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.IdGenerator
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef

/** Deterministic builders shared by conversation JVM tests. */
object ConversationFixtures {
    val projection: String = CONVERSATION_PROJECTION_ANDROID_V1

    fun host(
        instance: String = "instance-1",
        host: String = "host-1",
    ): HostRef = HostRef(
        instance = InstanceId(instance),
        hostId = HostId(host),
    )

    fun session(
        instance: String = "instance-1",
        host: String = "host-1",
        sessionId: String = "session-1",
    ): SessionRef = SessionRef(
        host = host(instance, host),
        sessionId = SessionId(sessionId),
    )

    fun sessionSummary(
        sessionId: String = "session-1",
        title: String? = "Session",
        updatedAtMillis: Long = 0,
        hasPendingInput: Boolean = false,
    ): SessionSummary = SessionSummary(
        sessionId = SessionId(sessionId),
        title = title,
        updatedAtMillis = updatedAtMillis,
        hasPendingInput = hasPendingInput,
    )

    fun sessionDetails(
        session: SessionRef = session(),
        title: String? = "Session",
        workingDirectory: String? = "/workspace",
        workspace: String? = null,
        preset: String? = null,
        latestCompletedTurnId: String? = "turn-1",
        updatedAtMillis: Long = 0,
        cached: Boolean = false,
    ): SessionDetails = SessionDetails(
        session = session,
        title = title,
        workingDirectory = workingDirectory,
        workspace = workspace,
        preset = preset,
        latestCompletedTurnId = latestCompletedTurnId,
        updatedAtMillis = updatedAtMillis,
        cached = cached,
    )

    fun capabilities(
        canCreate: Boolean = true,
        canFork: Boolean = true,
        supportsWorkingDirectory: Boolean = true,
        supportsWorkspace: Boolean = true,
        supportsPreset: Boolean = true,
        supportsPreferredSessionId: Boolean = true,
    ): SessionCapabilities = SessionCapabilities(
        canCreate = canCreate,
        canFork = canFork,
        supportsWorkingDirectory = supportsWorkingDirectory,
        supportsWorkspace = supportsWorkspace,
        supportsPreset = supportsPreset,
        supportsPreferredSessionId = supportsPreferredSessionId,
    )

    fun created(
        session: SessionRef = session(sessionId = "created-1"),
        details: SessionDetails = sessionDetails(session),
    ): CreatedSession = CreatedSession(session, details)

    fun forked(
        child: SessionRef = session(sessionId = "child-1"),
        details: SessionDetails = sessionDetails(child),
        workingDirectoryAvailable: Boolean = true,
    ): ForkedSession = ForkedSession(child, details, workingDirectoryAvailable)

    fun createInput(
        workingDirectory: String? = null,
        workspace: String? = null,
        preset: String? = null,
        preferredSessionId: String? = null,
    ): CreateSessionInput = CreateSessionInput(
        workingDirectory = workingDirectory,
        workspace = workspace,
        preset = preset,
        preferredSessionId = preferredSessionId?.let { SessionId(it) },
    )

    fun attempt(id: String = "attempt-1", epoch: Long = 1): AttemptRef =
        AttemptRef(liveEpoch = epoch, attemptId = AttemptId(id))

    fun block(
        blockId: String,
        kind: MessageBlockKind = MessageBlockKind.Text,
        text: String = "",
        fields: Map<String, String> = emptyMap(),
    ): MessageBlock = MessageBlock(blockId, kind, text, fields)

    fun message(
        messageId: String,
        sequence: Long,
        blocks: List<MessageBlock>,
        role: MessageRole = MessageRole.Assistant,
        origin: AttemptRef? = null,
        createdAtMillis: Long = 0,
    ): Message = Message(
        messageId = messageId,
        role = role,
        blocks = blocks,
        sequence = sequence,
        origin = origin,
        createdAtMillis = createdAtMillis,
    )

    fun textMessage(
        messageId: String,
        text: String,
        sequence: Long = 1,
        origin: AttemptRef? = null,
    ): Message = message(
        messageId = messageId,
        sequence = sequence,
        blocks = listOf(block("$messageId-answer", MessageBlockKind.Text, text)),
        origin = origin,
    )

    fun preview(
        attempt: AttemptRef,
        blockId: String,
        kind: MessageBlockKind,
        text: String,
        incomplete: Boolean = false,
    ): PreviewBlock = PreviewBlock(
        attempt = attempt,
        blockId = blockId,
        kind = kind,
        text = text,
        incomplete = incomplete,
    )

    fun cursor(
        continuation: String,
        session: SessionRef = session(),
        projection: String = this.projection,
        expiresAtMillis: Long = Long.MAX_VALUE,
    ): HistoryCursor = HistoryCursor(
        continuation = continuation,
        session = session,
        projection = projection,
        expiresAtMillis = expiresAtMillis,
    )

    fun page(
        messages: List<Message>,
        session: SessionRef = session(),
        projection: String = this.projection,
        continuation: HistoryCursor? = null,
        cached: Boolean = false,
        exact: Boolean = false,
    ): HistoryPage = HistoryPage(
        session = session,
        projection = projection,
        messages = messages,
        continuation = continuation,
        cached = cached,
        exact = exact,
    )

    fun approval(
        approvalId: String = "approval-1",
        guard: String = "guard-1",
        expiresAtMillis: Long? = null,
        title: String = "Run command",
        body: String? = "rm -rf build",
    ): PendingApproval = PendingApproval(
        approvalId = approvalId,
        title = title,
        body = body,
        guard = Guard(guard),
        expiresAtMillis = expiresAtMillis,
    )

    fun approvalItem(
        approvalId: String = "approval-1",
        guard: String = "guard-1",
        expiresAtMillis: Long? = null,
    ): PendingItem.Approval = PendingItem.Approval(
        approval(
            approvalId = approvalId,
            guard = guard,
            expiresAtMillis = expiresAtMillis,
        ),
    )

    fun questionBatch(
        batchId: String = "batch-1",
        guard: String = "guard-1",
        questions: List<Question> = listOf(
            Question(
                questionId = "question-1",
                kind = QuestionKind.Open,
                prompt = "What should the next step be?",
                allowOpenAnswer = true,
            ),
        ),
        expiresAtMillis: Long? = null,
    ): QuestionBatch = QuestionBatch(
        batchId = batchId,
        questions = questions,
        guard = Guard(guard),
        expiresAtMillis = expiresAtMillis,
    )

    fun questionItem(
        batchId: String = "batch-1",
        guard: String = "guard-1",
        expiresAtMillis: Long? = null,
    ): PendingItem.Questions = PendingItem.Questions(
        questionBatch(
            batchId = batchId,
            guard = guard,
            expiresAtMillis = expiresAtMillis,
        ),
    )

    fun multiChoiceQuestion(
        questionId: String = "question-2",
        options: List<QuestionOption> = listOf(
            QuestionOption("a", "Option A"),
            QuestionOption("b", "Option B"),
        ),
    ): Question = Question(
        questionId = questionId,
        kind = QuestionKind.MultiChoice,
        prompt = "Pick any that apply",
        options = options,
        allowOpenAnswer = true,
    )

    fun queueItem(
        itemId: String = "item-1",
        text: String = "queued message",
        guard: String = "guard-1",
        steerable: Boolean = true,
    ): PendingQueueItem = PendingQueueItem(
        itemId = itemId,
        text = text,
        guard = Guard(guard),
        steerable = steerable,
    )
}

class FakeClock(var nowMillis: Long = 1_000L) : Clock {
    override fun epochMillis(): Long = nowMillis

    override fun elapsedMillis(): Long = nowMillis
}

class SequentialIdGenerator(private val prefix: String = "id") : IdGenerator {
    private var next = 0

    override fun newId(): String {
        next += 1
        return "$prefix-$next"
    }
}

class RecordingDiagnosticSink : DiagnosticSink {
    val events = mutableListOf<DiagnosticEvent>()

    override fun emit(event: DiagnosticEvent) {
        events += event
    }
}
