package com.whalerein.android.ui.conversation

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import com.whalerein.android.R
import com.whalerein.android.state.conversation.CreateSessionInput
import com.whalerein.android.state.conversation.CreateSessionUiState
import com.whalerein.android.state.conversation.ForkSessionUiState
import com.whalerein.android.state.conversation.SessionDetails
import com.whalerein.android.state.conversation.SessionDetailsUiState
import com.whalerein.android.state.conversation.SessionListStatus
import com.whalerein.android.state.conversation.SessionListUiState
import com.whalerein.android.state.conversation.SessionSummary
import com.whalerein.android.state.core.SessionId

/** Stateless Host-qualified session list (AC-08). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionListScreen(
    state: SessionListUiState,
    onBack: () -> Unit,
    onRefresh: () -> Unit,
    onSelect: (SessionId) -> Unit,
    onRefreshDetails: () -> Unit,
    onShowCreate: () -> Unit,
    onHideCreate: () -> Unit,
    onCreateInputChange: (CreateSessionInput) -> Unit,
    onCreate: () -> Unit,
    onFork: (String?) -> Unit,
    onDismissDetails: () -> Unit,
    onClearOperation: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var forkDialogVisible by rememberSaveable { mutableStateOf(false) }
    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.conversation_sessions_title)) },
                navigationIcon = {
                    TextButton(onClick = onBack) {
                        Text(stringResource(R.string.conversation_back))
                    }
                },
                actions = {
                    TextButton(onClick = onRefresh) {
                        Text(stringResource(R.string.conversation_sessions_refresh))
                    }
                    TextButton(onClick = onShowCreate) {
                        Text(stringResource(R.string.conversation_sessions_create))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            state.host?.let { host ->
                Text(
                    text = stringResource(
                        R.string.conversation_sessions_host,
                        host.hostId.value,
                    ),
                    style = MaterialTheme.typography.labelMedium,
                    fontFamily = FontFamily.Monospace,
                )
            }
            if (state.cached) {
                Text(
                    text = stringResource(R.string.conversation_sessions_cached),
                    style = MaterialTheme.typography.labelMedium,
                )
            }
            when (state.status) {
                SessionListStatus.Loading -> Text(stringResource(R.string.conversation_sessions_loading))
                SessionListStatus.Failed -> Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        text = stringResource(R.string.conversation_sessions_unavailable),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    TextButton(onClick = onRefresh) {
                        Text(stringResource(R.string.conversation_action_retry))
                    }
                }

                SessionListStatus.Idle,
                SessionListStatus.Ready,
                SessionListStatus.Cached,
                -> if (state.sessions.isEmpty()) {
                    Text(stringResource(R.string.conversation_sessions_empty))
                } else {
                    state.sessions.forEach { summary ->
                        SessionRow(
                            summary = summary,
                            selected = summary.sessionId == state.selectedSessionId,
                            onClick = { onSelect(summary.sessionId) },
                        )
                    }
                }
            }
            if (state.createFormVisible) {
                CreateSessionDialog(
                    input = state.createInput,
                    state = state.create,
                    onChange = onCreateInputChange,
                    onCreate = onCreate,
                    onDismiss = onHideCreate,
                )
            }
            SessionDetailSheet(
                visible = state.selectedSessionId != null,
                details = state.details,
                fork = state.fork,
                onDismiss = onDismissDetails,
                onRefresh = onRefreshDetails,
                onFork = { forkDialogVisible = true },
            )
            if (forkDialogVisible) {
                ForkSessionDialog(
                    details = (state.details as? SessionDetailsUiState.Loaded)?.details,
                    onConfirm = {
                        forkDialogVisible = false
                        onFork(null)
                    },
                    onDismiss = { forkDialogVisible = false },
                )
            }
            ForkStatus(fork = state.fork)
        }
    }
}

@Composable
private fun SessionRow(
    summary: SessionSummary,
    selected: Boolean,
    onClick: () -> Unit,
) {
    Card(
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick),
    ) {
        Column(modifier = Modifier.padding(12.dp)) {
            Text(
                text = summary.title ?: summary.sessionId.value,
                style = MaterialTheme.typography.titleSmall,
            )
            Text(
                text = summary.sessionId.value,
                style = MaterialTheme.typography.labelSmall,
                fontFamily = FontFamily.Monospace,
            )
            if (selected) {
                Text(
                    text = stringResource(R.string.conversation_sessions_selected),
                    style = MaterialTheme.typography.labelSmall,
                )
            }
            if (summary.hasPendingInput) {
                Text(
                    text = stringResource(R.string.conversation_sessions_pending),
                    style = MaterialTheme.typography.labelSmall,
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionDetailSheet(
    visible: Boolean,
    details: SessionDetailsUiState,
    fork: ForkSessionUiState,
    onDismiss: () -> Unit,
    onRefresh: () -> Unit,
    onFork: () -> Unit,
) {
    if (!visible) {
        return
    }
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .imePadding()
                .verticalScroll(rememberScrollState())
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                text = stringResource(R.string.conversation_session_details_title),
                style = MaterialTheme.typography.titleLarge,
            )
            when (val current = details) {
                SessionDetailsUiState.None -> Text(stringResource(R.string.conversation_sessions_empty))
                SessionDetailsUiState.Loading ->
                    Text(stringResource(R.string.conversation_sessions_loading))

                is SessionDetailsUiState.Unavailable ->
                    Text(stringResource(R.string.conversation_session_details_unavailable))

                is SessionDetailsUiState.Loaded -> LoadedDetails(current.details)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = onRefresh) {
                    Text(stringResource(R.string.conversation_sessions_refresh))
                }
                Button(
                    onClick = onFork,
                    enabled = details is SessionDetailsUiState.Loaded &&
                        (details as SessionDetailsUiState.Loaded)
                            .details.latestCompletedTurnId != null &&
                        fork !is ForkSessionUiState.InFlight,
                ) {
                    Text(stringResource(R.string.conversation_session_fork))
                }
            }
        }
    }
}

@Composable
private fun LoadedDetails(details: SessionDetails) {
    Text(
        text = details.title ?: details.session.sessionId.value,
        style = MaterialTheme.typography.titleMedium,
    )
    DetailRow(
        label = stringResource(R.string.conversation_session_working_directory),
        value = details.workingDirectory,
    )
    DetailRow(
        label = stringResource(R.string.conversation_session_workspace),
        value = details.workspace,
    )
    DetailRow(
        label = stringResource(R.string.conversation_session_preset),
        value = details.preset,
    )
    DetailRow(
        label = stringResource(R.string.conversation_session_latest_turn),
        value = details.latestCompletedTurnId
            ?: stringResource(R.string.conversation_session_none),
    )
    if (details.cached) {
        Text(
            text = stringResource(R.string.conversation_session_details_cached),
            style = MaterialTheme.typography.labelMedium,
        )
    }
}

@Composable
private fun DetailRow(label: String, value: String?) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelMedium,
            modifier = Modifier.weight(1f),
        )
        Text(
            text = value ?: stringResource(R.string.conversation_session_none),
            style = MaterialTheme.typography.bodyMedium,
        )
    }
}

@Composable
fun CreateSessionDialog(
    input: CreateSessionInput,
    state: CreateSessionUiState,
    onChange: (CreateSessionInput) -> Unit,
    onCreate: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.conversation_create_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    text = stringResource(R.string.conversation_create_hint),
                    style = MaterialTheme.typography.bodySmall,
                )
                InputField(
                    value = input.workingDirectory.orEmpty(),
                    label = stringResource(R.string.conversation_create_working_directory),
                    onValueChange = { onChange(input.copy(workingDirectory = it.ifBlank { null })) },
                )
                InputField(
                    value = input.workspace.orEmpty(),
                    label = stringResource(R.string.conversation_create_workspace),
                    onValueChange = { onChange(input.copy(workspace = it.ifBlank { null })) },
                )
                InputField(
                    value = input.preset.orEmpty(),
                    label = stringResource(R.string.conversation_create_preset),
                    onValueChange = { onChange(input.copy(preset = it.ifBlank { null })) },
                )
                InputField(
                    value = input.preferredSessionId?.value.orEmpty(),
                    label = stringResource(R.string.conversation_create_preferred_id),
                    onValueChange = {
                        onChange(input.copy(preferredSessionId = it.ifBlank { null }?.let(::SessionId)))
                    },
                )
                CreateStatus(state)
            }
        },
        confirmButton = {
            TextButton(
                onClick = onCreate,
                enabled = state !is CreateSessionUiState.InFlight,
            ) {
                Text(stringResource(R.string.conversation_create_action))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(stringResource(R.string.conversation_cancel))
            }
        },
    )
}

@Composable
private fun InputField(value: String, label: String, onValueChange: (String) -> Unit) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
private fun CreateStatus(state: CreateSessionUiState) {
    val message = when (state) {
        is CreateSessionUiState.Unsupported -> R.string.conversation_create_unsupported
        is CreateSessionUiState.Failed -> R.string.conversation_create_failed
        is CreateSessionUiState.Unconfirmed -> R.string.conversation_create_unconfirmed
        is CreateSessionUiState.Created -> R.string.conversation_create_succeeded
        else -> null
    }
    if (message != null) {
        Text(
            text = stringResource(message),
            style = MaterialTheme.typography.labelMedium,
        )
    }
}

@Composable
fun ForkSessionDialog(
    details: SessionDetails?,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    val turn = details?.latestCompletedTurnId
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.conversation_session_fork_confirm_title)) },
        text = {
            Text(
                if (turn != null) {
                    stringResource(R.string.conversation_session_fork_confirm_message, turn)
                } else {
                    stringResource(R.string.conversation_session_no_completed_turn)
                },
            )
        },
        confirmButton = {
            TextButton(onClick = onConfirm, enabled = turn != null) {
                Text(stringResource(R.string.conversation_session_fork))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(stringResource(R.string.conversation_cancel))
            }
        },
    )
}

@Composable
private fun ForkStatus(fork: ForkSessionUiState) {
    val message = when (fork) {
        is ForkSessionUiState.Forked -> if (fork.workingDirectoryAvailable) {
            R.string.conversation_fork_succeeded
        } else {
            R.string.conversation_fork_no_working_directory
        }

        is ForkSessionUiState.Failed -> R.string.conversation_fork_failed
        is ForkSessionUiState.Unconfirmed -> R.string.conversation_fork_unconfirmed
        ForkSessionUiState.Unsupported -> R.string.conversation_fork_unsupported
        else -> null
    }
    if (message != null) {
        Text(
            text = stringResource(message),
            style = MaterialTheme.typography.labelMedium,
        )
    }
}
