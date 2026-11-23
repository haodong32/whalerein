package com.whalerein.android.state.access

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.ScreenModel
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

data class DirectoryUiState(
    val loading: Boolean = true,
    val hosts: List<DirectoryHostView> = emptyList(),
    val activeHost: HostRef? = null,
    val session: CloudSession? = null,
    val defaultHostId: HostId? = null,
    val destination: Route? = null,
    val error: CoreFailure? = null,
    val deniedHostId: HostId? = null,
    /** A Cloud directory without an account session cannot list Hosts. */
    val needsSignIn: Boolean = false,
) {
    /**
     * A signed-out or first-use directory is the only fresh-start surface, so it
     * must keep Standalone setup reachable even though Welcome is not shown.
     */
    val standaloneSetupAvailable: Boolean get() = needsSignIn || hosts.isEmpty()
}

/**
 * Presents the Cloud directory.
 *
 * Presence, grant readiness and connection state stay separate. Only a Host
 * whose grant Core confirms as ready can set [DirectoryUiState.destination];
 * a denial never silently substitutes another Host.
 */
class DirectoryViewModel(
    private val instance: InstanceId,
    private val directory: DirectoryPort,
    private val diagnostics: DiagnosticSink,
    private val dispatcher: CoroutineDispatcher = Dispatchers.Default,
) : ScreenModel<DirectoryUiState>(DirectoryUiState()) {

    init {
        scope.launch(dispatcher) {
            loadInternal()
        }
    }

    fun refresh() {
        scope.launch(dispatcher) {
            loadInternal()
        }
    }

    fun connect(hostId: HostId) {
        scope.launch(dispatcher) {
            val host = state.value.hosts.firstOrNull { it.hostId == hostId }
            if (host == null) {
                deny(hostId, CoreFailure("host_unknown", "That Host is not in the directory."))
                return@launch
            }
            if (!host.connectable) {
                deny(hostId, unavailableFailure(host.availability))
                return@launch
            }
            setState(state.value.copy(error = null, deniedHostId = null))
            val result = guardedPortCall(diagnostics, AccessStage.Transport, "directory_connect", instance) {
                directory.connect(instance, hostId)
            }
            when (result) {
                is Outcome.Success -> {
                    val connection = result.value
                    if (connection.host.availability != HostAvailability.Ready ||
                        !connection.host.connectable ||
                        !connection.host.online
                    ) {
                        deny(hostId, unavailableFailure(connection.host.availability))
                        return@launch
                    }
                    val hostRef = HostRef(instance, hostId)
                    setState(
                        state.value.copy(
                            activeHost = hostRef,
                            session = connection.session,
                            destination = Route.HostSessions(hostRef),
                            error = null,
                            deniedHostId = null,
                        ),
                    )
                    diagnostics.accessEvent(
                        level = DiagnosticLevel.Info,
                        event = "transport_host_opened",
                        stage = AccessStage.Transport,
                        instanceId = instance,
                    )
                }

                else -> {
                    val failure = result.asFailure()
                    deny(hostId, failure)
                    diagnostics.accessEvent(
                        level = DiagnosticLevel.Warn,
                        event = "transport_host_connect_failed",
                        stage = AccessStage.Transport,
                        code = failure.code,
                        instanceId = instance,
                    )
                }
            }
        }
    }

    fun selectDefault(hostId: HostId) {
        scope.launch(dispatcher) {
            val host = state.value.hosts.firstOrNull { it.hostId == hostId }
            if (host == null) {
                setState(
                    state.value.copy(
                        error = CoreFailure("host_unknown", "That Host is not in the directory."),
                    ),
                )
                return@launch
            }
            if (!host.connectable || host.availability != HostAvailability.Ready) {
                setState(
                    state.value.copy(
                        error = CoreFailure(
                            "default_unauthorized",
                            "Choose an authorized Host as the default.",
                        ),
                    ),
                )
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Warn,
                    event = "transport_default_rejected",
                    stage = AccessStage.Transport,
                    code = "default_unauthorized",
                    instanceId = instance,
                )
                return@launch
            }
            val result = guardedPortCall(diagnostics, AccessStage.Transport, "directory_default", instance) {
                directory.selectDefault(instance, hostId)
            }
            when (result) {
                is Outcome.Success -> {
                    setState(
                        state.value.copy(
                            defaultHostId = result.value.hostId,
                            error = null,
                        ),
                    )
                    diagnostics.accessEvent(
                        level = DiagnosticLevel.Info,
                        event = "transport_default_selected",
                        stage = AccessStage.Transport,
                        instanceId = instance,
                    )
                }

                else -> setState(
                    state.value.copy(
                        error = result.asFailure(),
                    ),
                )
            }
        }
    }

    fun clearError() {
        setState(state.value.copy(error = null, deniedHostId = null))
    }

    private suspend fun loadInternal() {
        setState(state.value.copy(loading = true, error = null))
        val result = guardedPortCall(diagnostics, AccessStage.Transport, "directory_list", instance) {
            directory.list(instance)
        }
        if (result is Outcome.Failure && isSignedOut(result.failure)) {
            val session = guardedPortCall(
                diagnostics,
                AccessStage.Transport,
                "directory_current_session",
                instance,
            ) {
                directory.currentSession(instance)
            }
            if (session.valueOrNull() == null) {
                diagnostics.accessEvent(
                    level = DiagnosticLevel.Info,
                    event = "directory_sign_in_required",
                    stage = AccessStage.Transport,
                    instanceId = instance,
                )
                setState(
                    state.value.copy(
                        loading = false,
                        hosts = emptyList(),
                        session = null,
                        destination = null,
                        error = null,
                        needsSignIn = true,
                    ),
                )
                return
            }
        }
        when (result) {
            is Outcome.Success -> {
                val session = guardedPortCall(
                    diagnostics,
                    AccessStage.Transport,
                    "directory_current_session",
                    instance,
                ) {
                    directory.currentSession(instance)
                }
                setState(
                    state.value.copy(
                        loading = false,
                        hosts = result.value,
                        session = session.valueOrNull() ?: state.value.session,
                        error = null,
                        needsSignIn = false,
                    ),
                )
            }

            else -> setState(
                state.value.copy(
                    loading = false,
                    error = result.asFailure(),
                ),
            )
        }
    }

    private fun isSignedOut(failure: CoreFailure): Boolean =
        failure.code == "security" || failure.code == "unauthorized"

    private fun deny(hostId: HostId, failure: CoreFailure) {
        setState(
            state.value.copy(
                destination = null,
                deniedHostId = hostId,
                error = failure,
            ),
        )
    }

    private fun unavailableFailure(availability: HostAvailability): CoreFailure = when (availability) {
        HostAvailability.Ready -> CoreFailure("host_unavailable", "The Host is not reachable.")
        HostAvailability.NoGrant -> CoreFailure("host_no_grant", "This device has no grant for that Host.")
        HostAvailability.Offline -> CoreFailure("host_offline", "That Host is offline.")
        HostAvailability.AuthorizationUnavailable -> CoreFailure(
            "host_authorization_unavailable",
            "Authorization for that Host could not be checked.",
        )
        HostAvailability.Unknown -> CoreFailure("host_unknown_state", "The Host state is unknown.")
    }
}
