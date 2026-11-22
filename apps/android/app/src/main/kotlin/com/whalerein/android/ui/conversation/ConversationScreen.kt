package com.whalerein.android.ui.conversation

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.whalerein.android.R
import com.whalerein.android.state.conversation.ConversationRenderModel
import com.whalerein.android.state.conversation.ConversationStatus
import com.whalerein.android.state.conversation.ConversationUiState
import com.whalerein.android.state.conversation.DeliveryMode
import com.whalerein.android.state.conversation.FollowMode
import com.whalerein.android.state.conversation.HistoryPagerStatus
import com.whalerein.android.state.conversation.RenderOptions

/**
 * Stateless conversation screen. It never constructs a ViewModel; the
 * composition root owns the model and passes immutable state plus callbacks.
 *
 * Local state is limited to presentation choices (reasoning visibility and
 * expanded tool cards) and existing LazyList scroll position.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConversationScreen(
    state: ConversationUiState,
    listState: LazyListState,
    onBack: () -> Unit,
    onOpenTools: () -> Unit,
    onOpenPendingInput: () -> Unit,
    onLoadOlder: () -> Unit,
    onLoadCached: () -> Unit,
    onRetryHistory: () -> Unit,
    onDraftChange: (String) -> Unit,
    onSelectMode: (DeliveryMode) -> Unit,
    onSelectFollowMode: (FollowMode) -> Unit,
    onSubmit: () -> Unit,
    onStop: () -> Unit,
    onWait: () -> Unit,
    onCancelWait: () -> Unit,
    onRecoverSend: () -> Unit,
    onRetryContentRead: () -> Unit,
    onConsumeAnnouncement: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var reasoningEnabled by rememberSaveable { mutableStateOf(false) }
    var expandedToolCalls by remember { mutableStateOf(emptySet<String>()) }
    val rendered = ConversationRenderModel.renderItems(
        items = state.items,
        options = RenderOptions(
            reasoningEnabled = reasoningEnabled,
            expandedToolCalls = expandedToolCalls,
        ),
    )

    AnnouncementPolicy(state = state, onConsumed = onConsumeAnnouncement)

    // Paged insertion reports a stable anchor; restore it after the list changes.
    LaunchedEffect(state.history.lastInsertion) {
        val anchor = state.history.lastInsertion?.anchor ?: return@LaunchedEffect
        val index = state.items.indexOfFirst { it.key == anchor.itemKey }
        if (index >= 0) {
            listState.scrollToItem(index, anchor.offsetFromItemTopPx)
        }
    }

    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.conversation_title)) },
                navigationIcon = {
                    TextButton(onClick = onBack) {
                        Text(stringResource(R.string.conversation_back))
                    }
                },
                actions = {
                    TextButton(onClick = onOpenTools) {
                        Text(stringResource(R.string.conversation_tools))
                    }
                },
            )
        },
        bottomBar = {
            ComposerBar(
                state = state,
                onDraftChange = onDraftChange,
                onSelectMode = onSelectMode,
                onSelectFollowMode = onSelectFollowMode,
                onSubmit = onSubmit,
                onStop = onStop,
                onWait = onWait,
                onCancelWait = onCancelWait,
                onRecoverSend = onRecoverSend,
                onRetryContentRead = onRetryContentRead,
                onOpenPendingInput = onOpenPendingInput,
            )
        },
    ) { padding ->
        Column(modifier = Modifier.padding(padding).fillMaxSize()) {
            if (state.status == ConversationStatus.Resyncing) {
                LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
                Text(
                    text = stringResource(R.string.conversation_status_resyncing),
                    style = MaterialTheme.typography.labelSmall,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }
            if (rendered.reasoningBlockCount > 0) {
                TextButton(onClick = { reasoningEnabled = !reasoningEnabled }) {
                    Text(
                        stringResource(
                            if (reasoningEnabled) {
                                R.string.conversation_hide_reasoning
                            } else {
                                R.string.conversation_show_reasoning
                            },
                        ),
                    )
                }
            }
            Box(modifier = Modifier.weight(1f)) {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize(),
                ) {
                    item(key = "history-header") {
                        HistoryHeader(
                            status = state.history.status,
                            cached = state.history.cached,
                            hasOlder = state.history.hasOlder,
                            onLoadOlder = onLoadOlder,
                            onLoadCached = onLoadCached,
                            onRetry = onRetryHistory,
                        )
                    }
                    items(
                        items = rendered.items,
                        key = { item -> item.key.toString() },
                    ) { item ->
                        MessageContent(
                            item = item,
                            expandedToolCalls = expandedToolCalls,
                            onToggleTool = { callId ->
                                expandedToolCalls = if (callId in expandedToolCalls) {
                                    expandedToolCalls - callId
                                } else {
                                    expandedToolCalls + callId
                                }
                            },
                            modifier = Modifier.fillMaxWidth(),
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun HistoryHeader(
    status: HistoryPagerStatus,
    cached: Boolean,
    hasOlder: Boolean,
    onLoadOlder: () -> Unit,
    onLoadCached: () -> Unit,
    onRetry: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        if (cached) {
            Text(
                text = stringResource(R.string.conversation_cached_badge),
                style = MaterialTheme.typography.labelMedium,
            )
        }
        when (status) {
            HistoryPagerStatus.LoadingLatest,
            HistoryPagerStatus.LoadingOlder,
            -> Text(
                text = stringResource(R.string.conversation_status_loading),
                style = MaterialTheme.typography.labelMedium,
            )

            is HistoryPagerStatus.Invalidated -> Button(onClick = onRetry) {
                Text(stringResource(R.string.conversation_history_reload))
            }

            is HistoryPagerStatus.Failed -> Button(onClick = onRetry) {
                Text(stringResource(R.string.conversation_action_retry))
            }

            is HistoryPagerStatus.CachedUnavailable -> Button(onClick = onLoadCached) {
                Text(stringResource(R.string.conversation_history_cached_unavailable))
            }

            else -> Unit
        }
        if (hasOlder) {
            TextButton(onClick = onLoadOlder) {
                Text(stringResource(R.string.conversation_history_load_older))
            }
        }
        if (!cached && status is HistoryPagerStatus.Cached) {
            TextButton(onClick = onLoadCached) {
                Text(stringResource(R.string.conversation_history_load_cached))
            }
        }
    }
}
