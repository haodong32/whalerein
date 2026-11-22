package com.whalerein.android.state.fixtures.access

import com.whalerein.android.state.access.CaImportState
import com.whalerein.android.state.access.CloudAttempt
import com.whalerein.android.state.access.CloudAuthPort
import com.whalerein.android.state.access.CloudAuthResult
import com.whalerein.android.state.access.CloudSession
import com.whalerein.android.state.access.DirectoryHostView
import com.whalerein.android.state.access.DirectoryPort
import com.whalerein.android.state.access.ExternalAuthorization
import com.whalerein.android.state.access.ExternalEntryPort
import com.whalerein.android.state.access.ExternalTarget
import com.whalerein.android.state.access.FingerprintCheck
import com.whalerein.android.state.access.HostAvailability
import com.whalerein.android.state.access.HostConnection
import com.whalerein.android.state.access.HostSelection
import com.whalerein.android.state.access.LanPermissionState
import com.whalerein.android.state.access.PairingPort
import com.whalerein.android.state.access.PairingResult
import com.whalerein.android.state.access.Profile
import com.whalerein.android.state.access.ProfileActivation
import com.whalerein.android.state.access.ProfilePort
import com.whalerein.android.state.access.ProfileStorageScope
import com.whalerein.android.state.access.ProfileValidation
import com.whalerein.android.state.access.CredentialState
import com.whalerein.android.state.access.StartupPort
import com.whalerein.android.state.access.StartupSnapshot
import com.whalerein.android.state.access.StandaloneInvitation
import com.whalerein.android.state.access.StandaloneTarget
import com.whalerein.android.state.access.TransportValidation
import com.whalerein.android.state.core.AttemptId
import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome

class FakeClock(
    var nowMillis: Long = 1_000_000L,
    var elapsed: Long = 0L,
) : Clock {
    override fun epochMillis(): Long = nowMillis

    override fun elapsedMillis(): Long = elapsed
}

class RecordingDiagnosticSink : DiagnosticSink {
    private val recorded = mutableListOf<DiagnosticEvent>()

    val events: List<DiagnosticEvent> get() = recorded.toList()

    override fun emit(event: DiagnosticEvent) {
        recorded += event
    }

    fun clear() {
        recorded.clear()
    }
}

fun cloudSession(
    instance: InstanceId = InstanceId("instance-1"),
    account: String = "account-1",
    workspace: String = "workspace-1",
    environment: String = "local",
): CloudSession = CloudSession(instance, account, workspace, environment)

fun cloudAttempt(
    instance: InstanceId = InstanceId("instance-1"),
    environment: String = "local",
    redirect: String = "https://app.whalerein.test/client/android/callback",
    state: String = "state-1",
    id: String = "attempt-1",
    expiresAt: Long = Long.MAX_VALUE,
    expectedAccountId: String? = null,
    expectedWorkspaceId: String? = null,
): CloudAttempt = CloudAttempt(
    attemptId = AttemptId(id),
    authorizationUrl = "https://auth.whalerein.test/oidc/auth?state=$state",
    state = state,
    redirectUri = redirect,
    environment = environment,
    expiresAtEpochMillis = expiresAt,
    expectedAccountId = expectedAccountId,
    expectedWorkspaceId = expectedWorkspaceId,
)

class FakeCloudAuthPort : CloudAuthPort {
    override fun androidRedirectUri(environment: String): String = when (environment) {
        "staging" -> "https://app.staging.whalerein.com/client/android/callback"
        "production" -> "https://app.whalerein.com/client/android/callback"
        else -> "https://app.whalerein.test/client/android/callback"
    }

    var pending: CloudAttempt? = null
    var startResult: Outcome<CloudAttempt>? = null
    var startException: Exception? = null
    var currentSessionResult: Outcome<CloudSession?> = Outcome.Success(null)
    var completeResults: ArrayDeque<CloudAuthResult> = ArrayDeque()
    var recoverResults: ArrayDeque<CloudAuthResult> = ArrayDeque()
    var completeException: Exception? = null
    var recoverException: Exception? = null
    var nextAttempt: (InstanceId, String) -> CloudAttempt = { instance, environment ->
        cloudAttempt(
            instance = instance,
            environment = environment,
            state = "state-$attemptsStarted",
            id = "attempt-$attemptsStarted",
        )
    }

    val startCalls: MutableList<InstanceId> = mutableListOf()
    val completedCallbacks: MutableList<String> = mutableListOf()
    val recoveredAttempts: MutableList<AttemptId> = mutableListOf()
    val cancelledAttempts: MutableList<AttemptId> = mutableListOf()

    private var attemptsStarted = 0

    fun enqueueComplete(result: CloudAuthResult) {
        completeResults.addLast(result)
    }

    fun enqueueRecover(result: CloudAuthResult) {
        recoverResults.addLast(result)
    }

    override suspend fun pendingAttempt(instance: InstanceId): Outcome<CloudAttempt?> =
        Outcome.Success(pending)

    override suspend fun start(instance: InstanceId, environment: String): Outcome<CloudAttempt> {
        startCalls += instance
        startException?.let { throw it }
        startResult?.let { return it }
        attemptsStarted += 1
        val attempt = nextAttempt(instance, environment)
        pending = attempt
        return Outcome.Success(attempt)
    }

    override suspend fun complete(
        instance: InstanceId,
        attemptId: AttemptId,
        callbackUri: String,
    ): CloudAuthResult {
        completedCallbacks += callbackUri
        completeException?.let { throw it }
        return completeResults.removeFirstOrNull() ?: CloudAuthResult.Unconfirmed(
            CoreFailure("exchange_unconfirmed", "unconfirmed", true),
        )
    }

    override suspend fun recoverLostExchange(
        instance: InstanceId,
        attemptId: AttemptId,
    ): CloudAuthResult {
        recoveredAttempts += attemptId
        recoverException?.let { throw it }
        return recoverResults.removeFirstOrNull() ?: CloudAuthResult.Unconfirmed(
            CoreFailure("recovery_unconfirmed", "unconfirmed", true),
        )
    }

    override suspend fun cancel(instance: InstanceId, attemptId: AttemptId): Outcome<Unit> {
        cancelledAttempts += attemptId
        if (pending?.attemptId == attemptId) pending = null
        return Outcome.Success(Unit)
    }

    override suspend fun currentSession(instance: InstanceId): Outcome<CloudSession?> =
        currentSessionResult
}

class FakeProfilePort : ProfilePort {
    val listedProfiles: MutableList<Profile> = mutableListOf()
    var activeProfile: Profile? = null
    var listResult: Outcome<List<Profile>>? = null
    var activeResult: Outcome<Profile?>? = null
    var validateResult: (Profile) -> Outcome<ProfileValidation> = {
        Outcome.Success(ProfileValidation.Valid(it))
    }
    var activationResults: ArrayDeque<Outcome<ProfileActivation>> = ArrayDeque()
    var saveResult: Outcome<Profile>? = null
    var removeResult: Outcome<Unit>? = null
    var storageScopeResult: Outcome<ProfileStorageScope>? = null
    var credentialException: Exception? = null
    var credentialStates: MutableMap<InstanceId, CredentialState> = mutableMapOf()
    var scopes: MutableMap<InstanceId, ProfileStorageScope> = mutableMapOf()
    val activationCalls: MutableList<InstanceId> = mutableListOf()
    val removeCalls: MutableList<InstanceId> = mutableListOf()

    override suspend fun list(): Outcome<List<Profile>> =
        listResult ?: Outcome.Success(listedProfiles.toList())

    override suspend fun active(): Outcome<Profile?> =
        activeResult ?: Outcome.Success(activeProfile)

    override suspend fun validate(profile: Profile): Outcome<ProfileValidation> =
        validateResult(profile)

    override suspend fun save(profile: Profile): Outcome<Profile> {
        saveResult?.let { return it }
        listedProfiles.removeAll { it.instance == profile.instance }
        listedProfiles += profile
        return Outcome.Success(profile)
    }

    override suspend fun activate(instance: InstanceId): Outcome<ProfileActivation> {
        activationCalls += instance
        activationResults.removeFirstOrNull()?.let { return it }
        val target = listedProfiles.firstOrNull { it.instance == instance }
            ?: return Outcome.Failure(CoreFailure("profile_missing", "missing", false))
        val previous = activeProfile
        activeProfile = target.copy(active = true)
        listedProfiles.replaceAll { profile ->
            if (profile.instance == instance) profile.copy(active = true) else profile.copy(active = false)
        }
        return Outcome.Success(ProfileActivation(active = target.copy(active = true), previous = previous))
    }

    override suspend fun remove(instance: InstanceId): Outcome<Unit> {
        removeCalls += instance
        removeResult?.let { return it }
        listedProfiles.removeAll { it.instance == instance }
        if (activeProfile?.instance == instance) activeProfile = null
        credentialStates.remove(instance)
        scopes.remove(instance)
        return Outcome.Success(Unit)
    }

    override suspend fun credentialState(instance: InstanceId): CredentialState {
        credentialException?.let { throw it }
        return credentialStates[instance] ?: CredentialState.Present
    }

    override suspend fun storageScope(instance: InstanceId): Outcome<ProfileStorageScope> {
        storageScopeResult?.let { return it }
        return Outcome.Success(scopes[instance] ?: ProfileStorageScope(instance, backupExcluded = true))
    }
}

class FakeDirectoryPort : DirectoryPort {
    val listedHosts: MutableList<DirectoryHostView> = mutableListOf()
    var listResult: Outcome<List<DirectoryHostView>>? = null
    var connectResults: MutableMap<HostId, Outcome<HostConnection>> = mutableMapOf()
    var selectResults: MutableMap<HostId, Outcome<HostSelection>> = mutableMapOf()
    var currentSessionResult: Outcome<CloudSession?> = Outcome.Success(null)
    val connectCalls: MutableList<HostId> = mutableListOf()
    val selectCalls: MutableList<HostId> = mutableListOf()

    override suspend fun list(instance: InstanceId): Outcome<List<DirectoryHostView>> =
        listResult ?: Outcome.Success(listedHosts.toList())

    override suspend fun connect(instance: InstanceId, hostId: HostId): Outcome<HostConnection> {
        connectCalls += hostId
        connectResults[hostId]?.let { return it }
        val host = listedHosts.firstOrNull { it.hostId == hostId }
            ?: return Outcome.Failure(CoreFailure("host_unknown", "unknown", false))
        return Outcome.Success(HostConnection(host, cloudSession(instance = instance)))
    }

    override suspend fun selectDefault(
        instance: InstanceId,
        hostId: HostId,
    ): Outcome<HostSelection> {
        selectCalls += hostId
        selectResults[hostId]?.let { return it }
        return Outcome.Success(HostSelection(hostId))
    }

    override suspend fun currentSession(instance: InstanceId): Outcome<CloudSession?> =
        currentSessionResult
}

class FakePairingPort : PairingPort {
    var invitationResult: Outcome<StandaloneInvitation> = Outcome.Success(
        StandaloneInvitation("Desk Host", "AA:BB:CC", caRequired = false),
    )
    var fingerprintResult: Outcome<FingerprintCheck> = Outcome.Success(FingerprintCheck.Match)
    var importCaResult: Outcome<CaImportState> = Outcome.Success(CaImportState.NotRequired)
    var decideResult: (InstanceId) -> Outcome<PairingResult> = { instance ->
        Outcome.Success(PairingResult.Success(HostRef(instance, HostId("standalone-host"))))
    }
    var lanState: LanPermissionState = LanPermissionState.Unknown
    var requestLanState: LanPermissionState = LanPermissionState.Granted
    var validateTargetResult: Outcome<TransportValidation> = Outcome.Success(TransportValidation.Valid)
    var lanPermissionException: Exception? = null
    var requestLanPermissionException: Exception? = null
    var lanQueries: Int = 0
    var lanRequests: Int = 0
    val importedPems: MutableList<String> = mutableListOf()
    val decidedAccepts: MutableList<Boolean> = mutableListOf()

    override suspend fun reviewInvitation(encoded: String): Outcome<StandaloneInvitation> =
        invitationResult

    override suspend fun checkFingerprint(
        instance: InstanceId,
        presented: String,
    ): Outcome<FingerprintCheck> = fingerprintResult

    override suspend fun importCa(instance: InstanceId, pem: String): Outcome<CaImportState> {
        importedPems += pem
        return importCaResult
    }

    override suspend fun decide(instance: InstanceId, accept: Boolean): Outcome<PairingResult> {
        decidedAccepts += accept
        return decideResult(instance)
    }

    override suspend fun lanPermission(): LanPermissionState {
        lanQueries += 1
        lanPermissionException?.let { throw it }
        return lanState
    }

    override suspend fun requestLanPermission(): LanPermissionState {
        lanRequests += 1
        requestLanPermissionException?.let { throw it }
        lanState = requestLanState
        return requestLanState
    }

    override suspend fun validateTarget(
        instance: InstanceId,
        target: StandaloneTarget,
    ): Outcome<TransportValidation> = validateTargetResult
}

class FakeStartupPort : StartupPort {
    var snapshot: Outcome<StartupSnapshot> = Outcome.Success(StartupSnapshot())

    override suspend fun restore(instance: InstanceId): Outcome<StartupSnapshot> = snapshot
}

class FakeExternalEntryPort : ExternalEntryPort {
    var currentProfileResult: Outcome<Profile?> = Outcome.Success(null)
    var currentProfileException: Exception? = null
    var authorizeException: Exception? = null
    var authorizeResult: (ExternalTarget) -> ExternalAuthorization = {
        ExternalAuthorization.Unauthorized
    }
    val authorizedTargets: MutableList<ExternalTarget> = mutableListOf()

    override suspend fun currentProfile(): Outcome<Profile?> {
        currentProfileException?.let { throw it }
        return currentProfileResult
    }

    override suspend fun authorize(target: ExternalTarget): ExternalAuthorization {
        authorizedTargets += target
        authorizeException?.let { throw it }
        return authorizeResult(target)
    }
}

fun hostView(
    hostId: String,
    label: String = hostId,
    online: Boolean = true,
    availability: HostAvailability = HostAvailability.Ready,
    connectable: Boolean = availability == HostAvailability.Ready && online,
): DirectoryHostView = DirectoryHostView(
    hostId = HostId(hostId),
    label = label,
    online = online,
    availability = availability,
    connectable = connectable,
)

fun profile(
    instance: String,
    label: String = instance,
    kind: com.whalerein.android.state.core.InstanceKind =
        com.whalerein.android.state.core.InstanceKind.Cloud,
    active: Boolean = false,
): Profile = Profile(
    instance = InstanceId(instance),
    label = label,
    kind = kind,
    active = active,
    backupExcluded = true,
)
