package com.whalerein.android.state.support

import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.ScreenModel

/**
 * Account and device settings contract.
 *
 * Every destructive action first produces a [AccountConfirmation] that names its
 * scope. Cancelling clears the confirmation without calling the port. Expected
 * outcomes, including retention uncertainty, are reported to the user.
 */
class AccountViewModel(
    instanceId: InstanceId,
    private val port: AccountPort,
    private val diagnostics: DiagnosticSink,
    private val clock: Clock,
) : ScreenModel<AccountUiState>(AccountUiState(instanceId = instanceId)) {
    suspend fun load() {
        setState(state.value.copy(loading = true))
        try {
            when (val statusOutcome = port.status(state.value.instanceId)) {
                is Outcome.Success -> {
                    val status = statusOutcome.value
                    val sessions = if (status.signedIn) {
                        port.listAppSessions(state.value.instanceId)
                    } else {
                        null
                    }
                    val (appSessions, sessionsNotice) = when (sessions) {
                        is Outcome.Success -> sessions.value to null
                        is Outcome.Failure -> emptyList<AppSessionInfo>() to
                            AccountNotice.SessionsFailed(sessions.failure)
                        is Outcome.Unconfirmed -> emptyList<AppSessionInfo>() to
                            AccountNotice.SessionsFailed(sessions.failure)
                        null, is Outcome.Cancelled -> emptyList<AppSessionInfo>() to null
                    }
                    val targetsOutcome = if (status.signedIn) {
                        port.revocableTargets(state.value.instanceId)
                    } else {
                        null
                    }
                    val (targets, targetsNotice) = when (targetsOutcome) {
                        is Outcome.Success -> targetsOutcome.value to null
                        is Outcome.Failure ->
                            AccountRevocableTargets() to
                                AccountNotice.TargetsFailed(targetsOutcome.failure)
                        is Outcome.Unconfirmed ->
                            AccountRevocableTargets() to
                                AccountNotice.TargetsFailed(targetsOutcome.failure)
                        null, is Outcome.Cancelled -> AccountRevocableTargets() to null
                    }
                    setState(
                        state.value.copy(
                            status = status,
                            appSessions = appSessions,
                            targets = targets,
                            notice = sessionsNotice ?: targetsNotice,
                        ),
                    )
                }
                is Outcome.Failure -> {
                    setState(
                        state.value.copy(
                            notice = AccountNotice.LoadFailed(statusOutcome.failure),
                        ),
                    )
                }
                is Outcome.Unconfirmed -> {
                    setState(
                        state.value.copy(
                            notice = AccountNotice.LoadFailed(statusOutcome.failure),
                        ),
                    )
                }
                is Outcome.Cancelled -> Unit
            }
        } finally {
            setState(state.value.copy(loading = false))
        }
    }

    fun requestSignOut() {
        val status = state.value.status
        setState(
            state.value.copy(
                confirmation = AccountConfirmation(
                    action = AccountAction.SignOut,
                    instanceId = state.value.instanceId,
                    accountId = status?.accountId,
                    scopeKind = AccountScopeKind.Account,
                    targetLabel = status?.workspace ?: status?.accountId,
                ),
                notice = null,
            ),
        )
    }

    fun requestRevokeAppSession(sessionId: String) {
        val session = state.value.appSessions.firstOrNull { it.sessionId == sessionId }
        setState(
            state.value.copy(
                confirmation = AccountConfirmation(
                    action = AccountAction.RevokeAppSession(sessionId),
                    instanceId = state.value.instanceId,
                    accountId = state.value.status?.accountId,
                    scopeKind = AccountScopeKind.AppSession,
                    targetLabel = session?.deviceLabel ?: sessionId,
                ),
                notice = null,
            ),
        )
    }

    fun requestRevokeAllAppSessions() {
        setState(
            state.value.copy(
                confirmation = AccountConfirmation(
                    action = AccountAction.RevokeAllAppSessions,
                    instanceId = state.value.instanceId,
                    accountId = state.value.status?.accountId,
                    scopeKind = AccountScopeKind.AllAppSessions,
                ),
                notice = null,
            ),
        )
    }

    fun requestRevokeCurrentDevice() {
        setState(
            state.value.copy(
                confirmation = AccountConfirmation(
                    action = AccountAction.RevokeCurrentDevice,
                    instanceId = state.value.instanceId,
                    accountId = state.value.status?.accountId,
                    scopeKind = AccountScopeKind.CurrentDevice,
                ),
                notice = null,
            ),
        )
    }

    fun requestRevokeCloudHostGrant(hostId: HostId) {
        setState(
            state.value.copy(
                confirmation = AccountConfirmation(
                    action = AccountAction.RevokeCloudHostGrant(hostId),
                    instanceId = state.value.instanceId,
                    accountId = state.value.status?.accountId,
                    scopeKind = AccountScopeKind.CloudHostGrant,
                    targetLabel = hostId.value,
                ),
                notice = null,
            ),
        )
    }

    fun requestRevokeStandaloneDevice(deviceId: String) {
        setState(
            state.value.copy(
                confirmation = AccountConfirmation(
                    action = AccountAction.RevokeStandaloneDevice(deviceId),
                    instanceId = state.value.instanceId,
                    accountId = null,
                    scopeKind = AccountScopeKind.StandaloneDevice,
                    targetLabel = deviceId,
                ),
                notice = null,
            ),
        )
    }

    fun requestForget(mode: ForgetMode) {
        setState(
            state.value.copy(
                confirmation = AccountConfirmation(
                    action = AccountAction.Forget(mode),
                    instanceId = state.value.instanceId,
                    accountId = state.value.status?.accountId,
                    scopeKind = if (mode == ForgetMode.LocalOnly) {
                        AccountScopeKind.LocalIdentityWithRemoteAuthority
                    } else {
                        AccountScopeKind.LocalIdentity
                    },
                    targetLabel = state.value.status?.accountId,
                ),
                notice = null,
            ),
        )
    }

    /** A cancelled destructive action leaves the port untouched. */
    fun cancelConfirmation() {
        setState(
            state.value.copy(
                confirmation = null,
                notice = AccountNotice.ActionCancelled,
            ),
        )
    }

    suspend fun confirm() {
        val confirmation = state.value.confirmation ?: return
        setState(state.value.copy(confirmation = null, loading = true))
        try {
            when (val action = confirmation.action) {
                AccountAction.SignOut -> performSignOut()
                is AccountAction.RevokeAppSession -> performRevocation(
                    scope = RevocationScope.AppSession(action.sessionId),
                    outcome = port.revokeAppSession(state.value.instanceId, action.sessionId),
                    removedSessionId = action.sessionId,
                )
                AccountAction.RevokeAllAppSessions -> performRevocation(
                    scope = RevocationScope.AllAppSessions,
                    outcome = port.revokeAppSession(state.value.instanceId, null),
                )
                AccountAction.RevokeCurrentDevice -> performRevocation(
                    scope = RevocationScope.CurrentDevice,
                    outcome = port.revokeCurrentDevice(state.value.instanceId),
                )
                is AccountAction.RevokeCloudHostGrant -> performRevocation(
                    scope = RevocationScope.CloudHostGrant(action.hostId),
                    outcome = port.revokeCloudHostGrant(
                        state.value.instanceId,
                        action.hostId,
                    ),
                )
                is AccountAction.RevokeStandaloneDevice -> performRevocation(
                    scope = RevocationScope.StandaloneDevice(action.deviceId),
                    outcome = port.revokeStandaloneDevice(
                        state.value.instanceId,
                        action.deviceId,
                    ),
                )
                is AccountAction.Forget -> performForget(action.mode)
            }
        } finally {
            setState(state.value.copy(loading = false))
        }
    }

    private suspend fun performSignOut() {
        val connection = state.value.status?.connection ?: AccountConnection.Online
        when (val outcome = port.signOut(state.value.instanceId, connection)) {
            is Outcome.Success -> {
                val reported = outcome.value
                val remotePending = reported.remotePending || connection == AccountConnection.Offline
                val report = reported.copy(remotePending = remotePending)
                val base = state.value.status ?: AccountStatus(
                    state = CloudAccountState.SignedIn,
                    connection = connection,
                )
                setState(
                    state.value.copy(
                        status = base.copy(
                            state = if (remotePending) {
                                CloudAccountState.PendingRemoteSignOut
                            } else {
                                CloudAccountState.SignedOut
                            },
                            connection = connection,
                        ),
                        appSessions = emptyList(),
                        report = AccountReport.SignedOut(report),
                        notice = if (remotePending) {
                            AccountNotice.SignOutPendingRemote()
                        } else {
                            null
                        },
                    ),
                )
                emit(
                    if (remotePending) DiagnosticLevel.Warn else DiagnosticLevel.Info,
                    if (remotePending) {
                        DiagnosticEvents.ACCOUNT_SIGN_OUT_PENDING
                    } else {
                        DiagnosticEvents.ACCOUNT_SIGNED_OUT
                    },
                    DiagnosticStages.LOGIN,
                )
            }
            is Outcome.Failure -> setNotice(AccountNotice.ActionFailed(outcome.failure))
            is Outcome.Unconfirmed -> setNotice(AccountNotice.ActionUnconfirmed(outcome.failure))
            is Outcome.Cancelled -> setNotice(AccountNotice.ActionCancelled)
        }
    }

    private suspend fun performRevocation(
        scope: RevocationScope,
        outcome: Outcome<RevocationReport>,
        removedSessionId: String? = null,
    ) {
        when (outcome) {
            is Outcome.Success -> {
                val report = outcome.value
                val remaining = if (scope is RevocationScope.AllAppSessions) {
                    emptyList()
                } else if (removedSessionId != null) {
                    state.value.appSessions.filterNot { it.sessionId == removedSessionId }
                } else {
                    state.value.appSessions
                }
                val remainingTargets = when (scope) {
                    is RevocationScope.CloudHostGrant -> state.value.targets.copy(
                        cloudHostGrants = state.value.targets.cloudHostGrants
                            .filterNot { it == scope.hostId },
                    )
                    is RevocationScope.StandaloneDevice -> state.value.targets.copy(
                        standaloneDevices = state.value.targets.standaloneDevices
                            .filterNot { it == scope.deviceId },
                    )
                    RevocationScope.CurrentDevice -> AccountRevocableTargets()
                    else -> state.value.targets
                }
                setState(
                    state.value.copy(
                        appSessions = remaining,
                        targets = remainingTargets,
                        report = AccountReport.Revoked(report),
                        notice = if (report.retention == RetentionCertainty.Uncertain) {
                            AccountNotice.RevocationRetentionUncertain(
                                scope = report.scope,
                                code = report.retentionCode,
                            )
                        } else {
                            null
                        },
                    ),
                )
                emit(
                    if (report.retention == RetentionCertainty.Uncertain) {
                        DiagnosticLevel.Warn
                    } else {
                        DiagnosticLevel.Info
                    },
                    when (scope) {
                        is RevocationScope.AppSession ->
                            DiagnosticEvents.ACCOUNT_SESSION_REVOKED
                        RevocationScope.AllAppSessions ->
                            DiagnosticEvents.ACCOUNT_SESSIONS_REVOKED_ALL
                        RevocationScope.CurrentDevice ->
                            DiagnosticEvents.ACCOUNT_CURRENT_DEVICE_REVOKED
                        is RevocationScope.CloudHostGrant ->
                            DiagnosticEvents.ACCOUNT_GRANT_REVOKED
                        is RevocationScope.StandaloneDevice ->
                            DiagnosticEvents.ACCOUNT_DEVICE_REVOKED
                    },
                    DiagnosticStages.LOGIN,
                    code = report.retentionCode,
                )
            }
            is Outcome.Failure -> setNotice(AccountNotice.ActionFailed(outcome.failure))
            is Outcome.Unconfirmed -> setNotice(AccountNotice.ActionUnconfirmed(outcome.failure))
            is Outcome.Cancelled -> setNotice(AccountNotice.ActionCancelled)
        }
    }

    private suspend fun performForget(mode: ForgetMode) {
        when (val outcome = port.forget(state.value.instanceId, mode)) {
            is Outcome.Success -> {
                val report = outcome.value
                if (!report.forgotten) {
                    setNotice(
                        AccountNotice.ActionFailed(
                            CoreFailure(
                                code = "forget_not_applied",
                                message = "The local identity was not removed.",
                            ),
                        ),
                    )
                    return
                }
                setState(
                    state.value.copy(
                        status = AccountStatus(
                            state = CloudAccountState.SignedOut,
                            accountId = null,
                            workspace = null,
                            connection = state.value.status?.connection
                                ?: AccountConnection.Online,
                        ),
                        appSessions = emptyList(),
                        targets = AccountRevocableTargets(),
                        report = AccountReport.Forgot(report),
                        notice = if (mode == ForgetMode.LocalOnly) {
                            AccountNotice.ForgottenLocalOnly(report.remoteAuthorityRetained)
                        } else {
                            null
                        },
                    ),
                )
                emit(
                    DiagnosticLevel.Info,
                    DiagnosticEvents.PROFILE_FORGOTTEN,
                    DiagnosticStages.PROFILE,
                    fields = mapOf("scope" to mode.name.lowercase()),
                )
            }
            is Outcome.Failure -> setNotice(AccountNotice.ActionFailed(outcome.failure))
            is Outcome.Unconfirmed -> setNotice(AccountNotice.ActionUnconfirmed(outcome.failure))
            is Outcome.Cancelled -> setNotice(AccountNotice.ActionCancelled)
        }
    }

    private fun setNotice(notice: AccountNotice) {
        setState(state.value.copy(notice = notice))
    }

    private fun emit(
        level: DiagnosticLevel,
        event: String,
        stage: String,
        code: String? = null,
        fields: Map<String, String> = emptyMap(),
    ) {
        DiagnosticDelivery.deliver(
            diagnostics,
            DiagnosticEvent(
                level = level,
                event = event,
                stage = stage,
                code = code,
                instanceId = state.value.instanceId.value,
                actionId = "account-${clock.epochMillis()}",
                fields = fields,
            ),
        )
    }
}
