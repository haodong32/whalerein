package com.whalerein.android.corebridge

import com.whalerein.android.corebridge.diagnostics.CoreDiagnosticReceiver
import com.whalerein.android.corebridge.diagnostics.newDiagnosticContext
import com.whalerein.android.corebridge.secret.AndroidSecretStore
import com.whalerein.android.corebridge.secret.SecretStoreDiagnostics
import com.whalerein.android.corebridge.trust.AndroidTrustRoots
import java.io.File
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/** One queued event with the observation scope that produced it. */
data class ScopedCoreEvent(
    val hostId: String,
    val sessionId: String,
    val event: ConversationEvent,
)

/**
 * Owns the installed Core handle, its single event consumer and the Android
 * platform adapters.
 *
 * Event delivery is off the UI thread and serialized: [startObservation]
 * replaces any previous watch and the previous poller is fenced by a
 * generation counter, so an in-flight batch cannot be relabelled. The listener
 * must return promptly; it is never called after [close] or a newer
 * observation.
 *
 * Only ports that already exist are adapted here. `:state` currently defines a
 * `DiagnosticSink`; access, conversation, support and attachment ports are not
 * defined yet and are intentionally absent rather than invented.
 */
class CoreBridge private constructor(
    private val core: MobileCore,
    private val dispatcher: CoroutineDispatcher,
) : AutoCloseable {
    internal val rawCore: MobileCore get() = core
    private val scope = CoroutineScope(SupervisorJob() + dispatcher)
    private val generation = AtomicLong(0)
    private val observationLock = Any()
    private var poller: Job? = null

    val coreRevision: String
        get() = core.coreRevision()

    val coreVersion: String
        get() = CoreProvenance.coreVersion

    val appVersion: String
        get() = CoreProvenance.appVersion

    /** Begin one bounded event consumer for the exact Host/session scope. */
    fun startObservation(
        hostId: String,
        sessionId: String,
        listener: (ScopedCoreEvent) -> Unit,
    ) {
        synchronized(observationLock) {
            val current = generation.incrementAndGet()
            poller?.cancel()
            core.stopWatch()
            core.startWatch(hostId, sessionId)
            poller = scope.launch {
                while (isActive && generation.get() == current) {
                    val events = core.pollEvents(POLL_TIMEOUT_MS.toULong())
                    if (!isActive || generation.get() != current) return@launch
                    for (event in events) {
                        listener(ScopedCoreEvent(hostId, sessionId, event))
                    }
                }
            }
        }
    }

    /** Stop the consumer before leaving the foreground or replacing scope. */
    fun stopObservation() {
        synchronized(observationLock) {
            generation.incrementAndGet()
            poller?.cancel()
            poller = null
            core.stopWatch()
        }
    }

    /** Send with a stable identity allocated before submission when supplied. */
    fun send(
        hostId: String,
        sessionId: String,
        text: String,
        delivery: DeliveryChoice,
        operationId: String? = null,
    ): SendOperationView =
        if (operationId == null) {
            core.sendWithOperationId(hostId, sessionId, text, delivery, newOperationId())
        } else {
            core.sendWithOperationId(hostId, sessionId, text, delivery, operationId)
        }

    fun hosts(): List<HostSummary> = core.hosts()

    fun sessions(hostId: String): List<SessionSummaryView> = core.sessions(hostId)

    fun cachedMessages(hostId: String, sessionId: String): List<MessageView> =
        core.cachedMessages(hostId, sessionId)

    fun capabilities(hostId: String, sessionId: String): SessionCapabilitiesView =
        core.capabilities(hostId, sessionId)

    override fun close() {
        stopObservation()
        scope.cancel()
        core.close()
    }

    companion object {
        private const val POLL_TIMEOUT_MS = 500L

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
            dispatcher: CoroutineDispatcher = Dispatchers.IO,
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
            return CoreBridge(core, dispatcher)
        }

        private fun timeUnixMillis(): Long = System.currentTimeMillis()
    }
}
