package com.whalerein.android.corebridge.ports

import com.whalerein.android.corebridge.AndroidCore
import com.whalerein.android.corebridge.HostSummary
import com.whalerein.android.corebridge.SessionCapabilitiesView
import com.whalerein.android.corebridge.SessionCreateInputView
import com.whalerein.android.corebridge.SessionSummaryView
import com.whalerein.android.state.conversation.CreateSessionInput
import com.whalerein.android.state.conversation.CreatedSession
import com.whalerein.android.state.conversation.ForkSessionInput
import com.whalerein.android.state.conversation.ForkedSession
import com.whalerein.android.state.conversation.SessionCapabilities
import com.whalerein.android.state.conversation.SessionDetails
import com.whalerein.android.state.conversation.SessionListFailureCodes
import com.whalerein.android.state.conversation.SessionListPort
import com.whalerein.android.state.conversation.SessionListSnapshot
import com.whalerein.android.state.conversation.SessionSummary
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionRef

/**
 * Session lifecycle adapter (AC-08). Every result is bound to the requested
 * Host/session; a list whose Host is no longer authorized is `foreign_host`.
 */
internal class SessionListAdapter(private val owner: AndroidCore) : SessionListPort {
    override suspend fun list(host: HostRef): Outcome<SessionListSnapshot> = suspendOutcome {
        requireHost(host)
        try {
            val sessions = owner.core().sessions(host.hostId.value)
            SessionListSnapshot(
                host = host,
                sessions = sessions.map { it.toSummary() },
                cached = false,
                capabilities = snapshotCapabilities(host, sessions),
            )
        } catch (error: Throwable) {
            val failure = error.toCoreFailure()
            if (failure.code != "host_offline" && failure.code != "unavailable") throw error
            val cached = owner.core().cachedSessions(host.hostId.value)
            SessionListSnapshot(
                host = host,
                sessions = cached.map { it.toSummary() },
                cached = true,
                capabilities = snapshotCapabilities(host, cached),
            )
        }
    }

    override suspend fun details(session: SessionRef): Outcome<SessionDetails> = suspendOutcome {
        val view = owner.core().sessionDetails(
            session.host.hostId.value,
            session.sessionId.value,
        )
        if (view.hostId != session.host.hostId.value || view.sessionId != session.sessionId.value) {
            throw ForeignResult(SessionListFailureCodes.FOREIGN_SESSION)
        }
        view.toDetails(session)
    }

    override suspend fun create(
        host: HostRef,
        input: CreateSessionInput,
    ): Outcome<CreatedSession> = suspendOutcome {
        requireHost(host)
        val view = owner.core().createSession(
            host.hostId.value,
            SessionCreateInputView(
                cwd = input.workingDirectory,
                workspaceId = input.workspace,
                agentPreset = input.preset,
                preferredSessionId = input.preferredSessionId?.value,
            ),
        )
        val sessionId = view.sessionId
            ?: throw CoreFailureException(
                CoreFailure(
                    SessionListFailureCodes.DETAILS_REQUIRED,
                    "The Host did not confirm a created session.",
                    retryable = true,
                ),
            )
        val session = SessionRef(host, com.whalerein.android.state.core.SessionId(sessionId))
        // The Host confirmed this identity. A failed details read must not
        // discard it, otherwise a retry would create a second session.
        val details = try {
            owner.core().sessionDetails(host.hostId.value, sessionId).let { view ->
                if (view.hostId == host.hostId.value && view.sessionId == sessionId) {
                    view.toDetails(session)
                } else {
                    null
                }
            }
        } catch (_: Throwable) {
            null
        }
        CreatedSession(session, details)
    }

    override suspend fun fork(
        session: SessionRef,
        input: ForkSessionInput,
    ): Outcome<ForkedSession> = suspendOutcome {
        // Core's fork request targets the session's latest completed turn.
        // A resolved turn id from session details therefore selects the same
        // operation; it is not a second wire field and must not be rejected.
        val view = owner.core().forkSessionOutcome(
            session.host.hostId.value,
            session.sessionId.value,
        )
        val childId = view.sessionId
            ?: throw CoreFailureException(
                CoreFailure(
                    SessionListFailureCodes.NO_COMPLETED_TURN,
                    "The session has no completed turn to fork.",
                ),
            )
        if (view.parentSessionId != null && view.parentSessionId != session.sessionId.value) {
            throw ForeignResult(SessionListFailureCodes.FOREIGN_SESSION)
        }
        val child = SessionRef(session.host, com.whalerein.android.state.core.SessionId(childId))
        val details = try {
            owner.core().sessionDetails(session.host.hostId.value, childId)
        } catch (_: Throwable) {
            throw CoreFailureException(
                CoreFailure(
                    SessionListFailureCodes.DETAILS_REQUIRED,
                    "The forked session details could not be read.",
                ),
            )
        }
        ForkedSession(
            child = child,
            details = details.toDetails(child),
            workingDirectoryAvailable =
                view.workingDirectory != null && view.workingDirectoryError == null,
        )
    }

    private suspend fun requireHost(host: HostRef) {
        val hosts: List<HostSummary> = owner.core().hosts()
        if (hosts.none { it.hostId == host.hostId.value }) {
            throw ForeignResult(SessionListFailureCodes.FOREIGN_HOST)
        }
    }

    private suspend fun snapshotCapabilities(
        host: HostRef,
        sessions: List<SessionSummaryView>,
    ): SessionCapabilities {
        val supportsCreate = try {
            owner.core().sessionCreateSupported(host.hostId.value)
        } catch (_: Throwable) {
            false
        }
        val capabilities = sessions.firstNotNullOfOrNull { summary ->
            try {
                owner.core().capabilities(host.hostId.value, summary.sessionId)
            } catch (_: Throwable) {
                null
            }
        }
        return capabilities.toStateCapabilities(supportsCreate)
    }
}

private fun SessionSummaryView.toSummary(): SessionSummary = SessionSummary(
    sessionId = com.whalerein.android.state.core.SessionId(sessionId),
    title = title.ifEmpty { null },
    updatedAtMillis = updatedAtUnixMs,
)

internal fun com.whalerein.android.corebridge.SessionDetailView.toDetails(
    session: SessionRef,
): SessionDetails = SessionDetails(
    session = session,
    title = title.ifEmpty { null },
    workingDirectory = cwd.ifEmpty { null },
    workspace = null,
    preset = null,
    // The binding's fork is explicitly "latest completed turn". A non-blank
    // history cursor proves a committed turn exists, so Fork is enabled and the
    // resolved locator is accepted back by the latest-turn fork request.
    latestCompletedTurnId = latestCompletedTurn(historyCursor, blank),
    updatedAtMillis = updatedAtUnixMs,
    cached = false,
)

private fun SessionCapabilitiesView?.toStateCapabilities(canCreate: Boolean): SessionCapabilities =
    if (this == null) {
        SessionCapabilities(canCreate = canCreate)
    } else {
        SessionCapabilities(
            canCreate = canCreate,
            canFork = sessionFork,
            supportsWorkingDirectory = true,
            supportsWorkspace = true,
            supportsPreset = true,
            supportsPreferredSessionId = true,
        )
    }

private class ForeignResult(code: String) : Exception(code)

private class CoreFailureException(val failure: CoreFailure) : Exception(failure.message)

internal fun Throwable.asCoreFailure(): CoreFailure = when (this) {
    is ForeignResult -> CoreFailure(message ?: SessionListFailureCodes.FOREIGN_HOST, "Foreign result.")
    is CoreFailureException -> failure
    else -> toCoreFailure()
}
