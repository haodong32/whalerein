package com.whalerein.android.ui.routes

import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.whalerein.android.AppDependencies
import com.whalerein.android.state.conversation.CreateSessionUiState
import com.whalerein.android.state.conversation.ForkSessionUiState
import com.whalerein.android.state.conversation.SessionListViewModel
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.SessionRef
import com.whalerein.android.ui.conversation.SessionListScreen

/**
 * Host-qualified session list.
 *
 * The list, details, create and fork actions all run through the same typed
 * `SessionListPort` as the JVM tests. Selecting a session (or confirming a
 * created/forked child) routes through the shared `Route.Conversation` mapper;
 * an unauthorized Host cannot reach this screen because the route was produced
 * by `:state` authorization.
 */
@Composable
fun SessionsRoute(
    dependencies: AppDependencies,
    instanceId: InstanceId,
    hostId: HostId,
    onBack: () -> Unit,
    onOpenSession: (SessionRef) -> Unit,
) {
    val host = remember(instanceId, hostId) { HostRef(instanceId, hostId) }
    val viewModel: SessionListViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                SessionListViewModel(
                    port = dependencies.sessionList,
                    diagnostics = dependencies.diagnosticSink,
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(host) {
        viewModel.open(host)
    }
    LaunchedEffect(state.create) {
        val created = state.create as? CreateSessionUiState.Created ?: return@LaunchedEffect
        viewModel.clearOperation()
        onOpenSession(created.session)
    }
    LaunchedEffect(state.fork) {
        val forked = state.fork as? ForkSessionUiState.Forked ?: return@LaunchedEffect
        viewModel.clearOperation()
        onOpenSession(forked.child)
    }
    SessionListScreen(
        state = state,
        onBack = onBack,
        onRefresh = viewModel::refresh,
        onSelect = { sessionId ->
            viewModel.clearSelection()
            onOpenSession(SessionRef(host, sessionId))
        },
        onRefreshDetails = viewModel::refreshDetails,
        onShowCreate = viewModel::showCreateForm,
        onHideCreate = viewModel::hideCreateForm,
        onCreateInputChange = viewModel::updateCreateInput,
        onCreate = viewModel::create,
        onFork = viewModel::fork,
        onDismissDetails = viewModel::clearSelection,
        onClearOperation = viewModel::clearOperation,
        modifier = Modifier.fillMaxSize(),
    )
}
