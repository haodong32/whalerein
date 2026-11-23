package com.whalerein.android.corebridge.ports

import com.whalerein.android.state.access.CloudAttempt
import com.whalerein.android.state.access.CloudSession
import com.whalerein.android.state.access.ExternalAuthorization
import com.whalerein.android.state.access.ExternalTarget
import com.whalerein.android.state.access.StartupAuthorization
import com.whalerein.android.state.access.StartupSnapshot
import com.whalerein.android.state.core.AttemptId
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.InstanceKind
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import java.net.URI
import java.net.URLDecoder
import java.nio.charset.StandardCharsets

/**
 * Pure access mappings and decisions.
 *
 * These functions hold no Android or UniFFI state so the JDK-only bridge tests
 * can prove the exact values the adapters hand to `:state` and the exact
 * decision taken for an untrusted route hint or a cold-launch record.
 */

/** One native attempt mapped to the app model with a concrete deadline. */
internal fun cloudAttemptFrom(
    attemptId: String,
    authorizationUrl: String,
    redirectUri: String,
    environment: String,
    startedAtEpochMillis: Long,
): CloudAttempt = CloudAttempt(
    attemptId = AttemptId(attemptId),
    authorizationUrl = authorizationUrl,
    state = stateFromAuthorizationUrl(authorizationUrl),
    redirectUri = redirectUri,
    environment = environment,
    expiresAtEpochMillis = startedAtEpochMillis +
        AccessRecordStore.PENDING_ATTEMPT_LIFETIME_MILLIS,
)

/** The exact Cloud state Core embedded in the hosted authorization URL. */
internal fun stateFromAuthorizationUrl(url: String): String = try {
    URI(url).rawQuery.orEmpty().split('&')
        .firstOrNull { it.startsWith("state=") }
        ?.substringAfter("state=")
        ?.let { URLDecoder.decode(it, StandardCharsets.UTF_8) }
        .orEmpty()
} catch (_: Exception) {
    ""
}

/** A verified Cloud identity scoped to the active profile's environment. */
internal fun cloudSessionFrom(
    instance: InstanceId,
    accountId: String,
    workspaceId: String,
    environment: String,
): CloudSession = CloudSession(
    instance = instance,
    accountId = accountId,
    workspaceId = workspaceId,
    environment = environment,
)

/**
 * Normalize the user-visible fingerprint: Core renders groups separated by
 * colons and the canonical value is lowercase hex. Both forms name the same
 * pin, so comparison ignores separators and case.
 */
internal fun normalizeFingerprint(value: String): String =
    value.filter { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }.lowercase()

/** True only for a non-blank value that equals one of the expected forms. */
internal fun fingerprintMatches(presented: String, expected: Iterable<String>): Boolean {
    val candidate = normalizeFingerprint(presented)
    if (candidate.isEmpty()) return false
    return expected.map(::normalizeFingerprint).any { it.isNotEmpty() && it == candidate }
}

/** Core evidence gathered before deciding a cold-launch route. */
internal data class StartupRestoreEvidence(
    val instance: InstanceId,
    val kind: InstanceKind,
    val signedIn: Boolean,
    /** Paired Host ids from Core, or null when the list could not be read. */
    val pairedHostIds: Set<String>? = null,
    val defaultHostId: String? = null,
    val lastTarget: SessionRef? = null,
    /** Session ids reported by the Host, or null for an offline/cache result. */
    val sessionIds: Set<String>? = null,
    val cachedSessionIds: Set<String> = emptySet(),
    val cachedMessageCount: Int = 0,
    val hostRevoked: Boolean = false,
)

/**
 * Decide the startup snapshot from Core evidence.
 *
 * Authorization loss, a missing target and temporary connectivity stay
 * distinct. A signed-out or revoked profile never carries the previous target
 * into the snapshot, so the UI cannot open cached content for an account that
 * no longer owns it. When no target was recorded the default/first paired Host
 * leads to its session list, which is the migration-safe first-use route.
 */
internal fun decideStartup(evidence: StartupRestoreEvidence): StartupSnapshot {
    if (evidence.kind == InstanceKind.Cloud && !evidence.signedIn) {
        return StartupSnapshot(
            target = null,
            host = null,
            authorization = StartupAuthorization.SignedOut,
            online = false,
        )
    }
    val paired = evidence.pairedHostIds
        ?: return StartupSnapshot(
            target = null,
            host = null,
            authorization = StartupAuthorization.Unavailable,
            online = false,
        )
    if (evidence.kind == InstanceKind.Standalone && paired.isEmpty() && !evidence.signedIn) {
        return StartupSnapshot(
            target = null,
            host = null,
            authorization = StartupAuthorization.Unpaired,
            online = false,
        )
    }
    val last = evidence.lastTarget
    if (last == null) {
        val hostId = evidence.defaultHostId?.takeIf { it in paired } ?: paired.firstOrNull()
        return StartupSnapshot(
            target = null,
            host = hostId?.let { HostRef(evidence.instance, HostId(it)) },
            authorization = StartupAuthorization.Authorized,
            online = true,
        )
    }
    val hostRef = HostRef(last.host.instance, last.host.hostId)
    if (last.host.hostId.value !in paired || evidence.hostRevoked) {
        return StartupSnapshot(
            target = null,
            host = null,
            authorization = StartupAuthorization.Revoked,
            online = false,
        )
    }
    val sessionIds = evidence.sessionIds
    if (sessionIds == null) {
        val cached = last.sessionId.value in evidence.cachedSessionIds ||
            evidence.cachedMessageCount > 0
        return StartupSnapshot(
            target = last,
            host = hostRef,
            authorization = StartupAuthorization.Authorized,
            online = false,
            readOnlyCacheAvailable = cached,
            missingSession = false,
        )
    }
    if (last.sessionId.value !in sessionIds) {
        return StartupSnapshot(
            target = null,
            host = hostRef,
            authorization = StartupAuthorization.Authorized,
            online = true,
            missingSession = true,
        )
    }
    return StartupSnapshot(
        target = last,
        host = hostRef,
        authorization = StartupAuthorization.Authorized,
        online = true,
    )
}

/** Evidence gathered before authorizing one external session hint. */
internal data class ExternalAuthorizationEvidence(
    val target: ExternalTarget,
    val activeInstance: InstanceId,
    val kind: InstanceKind,
    val signedIn: Boolean,
    /** Paired Host ids, or null when Core could not answer. */
    val pairedHostIds: Set<String>? = null,
    val hostRevoked: Boolean = false,
    /** True when the Cloud directory no longer grants this Host. */
    val hostGrantRevoked: Boolean = false,
    /** Host session ids, or null when the list could not be read. */
    val sessionIds: Set<String>? = null,
)

/**
 * Authorize an untrusted hint against current Core state.
 *
 * A hint never becomes a route on shape alone. A different profile, a Host
 * that is no longer paired or granted, a session that is gone, and a Core that
 * cannot answer all produce a denial instead of opening stale content.
 */
internal fun decideExternalAuthorization(
    evidence: ExternalAuthorizationEvidence,
): ExternalAuthorization {
    val target = evidence.target
    if (target.profileId != null && target.profileId != evidence.activeInstance) {
        return ExternalAuthorization.WrongProfile
    }
    val hostId = target.hostId ?: return ExternalAuthorization.Unauthorized
    if (evidence.kind == InstanceKind.Cloud && !evidence.signedIn) {
        return ExternalAuthorization.Unauthorized
    }
    val paired = evidence.pairedHostIds ?: return ExternalAuthorization.Unauthorized
    if (hostId.value !in paired) return ExternalAuthorization.HostRevoked
    if (evidence.hostRevoked || evidence.hostGrantRevoked) {
        return ExternalAuthorization.HostRevoked
    }
    val hostRef = HostRef(evidence.activeInstance, hostId)
    val sessionId = target.sessionId
        ?: return ExternalAuthorization.Authorized(Route.HostSessions(hostRef))
    val sessions = evidence.sessionIds ?: return ExternalAuthorization.Unauthorized
    if (sessionId !in sessions) return ExternalAuthorization.SessionNotFound
    return ExternalAuthorization.Authorized(
        Route.Conversation(SessionRef(hostRef, SessionId(sessionId))),
    )
}
