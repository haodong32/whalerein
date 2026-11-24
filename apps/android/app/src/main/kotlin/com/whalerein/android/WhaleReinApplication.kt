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
 *
 * Release builds open the graph here, exactly as before. Debug builds defer it
 * to the first production caller so the Debug State Gallery can run a scenario
 * without opening Core, the journal or production diagnostics. [ensureGraph] is
 * idempotent and per process.
 */
class WhaleReinApplication : Application() {
    private var appGraph: AppContainer? = null

    lateinit var diagnostics: DiagnosticsRuntime
        private set

    lateinit var platform: PlatformServices
        private set

    val container: AppContainer
        get() = ensureGraph()

    override fun onCreate() {
        super.onCreate()
        if (!BuildConfig.DEBUG) {
            ensureGraph()
        }
    }

    /** Opens the production graph once; later calls return the same instance. */
    @Synchronized
    fun ensureGraph(): AppContainer {
        appGraph?.let { return it }
        diagnostics = DiagnosticsRuntime(this, AndroidClock)
        platform = PlatformServices()
        val created = AppContainer(this, diagnostics, platform)
        appGraph = created
        created.start()
        return created
    }
}
