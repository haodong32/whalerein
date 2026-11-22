package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome

enum class AccountConnection { Online, Offline }

enum class CloudAccountState {
    SignedIn,
    SignedOut,
    /** Local credentials are gone; the remote sign-out is still outstanding. */
    PendingRemoteSignOut,
}

data class AccountStatus(
    val state: CloudAccountState,
    val accountId: String? = null,
    val workspace: String? = null,
    val connection: AccountConnection = AccountConnection.Online,
) {
    val signedIn: Boolean get() = state != CloudAccountState.SignedOut
}

data class AppSessionInfo(
    val sessionId: String,
    val deviceLabel: String,
    val createdAtEpochMillis: Long,
    val lastSeenEpochMillis: Long? = null,
    val current: Boolean = false,
)

enum class RetentionCertainty { Certain, Uncertain }

sealed interface RevocationScope {
    data class AppSession(val sessionId: String) : RevocationScope

    data object AllAppSessions : RevocationScope

    data class CloudHostGrant(val hostId: HostId) : RevocationScope

    data class StandaloneDevice(val deviceId: String) : RevocationScope
}

data class RevocationReport(
    val scope: RevocationScope,
    val retention: RetentionCertainty = RetentionCertainty.Certain,
    /** Stable, redaction-safe reason when [retention] is [RetentionCertainty.Uncertain]. */
    val retentionCode: String? = null,
)

enum class ForgetMode { Ordinary, LocalOnly }

data class ForgetReport(
    val mode: ForgetMode,
    val forgotten: Boolean,
    /** A local-only forget leaves the remote Cloud grant or device record. */
    val remoteAuthorityRetained: Boolean,
)

data class SignOutReport(
    val localSignedOut: Boolean,
    val remotePending: Boolean,
    val connection: AccountConnection,
)

/** Account/device operations. Expected failures are typed outcomes. */
interface AccountPort {
    suspend fun status(instance: InstanceId): Outcome<AccountStatus>

    suspend fun signOut(instance: InstanceId, connection: AccountConnection): Outcome<SignOutReport>

    suspend fun listAppSessions(instance: InstanceId): Outcome<List<AppSessionInfo>>

    /**
     * Revokes exactly one App Session when [sessionId] is non-null, or all
     * sessions for the instance when it is null.
     */
    suspend fun revokeAppSession(
        instance: InstanceId,
        sessionId: String?,
    ): Outcome<RevocationReport>

    suspend fun revokeCloudHostGrant(
        instance: InstanceId,
        hostId: HostId,
    ): Outcome<RevocationReport>

    suspend fun revokeStandaloneDevice(
        instance: InstanceId,
        deviceId: String,
    ): Outcome<RevocationReport>

    suspend fun forget(instance: InstanceId, mode: ForgetMode): Outcome<ForgetReport>
}

sealed interface AccountAction {
    data object SignOut : AccountAction

    data class RevokeAppSession(val sessionId: String) : AccountAction

    data object RevokeAllAppSessions : AccountAction

    data class RevokeCloudHostGrant(val hostId: HostId) : AccountAction

    data class RevokeStandaloneDevice(val deviceId: String) : AccountAction

    data class Forget(val mode: ForgetMode) : AccountAction
}

enum class AccountScopeKind {
    Account,
    AppSession,
    AllAppSessions,
    CloudHostGrant,
    StandaloneDevice,
    LocalIdentity,
    LocalIdentityWithRemoteAuthority,
}

data class AccountConfirmation(
    val action: AccountAction,
    val instanceId: InstanceId,
    val accountId: String?,
    val scopeKind: AccountScopeKind,
    val targetLabel: String? = null,
)

sealed interface AccountReport {
    data class SignedOut(val report: SignOutReport) : AccountReport

    data class Revoked(val report: RevocationReport) : AccountReport

    data class Forgot(val report: ForgetReport) : AccountReport
}

sealed interface AccountNotice {
    data class LoadFailed(val failure: CoreFailure) : AccountNotice

    data class SessionsFailed(val failure: CoreFailure) : AccountNotice

    data object ActionCancelled : AccountNotice

    data class SignOutPendingRemote(val failure: CoreFailure? = null) : AccountNotice

    data class RevocationRetentionUncertain(
        val scope: RevocationScope,
        val code: String?,
    ) : AccountNotice

    data class ForgottenLocalOnly(val remoteAuthorityRetained: Boolean) : AccountNotice

    data class ActionFailed(val failure: CoreFailure) : AccountNotice

    data class ActionUnconfirmed(val failure: CoreFailure) : AccountNotice
}

data class AccountUiState(
    val instanceId: InstanceId,
    val loading: Boolean = false,
    val status: AccountStatus? = null,
    val appSessions: List<AppSessionInfo> = emptyList(),
    val confirmation: AccountConfirmation? = null,
    val report: AccountReport? = null,
    val notice: AccountNotice? = null,
) {
    val signedIn: Boolean get() = status?.signedIn == true

    val pendingRemoteSignOut: Boolean
        get() = status?.state == CloudAccountState.PendingRemoteSignOut
}
