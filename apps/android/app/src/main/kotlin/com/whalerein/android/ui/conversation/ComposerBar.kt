package com.whalerein.android.ui.conversation

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.whalerein.android.R
import com.whalerein.android.state.conversation.ComposerAvailability
import com.whalerein.android.state.conversation.ConversationUiState
import com.whalerein.android.state.conversation.DeliveryMode
import com.whalerein.android.state.conversation.FollowMode
import com.whalerein.android.state.conversation.PendingEntryUiState
import com.whalerein.android.state.conversation.SendUiState
import com.whalerein.android.state.conversation.StopUiState
import com.whalerein.android.state.conversation.WaitUiState

/**
 * Explicit composer actions. The primary button label is derived from the
 * selected delivery mode so a queue action can never masquerade as steering or
 * start-if-idle.
 */
@Composable
fun ComposerBar(
    state: ConversationUiState,
    onDraftChange: (String) -> Unit,
    onSelectMode: (DeliveryMode) -> Unit,
    onSelectFollowMode: (FollowMode) -> Unit,
    onSubmit: () -> Unit,
    onStop: () -> Unit,
    onWait: () -> Unit,
    onCancelWait: () -> Unit,
    onRecoverSend: () -> Unit,
    onRetryContentRead: () -> Unit,
    onOpenPendingInput: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier.fillMaxWidth().imePadding(),
        tonalElevation = 3.dp,
    ) {
        Column(
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            PendingInputEntry(state.pendingEntry, onOpenPendingInput)
            SendStateRow(state, onRecoverSend, onRetryContentRead)
            WaitStateRow(state.wait, onCancelWait)
            StopStateRow(state.stop)

            OutlinedTextField(
                value = state.composer.text,
                onValueChange = onDraftChange,
                modifier = Modifier.fillMaxWidth(),
                enabled = state.composer.availability != ComposerAvailability.Unsupported,
                placeholder = { Text(stringResource(R.string.conversation_composer_hint)) },
                minLines = 1,
                maxLines = 4,
            )
            if (state.composer.availability != ComposerAvailability.Available) {
                Text(
                    text = stringResource(availabilityLabel(state.composer.availability)),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.error,
                )
            }

            Row(
                modifier = Modifier.horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                DeliveryMode.entries.forEach { mode ->
                    FilterChip(
                        selected = state.composer.mode == mode,
                        onClick = { onSelectMode(mode) },
                        label = { Text(stringResource(modeLabel(mode))) },
                    )
                }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                FollowMode.entries.forEach { follow ->
                    FilterChip(
                        selected = state.composer.follow == follow,
                        onClick = { onSelectFollowMode(follow) },
                        label = { Text(stringResource(followLabel(follow))) },
                    )
                }
            }
            Row(
                modifier = Modifier.horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Button(
                    onClick = onSubmit,
                    enabled = state.composer.text.isNotBlank() &&
                        !state.composer.submitting &&
                        state.composer.availability != ComposerAvailability.Unsupported,
                ) {
                    Text(stringResource(primaryActionLabel(state.composer.mode)))
                }
                OutlinedButton(
                    onClick = onWait,
                    enabled = state.wait !is WaitUiState.Waiting,
                ) {
                    Text(stringResource(R.string.conversation_action_wait))
                }
                OutlinedButton(
                    onClick = onStop,
                    enabled = state.phase.isActive && state.stop !is StopUiState.InFlight,
                ) {
                    Text(stringResource(R.string.conversation_action_stop))
                }
            }
        }
    }
}

@Composable
private fun SendStateRow(
    state: ConversationUiState,
    onRecover: () -> Unit,
    onRetryContentRead: () -> Unit,
) {
    when (val send = state.send) {
        is SendUiState.ReceiptOnly -> StatusText(R.string.conversation_send_receipt_only)
        is SendUiState.FollowPending -> StatusText(R.string.conversation_send_follow_pending)
        is SendUiState.FollowedReply -> StatusText(R.string.conversation_send_followed_reply)
        is SendUiState.FollowPendingInput -> StatusText(R.string.conversation_send_follow_pending_input)
        is SendUiState.FollowTimedOut -> Row {
            StatusText(R.string.conversation_send_follow_timeout)
            TextButton(onClick = onRecover) {
                Text(stringResource(R.string.conversation_action_recover))
            }
        }

        is SendUiState.FollowFailed -> StatusText(R.string.conversation_send_follow_failed)
        is SendUiState.Unconfirmed -> Row {
            StatusText(R.string.conversation_send_unconfirmed)
            TextButton(onClick = onRecover) {
                Text(stringResource(R.string.conversation_action_recover))
            }
        }

        is SendUiState.DefiniteFailure -> StatusText(R.string.conversation_send_definite_failure)
        is SendUiState.ConfirmedUnread -> Row {
            StatusText(R.string.conversation_send_confirmed_unread)
            if (send.canRetry) {
                TextButton(onClick = onRetryContentRead) {
                    Text(stringResource(R.string.conversation_action_retry_content))
                }
            }
        }

        is SendUiState.Recovering -> StatusText(R.string.conversation_send_recovering)
        is SendUiState.Recovered -> StatusText(R.string.conversation_send_recovered)
        is SendUiState.Cancelled -> StatusText(R.string.conversation_send_cancelled)
        is SendUiState.Submitting -> StatusText(R.string.conversation_send_submitting)
        SendUiState.Idle -> Unit
    }
}

@Composable
private fun WaitStateRow(wait: WaitUiState, onCancelWait: () -> Unit) {
    when (wait) {
        is WaitUiState.Waiting -> Row {
            StatusText(R.string.conversation_wait_waiting)
            TextButton(onClick = onCancelWait) {
                Text(stringResource(R.string.conversation_action_cancel_wait))
            }
        }

        WaitUiState.Quiescent -> StatusText(R.string.conversation_wait_quiescent)
        is WaitUiState.ApprovalInput -> StatusText(R.string.conversation_wait_approval_input)
        is WaitUiState.QuestionInput -> StatusText(R.string.conversation_wait_question_input)
        WaitUiState.TimedOut -> StatusText(R.string.conversation_wait_timed_out)
        WaitUiState.Cancelled -> StatusText(R.string.conversation_wait_cancelled)
        is WaitUiState.Failed -> StatusText(R.string.conversation_wait_failed)
        WaitUiState.Idle -> Unit
    }
}

@Composable
private fun StopStateRow(stop: StopUiState) {
    when (stop) {
        StopUiState.Accepted -> StatusText(R.string.conversation_stop_accepted)
        StopUiState.Confirmed -> StatusText(R.string.conversation_stop_confirmed)
        is StopUiState.Failed -> StatusText(R.string.conversation_stop_failed)
        is StopUiState.Unconfirmed -> StatusText(R.string.conversation_stop_unconfirmed)
        StopUiState.InFlight -> StatusText(R.string.conversation_stop_pending)
        StopUiState.Idle -> Unit
    }
}

@Composable
fun PendingInputEntry(
    entry: PendingEntryUiState,
    onOpen: () -> Unit,
    modifier: Modifier = Modifier,
) {
    when (entry) {
        PendingEntryUiState.Loading -> StatusText(R.string.conversation_pending_loading, modifier)
        PendingEntryUiState.CurrentEmpty -> Unit
        is PendingEntryUiState.Current -> AssistChip(
            onClick = onOpen,
            label = {
                Text(
                    stringResource(
                        R.string.conversation_pending_entry,
                        entry.items.size,
                    ),
                )
            },
            modifier = modifier,
        )

        is PendingEntryUiState.Stale -> StatusText(R.string.conversation_pending_stale, modifier)
        is PendingEntryUiState.Unavailable ->
            StatusText(R.string.conversation_pending_unavailable, modifier)

        PendingEntryUiState.Unsupported -> StatusText(
            R.string.conversation_pending_unsupported,
            modifier,
        )
    }
}

@Composable
private fun StatusText(@androidx.annotation.StringRes id: Int, modifier: Modifier = Modifier) {
    Text(
        text = stringResource(id),
        style = MaterialTheme.typography.labelMedium,
        modifier = modifier,
    )
}

private fun availabilityLabel(availability: ComposerAvailability): Int = when (availability) {
    ComposerAvailability.Available -> R.string.conversation_composer_available
    ComposerAvailability.Offline -> R.string.conversation_composer_offline
    ComposerAvailability.Unavailable -> R.string.conversation_composer_unavailable
    ComposerAvailability.Unsupported -> R.string.conversation_composer_unsupported
}

private fun modeLabel(mode: DeliveryMode): Int = when (mode) {
    DeliveryMode.Queue -> R.string.conversation_mode_queue
    DeliveryMode.StartIfIdle -> R.string.conversation_mode_start_if_idle
    DeliveryMode.Steer -> R.string.conversation_mode_steer
    DeliveryMode.Send -> R.string.conversation_mode_send
}

private fun primaryActionLabel(mode: DeliveryMode): Int = when (mode) {
    DeliveryMode.Queue -> R.string.conversation_action_queue
    DeliveryMode.StartIfIdle -> R.string.conversation_action_start_if_idle
    DeliveryMode.Steer -> R.string.conversation_action_steer
    DeliveryMode.Send -> R.string.conversation_action_send
}

private fun followLabel(follow: FollowMode): Int = when (follow) {
    FollowMode.ReceiptOnly -> R.string.conversation_follow_receipt_only
    FollowMode.FollowReply -> R.string.conversation_follow_reply
}
