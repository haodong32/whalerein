package com.whalerein.android.state.access

import com.whalerein.android.state.core.AttemptId
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.InstanceKind
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.SessionRef

/**
 * Domain values presented by the access slice.
 *
 * Every value is immutable and free of Android, UniFFI and JNA types so the
 * same models are rendered by Compose and asserted by the JDK-only tests.
 */

/** One isolated connection profile selected by the app. */
data class Profile(
    val instance: InstanceId,
    val label: String,
    val kind: InstanceKind,
    val active: Boolean,
    val backupExcluded: Boolean,
)

/** Private storage boundary of one profile. */
data class ProfileStorageScope(
    val instance: InstanceId,
    val backupExcluded: Boolean,
)

/** Local credential-store read outcome for one profile. */
sealed interface CredentialState {
    data object Present : CredentialState

    data object Absent : CredentialState

    data object Unavailable : CredentialState

    data class Invalid(val code: String) : CredentialState
}

/** Result of validating a candidate profile before activation. */
sealed interface ProfileValidation {
    data class Valid(val profile: Profile) : ProfileValidation

    data class Invalid(val code: String) : ProfileValidation
}

/** Result of an explicit profile switch. [previous] is informative only. */
data class ProfileActivation(
    val active: Profile,
    val previous: Profile?,
)

enum class HostAvailability {
    /** Core confirmed a usable device grant for this Host. */
    Ready,

    /** The Host is visible but this device has no grant yet. */
    NoGrant,

    /** The Host is not currently online. */
    Offline,

    /** The authorization check itself could not be completed. */
    AuthorizationUnavailable,

    /** Core has no reliable availability evidence. */
    Unknown,
}

/** One Host row from the Cloud directory. Presence and grant are separate. */
data class DirectoryHostView(
    val hostId: HostId,
    val label: String,
    val online: Boolean,
    val availability: HostAvailability,
    val connectable: Boolean,
)

/** Verified Cloud account and workspace binding. */
data class CloudSession(
    val instance: InstanceId,
    val accountId: String,
    val workspaceId: String,
    val environment: String,
)

/** One pending native PKCE attempt. It never contains a code or verifier. */
data class CloudAttempt(
    val attemptId: AttemptId,
    val authorizationUrl: String,
    val state: String,
    val redirectUri: String,
    val environment: String,
    val expiresAtEpochMillis: Long,
    /** Optional expected authority carried by a recovered attempt. */
    val expectedAccountId: String? = null,
    /** Optional expected workspace carried by a recovered attempt. */
    val expectedWorkspaceId: String? = null,
)

/** Result of a connect attempt for one directory Host. */
data class HostConnection(
    val host: DirectoryHostView,
    val session: CloudSession,
)

data class HostSelection(val hostId: HostId)

/** Typed evidence Core returns when restoring the last target. */
enum class StartupAuthorization {
    Authorized,
    SignedOut,
    Unpaired,
    Revoked,
    Expired,
    Unavailable,
}

/** One Core restore observation, before the route is decided. */
data class StartupSnapshot(
    val target: SessionRef? = null,
    val host: HostRef? = null,
    val authorization: StartupAuthorization = StartupAuthorization.Authorized,
    val online: Boolean = true,
    val missingSession: Boolean = false,
    val readOnlyCacheAvailable: Boolean = false,
)

enum class StartupEvidence {
    ValidPreviousConversation,
    MissingSession,
    SignedOut,
    Unpaired,
    Revoked,
    Expired,
    AuthorizationUnavailable,
    OfflineCacheAvailable,
    OfflineCacheUnavailable,
    FirstCloudUse,
}

/** Exact authorized route plus the evidence that selected it. */
data class StartupDestination(
    val route: Route,
    val evidence: StartupEvidence,
)

/** Standalone invitation review material. The fingerprint is user-facing. */
data class StandaloneInvitation(
    val hostLabel: String,
    val fingerprint: String,
    val caRequired: Boolean,
)

/** Scoped import state for a Standalone private CA. */
sealed interface CaImportState {
    data object NotRequired : CaImportState

    data object AwaitingSelection : CaImportState

    data class Ready(val instance: InstanceId) : CaImportState

    data class Rejected(val code: String) : CaImportState
}

enum class LanPermissionState {
    Unknown,
    Granted,
    Denied,
    Revoked,
    NotRequired,
}

enum class FingerprintCheck { Match, Mismatch }

data class StandaloneTarget(val url: String)

sealed interface TransportValidation {
    data object Valid : TransportValidation

    data class Rejected(val code: String) : TransportValidation
}

sealed interface PairingResult {
    data class Success(val host: HostRef) : PairingResult

    data class Rejected(val code: String) : PairingResult
}

/**
 * A non-authorizing external navigation hint.
 *
 * Every field is optional because an unknown or hostile hint may omit them.
 * The resolver never reads target content from the hint.
 */
data class ExternalTarget(
    val profileId: InstanceId? = null,
    val hostId: HostId? = null,
    val sessionId: String? = null,
    val itemId: String? = null,
)

enum class ExternalItemState { NotRequested, Pending, Resolved, Unavailable }

enum class ExternalEntryReason {
    Resolved,
    WrongProfile,
    SwitchedSession,
    RevokedHost,
    ExpiredTarget,
    PendingItemUnavailable,
    Unauthorized,
    InvalidTarget,
}

/** Result of authorization. A null route never carries any target detail. */
data class ExternalEntryResolution(
    val route: Route?,
    val reason: ExternalEntryReason,
    val itemState: ExternalItemState = ExternalItemState.NotRequested,
    val revealNothing: Boolean = false,
)

/** Core's answer for one external hint after current-scope authorization. */
sealed interface ExternalAuthorization {
    data class Authorized(
        val route: Route,
        val itemState: ExternalItemState = ExternalItemState.NotRequested,
        val pendingItemId: String? = null,
    ) : ExternalAuthorization

    data object WrongProfile : ExternalAuthorization

    data object SessionNotFound : ExternalAuthorization

    data object HostRevoked : ExternalAuthorization

    data object ExpiredTarget : ExternalAuthorization

    data object PendingItemUnavailable : ExternalAuthorization

    data object Unauthorized : ExternalAuthorization
}
