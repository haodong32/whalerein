package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.ScreenModel
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

/** Host-qualified session list state (AC-08). */
data class SessionListUiState(
    val host: HostRef? = null,
    val status: SessionListStatus = SessionListStatus.Idle,
    val sessions: List<SessionSummary> = emptyList(),
    /** True when the list came from the eligible offline cache. */
    val cached: Boolean = false,
    val capabilities: SessionCapabilities = SessionCapabilities(),
    val failure: CoreFailure? = null,
    val selectedSessionId: SessionId? = null,
    val details: SessionDetailsUiState = SessionDetailsUiState.None,
    val createFormVisible: Boolean = false,
    val createInput: CreateSessionInput = CreateSessionInput(),
    val create: CreateSessionUiState = CreateSessionUiState.Idle,
    val fork: ForkSessionUiState = ForkSessionUiState.Idle,
)

enum class SessionListStatus { Idle, Loading, Ready, Cached, Failed }

sealed interface SessionDetailsUiState {
    data object None : SessionDetailsUiState

    data object Loading : SessionDetailsUiState

    data class Loaded(val details: SessionDetails) : SessionDetailsUiState

    data class Unavailable(val failure: CoreFailure) : SessionDetailsUiState
}

/** Typed create outcomes; unsupported and unconfirmed are never collapsed. */
sealed interface CreateSessionUiState {
    data object Idle : CreateSessionUiState

    data object InFlight : CreateSessionUiState

    data class Created(val session: SessionRef) : CreateSessionUiState

    data class Unconfirmed(val failure: CoreFailure) : CreateSessionUiState

    data object Unsupported : CreateSessionUiState

    data class Failed(val failure: CoreFailure) : CreateSessionUiState
}

/**
 * Typed fork outcomes. A confirmed child with an unusable working directory is
 * still [Forked]; execution never starts automatically.
 */
sealed interface ForkSessionUiState {
    data object Idle : ForkSessionUiState

    data object InFlight : ForkSessionUiState

    data class Forked(
        val child: SessionRef,
        val workingDirectoryAvailable: Boolean,
        val autoExecutionStarted: Boolean = false,
    ) : ForkSessionUiState

    data class Unconfirmed(val failure: CoreFailure) : ForkSessionUiState

    data object Unsupported : ForkSessionUiState

    data class Failed(val failure: CoreFailure) : ForkSessionUiState
}

class SessionListViewModel(
    private val port: SessionListPort,
    diagnostics: DiagnosticSink,
) : ScreenModel<SessionListUiState>(SessionListUiState()) {
    private val diag = ConversationDiagnostics(diagnostics)
    private var loadedHost: HostRef? = null

    /** Opens one explicit Host. Reopening the same Host is a no-op. */
    fun open(host: HostRef) {
        if (loadedHost == host) {
            return
        }
        loadedHost = host
        setState(
            SessionListUiState(
                host = host,
                status = SessionListStatus.Loading,
            ),
        )
        loadList(host)
    }

    fun refresh() {
        val host = state.value.host ?: return
        edit { copy(status = SessionListStatus.Loading) }
        loadList(host)
    }

    fun select(sessionId: SessionId) {
        val host = state.value.host ?: return
        edit {
            copy(
                selectedSessionId = sessionId,
                details = SessionDetailsUiState.Loading,
                fork = ForkSessionUiState.Idle,
            )
        }
        loadDetails(SessionRef(host, sessionId))
    }

    fun refreshDetails() {
        val sessionId = state.value.selectedSessionId ?: return
        select(sessionId)
    }

    fun clearSelection() {
        edit {
            copy(
                selectedSessionId = null,
                details = SessionDetailsUiState.None,
                fork = ForkSessionUiState.Idle,
            )
        }
    }

    fun showCreateForm() {
        edit { copy(createFormVisible = true, create = CreateSessionUiState.Idle) }
    }

    fun hideCreateForm() {
        edit { copy(createFormVisible = false) }
    }

    fun updateCreateInput(input: CreateSessionInput) {
        edit { copy(createInput = input) }
    }

    fun clearCreateInput() {
        edit { copy(createInput = CreateSessionInput()) }
    }

    fun create() {
        create(state.value.createInput)
    }

    fun createDefault() {
        create(CreateSessionInput())
    }

    fun create(input: CreateSessionInput) {
        val host = state.value.host ?: return
        if (state.value.create is CreateSessionUiState.InFlight) {
            return
        }
        validationFailure(input)?.let { typed ->
            edit { copy(create = typed, createInput = input) }
            return
        }
        edit {
            copy(
                create = CreateSessionUiState.InFlight,
                createInput = input,
            )
        }
        scope.launch {
            val result = try {
                port.create(host, input)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                Outcome.Failure(unexpectedFailure())
            }
            if (state.value.host != host) {
                return@launch
            }
            when (result) {
                is Outcome.Success -> {
                    val created = result.value
                    if (created.session.host != host) {
                        edit {
                            copy(
                                create = CreateSessionUiState.Failed(foreignHostFailure()),
                            )
                        }
                        diag.warn(
                            ConversationDiagnosticStage.COMMAND,
                            ConversationDiagnosticEventName.SESSION_OPERATION,
                            code = SessionListFailureCodes.FOREIGN_HOST,
                        )
                    } else {
                        val summary = created.details.toSummary(created.session.sessionId)
                        edit {
                            copy(
                                create = CreateSessionUiState.Created(created.session),
                                createFormVisible = false,
                                createInput = CreateSessionInput(),
                                sessions = listOf(summary) + sessions.filterNot {
                                    it.sessionId == created.session.sessionId
                                },
                                selectedSessionId = created.session.sessionId,
                                details = SessionDetailsUiState.Loaded(created.details),
                            )
                        }
                        diag.info(
                            ConversationDiagnosticStage.COMMAND,
                            ConversationDiagnosticEventName.SESSION_OPERATION,
                            code = "created",
                        )
                    }
                }

                is Outcome.Failure -> edit {
                    copy(create = createFailure(result.failure))
                }

                is Outcome.Unconfirmed -> edit {
                    copy(create = CreateSessionUiState.Unconfirmed(result.failure))
                }

                Outcome.Cancelled -> edit { copy(create = CreateSessionUiState.Idle) }
            }
        }
    }

    /**
     * Forks the selected session's latest completed turn, or [turnId] when the
     * caller has an explicit completed turn. The child is displayed, never run.
     */
    fun fork(turnId: String? = null) {
        val host = state.value.host ?: return
        if (state.value.fork is ForkSessionUiState.InFlight) {
            return
        }
        val sessionId = state.value.selectedSessionId ?: return
        if (!state.value.capabilities.canFork) {
            edit { copy(fork = ForkSessionUiState.Unsupported) }
            return
        }
        val details = state.value.details as? SessionDetailsUiState.Loaded
        val targetTurn = turnId ?: details?.details?.latestCompletedTurnId
        if (targetTurn == null) {
            val code = if (details == null) {
                SessionListFailureCodes.DETAILS_REQUIRED
            } else {
                SessionListFailureCodes.NO_COMPLETED_TURN
            }
            edit {
                copy(
                    fork = ForkSessionUiState.Failed(
                        CoreFailure(code, "No completed turn is available to fork.", false),
                    ),
                )
            }
            return
        }
        val session = SessionRef(host, sessionId)
        edit { copy(fork = ForkSessionUiState.InFlight) }
        scope.launch {
            val result = try {
                port.fork(session, ForkSessionInput(targetTurn))
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                Outcome.Failure(unexpectedFailure())
            }
            if (state.value.host != host) {
                return@launch
            }
            when (result) {
                is Outcome.Success -> {
                    val forked = result.value
                    if (forked.child.host != host) {
                        edit {
                            copy(fork = ForkSessionUiState.Failed(foreignHostFailure()))
                        }
                        diag.warn(
                            ConversationDiagnosticStage.COMMAND,
                            ConversationDiagnosticEventName.SESSION_OPERATION,
                            code = SessionListFailureCodes.FOREIGN_HOST,
                        )
                    } else {
                        val summary = forked.details.toSummary(forked.child.sessionId)
                        edit {
                            copy(
                                fork = ForkSessionUiState.Forked(
                                    child = forked.child,
                                    workingDirectoryAvailable = forked.workingDirectoryAvailable,
                                    // Core never starts execution for a fork.
                                    autoExecutionStarted = false,
                                ),
                                sessions = listOf(summary) + sessions.filterNot {
                                    it.sessionId == forked.child.sessionId
                                },
                                selectedSessionId = forked.child.sessionId,
                                details = SessionDetailsUiState.Loaded(forked.details),
                            )
                        }
                        diag.info(
                            ConversationDiagnosticStage.COMMAND,
                            ConversationDiagnosticEventName.SESSION_OPERATION,
                            code = "forked",
                        )
                    }
                }

                is Outcome.Failure -> edit { copy(fork = forkFailure(result.failure)) }
                is Outcome.Unconfirmed -> edit {
                    copy(fork = ForkSessionUiState.Unconfirmed(result.failure))
                }

                Outcome.Cancelled -> edit { copy(fork = ForkSessionUiState.Idle) }
            }
        }
    }

    fun clearOperation() {
        edit {
            copy(
                create = CreateSessionUiState.Idle,
                fork = ForkSessionUiState.Idle,
            )
        }
    }

    private fun loadList(host: HostRef) {
        scope.launch {
            val result = try {
                port.list(host)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                Outcome.Failure(unexpectedFailure())
            }
            if (state.value.host != host) {
                return@launch
            }
            when (result) {
                is Outcome.Success -> {
                    val snapshot = result.value
                    if (snapshot.host != host) {
                        failList(foreignHostFailure())
                        diag.warn(
                            ConversationDiagnosticStage.COMMAND,
                            ConversationDiagnosticEventName.SESSION_LIST_FAILED,
                            code = SessionListFailureCodes.FOREIGN_HOST,
                        )
                    } else {
                        edit {
                            copy(
                                status = if (snapshot.cached) {
                                    SessionListStatus.Cached
                                } else {
                                    SessionListStatus.Ready
                                },
                                sessions = snapshot.sessions,
                                cached = snapshot.cached,
                                capabilities = snapshot.capabilities,
                                failure = null,
                            )
                        }
                        diag.info(
                            ConversationDiagnosticStage.COMMAND,
                            ConversationDiagnosticEventName.SESSION_LIST_LOADED,
                            fields = mapOf("count" to snapshot.sessions.size.toString()),
                        )
                    }
                }

                is Outcome.Failure -> failList(result.failure)
                is Outcome.Unconfirmed -> failList(result.failure)
                Outcome.Cancelled -> Unit
            }
        }
    }

    private fun loadDetails(session: SessionRef) {
        scope.launch {
            val result = try {
                port.details(session)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                Outcome.Failure(unexpectedFailure())
            }
            if (state.value.host != session.host ||
                state.value.selectedSessionId != session.sessionId
            ) {
                return@launch
            }
            when (result) {
                is Outcome.Success -> {
                    if (result.value.session != session) {
                        edit {
                            copy(
                                details = SessionDetailsUiState.Unavailable(
                                    CoreFailure(
                                        SessionListFailureCodes.FOREIGN_SESSION,
                                        "The Session belongs to another scope.",
                                        false,
                                    ),
                                ),
                            )
                        }
                    } else {
                        edit {
                            copy(details = SessionDetailsUiState.Loaded(result.value))
                        }
                    }
                }

                is Outcome.Failure -> edit {
                    copy(details = SessionDetailsUiState.Unavailable(result.failure))
                }

                is Outcome.Unconfirmed -> edit {
                    copy(details = SessionDetailsUiState.Unavailable(result.failure))
                }

                Outcome.Cancelled -> Unit
            }
        }
    }

    private fun failList(failure: CoreFailure) {
        diag.warn(
            ConversationDiagnosticStage.COMMAND,
            ConversationDiagnosticEventName.SESSION_LIST_FAILED,
            code = failure.code,
        )
        edit {
            copy(
                status = SessionListStatus.Failed,
                failure = failure,
            )
        }
    }

    private fun validationFailure(input: CreateSessionInput): CreateSessionUiState? {
        val capabilities = state.value.capabilities
        if (!capabilities.canCreate) {
            return CreateSessionUiState.Unsupported
        }
        val workingDirectory = input.workingDirectory?.trim().orEmpty()
        val workspace = input.workspace?.trim().orEmpty()
        val preset = input.preset?.trim().orEmpty()
        if (workingDirectory.isNotEmpty() && workspace.isNotEmpty()) {
            return CreateSessionUiState.Failed(
                CoreFailure(
                    SessionListFailureCodes.INVALID_INPUT,
                    "Choose a working directory or a workspace, not both.",
                    false,
                ),
            )
        }
        if (input.workingDirectory != null && workingDirectory.isEmpty()) {
            return invalidInput("The working directory is blank.")
        }
        if (input.workspace != null && workspace.isEmpty()) {
            return invalidInput("The workspace is blank.")
        }
        if (input.preset != null && preset.isEmpty()) {
            return invalidInput("The preset is blank.")
        }
        if (workingDirectory.isNotEmpty() && !capabilities.supportsWorkingDirectory) {
            return CreateSessionUiState.Unsupported
        }
        if (workspace.isNotEmpty() && !capabilities.supportsWorkspace) {
            return CreateSessionUiState.Unsupported
        }
        if (preset.isNotEmpty() && !capabilities.supportsPreset) {
            return CreateSessionUiState.Unsupported
        }
        if (input.preferredSessionId != null &&
            !capabilities.supportsPreferredSessionId
        ) {
            return CreateSessionUiState.Unsupported
        }
        return null
    }

    private fun createFailure(failure: CoreFailure): CreateSessionUiState =
        when (failure.code) {
            ConversationFailureCodes.CAPABILITY_MISSING -> CreateSessionUiState.Unsupported
            SessionListFailureCodes.FOREIGN_HOST -> CreateSessionUiState.Failed(failure)
            else -> CreateSessionUiState.Failed(failure)
        }

    private fun forkFailure(failure: CoreFailure): ForkSessionUiState =
        when (failure.code) {
            ConversationFailureCodes.CAPABILITY_MISSING -> ForkSessionUiState.Unsupported
            SessionListFailureCodes.FOREIGN_HOST -> ForkSessionUiState.Failed(failure)
            else -> ForkSessionUiState.Failed(failure)
        }

    private fun invalidInput(message: String): CreateSessionUiState =
        CreateSessionUiState.Failed(CoreFailure(SessionListFailureCodes.INVALID_INPUT, message, false))

    private fun foreignHostFailure(): CoreFailure = CoreFailure(
        SessionListFailureCodes.FOREIGN_HOST,
        "The result belongs to another Host.",
        false,
    )

    private fun unexpectedFailure(): CoreFailure =
        CoreFailure("unexpected", "Unexpected failure", retryable = true)

    private fun SessionDetails.toSummary(sessionId: SessionId): SessionSummary =
        SessionSummary(
            sessionId = sessionId,
            title = title,
            updatedAtMillis = updatedAtMillis,
            hasPendingInput = false,
        )
    private fun edit(transform: SessionListUiState.() -> SessionListUiState) {
        update { transform(it) }
    }

}
