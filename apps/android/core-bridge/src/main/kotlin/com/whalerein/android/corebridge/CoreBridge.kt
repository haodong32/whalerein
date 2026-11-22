package com.whalerein.android.corebridge

import com.whalerein.android.corebridge.diagnostics.CoreDiagnosticReceiver
import com.whalerein.android.corebridge.diagnostics.newDiagnosticContext
import com.whalerein.android.corebridge.secret.AndroidSecretStore
import com.whalerein.android.corebridge.secret.SecretStoreDiagnostics
import com.whalerein.android.corebridge.trust.AndroidTrustRoots
import java.io.File

/**
 * Owns the installed Core handle and the Android platform adapters.
 *
 * The screen-facing observation consumer lives in `ObservationAdapter`; this
 * handle only exposes the raw Core to those adapters and owns its lifetime.
 */
class CoreBridge private constructor(
    private val core: MobileCore,
) : AutoCloseable {
    internal val rawCore: MobileCore get() = core

    val coreRevision: String
        get() = core.coreRevision()

    val coreVersion: String
        get() = CoreProvenance.coreVersion

    val appVersion: String
        get() = CoreProvenance.appVersion

    override fun close() {
        core.stopWatch()
        core.close()
    }

    companion object {
        /**
         * Prepare Android trust, build the per-instance Keystore store and open
         * Core. The diagnostic context is reserved before configuration loads
         * so startup failures are observable; the receiver may be null when the
         * app has not enabled the journal.
         */
        fun open(
            configPath: String,
            instanceDirectory: File,
            filesDir: File,
            diagnosticReceiver: CoreDiagnosticReceiver? = null,
            appVersion: String = CoreProvenance.appVersion,
        ): CoreBridge {
            val context = diagnosticReceiver?.let { newDiagnosticContext(it) }
            val secretDiagnostics = diagnosticReceiver?.let { receiver ->
                SecretStoreDiagnostics { code ->
                    receiver.onDiagnostic(
                        DiagnosticEventView(
                            timeUnixMillis(),
                            context?.traceId() ?: "",
                            "warn",
                            code,
                            emptyMap(),
                        ),
                    )
                }
            }
            try {
                AndroidTrustRoots.install(filesDir)
            } catch (_: Exception) {
                diagnosticReceiver?.onDiagnostic(
                    DiagnosticEventView(
                        timeUnixMillis(),
                        context?.traceId() ?: "",
                        "warn",
                        "native_trust_roots_unavailable",
                        emptyMap(),
                    ),
                )
            }
            val secrets = AndroidSecretStore.create(instanceDirectory, secretDiagnostics)
            val core = if (context != null) {
                MobileCore.newWithDiagnosticContext(
                    configPath,
                    secrets,
                    appVersion,
                    context,
                )
            } else {
                MobileCore(configPath, secrets, appVersion)
            }
            return CoreBridge(core)
        }

        private fun timeUnixMillis(): Long = System.currentTimeMillis()
    }
}
