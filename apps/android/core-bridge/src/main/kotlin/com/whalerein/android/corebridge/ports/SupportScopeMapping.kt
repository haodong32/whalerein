package com.whalerein.android.corebridge.ports

import com.whalerein.android.corebridge.AndroidCore
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.support.ForgetMode
import com.whalerein.android.state.support.ForgetReport
import com.whalerein.android.state.support.RetentionCertainty
import com.whalerein.android.state.support.RevocationReport
import com.whalerein.android.state.support.RevocationScope

/**
 * Core operations selected by one account confirmation.
 *
 * The account screen names an exact target. Each target maps to exactly one
 * Core operation so a confirmation for Host A or Device A can never revoke the
 * current device, Host B, or an unrelated grant.
 */
internal sealed interface RevocationCommand {
    data object CurrentDevice : RevocationCommand

    data class CloudHostGrant(val hostId: HostId) : RevocationCommand

    data class StandaloneDevice(val deviceId: String) : RevocationCommand

    data class CloudAppSession(val sessionId: String) : RevocationCommand

    data object AllCloudAppSessions : RevocationCommand
}

/** Core's two distinct forget modes. */
internal enum class ForgetCommand { Ordinal, LocalOnly }

internal object SupportScopeMapping {
    fun revocation(scope: RevocationScope): RevocationCommand = when (scope) {
        RevocationScope.CurrentDevice -> RevocationCommand.CurrentDevice
        is RevocationScope.CloudHostGrant -> RevocationCommand.CloudHostGrant(scope.hostId)
        is RevocationScope.StandaloneDevice -> RevocationCommand.StandaloneDevice(scope.deviceId)
        is RevocationScope.AppSession -> RevocationCommand.CloudAppSession(scope.sessionId)
        RevocationScope.AllAppSessions -> RevocationCommand.AllCloudAppSessions
    }

    fun forget(mode: ForgetMode): ForgetCommand = when (mode) {
        ForgetMode.Ordinary -> ForgetCommand.Ordinal
        ForgetMode.LocalOnly -> ForgetCommand.LocalOnly
    }
}

/** Result of one scoped Core revocation. */
internal data class RevocationResult(
    val retention: RetentionCertainty = RetentionCertainty.Certain,
    val code: String? = null,
)

/**
 * Narrow seam over Core's scoped device operations.
 *
 * Implementations must revoke exactly the requested target. Returning a typed
 * failure is always preferable to revoking a different device or Host.
 */
internal interface ScopedRevocationBackend {
    suspend fun revokeCurrentDevice(): Outcome<RevocationResult>

    suspend fun revokeCloudHostGrant(hostId: HostId): Outcome<RevocationResult>

    suspend fun revokeStandaloneDevice(deviceId: String): Outcome<RevocationResult>

    suspend fun forget(mode: ForgetMode): Outcome<Unit>
}

/** Production backend over the generated Core handle. */
internal class CoreScopedRevocationBackend(
    private val owner: AndroidCore,
) : ScopedRevocationBackend {
    override suspend fun revokeCurrentDevice(): Outcome<RevocationResult> = suspendOutcome {
        owner.core().revoke()
        RevocationResult()
    }

    override suspend fun revokeCloudHostGrant(hostId: HostId): Outcome<RevocationResult> =
        suspendOutcome {
            val state = owner.core().revokeCloudHost(hostId.value)
            RevocationResult(
                retention = if (state == "revoked") {
                    RetentionCertainty.Certain
                } else {
                    RetentionCertainty.Uncertain
                },
                code = state,
            )
        }

    override suspend fun revokeStandaloneDevice(
        deviceId: String,
    ): Outcome<RevocationResult> = Outcome.Failure(
        CoreFailure(
            code = "revocation_scope_unavailable",
            message = "That Standalone device cannot be revoked from this client.",
        ),
    )

    override suspend fun forget(mode: ForgetMode): Outcome<Unit> = suspendOutcome {
        when (SupportScopeMapping.forget(mode)) {
            ForgetCommand.Ordinal -> owner.core().forgetDevice()
            ForgetCommand.LocalOnly -> owner.core().forgetLocalDevice()
        }
    }
}

/**
 * Maps one confirmed scope to exactly one backend operation and report.
 *
 * Kept free of Android and generated Core types so the mapping is exercised by
 * JVM tests with a recording backend.
 */
internal class ScopedAccountRevoker(private val backend: ScopedRevocationBackend) {
    suspend fun revoke(scope: RevocationScope): Outcome<RevocationReport> =
        when (val command = SupportScopeMapping.revocation(scope)) {
            RevocationCommand.CurrentDevice -> map(
                scope,
                backend.revokeCurrentDevice(),
            )
            is RevocationCommand.CloudHostGrant -> map(
                scope,
                backend.revokeCloudHostGrant(command.hostId),
            )
            is RevocationCommand.StandaloneDevice -> map(
                scope,
                backend.revokeStandaloneDevice(command.deviceId),
            )
            is RevocationCommand.CloudAppSession,
            RevocationCommand.AllCloudAppSessions,
            -> Outcome.Failure(
                CoreFailure(
                    code = "revocation_scope_mismatch",
                    message = "App sessions use the Cloud session operation.",
                ),
            )
        }

    suspend fun forget(mode: ForgetMode): Outcome<ForgetReport> =
        when (val outcome = backend.forget(mode)) {
            is Outcome.Success -> Outcome.Success(
                ForgetReport(
                    mode = mode,
                    forgotten = true,
                    remoteAuthorityRetained = mode == ForgetMode.LocalOnly,
                ),
            )
            is Outcome.Failure -> outcome
            is Outcome.Unconfirmed -> outcome
            is Outcome.Cancelled -> outcome
        }

    private fun map(
        scope: RevocationScope,
        outcome: Outcome<RevocationResult>,
    ): Outcome<RevocationReport> = when (outcome) {
        is Outcome.Success -> Outcome.Success(
            RevocationReport(
                scope = scope,
                retention = outcome.value.retention,
                retentionCode = outcome.value.code,
            ),
        )
        is Outcome.Failure -> outcome
        is Outcome.Unconfirmed -> outcome
        is Outcome.Cancelled -> outcome
    }
}

/** One Core Host row reduced to the fields that decide a revocable grant. */
internal data class HostGrantCandidate(
    val hostId: String,
    val deviceAccess: String,
)

/**
 * Selects only the targets Core can actually revoke.
 *
 * Cloud Host grants are revocable one Host at a time. Standalone has no
 * per-device Core operation, so it never advertises a device row that could only
 * fail; the current-device action remains available separately.
 */
internal object RevocableTargetPolicy {
    const val CLOUD_SERVICE: String = "whalerein_cloud"
    const val DEVICE_ACCESS_READY: String = "ready"

    fun targets(
        service: String?,
        hosts: List<HostGrantCandidate>,
    ): com.whalerein.android.state.support.AccountRevocableTargets =
        if (service == CLOUD_SERVICE) {
            com.whalerein.android.state.support.AccountRevocableTargets(
                cloudHostGrants = hosts
                    .filter { it.deviceAccess == DEVICE_ACCESS_READY }
                    .map { HostId(it.hostId) },
            )
        } else {
            com.whalerein.android.state.support.AccountRevocableTargets()
        }
}
