package com.whalerein.android.ui.conversation

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.whalerein.android.R
import com.whalerein.android.state.conversation.PendingActionUiState
import com.whalerein.android.state.conversation.PendingDetailUiState
import com.whalerein.android.state.conversation.PendingInputUiState
import com.whalerein.android.state.conversation.PendingItem
import com.whalerein.android.state.conversation.Question
import com.whalerein.android.state.conversation.QuestionAnswerDraft
import com.whalerein.android.state.conversation.QuestionKind

/**
 * Explicit review sheet for the currently focused pending-input item.
 *
 * The sheet opens only from an explicit selection: a live collection update can
 * refresh the fixed entry but never forces this dialog or moves reading position.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PendingInputSheet(
    state: PendingInputUiState,
    onDismiss: () -> Unit,
    onApprove: () -> Unit,
    onReject: () -> Unit,
    onToggleOption: (questionId: String, optionId: String) -> Unit,
    onAnswerText: (questionId: String, text: String) -> Unit,
    onSubmitAnswers: () -> Unit,
    onCancelAnswer: () -> Unit,
    modifier: Modifier = Modifier,
) {
    if (!state.sheetRequested) {
        return
    }
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
        modifier = modifier,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .imePadding()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                text = stringResource(R.string.conversation_pending_title),
                style = MaterialTheme.typography.titleLarge,
            )
            when (val detail = state.detail) {
                PendingDetailUiState.None,
                PendingDetailUiState.Loading,
                -> Text(stringResource(R.string.conversation_pending_loading))

                PendingDetailUiState.Missing ->
                    Text(stringResource(R.string.conversation_pending_missing))

                is PendingDetailUiState.Changed ->
                    Text(stringResource(R.string.conversation_pending_changed))

                is PendingDetailUiState.Unavailable ->
                    Text(stringResource(R.string.conversation_pending_unavailable))

                is PendingDetailUiState.Loaded -> when (val item = detail.item) {
                    is PendingItem.Approval -> ApprovalDetail(
                        title = item.approval.title,
                        body = item.approval.body,
                        reviewed = state.reviewedGuard != null,
                        onApprove = onApprove,
                        onReject = onReject,
                    )

                    is PendingItem.Questions -> QuestionsDetail(
                        questions = item.batch.questions,
                        answers = state.answers,
                        answerInFlight = state.answerInFlight,
                        onSubmit = onSubmitAnswers,
                        onCancelAnswer = onCancelAnswer,
                        onToggleOption = onToggleOption,
                        onAnswerText = onAnswerText,
                    )
                }
            }
            ActionStatus(state.action)
        }
    }
}

@Composable
private fun ApprovalDetail(
    title: String,
    body: String?,
    reviewed: Boolean,
    onApprove: () -> Unit,
    onReject: () -> Unit,
) {
    Text(text = title, style = MaterialTheme.typography.titleMedium)
    if (!body.isNullOrBlank()) {
        Text(text = body, style = MaterialTheme.typography.bodyMedium)
    }
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Button(onClick = onApprove, enabled = reviewed) {
            Text(stringResource(R.string.conversation_approve))
        }
        OutlinedButton(onClick = onReject, enabled = reviewed) {
            Text(stringResource(R.string.conversation_reject))
        }
    }
}

@Composable
private fun QuestionsDetail(
    questions: List<Question>,
    answers: Map<String, QuestionAnswerDraft>,
    answerInFlight: Boolean,
    onSubmit: () -> Unit,
    onCancelAnswer: () -> Unit,
    onToggleOption: (String, String) -> Unit,
    onAnswerText: (String, String) -> Unit,
) {
    questions.forEach { question ->
        val answer = answers[question.questionId] ?: QuestionAnswerDraft()
        Text(
            text = question.prompt,
            style = MaterialTheme.typography.titleSmall,
        )
        if (question.kind == QuestionKind.Continued) {
            Text(
                text = stringResource(R.string.conversation_question_continued),
                style = MaterialTheme.typography.labelSmall,
            )
        }
        if (question.options.isNotEmpty()) {
            Row(
                modifier = Modifier.horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                question.options.forEach { option ->
                    FilterChip(
                        selected = option.optionId in answer.selectedOptionIds,
                        onClick = { onToggleOption(question.questionId, option.optionId) },
                        label = { Text(option.label) },
                    )
                }
            }
        }
        if (question.allowOpenAnswer || question.kind == QuestionKind.Open ||
            question.kind == QuestionKind.Continued
        ) {
            OutlinedTextField(
                value = answer.text,
                onValueChange = { onAnswerText(question.questionId, it) },
                modifier = Modifier.fillMaxWidth(),
                label = { Text(stringResource(R.string.conversation_answer_text)) },
                minLines = 1,
                maxLines = 4,
            )
        }
        if (question.required) {
            Text(
                text = stringResource(R.string.conversation_question_required),
                style = MaterialTheme.typography.labelSmall,
            )
        }
    }
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Button(onClick = onSubmit, enabled = !answerInFlight) {
            Text(stringResource(R.string.conversation_submit_answers))
        }
        if (answerInFlight) {
            OutlinedButton(onClick = onCancelAnswer) {
                Text(stringResource(R.string.conversation_cancel_answer))
            }
        }
    }
}

@Composable
private fun ActionStatus(action: PendingActionUiState) {
    val message = when (action) {
        PendingActionUiState.Idle -> return
        is PendingActionUiState.InFlight -> R.string.conversation_pending_action_in_flight
        is PendingActionUiState.Applied -> R.string.conversation_pending_action_applied
        is PendingActionUiState.GuardConflict -> R.string.conversation_pending_action_guard_changed
        PendingActionUiState.Expired -> R.string.conversation_pending_action_expired
        PendingActionUiState.ResolvedElsewhere -> R.string.conversation_pending_action_resolved
        PendingActionUiState.Unsupported -> R.string.conversation_pending_action_unsupported
        is PendingActionUiState.Unconfirmed -> R.string.conversation_pending_action_unconfirmed
        is PendingActionUiState.Failed -> R.string.conversation_pending_action_failed
    }
    Text(
        text = stringResource(message),
        style = MaterialTheme.typography.labelMedium,
    )
}
