package com.whalerein.android.platform.applinks

import android.content.Intent
import android.net.Uri
import com.whalerein.android.state.access.ExternalTarget
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.InstanceId
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * App Link and future system-entry dispatch.
 *
 * The hosted PKCE callback is accepted only for the registered HTTPS callback
 * origins and path. The resolver, not this router, decides whether a session
 * target is authorized; the router only carries a non-authorizing hint.
 *
 * No callback URI, query value or deep-link content is logged or persisted in
 * the journal. Only a fixed rejection code is recorded for diagnostics.
 */
class AppLinkRouter(private val diagnostics: DiagnosticSink) {
    private val mutableCallback = MutableStateFlow<String?>(null)

    /** Raw callback URI, consumed by the cloud sign-in screen exactly once. */
    val callback: StateFlow<String?> = mutableCallback.asStateFlow()

    private val mutableExternalTarget = MutableStateFlow<ExternalTarget?>(null)
    val externalTarget: StateFlow<ExternalTarget?> = mutableExternalTarget.asStateFlow()

    /** Returns true when the URI belongs to a registered callback origin and path. */
    fun submitCallback(uri: Uri?): Boolean {
        if (uri == null || uri.scheme != "https") {
            reject("scheme")
            return false
        }
        val host = uri.host?.lowercase()
        if (host !in allowedHosts) {
            reject("origin")
            return false
        }
        if (uri.path != CALLBACK_PATH) {
            reject("path")
            return false
        }
        mutableCallback.value = uri.toString()
        return true
    }

    fun consumeCallback() {
        mutableCallback.value = null
    }

    fun submitExternalTarget(target: ExternalTarget) {
        mutableExternalTarget.value = target
    }

    fun consumeExternalTarget() {
        mutableExternalTarget.value = null
    }

    fun dispatch(intent: Intent?) {
        if (intent == null) return
        if (intent.action == ACTION_OPEN_SESSION) {
            mutableExternalTarget.value = readTarget(intent)
            return
        }
        if (intent.data != null) {
            submitCallback(intent.data)
        }
    }

    private fun readTarget(intent: Intent): ExternalTarget = ExternalTarget(
        profileId = intent.getStringExtra(EXTRA_PROFILE_ID)?.takeIf { it.isNotBlank() }?.let(::InstanceId),
        hostId = intent.getStringExtra(EXTRA_HOST_ID)?.takeIf { it.isNotBlank() }?.let(::HostId),
        sessionId = intent.getStringExtra(EXTRA_SESSION_ID)?.takeIf { it.isNotBlank() },
        itemId = intent.getStringExtra(EXTRA_ITEM_ID)?.takeIf { it.isNotBlank() },
    )

    private fun reject(reason: String) {
        diagnostics.emit(
            DiagnosticEvent(
                level = DiagnosticLevel.Warn,
                event = "transport_app_link_rejected",
                stage = "transport",
                code = "invalid_$reason",
            ),
        )
    }

    companion object {
        const val ACTION_OPEN_SESSION = "com.whalerein.android.action.OPEN_SESSION"
        const val EXTRA_PROFILE_ID = "com.whalerein.android.extra.PROFILE_ID"
        const val EXTRA_HOST_ID = "com.whalerein.android.extra.HOST_ID"
        const val EXTRA_SESSION_ID = "com.whalerein.android.extra.SESSION_ID"
        const val EXTRA_ITEM_ID = "com.whalerein.android.extra.ITEM_ID"
        private const val CALLBACK_PATH = "/client/android/callback"
        private val allowedHosts = setOf("app.whalerein.test", "app.whalerein.com")
    }
}
