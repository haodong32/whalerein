package com.whalerein.android.ui.routes

import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.whalerein.android.AppDependencies
import com.whalerein.android.state.attachments.AttachmentSaveViewModel
import com.whalerein.android.state.attachments.AttachmentScope
import com.whalerein.android.state.core.SessionRef
import com.whalerein.android.state.support.AccountViewModel
import com.whalerein.android.state.support.DiagnosticsViewModel
import com.whalerein.android.state.support.OperationsViewModel
import com.whalerein.android.ui.support.AccountScreen
import com.whalerein.android.ui.support.AttachmentSaveSheet
import com.whalerein.android.ui.support.DiagnosticsScreen
import com.whalerein.android.ui.support.OperationsScreen
import kotlinx.coroutines.launch

@Composable
fun OperationsRoute(
    dependencies: AppDependencies,
    onBack: () -> Unit,
) {
    val viewModel: OperationsViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                OperationsViewModel(
                    instanceId = dependencies.instance,
                    port = dependencies.operations,
                    diagnostics = dependencies.diagnosticSink,
                    clock = dependencies.clock,
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    LaunchedEffect(Unit) { viewModel.refresh() }
    OperationsScreen(
        state = state,
        onRefresh = { scope.launch { viewModel.refresh() } },
        onRecover = { id -> scope.launch { viewModel.recover(id) } },
        onBack = onBack,
        modifier = Modifier.fillMaxSize(),
    )
}

@Composable
fun AccountRoute(
    dependencies: AppDependencies,
    onBack: () -> Unit,
) {
    val viewModel: AccountViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                AccountViewModel(
                    instanceId = dependencies.instance,
                    port = dependencies.account,
                    diagnostics = dependencies.diagnosticSink,
                    clock = dependencies.clock,
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    LaunchedEffect(Unit) { viewModel.load() }
    AccountScreen(
        state = state,
        cloudHostGrants = emptyList(),
        standaloneDevices = emptyList(),
        onSignOut = viewModel::requestSignOut,
        onRevokeSession = viewModel::requestRevokeAppSession,
        onRevokeAllSessions = viewModel::requestRevokeAllAppSessions,
        onRevokeCloudGrant = viewModel::requestRevokeCloudHostGrant,
        onRevokeStandaloneDevice = viewModel::requestRevokeStandaloneDevice,
        onRequestForget = viewModel::requestForget,
        onConfirm = { scope.launch { viewModel.confirm() } },
        onCancelConfirmation = viewModel::cancelConfirmation,
        onBack = onBack,
        modifier = Modifier.fillMaxSize(),
    )
}

@Composable
fun DiagnosticsRoute(
    dependencies: AppDependencies,
    onBack: () -> Unit,
) {
    val viewModel: DiagnosticsViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                DiagnosticsViewModel(
                    instanceId = dependencies.instance,
                    journal = dependencies.journal,
                    exporter = dependencies.journalExporter,
                    diagnostics = dependencies.diagnosticSink,
                    coreFailureSummary = dependencies.coreFailureSummary,
                    appBuild = dependencies.appVersion,
                    coreBuild = dependencies.coreVersion,
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    LaunchedEffect(Unit) { viewModel.refresh() }
    DiagnosticsScreen(
        state = state,
        onRefresh = { scope.launch { viewModel.refresh() } },
        onRequestClear = viewModel::requestClear,
        onConfirmClear = { scope.launch { viewModel.confirmClear() } },
        onCancelClear = viewModel::cancelClear,
        onRequestExport = { scope.launch { viewModel.requestExport() } },
        onConfirmExport = { scope.launch { viewModel.confirmExport() } },
        onCancelExport = viewModel::cancelExport,
        onBack = onBack,
        modifier = Modifier.fillMaxSize(),
    )
}

@Composable
fun AttachmentSaveRoute(
    dependencies: AppDependencies,
    session: SessionRef,
    attachmentId: String,
    onBack: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val operationId = remember(session, attachmentId) {
        "attachment:${session.host.instance.value}:${session.sessionId.value}:$attachmentId"
    }
    val viewModel: AttachmentSaveViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                AttachmentSaveViewModel(
                    attachmentScope = AttachmentScope(
                        instanceId = session.host.instance,
                        session = session,
                        attachmentId = attachmentId,
                    ),
                    operationId = operationId,
                    port = dependencies.attachments,
                    staging = dependencies.attachmentStaging,
                    authorization = dependencies.attachmentAuthorization,
                    writer = dependencies.destinationWriter,
                    clock = dependencies.clock,
                    diagnostics = dependencies.diagnosticSink,
                    processId = dependencies.ids.newId(),
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) {
        viewModel.prepareAfterRestart()
        viewModel.load()
    }
    AttachmentSaveSheet(
        state = state,
        onStartTransfer = { scope.launch { viewModel.startTransfer() } },
        onRetryTransfer = { scope.launch { viewModel.startTransfer() } },
        onBeginSave = { scope.launch { viewModel.beginSave() } },
        onCancelTransfer = viewModel::cancelTransfer,
        onCancelSave = viewModel::cancelSave,
        onDeadlineOverride = { viewModel.setDeadlineOverrideMillis(it) },
        onDismiss = onBack,
        modifier = Modifier.fillMaxSize(),
    )
}
