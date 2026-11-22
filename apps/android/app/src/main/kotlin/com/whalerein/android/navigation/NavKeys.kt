package com.whalerein.android.navigation

import androidx.navigation3.runtime.NavKey
import kotlinx.serialization.Serializable

/**
 * Typed Navigation 3 destinations.
 *
 * Keys carry only opaque identities, never content or authorization. External
 * hints are resolved into a [com.whalerein.android.state.core.Route] first.
 */
@Serializable
data object StartupKey : NavKey

@Serializable
data object WelcomeKey : NavKey

@Serializable
data object SignInKey : NavKey

@Serializable
data object DirectoryKey : NavKey

@Serializable
data object StandaloneSetupKey : NavKey

@Serializable
data object ProfilesKey : NavKey

@Serializable
data class ProfileSettingsKey(val instanceId: String) : NavKey

@Serializable
data class SessionsKey(
    val instanceId: String,
    val hostId: String,
) : NavKey

@Serializable
data class ConversationKey(
    val instanceId: String,
    val hostId: String,
    val sessionId: String,
    val focusItemId: String? = null,
    val pendingSheet: Boolean = false,
) : NavKey

@Serializable
data class SessionToolsKey(
    val instanceId: String,
    val hostId: String,
    val sessionId: String,
) : NavKey

@Serializable
data class AttachmentSaveKey(
    val instanceId: String,
    val hostId: String,
    val sessionId: String,
    val attachmentId: String,
) : NavKey

@Serializable
data object OperationsKey : NavKey

@Serializable
data object AccountKey : NavKey

@Serializable
data object DiagnosticsKey : NavKey
