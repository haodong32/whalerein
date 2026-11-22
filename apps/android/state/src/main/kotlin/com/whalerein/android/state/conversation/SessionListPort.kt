package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef

/**
 * Host-qualified session lifecycle contract (AC-08).
 *
 * All requests are explicitly bound to one Host. A result whose Host/session
 * does not match the request is a foreign result and must never replace the
 * selected Host's list. This interface uses only `state/core` identities so
 * `:core-bridge` can adapt `MobileCore` without leaking UniFFI types.
 */
data class SessionCapabilities(
    val canCreate: Boolean = true,
    val canFork: Boolean = true,
    val supportsWorkingDirectory: Boolean = true,
    val supportsWorkspace: Boolean = true,
    val supportsPreset: Boolean = true,
    val supportsPreferredSessionId: Boolean = true,
)

data class SessionSummary(
    val sessionId: SessionId,
    val title: String? = null,
    val updatedAtMillis: Long = 0,
    val hasPendingInput: Boolean = false,
)

data class SessionListSnapshot(
    /** Explicit Host metadata for the list. */
    val host: HostRef,
    val sessions: List<SessionSummary>,
    /** True when Core served an eligible offline list. */
    val cached: Boolean = false,
    val capabilities: SessionCapabilities = SessionCapabilities(),
)

data class SessionDetails(
    val session: SessionRef,
    val title: String? = null,
    val workingDirectory: String? = null,
    val workspace: String? = null,
    val preset: String? = null,
    /** Latest completed turn; the default fork target. Null means no fork target. */
    val latestCompletedTurnId: String? = null,
    val updatedAtMillis: Long = 0,
    val cached: Boolean = false,
)

/** Default create input: every explicit field left for the Host to choose. */
data class CreateSessionInput(
    val workingDirectory: String? = null,
    val workspace: String? = null,
    val preset: String? = null,
    val preferredSessionId: SessionId? = null,
)

data class CreatedSession(
    val session: SessionRef,
    val details: SessionDetails,
)

data class ForkSessionInput(
    /** Null means "use the selected session's latest completed turn". */
    val turnId: String? = null,
)

data class ForkedSession(
    val child: SessionRef,
    val details: SessionDetails,
    /**
     * False keeps the fork successful; the created child simply has no usable
     * working directory yet.
     */
    val workingDirectoryAvailable: Boolean = true,
)

interface SessionListPort {
    suspend fun list(host: HostRef): Outcome<SessionListSnapshot>

    suspend fun details(session: SessionRef): Outcome<SessionDetails>

    suspend fun create(host: HostRef, input: CreateSessionInput): Outcome<CreatedSession>

    suspend fun fork(session: SessionRef, input: ForkSessionInput): Outcome<ForkedSession>
}

object SessionListFailureCodes {
    const val FOREIGN_HOST = "foreign_host"
    const val FOREIGN_SESSION = "foreign_session"
    const val NO_COMPLETED_TURN = "no_completed_turn"
    const val DETAILS_REQUIRED = "details_required"
    const val INVALID_INPUT = "invalid_input"
}
