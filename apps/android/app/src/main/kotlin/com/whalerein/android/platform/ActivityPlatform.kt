package com.whalerein.android.platform

import androidx.activity.ComponentActivity
import com.whalerein.android.state.access.LanPermissionState
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.platform.browser.CloudBrowserLauncher
import com.whalerein.android.platform.documents.AndroidDocumentGateway
import com.whalerein.android.platform.documents.PickerDestinationWriter
import com.whalerein.android.platform.documents.PickerJournalExporter
import com.whalerein.android.platform.lan.ActivityLanPermission
import com.whalerein.android.platform.lan.LanPermissionGateway
import java.util.concurrent.atomic.AtomicReference

/** Activity-owned Android integrations used by the composition root. */
class ActivityPlatform(
    activity: ComponentActivity,
    diagnostics: DiagnosticSink,
) {
    val documents: AndroidDocumentGateway = AndroidDocumentGateway(activity)
    val lan: LanPermissionGateway = ActivityLanPermission(activity, diagnostics)
    val browser: CloudBrowserLauncher = CloudBrowserLauncher(activity, diagnostics)
}

/**
 * Application-scoped access to the currently resumed Activity platform.
 *
 * The graph is built once and outlives rotation; delegates resolve the current
 * Activity lazily so pickers and permission prompts always target the live
 * Activity.
 */
class PlatformServices {
    private val current = AtomicReference<ActivityPlatform?>(null)

    val journalExporter = PickerJournalExporter { current.get()?.documents }
    val destinationWriter = PickerDestinationWriter { current.get()?.documents }

    fun attach(platform: ActivityPlatform) {
        current.set(platform)
    }

    fun detach(platform: ActivityPlatform) {
        current.compareAndSet(platform, null)
    }

    fun lan(): LanPermissionGateway = object : LanPermissionGateway {
        override suspend fun state(): LanPermissionState =
            current.get()?.lan?.state() ?: LanPermissionState.NotRequired

        override suspend fun request(): LanPermissionState =
            current.get()?.lan?.request() ?: LanPermissionState.NotRequired
    }
}
