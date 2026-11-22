package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.ScreenModel
import com.whalerein.android.state.core.SessionRef
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

enum class ReadOnlyCacheState { NotApplicable, Available, Unavailable }

data class StartupUiState(
    val loading: Boolean = true,
    val destination: StartupDestination? = null,
    /** Retained even when the route falls back to the Host list. */
    val target: SessionRef? = null,
    val readOnlyCache: ReadOnlyCacheState = ReadOnlyCacheState.NotApplicable,
    val offline: Boolean = false,
    val error: CoreFailure? = null,
)

/**
 * Restores the last authorized route from Core evidence.
 *
 * Absence, authorization loss and temporary connectivity are deliberately
 * different outcomes. A cached conversation is offered only when Core reports
 * it is eligible, and the state marks that view read-only.
 */
class StartupViewModel(
    private val instance: InstanceId,
    private val startup: StartupPort,
    private val diagnostics: DiagnosticSink,
    private val dispatcher: CoroutineDispatcher = Dispatchers.Default,
) : ScreenModel<StartupUiState>(StartupUiState()) {

    init {
        scope.launch(dispatcher) {
            restoreInternal()
        }
    }

    fun retry() {
        scope.launch(dispatcher) {
            restoreInternal()
        }
    }

    private suspend fun restoreInternal() {
        setState(state.value.copy(loading = true, error = null))
        val result = guardedPortCall(diagnostics, AccessStage.Startup, "startup_restore", instance) {
            startup.restore(instance)
        }
        when (result) {
            is Outcome.Success -> {
                val destination = decide(result.value)
                setState(
                    state.value.copy(
                        loading = false,
                        destination = destination,
                        target = result.value.target,
                        readOnlyCache = readOnlyCache(destination),
                        offline = !result.value.online,
                        error = null,
                    ),
                )
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Info,
                    event = "startup_route_selected",
                    stage = AccessStage.Startup,
                    code = destination.evidence.name.lowercase(),
                    instanceId = instance,
                )
                if (destination.evidence == StartupEvidence.OfflineCacheAvailable ||
                    destination.evidence == StartupEvidence.OfflineCacheUnavailable
                ) {
                    diagnostics.accessEvent(
                        level = DiagnosticLevel.Warn,
                        event = "startup_restore_offline",
                        stage = AccessStage.Startup,
                        code = destination.evidence.name.lowercase(),
                        instanceId = instance,
                    )
                }
            }

            else -> {
                setState(
                    state.value.copy(
                        loading = false,
                        error = result.asFailure(),
                    ),
                )
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Error,
                    event = "startup_restore_failed",
                    stage = AccessStage.Startup,
                    code = result.asFailure().code,
                    instanceId = instance,
                )
            }
        }
    }

    private fun decide(snapshot: StartupSnapshot): StartupDestination = when (snapshot.authorization) {
        StartupAuthorization.SignedOut -> StartupDestination(Route.Directory, StartupEvidence.SignedOut)
        StartupAuthorization.Unpaired -> StartupDestination(Route.Directory, StartupEvidence.Unpaired)
        StartupAuthorization.Revoked -> StartupDestination(Route.Directory, StartupEvidence.Revoked)
        StartupAuthorization.Expired -> StartupDestination(Route.Directory, StartupEvidence.Expired)
        StartupAuthorization.Unavailable -> StartupDestination(
            Route.Directory,
            StartupEvidence.AuthorizationUnavailable,
        )

        StartupAuthorization.Authorized -> decideAuthorized(snapshot)
    }

    private fun decideAuthorized(snapshot: StartupSnapshot): StartupDestination {
        val target = snapshot.target
        if (target != null && !snapshot.missingSession) {
            return when {
                snapshot.online -> StartupDestination(
                    Route.Conversation(target),
                    StartupEvidence.ValidPreviousConversation,
                )

                snapshot.readOnlyCacheAvailable -> StartupDestination(
                    Route.Conversation(target),
                    StartupEvidence.OfflineCacheAvailable,
                )

                else -> StartupDestination(
                    Route.HostSessions(target.host),
                    StartupEvidence.OfflineCacheUnavailable,
                )
            }
        }
        val host = snapshot.host ?: target?.host
        if (host != null) {
            return StartupDestination(Route.HostSessions(host), StartupEvidence.MissingSession)
        }
        return StartupDestination(Route.Directory, StartupEvidence.FirstCloudUse)
    }

    private fun readOnlyCache(destination: StartupDestination): ReadOnlyCacheState =
        when (destination.evidence) {
            StartupEvidence.OfflineCacheAvailable -> ReadOnlyCacheState.Available
            StartupEvidence.OfflineCacheUnavailable -> ReadOnlyCacheState.Unavailable
            else -> ReadOnlyCacheState.NotApplicable
        }
}
