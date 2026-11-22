package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.AttemptId

/**
 * Domain model for the conversation slice.
 *
 * These types are deliberately Compose-free and `android.*`-free so the whole
 * reducer, pager, render projection and screen models run as plain JVM code.
 * Message content and reasoning live only in these values; diagnostics never
 * receive them.
 */

/** A scoped live attempt identity: the observation epoch that saw it and its id. */
data class AttemptRef(
    val liveEpoch: Long,
    val attemptId: AttemptId,
)

enum class MessageRole { User, Assistant, Tool, System }

enum class MessageBlockKind { Text, Reasoning, Code, Notice, Tool, Attachment, Unknown }

/**
 * Content a first-visible-output observation may satisfy.
 *
 * Hidden reasoning and an unknown (fallback-only) block must not satisfy it.
 */
val MessageBlockKind.isVisibleOutputBlock: Boolean
    get() = when (this) {
        MessageBlockKind.Text,
        MessageBlockKind.Code,
        MessageBlockKind.Notice,
        MessageBlockKind.Tool,
        MessageBlockKind.Attachment,
        -> true

        MessageBlockKind.Reasoning,
        MessageBlockKind.Unknown,
        -> false
    }

/**
 * One typed committed or preview block.
 *
 * [fields] holds small structured values such as the code language fence, the
 * tool `call_id`, the attachment id, a notice kind or an availability flag. It
 * must never be used as a log payload.
 */
data class MessageBlock(
    val blockId: String,
    val kind: MessageBlockKind,
    val text: String = "",
    val fields: Map<String, String> = emptyMap(),
) {
    val isReasoning: Boolean get() = kind == MessageBlockKind.Reasoning

    /** Content a first-visible-output observation may satisfy. */
    val isVisibleOutput: Boolean get() = kind.isVisibleOutputBlock

    val callId: String? get() = fields[FIELD_CALL_ID]

    val language: String? get() = fields[FIELD_LANGUAGE]

    val noticeKind: String? get() = fields[FIELD_NOTICE_KIND]

    val attachmentId: String? get() = fields[FIELD_ATTACHMENT_ID]

    companion object {
        const val FIELD_CALL_ID = "call_id"
        const val FIELD_LANGUAGE = "language"
        const val FIELD_NOTICE_KIND = "notice_kind"
        const val FIELD_ATTACHMENT_ID = "attachment_id"
        const val FIELD_ATTACHMENT_AVAILABLE = "attachment_available"
        const val FIELD_ATTACHMENT_NAME = "attachment_name"
        const val FIELD_TITLE = "title"
    }
}

/** One canonical committed message; [messageId] is the stable merge identity. */
data class Message(
    val messageId: String,
    val role: MessageRole,
    val blocks: List<MessageBlock> = emptyList(),
    /** Source order refreshed from history; used only to merge pages. */
    val sequence: Long = 0,
    /**
     * The live attempt that produced this message, when Core supplies a verified
     * origin. Only a matching origin may supersede a preview.
     */
    val origin: AttemptRef? = null,
    val createdAtMillis: Long = 0,
)

/**
 * One provisional streaming block. Identity is the pair (attempt, blockId), so
 * an explicit replacement targets exactly one block.
 */
data class PreviewBlock(
    val attempt: AttemptRef,
    val blockId: String,
    val kind: MessageBlockKind,
    val text: String = "",
    /** True when the last applied update was a targeted replacement. */
    val replace: Boolean = false,
    /** True while the Source marked the block as unfinished. */
    val incomplete: Boolean = false,
    /** Insertion order for stable Compose item keys. */
    val order: Long = 0,
) {
    val attemptId: AttemptId get() = attempt.attemptId
}

enum class ToolActivityState { Pending, Running, Completed, Failed, Unknown }

/** Tool card state keyed by the canonical Source `call_id`. */
data class ToolActivity(
    val callId: String,
    val name: String,
    val state: ToolActivityState,
    val summary: String = "",
    val fullOutput: String? = null,
    val incomplete: Boolean = false,
)

enum class RunPhase {
    Unknown,
    Idle,
    Running,
    WaitingForInput,
    Completed,
    Failed,
    ;

    val isActive: Boolean
        get() = this == Running || this == WaitingForInput
}

/** A reviewed decision guard. It is never content and may appear in diagnostics. */
@JvmInline
value class Guard(val value: String)

data class PendingApproval(
    val approvalId: String,
    val title: String,
    val body: String? = null,
    val guard: Guard,
    val expiresAtMillis: Long? = null,
)

enum class QuestionKind { SingleChoice, MultiChoice, Open, Continued }

data class QuestionOption(
    val optionId: String,
    val label: String,
    val description: String? = null,
)

data class Question(
    val questionId: String,
    val kind: QuestionKind,
    val prompt: String,
    val options: List<QuestionOption> = emptyList(),
    /** A choice question may additionally accept an open answer. */
    val allowOpenAnswer: Boolean = false,
    val required: Boolean = true,
)

data class QuestionBatch(
    val batchId: String,
    val questions: List<Question>,
    val guard: Guard,
    val title: String? = null,
    val expiresAtMillis: Long? = null,
)

/** One item of the fixed pending-input entry. */
sealed interface PendingItem {
    val itemId: String
    val guard: Guard
    val expiresAtMillis: Long?

    data class Approval(val approval: PendingApproval) : PendingItem {
        override val itemId: String get() = approval.approvalId
        override val guard: Guard get() = approval.guard
        override val expiresAtMillis: Long? get() = approval.expiresAtMillis
    }

    data class Questions(val batch: QuestionBatch) : PendingItem {
        override val itemId: String get() = batch.batchId
        override val guard: Guard get() = batch.guard
        override val expiresAtMillis: Long? get() = batch.expiresAtMillis
    }
}

/** One structured answer to one question. */
data class QuestionAnswer(
    val questionId: String,
    val selectedOptionIds: List<String> = emptyList(),
    val text: String? = null,
)

/** Server projection identity used to reject mixed history pages. */
const val CONVERSATION_PROJECTION_ANDROID_V1 = "android-v1"
