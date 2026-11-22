package com.whalerein.android.ui.conversation

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.whalerein.android.R
import com.whalerein.android.state.conversation.ModelsUiState
import com.whalerein.android.state.conversation.PendingQueueItem
import com.whalerein.android.state.conversation.QueueUiState
import com.whalerein.android.state.conversation.SessionToolsUiState
import com.whalerein.android.state.conversation.ToolsOperationState

/** Stateless model/queue screen. Missing capability and unconfirmed stay distinct. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionToolsScreen(
    state: SessionToolsUiState,
    onBack: () -> Unit,
    onSelectModel: (String) -> Unit,
    onBeginEdit: (String) -> Unit,
    onEditDraftChange: (String) -> Unit,
    onSaveEdit: () -> Unit,
    onCancelEdit: () -> Unit,
    onRemoveItem: (String) -> Unit,
    onSteerItem: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.conversation_tools_title)) },
                navigationIcon = {
                    TextButton(onClick = onBack) {
                        Text(stringResource(R.string.conversation_back))
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
            Text(
                text = stringResource(R.string.conversation_models_title),
                style = MaterialTheme.typography.titleMedium,
            )
            when (val models = state.models) {
                ModelsUiState.Loading -> Text(stringResource(R.string.conversation_status_loading))
                ModelsUiState.Unsupported ->
                    Text(stringResource(R.string.conversation_tools_missing_capability))

                is ModelsUiState.Unavailable ->
                    Text(stringResource(R.string.conversation_tools_unavailable))

                is ModelsUiState.Ready -> models.models.forEach { model ->
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(
                            selected = model.modelId == models.currentModelId,
                            onClick = { onSelectModel(model.modelId) },
                        )
                        Text(
                            text = if (model.modelId == models.currentModelId) {
                                stringResource(
                                    R.string.conversation_model_current,
                                    model.displayName,
                                )
                            } else {
                                model.displayName
                            },
                            style = MaterialTheme.typography.bodyLarge,
                        )
                    }
                }
            }

            Text(
                text = stringResource(R.string.conversation_queue_title),
                style = MaterialTheme.typography.titleMedium,
            )
            when (val queue = state.queue) {
                QueueUiState.Loading -> Text(stringResource(R.string.conversation_status_loading))
                QueueUiState.Unsupported ->
                    Text(stringResource(R.string.conversation_tools_missing_capability))

                is QueueUiState.Unavailable ->
                    Text(stringResource(R.string.conversation_tools_unavailable))

                is QueueUiState.Ready -> if (queue.items.isEmpty()) {
                    Text(stringResource(R.string.conversation_queue_empty))
                } else {
                    queue.items.forEach { item ->
                        QueueItemCard(
                            item = item,
                            editing = state.editingItemId == item.itemId,
                            editDraft = state.editDraft,
                            onBeginEdit = { onBeginEdit(item.itemId) },
                            onEditDraftChange = onEditDraftChange,
                            onSaveEdit = onSaveEdit,
                            onCancelEdit = onCancelEdit,
                            onRemove = { onRemoveItem(item.itemId) },
                            onSteer = { onSteerItem(item.itemId) },
                            modifier = Modifier.fillMaxWidth(),
                        )
                    }
                }
            }

            OperationStatus(state.operation)
        }
    }
}

@Composable
private fun QueueItemCard(
    item: PendingQueueItem,
    editing: Boolean,
    editDraft: String,
    onBeginEdit: () -> Unit,
    onEditDraftChange: (String) -> Unit,
    onSaveEdit: () -> Unit,
    onCancelEdit: () -> Unit,
    onRemove: () -> Unit,
    onSteer: () -> Unit,
    modifier: Modifier = Modifier,
) {
    OutlinedCard(modifier = modifier) {
        Column(
            modifier = Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (editing) {
                OutlinedTextField(
                    value = editDraft,
                    onValueChange = onEditDraftChange,
                    modifier = Modifier.fillMaxWidth(),
                    label = { Text(stringResource(R.string.conversation_edit_hint)) },
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = onSaveEdit) {
                        Text(stringResource(R.string.conversation_queue_save))
                    }
                    TextButton(onClick = onCancelEdit) {
                        Text(stringResource(R.string.conversation_cancel))
                    }
                }
            } else {
                Text(text = item.text, style = MaterialTheme.typography.bodyMedium)
                Row(
                    modifier = Modifier.horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    TextButton(onClick = onBeginEdit) {
                        Text(stringResource(R.string.conversation_queue_edit))
                    }
                    TextButton(onClick = onRemove) {
                        Text(stringResource(R.string.conversation_queue_remove))
                    }
                    TextButton(onClick = onSteer, enabled = item.steerable) {
                        Text(stringResource(R.string.conversation_queue_steer))
                    }
                }
            }
        }
    }
}

@Composable
private fun OperationStatus(operation: ToolsOperationState) {
    val message = when (operation) {
        ToolsOperationState.Idle -> return
        is ToolsOperationState.InFlight -> R.string.conversation_tools_in_flight
        is ToolsOperationState.Applied -> R.string.conversation_tools_applied
        is ToolsOperationState.MissingCapability ->
            R.string.conversation_tools_missing_capability

        is ToolsOperationState.GuardConflict -> R.string.conversation_tools_guard_conflict
        is ToolsOperationState.Unconfirmed -> R.string.conversation_tools_unconfirmed
        is ToolsOperationState.Failed -> R.string.conversation_tools_failed
    }
    Text(
        text = stringResource(message),
        style = MaterialTheme.typography.labelMedium,
    )
}
