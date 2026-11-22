package com.whalerein.android.state.core

/**
 * Authorized application destinations.
 *
 * A route is produced only by app-level state after it has resolved the active
 * instance and the current authorization for the target. External hints never
 * carry content or authorization and are resolved into one of these routes by
 * `ExternalEntryResolver` before navigation.
 */
sealed interface Route {
    data object Welcome : Route

    data object SignIn : Route

    data object Directory : Route

    data object StandaloneSetup : Route

    data object Profiles : Route

    data class HostSessions(val host: HostRef) : Route

    data class Conversation(
        val session: SessionRef,
        /** A still-pending pending-input item to focus after opening. */
        val focusItemId: String? = null,
    ) : Route

    data class SessionTools(val session: SessionRef) : Route

    data class PendingInput(val session: SessionRef, val focusItemId: String? = null) : Route

    data class AttachmentSave(
        val session: SessionRef,
        val attachmentId: String,
    ) : Route

    data object Operations : Route

    data object Account : Route

    data object Diagnostics : Route
}
