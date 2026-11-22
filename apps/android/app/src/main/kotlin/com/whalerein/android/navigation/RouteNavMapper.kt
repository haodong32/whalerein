package com.whalerein.android.navigation

import androidx.navigation3.runtime.NavKey
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Route
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef

/**
 * Authoritative `Route <-> NavKey` mapping.
 *
 * A route can only be produced by `:state` after it resolved the active
 * instance and authorization; the key then carries the same opaque identities.
 */
object RouteNavMapper {
    fun toKey(route: Route): NavKey = when (route) {
        Route.Welcome -> WelcomeKey
        Route.SignIn -> SignInKey
        Route.Directory -> DirectoryKey
        Route.StandaloneSetup -> StandaloneSetupKey
        Route.Profiles -> ProfilesKey
        is Route.HostSessions -> SessionsKey(
            instanceId = route.host.instance.value,
            hostId = route.host.hostId.value,
        )

        is Route.Conversation -> ConversationKey(
            instanceId = route.session.host.instance.value,
            hostId = route.session.host.hostId.value,
            sessionId = route.session.sessionId.value,
            focusItemId = route.focusItemId,
        )

        is Route.SessionTools -> SessionToolsKey(
            instanceId = route.session.host.instance.value,
            hostId = route.session.host.hostId.value,
            sessionId = route.session.sessionId.value,
        )

        is Route.PendingInput -> ConversationKey(
            instanceId = route.session.host.instance.value,
            hostId = route.session.host.hostId.value,
            sessionId = route.session.sessionId.value,
            focusItemId = route.focusItemId,
            pendingSheet = true,
        )

        is Route.AttachmentSave -> AttachmentSaveKey(
            instanceId = route.session.host.instance.value,
            hostId = route.session.host.hostId.value,
            sessionId = route.session.sessionId.value,
            attachmentId = route.attachmentId,
        )

        Route.Operations -> OperationsKey
        Route.Account -> AccountKey
        Route.Diagnostics -> DiagnosticsKey
    }

    /** Maps a destination back to the closest authorized route. */
    fun toRoute(key: NavKey): Route? = when (key) {
        is StartupKey -> null
        is WelcomeKey -> Route.Welcome
        is SignInKey -> Route.SignIn
        is DirectoryKey -> Route.Directory
        is StandaloneSetupKey -> Route.StandaloneSetup
        is ProfilesKey -> Route.Profiles
        is ProfileSettingsKey -> Route.Profiles
        is SessionsKey -> Route.HostSessions(sessionRef(key).host)
        is ConversationKey -> Route.Conversation(sessionRef(key), key.focusItemId)
        is SessionToolsKey -> Route.SessionTools(sessionRef(key))
        is AttachmentSaveKey -> Route.AttachmentSave(sessionRef(key), key.attachmentId)
        is OperationsKey -> Route.Operations
        is AccountKey -> Route.Account
        is DiagnosticsKey -> Route.Diagnostics
        else -> null
    }

    fun sessionRef(key: ConversationKey): SessionRef = sessionRef(
        instanceId = key.instanceId,
        hostId = key.hostId,
        sessionId = key.sessionId,
    )

    fun sessionRef(key: SessionToolsKey): SessionRef = sessionRef(
        instanceId = key.instanceId,
        hostId = key.hostId,
        sessionId = key.sessionId,
    )

    fun sessionRef(key: AttachmentSaveKey): SessionRef = sessionRef(
        instanceId = key.instanceId,
        hostId = key.hostId,
        sessionId = key.sessionId,
    )

    private fun sessionRef(key: SessionsKey): SessionRef = sessionRef(
        instanceId = key.instanceId,
        hostId = key.hostId,
        sessionId = "",
    )

    private fun sessionRef(instanceId: String, hostId: String, sessionId: String): SessionRef =
        SessionRef(
            host = HostRef(InstanceId(instanceId), HostId(hostId)),
            sessionId = SessionId(sessionId),
        )
}
