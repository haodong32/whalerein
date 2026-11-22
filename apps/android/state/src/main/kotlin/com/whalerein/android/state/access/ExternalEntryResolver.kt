package com.whalerein.android.state.access

import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.Route
import kotlinx.coroutines.CancellationException

/**
 * Validates a non-authorizing navigation hint against the current profile,
 * Host, session and pending item before returning one exact [Route].
 *
 * Cold and warm entry behave the same: the resolver holds no navigation cache.
 * A hint for another profile, a switched session, a revoked Host, an expired
 * target or an unauthorized item yields `route == null` and no target detail.
 */
class ExternalEntryResolver(
    private val entry: ExternalEntryPort,
    private val diagnostics: DiagnosticSink,
) {

    suspend fun resolve(target: ExternalTarget): ExternalEntryResolution {
        val profileResult = try {
            entry.currentProfile()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            diagnostics.accessEvent(
                level = DiagnosticLevel.Error,
                event = "transport_external_entry_exception",
                stage = AccessStage.Transport,
                code = "unexpected_failure",
            )
            return denied(ExternalEntryReason.Unauthorized)
        }
        val profile = profileResult.valueOrNull()
        if (profileResult !is Outcome.Success || profile == null) {
            diagnostics.accessEvent(
                level = DiagnosticLevel.Warn,
                event = "transport_external_entry_denied",
                stage = AccessStage.Transport,
                code = "no_profile",
            )
            return denied(ExternalEntryReason.Unauthorized)
        }
        if (target.profileId != null && target.profileId != profile.instance) {
            diagnostics.accessEvent(
                level = DiagnosticLevel.Warn,
                event = "transport_external_entry_denied",
                stage = AccessStage.Transport,
                code = "wrong_profile",
            )
            return denied(ExternalEntryReason.WrongProfile)
        }
        if (target.hostId == null && target.sessionId == null && target.itemId == null) {
            return denied(ExternalEntryReason.InvalidTarget)
        }
        val authorization = try {
            entry.authorize(target)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            diagnostics.accessEvent(
                level = DiagnosticLevel.Error,
                event = "transport_external_entry_exception",
                stage = AccessStage.Transport,
                code = "unexpected_failure",
            )
            return denied(ExternalEntryReason.Unauthorized)
        }
        return when (authorization) {
            is ExternalAuthorization.Authorized -> authorized(target, authorization)
            ExternalAuthorization.WrongProfile -> denied(ExternalEntryReason.WrongProfile)
            ExternalAuthorization.SessionNotFound -> denied(ExternalEntryReason.SwitchedSession)
            ExternalAuthorization.HostRevoked -> denied(ExternalEntryReason.RevokedHost)
            ExternalAuthorization.ExpiredTarget -> denied(ExternalEntryReason.ExpiredTarget)
            ExternalAuthorization.PendingItemUnavailable -> ExternalEntryResolution(
                route = null,
                reason = ExternalEntryReason.PendingItemUnavailable,
                itemState = ExternalItemState.Unavailable,
            )
            ExternalAuthorization.Unauthorized -> denied(ExternalEntryReason.Unauthorized)
        }
    }

    private fun authorized(
        target: ExternalTarget,
        authorization: ExternalAuthorization.Authorized,
    ): ExternalEntryResolution {
        val focus = if (authorization.itemState == ExternalItemState.Pending) {
            authorization.pendingItemId ?: target.itemId
        } else {
            null
        }
        val route = when (val authorizedRoute = authorization.route) {
            is Route.Conversation -> authorizedRoute.copy(focusItemId = focus)
            is Route.PendingInput -> authorizedRoute.copy(focusItemId = focus)
            else -> authorizedRoute
        }
        val reason = if (authorization.itemState == ExternalItemState.Unavailable) {
            ExternalEntryReason.PendingItemUnavailable
        } else {
            ExternalEntryReason.Resolved
        }
        diagnostics.accessEvent(
            level = DiagnosticLevel.Info,
            event = "transport_external_entry_resolved",
            stage = AccessStage.Transport,
            code = reason.name.lowercase(),
        )
        return ExternalEntryResolution(
            route = route,
            reason = reason,
            itemState = authorization.itemState,
        )
    }

    private fun denied(reason: ExternalEntryReason): ExternalEntryResolution {
        if (reason != ExternalEntryReason.Resolved) {
            diagnostics.accessEvent(
                level = DiagnosticLevel.Warn,
                event = "transport_external_entry_denied",
                stage = AccessStage.Transport,
                code = reason.name.lowercase(),
            )
        }
        return ExternalEntryResolution(
            route = null,
            reason = reason,
            revealNothing = true,
        )
    }
}
