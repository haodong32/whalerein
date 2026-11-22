package com.whalerein.android.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.whalerein.android.AppContainer
import com.whalerein.android.AppGraphState
import com.whalerein.android.BuildConfig
import com.whalerein.android.R
import com.whalerein.android.navigation.AppNavHost
import com.whalerein.android.platform.ActivityPlatform
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.support.DiagnosticsViewModel
import com.whalerein.android.state.support.Journal
import com.whalerein.android.state.support.JournalExporter
import com.whalerein.android.ui.support.DiagnosticsScreen
import kotlinx.coroutines.launch

/** Root switch between graph opening, startup failure and the typed nav host. */
@Composable
fun AppRoot(
    container: AppContainer,
    platform: ActivityPlatform?,
) {
    val state by container.state.collectAsStateWithLifecycle()
    when (val current = state) {
        AppGraphState.Opening -> AppOpeningScreen()
        is AppGraphState.Failed -> AppStartupFailed(
            failure = current.failure,
            onRetry = container::retry,
            journal = container.diagnostics.journal,
            exporter = container.platform.journalExporter,
            sink = container.diagnostics.sink,
        )

        is AppGraphState.Ready -> AppNavHost(
            dependencies = current.dependencies,
            appLinks = container.appLinks,
            platform = platform,
        )
    }
}

@Composable
fun AppOpeningScreen() {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        CircularProgressIndicator()
        Text(
            text = stringResource(R.string.app_startup_loading),
            style = MaterialTheme.typography.bodyLarge,
        )
    }
}

@Composable
fun AppStartupFailed(
    failure: CoreFailure,
    onRetry: () -> Unit,
    journal: Journal,
    exporter: JournalExporter,
    sink: DiagnosticSink,
) {
    var showDiagnostics by rememberSaveable { mutableStateOf(false) }
    if (showDiagnostics) {
        OfflineDiagnostics(
            journal = journal,
            exporter = exporter,
            sink = sink,
            failure = failure,
            onBack = { showDiagnostics = false },
        )
        return
    }
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            text = stringResource(R.string.app_startup_failed),
            style = MaterialTheme.typography.titleMedium,
        )
        Text(
            text = stringResource(R.string.app_core_unavailable_body),
            style = MaterialTheme.typography.bodyMedium,
        )
        Text(
            text = failure.code,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Button(onClick = onRetry) {
            Text(stringResource(R.string.app_retry))
        }
        Button(onClick = { showDiagnostics = true }) {
            Text(stringResource(R.string.app_diagnostics))
        }
    }
}

/**
 * Diagnostics before Core opens: the local journal, export and clear stay
 * available while Core failed to initialize.
 */
@Composable
private fun OfflineDiagnostics(
    journal: Journal,
    exporter: JournalExporter,
    sink: DiagnosticSink,
    failure: CoreFailure,
    onBack: () -> Unit,
) {
    val viewModel: DiagnosticsViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                DiagnosticsViewModel(
                    instanceId = InstanceId(STARTUP_SCOPE),
                    journal = journal,
                    exporter = exporter,
                    diagnostics = sink,
                    coreFailureSummary = null,
                    appBuild = BuildConfig.APP_VERSION,
                    coreBuild = "unavailable",
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    LaunchedEffect(Unit) {
        viewModel.markCoreUnavailable(failure)
        viewModel.refresh()
    }
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

private const val STARTUP_SCOPE = "startup"
