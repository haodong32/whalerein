package com.whalerein.android

import android.app.Application
import com.whalerein.android.platform.AndroidClock
import com.whalerein.android.platform.PlatformServices
import com.whalerein.android.platform.journal.DiagnosticsRuntime

/**
 * Application entry point and process-scoped composition root.
 *
 * The app-scoped JSONL journal, picker seams and LAN gateway exist before Core
 * opens so startup diagnostics survive a failed native load. The graph itself is
 * opened in `AppContainer` off the main thread.
 */
class WhaleReinApplication : Application() {
    lateinit var diagnostics: DiagnosticsRuntime
        private set

    lateinit var platform: PlatformServices
        private set

    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        diagnostics = DiagnosticsRuntime(this, AndroidClock)
        platform = PlatformServices()
        container = AppContainer(this, diagnostics, platform)
        container.start()
    }
}
