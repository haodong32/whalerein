package com.whalerein.android.corebridge.ports

import com.whalerein.android.corebridge.AndroidCore
import com.whalerein.android.corebridge.CloudAccountView
import com.whalerein.android.corebridge.InstanceInput
import com.whalerein.android.corebridge.InstanceView
import com.whalerein.android.corebridge.MobileException
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

internal suspend fun <T> onIo(block: () -> T): T = withContext(Dispatchers.IO) { block() }

internal fun profileFor(owner: AndroidCore, instance: InstanceView): Profile = Profile(
    instance = InstanceId(instance.id),
    label = instance.id,
    kind = if (instance.service == "standalone") InstanceKind.Standalone else InstanceKind.Cloud,
    active = owner.activeInstance.value == instance.id,
    backupExcluded = true,
)

internal fun CloudAccountView.toSession(
    instance: InstanceId,
    environment: String,
): CloudSession = cloudSessionFrom(
    instance = instance,
    accountId = accountId,
    workspaceId = workspaceId,
    environment = environment,
)

internal class CloudAuthAdapter(
    private val owner: AndroidCore,
    private val store: AccessRecordStore,
    private val nowMillis: () -> Long = { System.currentTimeMillis() },
) : CloudAuthPort {
    override suspend fun pendingAttempt(instance: InstanceId): Outcome<CloudAttempt?> {
        val attempt = store.readPending(instance) ?: return Outcome.Success(null)
        // An attempt started under another profile environment belongs to that
        // environment and must not be completed from this one.
        if (attempt.environment != owner.currentCloudEnvironment()) {
            store.clearPending(instance)
            return Outcome.Success(null)
        }
        if (attempt.expiresAtEpochMillis <= nowMillis()) {
            // Report the expired attempt once so the model can explain it, but
            // do not keep presenting it on every later launch.
            store.clearPending(instance)
        }
        return Outcome.Success(attempt)
    }

    override fun androidRedirectUri(environment: String): String =
        com.whalerein.android.corebridge.androidRedirectUri(environment)

    override suspend fun start(
        instance: InstanceId,
        environment: String,
    ): Outcome<CloudAttempt> {
        // The active profile owns the environment. A route that asks for a
        // different one is a wiring bug, and silently starting would bind the
        // attempt to the wrong scope.
        val active = owner.currentCloudEnvironment()
        if (environment != active) {
            return Outcome.Failure(
                com.whalerein.android.state.core.CoreFailure(
                    "environment_mismatch",
                    "The sign-in attempt did not match the active profile.",
                ),
            )
        }
        return suspendOutcome {
            val attempt = owner.core().cloudAuthStartAndroid()
            val view = cloudAttemptFrom(
                attemptId = attempt.attemptId,
                authorizationUrl = attempt.url,
                redirectUri = androidRedirectUri(active),
                environment = active,
                startedAtEpochMillis = nowMillis(),
            )
            store.writePending(instance, view)
            view
        }
    }

    override suspend fun complete(
        instance: InstanceId,
        attemptId: AttemptId,
        callbackUri: String,
    ): CloudAuthResult = try {
        val expected = store.readPending(instance)
        when {
            expected == null -> CloudAuthResult.Rejected("attempt_missing")
            expected.attemptId != attemptId -> CloudAuthResult.Rejected("attempt_mismatch")
            expected.expiresAtEpochMillis <= nowMillis() -> {
                store.clearPending(instance)
                CloudAuthResult.Expired
            }

            else -> {
                // Consume before the exchange so a replayed App Link callback
                // can never invoke Core's exchange twice. The attempt remains
                // recoverable through `recoverLostExchange` because Core keeps
                // its own copy.
                store.clearPending(instance)
                when (val account = suspendOutcome { owner.core().cloudAuthComplete(callbackUri) }) {
                    is Outcome.Success -> CloudAuthResult.Success(
                        account.value.toSession(instance, expected.environment),
                    )

                    is Outcome.Unconfirmed -> CloudAuthResult.Unconfirmed(account.failure)
                    is Outcome.Failure -> if (account.failure.code == "expired") {
                        CloudAuthResult.Expired
                    } else {
                        CloudAuthResult.Rejected(account.failure.code)
                    }

                    Outcome.Cancelled -> CloudAuthResult.Cancelled
                }
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
        val environment = store.readPending(instance)?.environment
            ?: owner.currentCloudEnvironment()
        val account = suspendOutcome { owner.core().cloudAuthRecover() }
        when (account) {
            is Outcome.Success -> CloudAuthResult.Success(
                account.value.toSession(instance, environment),
            )

            is Outcome.Unconfirmed -> CloudAuthResult.Unconfirmed(account.failure)
            is Outcome.Failure -> if (account.failure.code == "expired") {
                CloudAuthResult.Expired
            } else {
                CloudAuthResult.Rejected(account.failure.code)
            }

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
            store.clearPending(instance)
        }

    override suspend fun currentSession(instance: InstanceId): Outcome<CloudSession?> = suspendOutcome {
        owner.core().cloudStatus().account?.toSession(instance, owner.currentCloudEnvironment())
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
        val session = owner.core().cloudStatus().account?.toSession(instance, owner.currentCloudEnvironment())
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
        status.account?.toSession(instance, owner.currentCloudEnvironment())
    }
}

internal class PairingAdapter(private val owner: AndroidCore) : PairingPort {
    private data class ReviewedInvitation(
        val hostId: String,
        val fingerprint: String,
        val displayFingerprint: String,
    )

    private val reviewed = mutableMapOf<InstanceId, ReviewedInvitation>()

    override suspend fun reviewInvitation(encoded: String): Outcome<StandaloneInvitation> = suspendOutcome {
        val instance = owner.activeInstance
        val prompt = owner.core().pairingStart(encoded)
        reviewed[instance] = ReviewedInvitation(
            hostId = prompt.hostId,
            fingerprint = prompt.fingerprint,
            displayFingerprint = prompt.fingerprintDisplay,
        )
        StandaloneInvitation(
            hostLabel = prompt.hostId,
            fingerprint = prompt.fingerprintDisplay,
            caRequired = true,
        )
    }

    override suspend fun checkFingerprint(
        instance: InstanceId,
        presented: String,
    ): Outcome<FingerprintCheck> {
        // A review belongs to exactly the profile that started it. Comparing
        // against another profile's invitation, or any non-blank text, would
        // report a pin the user never reviewed.
        val invitation = reviewed[instance] ?: return Outcome.Success(FingerprintCheck.Mismatch)
        val matches = fingerprintMatches(
            presented,
            listOf(invitation.fingerprint, invitation.displayFingerprint),
        )
        return Outcome.Success(
            if (matches) FingerprintCheck.Match else FingerprintCheck.Mismatch,
        )
    }

    override suspend fun importCa(instance: InstanceId, pem: String): Outcome<CaImportState> {
        if (instance != owner.activeInstance) {
            return Outcome.Success(CaImportState.Rejected("ca_scope_mismatch"))
        }
        val record = owner.appConfig().instances.firstOrNull { it.id == instance.value }
            ?: return Outcome.Success(CaImportState.Rejected("instance_unknown"))
        if (record.service != "standalone") {
            return Outcome.Success(CaImportState.Rejected("ca_scope_mismatch"))
        }
        if (pem.isBlank() || !pem.contains("-----BEGIN CERTIFICATE-----")) {
            return Outcome.Success(CaImportState.Rejected("ca_invalid"))
        }
        return try {
            // Route the selected CA through Core's own validated config path,
            // then reopen the instance so pairing uses the applied bundle.
            appConfigSaveInstance(
                owner.appConfigPath,
                InstanceInput(
                    id = record.id,
                    service = "standalone",
                    environment = null,
                    dataDir = "instances/${record.id}/data",
                    standaloneCaPem = pem,
                    cloudCaPem = null,
                ),
            )
            owner.reloadActive(instance)
            Outcome.Success(CaImportState.Ready(instance))
        } catch (error: Throwable) {
            Outcome.Success(CaImportState.Rejected(error.toCoreFailure().code))
        }
    }

    override suspend fun decide(instance: InstanceId, accept: Boolean): Outcome<PairingResult> =
        suspendOutcome {
            val invitation = reviewed[instance]
            if (invitation == null) {
                return@suspendOutcome PairingResult.Rejected("invitation_missing")
            }
            val expected = invitation.fingerprint.lowercase().takeIf {
                it.length == 64 && it.all { character -> character.isDigit() || character in 'a'..'f' }
            }
            owner.core().pairingDecide(accept, expected)
            if (accept) {
                reviewed.remove(instance)
                // Pairing finishes asynchronously in Core. Give the committed
                // default Host a bounded chance to appear before reporting.
                var final: String? = owner.core().defaultHost()
                var attempts = 0
                while (final == null && attempts < PAIRING_COMPLETION_POLLS) {
                    kotlinx.coroutines.delay(PAIRING_COMPLETION_DELAY_MILLIS)
                    final = owner.core().defaultHost()
                    attempts += 1
                }
                if (final == null) PairingResult.Rejected("pairing_incomplete")
                else PairingResult.Success(HostRef(instance, HostId(final)))
            } else {
                reviewed.remove(instance)
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

    private companion object {
        const val PAIRING_COMPLETION_POLLS = 40
        const val PAIRING_COMPLETION_DELAY_MILLIS = 100L
    }
}

internal class StartupAdapter(
    private val owner: AndroidCore,
    private val store: AccessRecordStore,
) : StartupPort {
    override suspend fun recordTarget(session: SessionRef) {
        store.writeTarget(session.host.instance, session)
    }

    override suspend fun restore(instance: InstanceId): Outcome<StartupSnapshot> = suspendOutcome {
        val record = owner.appConfig().instances.firstOrNull { it.id == instance.value }
        val kind = if (record?.service == "standalone") InstanceKind.Standalone else InstanceKind.Cloud
        val status = owner.core().cloudStatus()
        val signedIn = status.account != null
        val lastTarget = store.readTarget(instance)
        val pairedHosts = try {
            owner.core().hosts().map { it.hostId }.toSet()
        } catch (_: Exception) {
            null
        }
        var hostRevoked = lastTarget?.let { target ->
            try {
                owner.core().hostConnection(target.host.hostId.value)?.state == "revoked"
            } catch (_: Exception) {
                false
            }
        } ?: false
        var sessionIds: Set<String>? = null
        var cachedSessionIds: Set<String> = emptySet()
        var cachedMessageCount = 0
        if (lastTarget != null && pairedHosts?.contains(lastTarget.host.hostId.value) == true) {
            val hostId = lastTarget.host.hostId.value
            val sessionId = lastTarget.sessionId.value
            val reachable = try {
                owner.core().sessions(hostId).map { it.sessionId }.toSet()
            } catch (_: MobileException.Revoked) {
                // A revoked grant must never fall through to the offline cache.
                hostRevoked = true
                null
            } catch (_: MobileException.NotPaired) {
                hostRevoked = true
                null
            } catch (_: MobileException.NotFound) {
                hostRevoked = true
                null
            } catch (_: Exception) {
                null
            }
            // `sessions()` falls back to the local cache on a Host failure, so
            // the connection state is the evidence that separates a live list
            // from a read-only cached one.
            val connection = runCatching {
                owner.core().hostConnection(hostId)?.state.orEmpty()
            }.getOrDefault("")
            sessionIds = reachable?.takeIf { connection == "connected" }
            cachedSessionIds = try {
                owner.core().cachedSessions(hostId).map { it.sessionId }.toSet()
            } catch (_: Exception) {
                emptySet()
            }
            cachedMessageCount = try {
                owner.core().cachedMessages(hostId, sessionId).size
            } catch (_: Exception) {
                0
            }
        }
        decideStartup(
            StartupRestoreEvidence(
                instance = instance,
                kind = kind,
                signedIn = signedIn,
                pairedHostIds = pairedHosts,
                defaultHostId = runCatching { owner.core().defaultHost() }.getOrNull(),
                lastTarget = lastTarget,
                sessionIds = sessionIds,
                cachedSessionIds = cachedSessionIds,
                cachedMessageCount = cachedMessageCount,
                hostRevoked = hostRevoked,
            ),
        )
    }
}

internal class ExternalEntryAdapter(private val owner: AndroidCore) : ExternalEntryPort {
    override suspend fun currentProfile(): Outcome<Profile?> =
        ProfileAdapter(owner).active()

    override suspend fun authorize(target: ExternalTarget): ExternalAuthorization {
        if (target.profileId != null && target.profileId != owner.activeInstance) {
            return ExternalAuthorization.WrongProfile
        }
        val record = owner.appConfig().instances.firstOrNull { it.id == owner.activeInstance.value }
        val kind = if (record?.service == "standalone") InstanceKind.Standalone else InstanceKind.Cloud
        val signedIn = try {
            owner.core().cloudStatus().account != null
        } catch (_: Exception) {
            return ExternalAuthorization.Unauthorized
        }
        val pairedHosts = try {
            owner.core().hosts().map { it.hostId }.toSet()
        } catch (_: Exception) {
            return ExternalAuthorization.Unauthorized
        }
        val hostId = target.hostId
        val hostRevoked = hostId?.let { host ->
            try {
                owner.core().hostConnection(host.value)?.state == "revoked"
            } catch (_: Exception) {
                false
            }
        } ?: false
        val hostGrantRevoked = if (kind == InstanceKind.Cloud && hostId != null) {
            try {
                val grant = owner.core().cloudHosts().firstOrNull { it.hostId == hostId.value }
                grant == null || grant.deviceAccess != "granted"
            } catch (_: Exception) {
                // The directory cannot confirm current authorization, so the
                // hint must not open a conversation.
                true
            }
        } else {
            false
        }
        var sessionRevoked = false
        val sessionIds = if (hostId != null && target.sessionId != null &&
            !hostRevoked && !hostGrantRevoked && hostId.value in pairedHosts
        ) {
            try {
                owner.core().sessions(hostId.value).map { it.sessionId }.toSet()
            } catch (_: MobileException.Revoked) {
                sessionRevoked = true
                null
            } catch (_: MobileException.NotPaired) {
                sessionRevoked = true
                null
            } catch (_: MobileException.NotFound) {
                sessionRevoked = true
                null
            } catch (_: Exception) {
                null
            }
        } else {
            null
        }
        return decideExternalAuthorization(
            ExternalAuthorizationEvidence(
                target = target,
                activeInstance = owner.activeInstance,
                kind = kind,
                signedIn = signedIn,
                pairedHostIds = pairedHosts,
                hostRevoked = hostRevoked || sessionRevoked,
                hostGrantRevoked = hostGrantRevoked,
                sessionIds = sessionIds,
            ),
        )
    }
}
