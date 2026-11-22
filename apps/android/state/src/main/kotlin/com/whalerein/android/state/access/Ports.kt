package com.whalerein.android.state.access

import com.whalerein.android.state.core.AttemptId
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome

/**
 * Typed ports implemented later by `:core-bridge` over the shared Core.
 *
 * Expected failures travel in [Outcome] or the dedicated sealed result types;
 * no port throws a domain failure across this boundary. These interfaces are
 * deliberately small so a test can script one interaction at a time.
 */

sealed interface CloudAuthResult {
    data class Success(val session: CloudSession) : CloudAuthResult

    data class Rejected(val code: String) : CloudAuthResult

    data class Unconfirmed(val failure: CoreFailure) :
        CloudAuthResult

    data object Cancelled : CloudAuthResult

    data object Expired : CloudAuthResult
}

interface CloudAuthPort {
    /** Returns the durable pending attempt for process recreation, if any. */
    suspend fun pendingAttempt(instance: InstanceId): Outcome<CloudAttempt?>

    /** Creates and persists one native PKCE attempt before the browser opens. */
    suspend fun start(instance: InstanceId, environment: String): Outcome<CloudAttempt>

    /** Completes the callback exactly once for the supplied attempt. */
    suspend fun complete(
        instance: InstanceId,
        attemptId: AttemptId,
        callbackUri: String,
    ): CloudAuthResult

    /** Looks up the existing session with the original attempt credential. */
    suspend fun recoverLostExchange(instance: InstanceId, attemptId: AttemptId): CloudAuthResult

    suspend fun cancel(instance: InstanceId, attemptId: AttemptId): Outcome<Unit>

    suspend fun currentSession(instance: InstanceId): Outcome<CloudSession?>
}

interface ProfilePort {
    suspend fun list(): Outcome<List<Profile>>

    suspend fun active(): Outcome<Profile?>

    suspend fun validate(profile: Profile): Outcome<ProfileValidation>

    suspend fun save(profile: Profile): Outcome<Profile>

    /** Switches the active Core instance. The previous profile survives failure. */
    suspend fun activate(instance: InstanceId): Outcome<ProfileActivation>

    suspend fun remove(instance: InstanceId): Outcome<Unit>

    suspend fun credentialState(instance: InstanceId): CredentialState

    suspend fun storageScope(instance: InstanceId): Outcome<ProfileStorageScope>
}

interface DirectoryPort {
    suspend fun list(instance: InstanceId): Outcome<List<DirectoryHostView>>

    suspend fun connect(instance: InstanceId, hostId: HostId): Outcome<HostConnection>

    suspend fun selectDefault(instance: InstanceId, hostId: HostId): Outcome<HostSelection>

    suspend fun currentSession(instance: InstanceId): Outcome<CloudSession?>
}

interface PairingPort {
    suspend fun reviewInvitation(encoded: String): Outcome<StandaloneInvitation>

    suspend fun checkFingerprint(instance: InstanceId, presented: String): Outcome<FingerprintCheck>

    suspend fun importCa(instance: InstanceId, pem: String): Outcome<CaImportState>

    suspend fun decide(instance: InstanceId, accept: Boolean): Outcome<PairingResult>

    suspend fun lanPermission(): LanPermissionState

    suspend fun requestLanPermission(): LanPermissionState

    suspend fun validateTarget(
        instance: InstanceId,
        target: StandaloneTarget,
    ): Outcome<TransportValidation>
}

interface StartupPort {
    /** Reads the durable last target and Core's restore evidence. */
    suspend fun restore(instance: InstanceId): Outcome<StartupSnapshot>
}

interface ExternalEntryPort {
    suspend fun currentProfile(): Outcome<Profile?>

    /** Authorizes the hint against the current profile, Host and session. */
    suspend fun authorize(target: ExternalTarget): ExternalAuthorization
}

/**
 * Debug-build control for the official Cloud environment of the active
 * profile. Release builds never surface it.
 */
interface DebugCloudEnvironmentPort {
    fun current(): String

    suspend fun apply(environment: String, cloudCaPem: String?): com.whalerein.android.state.core.Outcome<Unit>
}

/** Composition-root marker implemented only by the real bridge. */
interface DebugEnvironmentAware {
    val debugCloudEnvironment: DebugCloudEnvironmentPort
}
