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
            // The platform store is unreadable from Rust on Android, so the
            // anchors travel to Core as data. A failed extraction degrades to
            // the portable public roots instead of blocking startup.
            val platformTrust = try {
                AndroidTrustRoots.platformPem()
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
                null
            }
            val secrets = AndroidSecretStore.create(instanceDirectory, secretDiagnostics)
            val core = when {
                context != null && platformTrust != null -> MobileCore.newWithDiagnosticContextAndPlatformTrust(
                    configPath,
                    secrets,
                    appVersion,
                    context,
                    platformTrust,
                )
                context != null -> MobileCore.newWithDiagnosticContext(
                    configPath,
                    secrets,
                    appVersion,
                    context,
                )
                platformTrust != null -> MobileCore.newWithPlatformTrust(
                    configPath,
                    secrets,
                    appVersion,
                    platformTrust,
                )
                else -> MobileCore(configPath, secrets, appVersion)
            }
            return CoreBridge(core)
        }

        private fun timeUnixMillis(): Long = System.currentTimeMillis()
    }
}
