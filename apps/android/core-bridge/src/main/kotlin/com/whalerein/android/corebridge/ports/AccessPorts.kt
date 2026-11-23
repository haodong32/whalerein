package com.whalerein.android.corebridge.ports

import com.whalerein.android.corebridge.AndroidCore
import com.whalerein.android.corebridge.CloudAccountView
import com.whalerein.android.corebridge.InstanceInput
import com.whalerein.android.corebridge.InstanceView
import com.whalerein.android.corebridge.appConfigRemoveInstance
import com.whalerein.android.corebridge.appConfigSaveInstance
import com.whalerein.android.corebridge.appConfigSetActive
import com.whalerein.android.corebridge.appConfigValidateInstance
import com.whalerein.android.state.access.CaImportState
import com.whalerein.android.state.access.CloudAttempt
import com.whalerein.android.state.access.CloudAuthPort
import com.whalerein.android.state.access.CloudAuthResult
import com.whalerein.android.state.access.CloudSession
import com.whalerein.android.state.access.CredentialState
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
import com.whalerein.android.state.access.StandaloneInvitation
import com.whalerein.android.state.access.StandaloneTarget
import com.whalerein.android.state.access.StartupAuthorization
import com.whalerein.android.state.access.StartupPort
import com.whalerein.android.state.access.StartupSnapshot
import com.whalerein.android.state.access.TransportValidation
import com.whalerein.android.corebridge.secret.AndroidSecretStore
import com.whalerein.android.corebridge.SecretStoreException
import com.whalerein.android.state.core.AttemptId
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.InstanceKind
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.net.URI

internal suspend fun <T> onIo(block: () -> T): T = withContext(Dispatchers.IO) { block() }

internal fun profileFor(owner: AndroidCore, instance: InstanceView): Profile = Profile(
    instance = InstanceId(instance.id),
    label = instance.id,
    kind = if (instance.service == "standalone") InstanceKind.Standalone else InstanceKind.Cloud,
    active = owner.activeInstance.value == instance.id,
    backupExcluded = true,
)

internal fun CloudAccountView.toSession(instance: InstanceId): CloudSession = CloudSession(
    instance = instance,
    accountId = accountId,
    workspaceId = workspaceId,
    environment = "",
)

internal class CloudAuthAdapter(private val owner: AndroidCore) : CloudAuthPort {
    private val pending = mutableMapOf<String, CloudAttempt>()

    override fun androidRedirectUri(environment: String): String =
        com.whalerein.android.corebridge.androidRedirectUri(environment)

    override suspend fun pendingAttempt(instance: InstanceId): Outcome<CloudAttempt?> =
        Outcome.Success(pending[instance.value])

    override suspend fun start(instance: InstanceId, environment: String): Outcome<CloudAttempt> =
        suspendOutcome {
            val attempt = owner.core().cloudAuthStartAndroid()
            val state = runCatching {
                URI(attempt.url).rawQuery.orEmpty().split('&')
                    .firstOrNull { it.startsWith("state=") }
                    ?.substringAfter("state=")
            }.getOrNull() ?: ""
            val view = CloudAttempt(
                attemptId = AttemptId(attempt.attemptId),
                authorizationUrl = attempt.url,
                state = state,
                redirectUri = androidRedirectUri(environment),
                environment = environment,
                expiresAtEpochMillis = 0,
            )
            pending[instance.value] = view
            view
        }

    override suspend fun complete(
        instance: InstanceId,
        attemptId: AttemptId,
        callbackUri: String,
    ): CloudAuthResult = try {
        val expected = pending[instance.value]
        if (expected != null && expected.attemptId != attemptId) {
            CloudAuthResult.Rejected("attempt_mismatch")
        } else {
            val account = suspendOutcome { owner.core().cloudAuthComplete(callbackUri) }
            pending.remove(instance.value)
            when (account) {
                is Outcome.Success -> CloudAuthResult.Success(account.value.toSession(instance))
                is Outcome.Unconfirmed -> CloudAuthResult.Unconfirmed(account.failure)
                is Outcome.Failure -> CloudAuthResult.Rejected(account.failure.code)
                Outcome.Cancelled -> CloudAuthResult.Cancelled
            }
        }
    } catch (error: Throwable) {
        val failure = error.toCoreFailure()
        when (failure.code) {
            "timeout", "unavailable", "host_offline" -> CloudAuthResult.Unconfirmed(failure)
            "cancelled" -> CloudAuthResult.Cancelled
            else -> CloudAuthResult.Rejected(failure.code)
        }
    }

    override suspend fun recoverLostExchange(
        instance: InstanceId,
        attemptId: AttemptId,
    ): CloudAuthResult = try {
        val account = suspendOutcome { owner.core().cloudAuthRecover() }
        pending.remove(instance.value)
        when (account) {
            is Outcome.Success -> CloudAuthResult.Success(account.value.toSession(instance))
            is Outcome.Unconfirmed -> CloudAuthResult.Unconfirmed(account.failure)
            is Outcome.Failure -> CloudAuthResult.Rejected(account.failure.code)
            Outcome.Cancelled -> CloudAuthResult.Cancelled
        }
    } catch (error: Throwable) {
        val failure = error.toCoreFailure()
        if (failure.retryable) CloudAuthResult.Unconfirmed(failure)
        else CloudAuthResult.Rejected(failure.code)
    }

    override suspend fun cancel(instance: InstanceId, attemptId: AttemptId): Outcome<Unit> =
        outcomeOf {
            owner.core().cloudAuthCancel()
            pending.remove(instance.value)
        }

    override suspend fun currentSession(instance: InstanceId): Outcome<CloudSession?> = suspendOutcome {
        owner.core().cloudStatus().account?.toSession(instance)
    }
}


internal class ProfileAdapter(private val owner: AndroidCore) : ProfilePort {
    override suspend fun list(): Outcome<List<Profile>> = outcomeOf {
        owner.appConfig().instances.map { profileFor(owner, it) }
    }

    override suspend fun active(): Outcome<Profile?> = outcomeOf {
        val document = owner.appConfig()
        document.instances.firstOrNull { it.id == document.app.activeInstanceId }
            ?.let { profileFor(owner, it) }
    }

    override suspend fun validate(profile: Profile): Outcome<ProfileValidation> = outcomeOf {
        val id = profile.instance.value
        if (!id.matches(Regex("[a-z0-9][a-z0-9_-]{0,63}"))) {
            ProfileValidation.Invalid("invalid_instance_id")
        } else {
            val seen = owner.appConfig().instances.any { it.id == id }
            ProfileValidation.Valid(profile.copy(active = seen && owner.activeInstance.value == id))
        }
    }

    override suspend fun save(profile: Profile): Outcome<Profile> = suspendOutcome {
        val input = InstanceInput(
            id = profile.instance.value,
            service = if (profile.kind == InstanceKind.Standalone) "standalone" else "whaleReinCloud",
            environment = if (profile.kind == InstanceKind.Cloud) "production" else null,
            dataDir = "instances/${profile.instance.value}/data",
            standaloneCaPem = null,
            cloudCaPem = null,
        )
        appConfigValidateInstance(owner.appConfigPath, input)
        val document = appConfigSaveInstance(owner.appConfigPath, input)
        profileFor(owner, document.instances.first { it.id == profile.instance.value })
    }

    override suspend fun activate(instance: InstanceId): Outcome<ProfileActivation> = suspendOutcome {
        val before = owner.appConfig()
        val previous = before.instances.firstOrNull { it.id == before.app.activeInstanceId }
        val document = appConfigSetActive(owner.appConfigPath, instance.value)
        val active = document.instances.first { it.id == instance.value }
        owner.switchTo(instance)
        ProfileActivation(
            active = profileFor(owner, active),
            previous = previous?.let { profileFor(owner, it) },
        )
    }

    override suspend fun remove(instance: InstanceId): Outcome<Unit> = suspendOutcome {
        appConfigRemoveInstance(owner.appConfigPath, instance.value)
    }

    override suspend fun credentialState(instance: InstanceId): CredentialState {
        val view = owner.appConfig().instances.firstOrNull { it.id == instance.value }
            ?: return CredentialState.Invalid("not_found")
        return try {
            val instanceRoot = File(view.configPath).parentFile ?: File(view.dataDir)
            val store = AndroidSecretStore.create(instanceRoot)
            if (store.load("cloud-app-session") == null) CredentialState.Absent else CredentialState.Present
        } catch (_: SecretStoreException.Unavailable) {
            CredentialState.Unavailable
        } catch (_: SecretStoreException.Failed) {
            CredentialState.Invalid("secret_invalid")
        } catch (_: Exception) {
            CredentialState.Unavailable
        }
    }

    override suspend fun storageScope(instance: InstanceId): Outcome<ProfileStorageScope> =
        Outcome.Success(ProfileStorageScope(instance, backupExcluded = true))
}

internal class DirectoryAdapter(private val owner: AndroidCore) : DirectoryPort {
    override suspend fun list(instance: InstanceId): Outcome<List<DirectoryHostView>> = suspendOutcome {
        owner.core().cloudHosts().map { host ->
            DirectoryHostView(
                hostId = HostId(host.hostId),
                label = host.displayName,
                online = host.online,
                availability = when {
                    host.deviceAccess == "granted" -> HostAvailability.Ready
                    !host.enrolled -> HostAvailability.NoGrant
                    host.online -> HostAvailability.Unknown
                    else -> HostAvailability.Offline
                },
                connectable = host.enrolled,
            )
        }
    }

    override suspend fun connect(
        instance: InstanceId,
        hostId: HostId,
    ): Outcome<HostConnection> = suspendOutcome {
        val result = owner.core().connectCloud(hostId.value)
        val session = owner.core().cloudStatus().account?.toSession(instance)
            ?: throw IllegalStateException("missing account")
        HostConnection(
            host = DirectoryHostView(
                hostId = HostId(result.hostId),
                label = result.hostId,
                online = true,
                availability = HostAvailability.Ready,
                connectable = true,
            ),
            session = session,
        )
    }

    override suspend fun selectDefault(
        instance: InstanceId,
        hostId: HostId,
    ): Outcome<HostSelection> = outcomeOf {
        owner.core().selectDefaultHost(hostId.value)
        HostSelection(hostId)
    }

    override suspend fun currentSession(instance: InstanceId): Outcome<CloudSession?> = suspendOutcome {
        val status = owner.core().cloudStatus()
        status.account?.toSession(instance)
    }
}

internal class PairingAdapter(private val owner: AndroidCore) : PairingPort {
    private var pendingInvitation: String? = null

    override suspend fun reviewInvitation(encoded: String): Outcome<StandaloneInvitation> = suspendOutcome {
        pendingInvitation = encoded
        val prompt = owner.core().pairingStart(encoded)
        StandaloneInvitation(
            hostLabel = prompt.hostId,
            fingerprint = prompt.fingerprintDisplay,
            caRequired = true,
        )
    }

    override suspend fun checkFingerprint(
        instance: InstanceId,
        presented: String,
    ): Outcome<FingerprintCheck> = Outcome.Success(
        if (presented.isBlank()) FingerprintCheck.Mismatch else FingerprintCheck.Match,
    )

    override suspend fun importCa(instance: InstanceId, pem: String): Outcome<CaImportState> =
        Outcome.Success(CaImportState.Ready(instance))

    override suspend fun decide(instance: InstanceId, accept: Boolean): Outcome<PairingResult> =
        suspendOutcome {
            owner.core().pairingDecide(accept, null)
            if (accept) {
                val final = owner.core().defaultHost()
                if (final == null) PairingResult.Rejected("pairing_incomplete")
                else PairingResult.Success(HostRef(instance, HostId(final)))
            } else {
                PairingResult.Rejected("declined")
            }
        }

    override suspend fun lanPermission(): LanPermissionState = LanPermissionState.NotRequired

    override suspend fun requestLanPermission(): LanPermissionState = LanPermissionState.NotRequired

    override suspend fun validateTarget(
        instance: InstanceId,
        target: StandaloneTarget,
    ): Outcome<TransportValidation> = outcomeOf {
        when (val policy = com.whalerein.android.corebridge.trust.TlsPolicy.validateStandaloneTarget(target.url)) {
            com.whalerein.android.corebridge.trust.TlsPolicy.Result.Accepted ->
                TransportValidation.Valid
            is com.whalerein.android.corebridge.trust.TlsPolicy.Result.Rejected ->
                TransportValidation.Rejected(policy.code)
        }
    }
}

internal class StartupAdapter(private val owner: AndroidCore) : StartupPort {
    override suspend fun restore(instance: InstanceId): Outcome<StartupSnapshot> = suspendOutcome {
        val current = owner.core().currentCloudSessionId()
        val default = owner.core().defaultHost()
        if (default.isNullOrEmpty()) {
            StartupSnapshot(
                target = null,
                host = null,
                authorization = if (current == null) StartupAuthorization.SignedOut
                else StartupAuthorization.Unpaired,
                online = false,
            )
        } else {
            StartupSnapshot(
                target = null,
                host = HostRef(instance, HostId(default)),
                authorization = if (current == null) StartupAuthorization.SignedOut
                else StartupAuthorization.Authorized,
                online = true,
            )
        }
    }
}

internal class ExternalEntryAdapter(private val owner: AndroidCore) : ExternalEntryPort {
    override suspend fun currentProfile(): Outcome<Profile?> =
        ProfileAdapter(owner).active()

    override suspend fun authorize(target: ExternalTarget): ExternalAuthorization =
        if (target.sessionId != null && target.hostId != null) {
            ExternalAuthorization.Authorized(
                com.whalerein.android.state.core.Route.Conversation(
                    com.whalerein.android.state.core.SessionRef(
                        com.whalerein.android.state.core.HostRef(
                            owner.activeInstance,
                            target.hostId!!,
                        ),
                        com.whalerein.android.state.core.SessionId(target.sessionId!!),
                    ),
                ),
            )
        } else {
            ExternalAuthorization.Unauthorized
        }
}
