package com.whalerein.android

import android.content.Context
import com.whalerein.android.platform.AndroidClock
import com.whalerein.android.platform.PlatformServices
import com.whalerein.android.platform.UuidIdGenerator
import com.whalerein.android.platform.applinks.AppLinkRouter
import com.whalerein.android.platform.journal.DiagnosticsRuntime
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.Outcome
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** Application graph lifecycle. The UI renders [AppContainer.state]. */
sealed interface AppGraphState {
    data object Opening : AppGraphState

    data class Ready(val dependencies: AppDependencies) : AppGraphState

    data class Failed(val failure: CoreFailure) : AppGraphState
}

/**
 * Application-scoped composition root.
 *
 * The journal and picker/permission seams are created before Core opens, so
 * startup failures are reviewable and exportable without a live Core handle.
 * Core is opened off the main thread exactly once per process; [retry] replaces
 * a failed attempt.
 */
class AppContainer(
    private val appContext: Context,
    val diagnostics: DiagnosticsRuntime,
    val platform: PlatformServices,
) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val mutableState = MutableStateFlow<AppGraphState>(AppGraphState.Opening)
    val state: StateFlow<AppGraphState> = mutableState.asStateFlow()

    val appLinks: AppLinkRouter = AppLinkRouter(diagnostics.sink)

    fun start() {
        if (mutableState.value !is AppGraphState.Opening) return
        open()
    }

    fun retry() {
        if (mutableState.value is AppGraphState.Opening) return
        mutableState.value = AppGraphState.Opening
        open()
    }

    /** Rebuild the graph after a debug environment switch. */
    fun restart() {
        if (mutableState.value is AppGraphState.Opening) return
        (mutableState.value as? AppGraphState.Ready)?.dependencies?.close()
        mutableState.value = AppGraphState.Opening
        open()
    }

    private fun open() {
        scope.launch {
            val opened = openBridgeFacade(
                context = appContext,
                coreSink = diagnostics.coreSink,
                appVersion = BuildConfig.APP_VERSION,
            )
            when (opened) {
                is Outcome.Success -> {
                    val facade = opened.value
                    mutableState.value = AppGraphState.Ready(
                        BridgeAppDependencies(
                            core = facade,
                            journal = diagnostics.journal,
                            journalExporter = platform.journalExporter,
                            destinationWriter = platform.destinationWriter,
                            clock = AndroidClock,
                            ids = UuidIdGenerator,
                            diagnosticSink = diagnostics.sink,
                            platform = platform,
                        ),
                    )
                    diagnostics.sink.emit(
                        DiagnosticEvent(
                            level = DiagnosticLevel.Info,
                            event = "startup_core_ready",
                            stage = "startup",
                            fields = mapOf("core_version" to facade.provenance.coreVersion),
                        ),
                    )
                }

                is Outcome.Failure -> fail(opened.failure)
                is Outcome.Unconfirmed -> fail(opened.failure)
                Outcome.Cancelled -> fail(
                    CoreFailure(code = "core_open_cancelled", message = "core_open_cancelled", retryable = true),
                )
            }
        }
    }

    private fun fail(failure: CoreFailure) {
        mutableState.value = AppGraphState.Failed(failure)
        diagnostics.sink.emit(
            DiagnosticEvent(
                level = DiagnosticLevel.Error,
                event = "startup_core_failed",
                stage = "startup",
                code = failure.code,
            ),
        )
        diagnostics.requestFlush()
    }

    fun close() {
        (mutableState.value as? AppGraphState.Ready)?.dependencies?.close()
        diagnostics.close()
    }
}
