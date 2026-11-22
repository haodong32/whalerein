package com.whalerein.android.ui.routes

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
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
import com.whalerein.android.AppDependencies
import com.whalerein.android.BuildConfig
import com.whalerein.android.R
import com.whalerein.android.platform.ActivityPlatform
import com.whalerein.android.state.access.CloudSignInPhase
import com.whalerein.android.state.access.CloudSignInViewModel
import com.whalerein.android.state.access.DebugEnvironmentAware
import com.whalerein.android.state.access.DirectoryViewModel
import com.whalerein.android.state.access.Profile
import com.whalerein.android.state.access.ProfilesViewModel
import com.whalerein.android.state.access.StandaloneSetupViewModel
import com.whalerein.android.state.access.StartupEvidence
import com.whalerein.android.state.access.StartupViewModel
import com.whalerein.android.state.access.WelcomeViewModel
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import com.whalerein.android.ui.access.CloudSignInScreen
import com.whalerein.android.ui.access.DebugEnvironmentPicker
import com.whalerein.android.ui.access.DirectoryScreen
import com.whalerein.android.ui.access.ProfileSettingsScreen
import com.whalerein.android.ui.access.ProfilesScreen
import com.whalerein.android.ui.access.StandaloneSetupScreen
import com.whalerein.android.ui.access.WelcomeScreen
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

@Composable
fun StartupRoute(
    dependencies: AppDependencies,
    onResolved: (Route, StartupEvidence) -> Unit,
    onOpenDiagnostics: () -> Unit,
) {
    val viewModel: StartupViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                StartupViewModel(
                    instance = dependencies.instance,
                    startup = dependencies.startup,
                    diagnostics = dependencies.diagnosticSink,
                    dispatcher = Dispatchers.Default,
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(state.destination) {
        val destination = state.destination ?: return@LaunchedEffect
        onResolved(destination.route, destination.evidence)
    }
    if (state.destination == null) {
        StartupFallback(
            failure = state.error,
            onRetry = viewModel::retry,
            onOpenDiagnostics = onOpenDiagnostics,
        )
    }
}

@Composable
fun WelcomeRoute(
    dependencies: AppDependencies,
    onNavigate: (Route) -> Unit,
) {
    val viewModel: WelcomeViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                WelcomeViewModel(
                    profiles = dependencies.profiles,
                    startup = dependencies.startup,
                    diagnostics = dependencies.diagnosticSink,
                    dispatcher = Dispatchers.Default,
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(state.destination) {
        state.destination?.let(onNavigate)
    }
    WelcomeScreen(
        state = state,
        onContinue = viewModel::continueWithCloud,
        onNavigate = onNavigate,
        modifier = Modifier.fillMaxSize(),
    )
}

@Composable
fun CloudSignInRoute(
    dependencies: AppDependencies,
    appLinks: com.whalerein.android.platform.applinks.AppLinkRouter,
    platform: ActivityPlatform?,
    onNavigate: (Route) -> Unit,
    onRestartGraph: () -> Unit,
) {
    val debug = dependencies as? DebugEnvironmentAware
    // The environment decides the installed App Link callback, so the screen
    // validates against the same value Core registered for this profile.
    val cloudEnvironment = remember {
        runCatching { debug?.debugCloudEnvironment?.current() }.getOrNull() ?: "production"
    }
    val viewModel: CloudSignInViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                CloudSignInViewModel(
                    instance = dependencies.instance,
                    auth = dependencies.cloudAuth,
                    clock = dependencies.clock,
                    diagnostics = dependencies.diagnosticSink,
                    environment = cloudEnvironment,
                    dispatcher = Dispatchers.Default,
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    val callback by appLinks.callback.collectAsStateWithLifecycle()
    var browserRequested by remember { mutableStateOf(false) }
    val browser = platform?.browser
    var debugEnvironment by remember {
        mutableStateOf(debug?.debugCloudEnvironment?.current() ?: "production")
    }
    var debugCaPem by remember { mutableStateOf<String?>(null) }
    var debugBusy by remember { mutableStateOf(false) }
    var debugError by remember { mutableStateOf<String?>(null) }
    val debugScope = rememberCoroutineScope()

    LaunchedEffect(state.destination) {
        state.destination?.let(onNavigate)
    }
    LaunchedEffect(state.phase, state.attempt?.attemptId, browserRequested) {
        if (!browserRequested || state.phase != CloudSignInPhase.AwaitingBrowser) return@LaunchedEffect
        val attempt = state.attempt ?: return@LaunchedEffect
        browserRequested = false
        val failure = browser?.open(attempt.authorizationUrl)
        if (failure != null) viewModel.cancel()
    }
    LaunchedEffect(callback, state.attempt?.attemptId) {
        val pendingCallback = callback ?: return@LaunchedEffect
        if (state.attempt == null) return@LaunchedEffect
        viewModel.handleCallback(pendingCallback)
        appLinks.consumeCallback()
    }
    Column(modifier = Modifier.fillMaxSize()) {
        CloudSignInScreen(
            state = state,
            onStart = {
                browserRequested = true
                viewModel.start()
            },
            onCancel = viewModel::cancel,
            onRecover = viewModel::recover,
            onNavigate = onNavigate,
            modifier = Modifier.weight(1f),
        )
        if (BuildConfig.DEBUG && debug != null) {
            DebugEnvironmentPicker(
                current = debugEnvironment,
                hasCa = debugCaPem != null,
                busy = debugBusy,
                error = debugError,
                onSelect = {
                    debugEnvironment = it
                    debugError = null
                },
                onChooseCa = {
                    debugScope.launch {
                        debugCaPem = platform?.documents?.openPem()
                        if (debugCaPem == null) {
                            debugError = "Development CA was not selected."
                        }
                    }
                },
                onApply = {
                    debugScope.launch {
                        debugBusy = true
                        debugError = null
                        when (val result = debug.debugCloudEnvironment.apply(debugEnvironment, debugCaPem)) {
                            is Outcome.Success -> onRestartGraph()
                            is Outcome.Failure -> debugError = result.failure.message
                            is Outcome.Unconfirmed -> debugError = result.failure.message
                            Outcome.Cancelled -> debugError = "Environment change cancelled."
                        }
                        debugBusy = false
                    }
                },
            )
        }
    }
}

@Composable
fun DirectoryRoute(
    dependencies: AppDependencies,
    onNavigate: (Route) -> Unit,
) {
    val viewModel: DirectoryViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                DirectoryViewModel(
                    instance = dependencies.instance,
                    directory = dependencies.directory,
                    diagnostics = dependencies.diagnosticSink,
                    dispatcher = Dispatchers.Default,
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(state.destination) {
        state.destination?.let(onNavigate)
    }
    DirectoryScreen(
        state = state,
        onConnect = viewModel::connect,
        onSelectDefault = viewModel::selectDefault,
        onNavigate = onNavigate,
        modifier = Modifier.fillMaxSize(),
    )
}

@Composable
fun ProfilesRoute(
    dependencies: AppDependencies,
    onOpenProfile: (InstanceId) -> Unit,
    onNavigate: (Route) -> Unit,
) {
    val viewModel: ProfilesViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                ProfilesViewModel(
                    profiles = dependencies.profiles,
                    diagnostics = dependencies.diagnosticSink,
                    dispatcher = Dispatchers.Default,
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    ProfilesScreen(
        state = state,
        onActivate = viewModel::activate,
        onRemove = viewModel::remove,
        onEdit = { profile -> onOpenProfile(profile.instance) },
        onRefresh = viewModel::refresh,
        onNavigate = onNavigate,
        modifier = Modifier.fillMaxSize(),
    )
}

@Composable
fun ProfileSettingsRoute(
    dependencies: AppDependencies,
    instanceId: InstanceId,
    onNavigate: (Route) -> Unit,
) {
    val viewModel: ProfilesViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                ProfilesViewModel(
                    profiles = dependencies.profiles,
                    diagnostics = dependencies.diagnosticSink,
                    dispatcher = Dispatchers.Default,
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(instanceId) {
        viewModel.refreshStorageScope(instanceId)
        viewModel.refreshCredentials(instanceId)
    }
    val profile: Profile? = state.profiles.firstOrNull { it.instance == instanceId }
    if (profile == null) {
        StartupFallback(failure = state.error, onRetry = viewModel::refresh, onOpenDiagnostics = null)
        return
    }
    ProfileSettingsScreen(
        state = state,
        profile = profile,
        onActivate = viewModel::activate,
        onRemove = viewModel::remove,
        onRefresh = viewModel::refresh,
        onNavigate = onNavigate,
        modifier = Modifier.fillMaxSize(),
    )
}

@Composable
fun StandaloneSetupRoute(
    dependencies: AppDependencies,
    platform: ActivityPlatform?,
    onNavigate: (Route) -> Unit,
) {
    val viewModel: StandaloneSetupViewModel = viewModel(
        factory = viewModelFactory {
            initializer {
                StandaloneSetupViewModel(
                    instance = dependencies.instance,
                    pairing = dependencies.pairing,
                    diagnostics = dependencies.diagnosticSink,
                    dispatcher = Dispatchers.Default,
                )
            }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    LaunchedEffect(state.destination) {
        state.destination?.let(onNavigate)
    }
    Box(modifier = Modifier.fillMaxSize()) {
        StandaloneSetupScreen(
            state = state,
            onLoadInvitation = viewModel::loadInvitation,
            onCheckFingerprint = viewModel::checkFingerprint,
            onImportCa = viewModel::importCa,
            onRequestLanPermission = viewModel::requestLanPermission,
            onValidateTarget = viewModel::validateTarget,
            onAccept = viewModel::accept,
            onReject = viewModel::reject,
            onNavigate = onNavigate,
            modifier = Modifier.fillMaxSize(),
        )
        FloatingActionButton(
            onClick = {
                scope.launch {
                    val pem = platform?.documents?.openPem() ?: return@launch
                    viewModel.importCa(pem)
                }
            },
            modifier = Modifier
                .align(Alignment.BottomEnd)
                .padding(16.dp),
        ) {
            Text(stringResource(R.string.app_import_ca_file))
        }
    }
}

@Composable
private fun StartupFallback(
    failure: CoreFailure?,
    onRetry: () -> Unit,
    onOpenDiagnostics: (() -> Unit)?,
) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        if (failure == null) {
            CircularProgressIndicator()
            Text(stringResource(R.string.app_startup_loading), style = MaterialTheme.typography.bodyLarge)
        } else {
            Text(
                text = stringResource(R.string.app_startup_failed),
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                text = stringResource(R.string.app_startup_unavailable_body),
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
            if (onOpenDiagnostics != null) {
                Button(onClick = onOpenDiagnostics) {
                    Text(stringResource(R.string.app_diagnostics))
                }
            }
        }
    }
}
