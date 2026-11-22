package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.IdGenerator
import com.whalerein.android.state.core.ScreenModel
import com.whalerein.android.state.core.SessionRef
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

/** Which entry matched the currently focused pending item. */
enum class PendingFocusOrigin { None, Live, Historical }

sealed interface PendingDetailUiState {
    data object None : PendingDetailUiState

    data object Loading : PendingDetailUiState

    data class Loaded(val item: PendingItem) : PendingDetailUiState

    data object Missing : PendingDetailUiState

    data class Changed(val failure: CoreFailure) : PendingDetailUiState

    data class Unavailable(val failure: CoreFailure) : PendingDetailUiState
}

data class QuestionAnswerDraft(
    val selectedOptionIds: List<String> = emptyList(),
    val text: String = "",
) {
    fun toAnswer(questionId: String): QuestionAnswer = QuestionAnswer(
        questionId = questionId,
        selectedOptionIds = selectedOptionIds,
        text = text.takeIf { it.isNotBlank() },
    )
}

enum class PendingAction { Load, Approve, Reject, Answer, CancelAnswer }

sealed interface PendingActionUiState {
    data object Idle : PendingActionUiState

    data class InFlight(val action: PendingAction) : PendingActionUiState

    data class Applied(
        val action: PendingAction,
        val itemId: String? = null,
    ) : PendingActionUiState

    data class GuardConflict(val currentGuard: Guard? = null) : PendingActionUiState

    data object Expired : PendingActionUiState

    data object ResolvedElsewhere : PendingActionUiState

    data object Unsupported : PendingActionUiState

    data class Unconfirmed(val failure: CoreFailure) : PendingActionUiState

    data class Failed(val failure: CoreFailure) : PendingActionUiState
}

/**
 * Fixed pending-input entry and review state.
 *
 * The entry updates in place; it never opens a dialog, moves reading position or
 * changes a draft. Native answer ownership is only touched by an explicit
 * [submitAnswers] or [cancelAnswer].
 */
data class PendingInputUiState(
    val session: SessionRef? = null,
    val entry: PendingEntryUiState = PendingEntryUiState.Loading,
    val focusedItemId: String? = null,
    val focusOrigin: PendingFocusOrigin = PendingFocusOrigin.None,
    val sheetRequested: Boolean = false,
    val detail: PendingDetailUiState = PendingDetailUiState.None,
    val reviewedGuard: Guard? = null,
    val answers: Map<String, QuestionAnswerDraft> = emptyMap(),
    val action: PendingActionUiState = PendingActionUiState.Idle,
    val answerInFlight: Boolean = false,
    val answerId: String? = null,
)

class PendingInputViewModel(
    private val port: PendingInputPort,
    private val clock: Clock,
    private val ids: IdGenerator,
    diagnostics: DiagnosticSink,
) : ScreenModel<PendingInputUiState>(PendingInputUiState()) {
    private val diag = ConversationDiagnostics(diagnostics)
    private var loadedSession: SessionRef? = null

    fun open(session: SessionRef) {
        if (loadedSession == session) {
            return
        }
        loadedSession = session
        setState(PendingInputUiState(session = session))
        refresh()
    }

    fun refresh() {
        val session = loadedSession ?: return
        edit { copy(entry = PendingEntryUiState.Loading) }
        scope.launch {
            val result = try {
                port.list(session)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                PendingListResult.Unavailable(unexpectedFailure())
            }
            if (loadedSession != session) {
                return@launch
            }
            applyCollection(result)
        }
    }

    /**
     * Applies a live collection update. It preserves focus, the open sheet and
     * answer drafts, and it never forces a dialog or reading movement.
     */
    fun observeLive(items: List<PendingItem>) {
        if (loadedSession == null) {
            return
        }
        applyCollection(PendingListResult.Current(items))
        val focusedId = state.value.focusedItemId ?: return
        val focused = items.firstOrNull { it.itemId == focusedId } ?: return
        edit {
            copy(
                detail = PendingDetailUiState.Loaded(focused),
                // The reviewed guard stays as the user reviewed it.
            )
        }
    }

    fun openItem(itemId: String, origin: PendingFocusOrigin = PendingFocusOrigin.Historical) {
        val session = loadedSession ?: return
        edit {
            copy(
                focusedItemId = itemId,
                focusOrigin = origin,
                sheetRequested = true,
                detail = PendingDetailUiState.Loading,
                action = PendingActionUiState.Idle,
                answers = emptyMap(),
                reviewedGuard = null,
            )
        }
        scope.launch {
            val result = try {
                port.get(session, itemId)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                PendingDetailResult.Unavailable(unexpectedFailure())
            }
            if (loadedSession != session || state.value.focusedItemId != itemId) {
                return@launch
            }
            when (result) {
                is PendingDetailResult.Found -> edit {
                    copy(
                        detail = PendingDetailUiState.Loaded(result.item),
                        reviewedGuard = result.item.guard,
                        answers = defaultAnswers(result.item),
                    )
                }

                PendingDetailResult.Missing -> edit {
                    copy(detail = PendingDetailUiState.Missing)
                }

                is PendingDetailResult.Changed -> edit {
                    copy(detail = PendingDetailUiState.Changed(result.failure))
                }

                is PendingDetailResult.Unavailable -> edit {
                    copy(detail = PendingDetailUiState.Unavailable(result.failure))
                }
            }
        }
    }

    fun dismissSheet() {
        edit { copy(sheetRequested = false) }
    }

    fun setAnswer(
        questionId: String,
        selectedOptionIds: List<String>,
        text: String,
    ) {
        // Editing never calls the port: native answer ownership stays untouched.
        edit {
            copy(
                answers = answers + (
                    questionId to QuestionAnswerDraft(
                        selectedOptionIds = selectedOptionIds,
                        text = text,
                    )
                    ),
            )
        }
    }

    fun approve() = decide(ApprovalDecision.Approve)

    fun reject() = decide(ApprovalDecision.Reject)

    private fun decide(decision: ApprovalDecision) {
        val item = currentItem() as? PendingItem.Approval ?: return
        val reviewed = state.value.reviewedGuard ?: return
        if (reviewed != item.guard) {
            edit { copy(action = PendingActionUiState.GuardConflict(item.guard)) }
            return
        }
        if (isExpired(item)) {
            edit { copy(action = PendingActionUiState.Expired) }
            return
        }
        val session = loadedSession ?: return
        val action = if (decision == ApprovalDecision.Approve) {
            PendingAction.Approve
        } else {
            PendingAction.Reject
        }
        edit { copy(action = PendingActionUiState.InFlight(action)) }
        scope.launch {
            val outcome = try {
                port.resolveApproval(session, item.itemId, decision, reviewed)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                PendingActionOutcome.Failed(unexpectedFailure())
            }
            if (loadedSession != session) {
                return@launch
            }
            applyAction(action, item.itemId, outcome)
        }
    }

    fun submitAnswers() {
        val item = currentItem() as? PendingItem.Questions ?: return
        val reviewed = state.value.reviewedGuard ?: return
        if (reviewed != item.guard) {
            edit { copy(action = PendingActionUiState.GuardConflict(item.guard)) }
            return
        }
        if (isExpired(item)) {
            edit { copy(action = PendingActionUiState.Expired) }
            return
        }
        val answers = item.batch.questions.mapNotNull { question ->
            state.value.answers[question.questionId]?.toAnswer(question.questionId)
        }
        if (answers.isEmpty()) {
            return
        }
        val session = loadedSession ?: return
        val answerRef = AnswerRef(ids.newId())
        edit {
            copy(
                action = PendingActionUiState.InFlight(PendingAction.Answer),
                answerInFlight = true,
                answerId = answerRef.answerId,
            )
        }
        scope.launch {
            val outcome = try {
                port.answerQuestion(session, item.itemId, answers, reviewed, answerRef)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                PendingActionOutcome.Failed(unexpectedFailure())
            }
            if (loadedSession != session) {
                return@launch
            }
            applyAction(PendingAction.Answer, item.itemId, outcome)
        }
    }

    /** Explicitly abandons an in-flight answer; it is never implicit. */
    fun cancelAnswer() {
        val answerId = state.value.answerId ?: return
        if (!state.value.answerInFlight) {
            return
        }
        val session = loadedSession ?: return
        val answerRef = AnswerRef(answerId)
        edit { copy(answerInFlight = false) }
        scope.launch {
            val outcome = try {
                port.cancelAnswer(session, answerRef)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                PendingActionOutcome.Failed(unexpectedFailure())
            }
            if (loadedSession != session) {
                return@launch
            }
            applyAction(PendingAction.CancelAnswer, null, outcome)
        }
    }

    fun clearAction() {
        edit { copy(action = PendingActionUiState.Idle) }
    }

    fun currentItem(): PendingItem? = (state.value.detail as? PendingDetailUiState.Loaded)?.item

    private fun applyCollection(result: PendingListResult) {
        val entry = when (result) {
            is PendingListResult.Current -> if (result.items.isEmpty()) {
                PendingEntryUiState.CurrentEmpty
            } else {
                PendingEntryUiState.Current(result.items)
            }

            PendingListResult.Unsupported -> PendingEntryUiState.Unsupported
            is PendingListResult.Stale -> PendingEntryUiState.Stale(result.failure)
            is PendingListResult.Unavailable -> PendingEntryUiState.Unavailable(result.failure)
        }
        edit {
            // A focused item that disappeared is a current-state outcome and
            // must not leave a stale detail, guard or answer draft behind.
            val focusLost = result is PendingListResult.Current &&
                focusedItemId != null &&
                result.items.none { it.itemId == focusedItemId }
            copy(
                entry = entry,
                focusedItemId = if (focusLost) null else focusedItemId,
                detail = if (focusLost) PendingDetailUiState.Missing else detail,
                reviewedGuard = if (focusLost) null else reviewedGuard,
                answers = if (focusLost) emptyMap() else answers,
                sheetRequested = if (focusLost) false else sheetRequested,
            )
        }
    }

    private fun applyAction(
        action: PendingAction,
        itemId: String?,
        outcome: PendingActionOutcome,
    ) {
        when (outcome) {
            is PendingActionOutcome.Applied -> {
                edit {
                    copy(
                        action = PendingActionUiState.Applied(action, itemId),
                        answerInFlight = false,
                        answerId = if (action == PendingAction.CancelAnswer) answerId else null,
                    )
                }
                if (itemId != null) {
                    removeItemLocally(itemId)
                }
                diag.info(
                    ConversationDiagnosticStage.PENDING_INPUT,
                    ConversationDiagnosticEventName.PENDING_INPUT_OPERATION,
                    actionId = itemId,
                    code = action.name.lowercase(),
                )
            }

            is PendingActionOutcome.GuardChanged -> {
                edit {
                    copy(
                        action = PendingActionUiState.GuardConflict(outcome.currentGuard),
                        answerInFlight = false,
                    )
                }
                diag.warn(
                    ConversationDiagnosticStage.PENDING_INPUT,
                    ConversationDiagnosticEventName.PENDING_INPUT_OPERATION,
                    code = "guard_changed",
                )
                refresh()
            }

            PendingActionOutcome.Expired -> {
                edit {
                    copy(action = PendingActionUiState.Expired, answerInFlight = false)
                }
            }

            PendingActionOutcome.ResolvedElsewhere -> {
                edit {
                    copy(
                        action = PendingActionUiState.ResolvedElsewhere,
                        answerInFlight = false,
                        detail = PendingDetailUiState.Missing,
                    )
                }
                if (itemId != null) {
                    removeItemLocally(itemId)
                }
            }

            PendingActionOutcome.Unsupported -> edit {
                copy(action = PendingActionUiState.Unsupported, answerInFlight = false)
            }

            is PendingActionOutcome.Unconfirmed -> edit {
                copy(
                    action = PendingActionUiState.Unconfirmed(outcome.failure),
                    answerInFlight = false,
                )
            }

            is PendingActionOutcome.Failed -> {
                diag.warn(
                    ConversationDiagnosticStage.PENDING_INPUT,
                    ConversationDiagnosticEventName.PENDING_INPUT_OPERATION,
                    code = outcome.failure.code,
                )
                edit {
                    copy(
                        action = PendingActionUiState.Failed(outcome.failure),
                        answerInFlight = false,
                    )
                }
            }
        }
    }

    private fun removeItemLocally(itemId: String) {
        edit {
            val entry = when (val current = entry) {
                is PendingEntryUiState.Current -> {
                    val remaining = current.items.filterNot { it.itemId == itemId }
                    if (remaining.isEmpty()) {
                        PendingEntryUiState.CurrentEmpty
                    } else {
                        PendingEntryUiState.Current(remaining)
                    }
                }

                else -> entry
            }
            copy(
                entry = entry,
                detail = if (focusedItemId == itemId) {
                    PendingDetailUiState.Missing
                } else {
                    detail
                },
                focusedItemId = if (focusedItemId == itemId) null else focusedItemId,
                sheetRequested = if (focusedItemId == itemId) false else sheetRequested,
                reviewedGuard = if (focusedItemId == itemId) null else reviewedGuard,
                answers = if (focusedItemId == itemId) emptyMap() else answers,
            )
        }
    }

    private fun defaultAnswers(item: PendingItem): Map<String, QuestionAnswerDraft> {
        if (item !is PendingItem.Questions) {
            return emptyMap()
        }
        return item.batch.questions.associate {
            it.questionId to QuestionAnswerDraft()
        }
    }

    private fun isExpired(item: PendingItem): Boolean =
        item.expiresAtMillis?.let { clock.epochMillis() > it } ?: false

    private fun unexpectedFailure(): CoreFailure =
        CoreFailure("unexpected", "Unexpected failure", retryable = true)

    private fun edit(transform: PendingInputUiState.() -> PendingInputUiState) {
        update { transform(it) }
    }


}
