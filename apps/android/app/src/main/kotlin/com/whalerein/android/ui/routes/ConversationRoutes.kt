package com.whalerein.android.ui.routes

import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.whalerein.android.AppDependencies
import com.whalerein.android.state.conversation.ConversationViewModel
import com.whalerein.android.state.conversation.PendingEntryUiState
import com.whalerein.android.state.conversation.PendingInputViewModel
import com.whalerein.android.state.core.SessionRef
import com.whalerein.android.state.conversation.SessionToolsViewModel
import com.whalerein.android.ui.conversation.ConversationScreen
import com.whalerein.android.ui.conversation.PendingInputSheet
import com.whalerein.android.ui.conversation.SessionToolsScreen

@Composable
fun ConversationRoute(
    dependencies: AppDependencies,
    session: SessionRef,
    focusItemId: String?,
    openPendingSheet: Boolean,
    onBack: () -> Unit,
    onOpenTools: () -> Unit,
) {
    val conversation: ConversationViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                ConversationViewModel(
                    observation = dependencies.observation,
                    history = dependencies.history,
                    composer = dependencies.composer,
                    wait = dependencies.wait,
                    commands = dependencies.commands,
                    pendingInput = dependencies.pendingInput,
                    drafts = dependencies.drafts,
                    clock = dependencies.clock,
                    ids = dependencies.ids,
                    diagnostics = dependencies.diagnosticSink,
                    queue = dependencies.modelsQueue,
                )
            }
        },
    )
    val pending: PendingInputViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                PendingInputViewModel(
                    port = dependencies.pendingInput,
                    clock = dependencies.clock,
                    ids = dependencies.ids,
                    diagnostics = dependencies.diagnosticSink,
                )
            }
        },
    )
    val state by conversation.state.collectAsStateWithLifecycle()
    val pendingState by pending.state.collectAsStateWithLifecycle()
    val listState = rememberLazyListState()
    var sheetRequested by remember { mutableStateOf(false) }

    LaunchedEffect(session) {
        conversation.open(session)
        pending.open(session)
    }
    ObserveForeground(
        onVisible = conversation::onVisible,
        onHidden = conversation::onHidden,
    )
    LaunchedEffect(focusItemId) {
        if (focusItemId != null) pending.openItem(focusItemId)
    }
    LaunchedEffect(openPendingSheet) {
        if (openPendingSheet) sheetRequested = true
    }
    LaunchedEffect(sheetRequested, pendingState.entry, pendingState.focusedItemId) {
        if (!sheetRequested) return@LaunchedEffect
        if (pendingState.focusedItemId != null) return@LaunchedEffect
        val items = (pendingState.entry as? PendingEntryUiState.Current)?.items ?: return@LaunchedEffect
        val first = items.firstOrNull() ?: return@LaunchedEffect
        pending.openItem(first.itemId)
    }

    ConversationScreen(
        state = state,
        listState = listState,
        onBack = onBack,
        onOpenTools = onOpenTools,
        onOpenPendingInput = {
            sheetRequested = true
            pending.open(session)
        },
        onLoadOlder = conversation::loadOlder,
        onLoadCached = conversation::loadCached,
        onRetryHistory = conversation::loadOlder,
        onDraftChange = conversation::updateDraft,
        onSelectMode = conversation::selectDeliveryMode,
        onSelectFollowMode = conversation::selectFollowMode,
        onSubmit = conversation::submit,
        onStop = conversation::stop,
        onWait = conversation::waitForQuiescence,
        onCancelWait = conversation::cancelWait,
        onRecoverSend = conversation::recoverSend,
        onRetryContentRead = conversation::retryContentRead,
        onConsumeAnnouncement = conversation::consumeAnnouncement,
        modifier = Modifier.fillMaxSize(),
    )

    PendingInputSheet(
        state = pendingState,
        onDismiss = {
            sheetRequested = false
            pending.dismissSheet()
        },
        onApprove = pending::approve,
        onReject = pending::reject,
        onToggleOption = { questionId, optionId ->
            val current = pendingState.answers[questionId]
            val selected = current?.selectedOptionIds.orEmpty().toMutableSet()
            if (!selected.add(optionId)) selected.remove(optionId)
            pending.setAnswer(questionId, selected.toList(), current?.text.orEmpty())
        },
        onAnswerText = { questionId, text ->
            val current = pendingState.answers[questionId]
            pending.setAnswer(questionId, current?.selectedOptionIds.orEmpty(), text)
        },
        onSubmitAnswers = pending::submitAnswers,
        onCancelAnswer = pending::cancelAnswer,
    )
}

@Composable
fun SessionToolsRoute(
    dependencies: AppDependencies,
    session: SessionRef,
    onBack: () -> Unit,
) {
    val viewModel: SessionToolsViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                SessionToolsViewModel(
                    port = dependencies.modelsQueue,
                    ids = dependencies.ids,
                    diagnostics = dependencies.diagnosticSink,
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(session) { viewModel.open(session) }
    SessionToolsScreen(
        state = state,
        onBack = onBack,
        onSelectModel = viewModel::selectModel,
        onBeginEdit = viewModel::beginEdit,
        onEditDraftChange = viewModel::updateEditDraft,
        onSaveEdit = viewModel::saveEdit,
        onCancelEdit = viewModel::cancelEdit,
        onRemoveItem = viewModel::removeItem,
        onSteerItem = viewModel::steerItem,
        modifier = Modifier.fillMaxSize(),
    )
}

@Composable
private fun ObserveForeground(
    onVisible: () -> Unit,
    onHidden: () -> Unit,
) {
    val lifecycleOwner = LocalLifecycleOwner.current
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_START -> onVisible()
                Lifecycle.Event.ON_STOP -> onHidden()
                else -> Unit
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        if (lifecycleOwner.lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) {
            onVisible()
        }
        onDispose {
            lifecycleOwner.lifecycle.removeObserver(observer)
            // Leaving the composition (background or another destination) must
            // release the scoped watch; reopening is idempotent in the model.
            onHidden()
        }
    }
}
