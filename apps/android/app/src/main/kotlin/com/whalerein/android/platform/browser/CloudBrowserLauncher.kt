package com.whalerein.android.platform.browser

import android.content.ActivityNotFoundException
import android.content.Context
import android.net.Uri
import androidx.browser.customtabs.CustomTabsIntent
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.DiagnosticEvent

/**
 * Hosted PKCE entry through a Custom Tab.
 *
 * Only HTTPS authorization URLs are accepted; there is deliberately no embedded
 * WebView fallback. The URL itself is never logged or copied into diagnostics.
 */
class CloudBrowserLauncher(
    private val context: Context,
    private val diagnostics: DiagnosticSink,
) {
    fun open(authorizationUrl: String): CoreFailure? {
        val uri = Uri.parse(authorizationUrl)
        if (uri.scheme != "https" || uri.host.isNullOrBlank()) {
            diagnostics.emit(
                DiagnosticEvent(
                    level = DiagnosticLevel.Error,
                    event = "login_browser_url_rejected",
                    stage = "login",
                    code = "authorization_url_insecure",
                ),
            )
            return CoreFailure(
                code = "authorization_url_insecure",
                message = "authorization_url_insecure",
            )
        }
        return try {
            CustomTabsIntent.Builder()
                .setShowTitle(true)
                .setUrlBarHidingEnabled(true)
                .build()
                .launchUrl(context, uri)
            null
        } catch (_: ActivityNotFoundException) {
            diagnostics.emit(
                DiagnosticEvent(
                    level = DiagnosticLevel.Warn,
                    event = "login_browser_unavailable",
                    stage = "login",
                    code = "browser_unavailable",
                ),
            )
            CoreFailure(code = "browser_unavailable", message = "browser_unavailable", retryable = true)
        } catch (_: Exception) {
            diagnostics.emit(
                DiagnosticEvent(
                    level = DiagnosticLevel.Warn,
                    event = "login_browser_failed",
                    stage = "login",
                    code = "browser_unavailable",
                ),
            )
            CoreFailure(code = "browser_unavailable", message = "browser_unavailable", retryable = true)
        }
    }
}
