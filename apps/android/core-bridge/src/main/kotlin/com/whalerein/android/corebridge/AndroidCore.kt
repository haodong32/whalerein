package com.whalerein.android.corebridge

import android.content.Context
import com.whalerein.android.corebridge.diagnostics.CoreDiagnosticReceiver
import com.whalerein.android.corebridge.ports.AccountAdapter
import com.whalerein.android.corebridge.ports.AccessPorts
import com.whalerein.android.corebridge.ports.AccessRecordStore
import com.whalerein.android.corebridge.ports.AttachmentAdapter
import com.whalerein.android.corebridge.ports.AttachmentAuthorizationAdapter
import com.whalerein.android.corebridge.ports.AttachmentPorts
import com.whalerein.android.corebridge.ports.AttachmentStagingAdapter
import com.whalerein.android.corebridge.ports.CloudAuthAdapter
import com.whalerein.android.corebridge.ports.CommandAdapter
import com.whalerein.android.corebridge.ports.ComposerAdapter
import com.whalerein.android.corebridge.ports.ConversationPorts
import com.whalerein.android.corebridge.ports.CoreFailureSummaryAdapter
import com.whalerein.android.corebridge.ports.DirectoryAdapter
import com.whalerein.android.corebridge.ports.DraftAdapter
import com.whalerein.android.corebridge.ports.ExternalEntryAdapter
import com.whalerein.android.corebridge.ports.HistoryAdapter
import com.whalerein.android.corebridge.ports.ModelsQueueAdapter
import com.whalerein.android.corebridge.ports.ObservationAdapter
import com.whalerein.android.corebridge.ports.OperationsAdapter
import com.whalerein.android.corebridge.ports.PairingAdapter
import com.whalerein.android.corebridge.ports.PendingInputAdapter
import com.whalerein.android.corebridge.ports.ProfileAdapter
import com.whalerein.android.corebridge.ports.SessionListAdapter
import com.whalerein.android.corebridge.ports.StartupAdapter
import com.whalerein.android.corebridge.ports.SupportPorts
import com.whalerein.android.corebridge.ports.WaitAdapter
import com.whalerein.android.corebridge.state.StateDiagnosticSinkAdapter
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import java.io.File
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Immutable app/Core provenance for diagnostics and settings. */
data class CoreProvenanceView(
    val coreRevision: String,
    val coreVersion: String,
    val appVersion: String,
    val androidRedirectUri: String,
)

/**
 * One installed Core handle plus every `:state` port adapter the app composes.
 *
 * The facade owns exactly one active instance. Profile activation closes the
 * previous handle after the new app document commits, so a failed activation
 * leaves the previous handle usable. All blocking FFI work runs on
 * [dispatcher]; observation has one consumer and a generation fence.
 */
class AndroidCore private constructor(
    val context: Context,
    val appConfigPath: String,
    private val journalSink: DiagnosticSink?,
    private val appVersion: String,
    private val dispatcher: CoroutineDispatcher,
    initial: InstanceId,
) : AutoCloseable {
    @Volatile
    var activeInstance: InstanceId = initial
        private set

    private val lock = Any()
    @Volatile
    private var installed: CoreBridge = openBridge(initial)
    private val closed = AtomicBoolean(false)

    /** Non-secret pending-attempt and last-target records. */
    internal val accessStore: AccessRecordStore by lazy {
        AccessRecordStore.besideConfig(appConfigPath)
    }

    val accessPorts: AccessPorts by lazy {
        AccessPorts(
            cloudAuth = CloudAuthAdapter(this, accessStore),
            profiles = ProfileAdapter(this),
            directory = DirectoryAdapter(this),
            pairing = PairingAdapter(this),
            startup = StartupAdapter(this, accessStore),
            externalEntry = ExternalEntryAdapter(this),
        )
    }

    val conversationPorts: ConversationPorts by lazy {
        ConversationPorts(
            observation = ObservationAdapter(this),
            history = HistoryAdapter(this),
            composer = ComposerAdapter(this),
            wait = WaitAdapter(this),
            command = CommandAdapter(this),
            modelsQueue = ModelsQueueAdapter(this),
            pendingInput = PendingInputAdapter(this),
            draft = DraftAdapter(this),
            sessionList = SessionListAdapter(this),
        )
    }

    val supportPorts: SupportPorts by lazy {
        SupportPorts(
            operations = OperationsAdapter(this),
            account = AccountAdapter(this),
            failureSummary = CoreFailureSummaryAdapter(this),
        )
    }

    val attachmentPorts: AttachmentPorts by lazy {
        AttachmentPorts(
            attachments = AttachmentAdapter(this),
            staging = AttachmentStagingAdapter(this),
            authorization = AttachmentAuthorizationAdapter(this),
        )
    }

    /** Alias matching the composition-root naming used by the app shell. */

    /** Direct session-lifecycle accessor for `SessionListViewModel`. */
    val sessionList: com.whalerein.android.state.conversation.SessionListPort
        get() = conversationPorts.sessionList

    val androidRedirectUri: String get() = CoreProvenance.androidRedirectUri

    internal val processId: String = UUID.randomUUID().toString()

    val provenance: CoreProvenanceView
        get() = CoreProvenanceView(
            coreRevision = installed.coreRevision,
            coreVersion = CoreProvenance.coreVersion,
            appVersion = appVersion,
            androidRedirectUri = CoreProvenance.androidRedirectUri,
        )

    /** Active generated handle for the port adapters in this package. */
    internal fun core(): MobileCore {
        check(!closed.get()) { "AndroidCore is closed" }
        return installed.rawCore
    }

    internal fun appConfig(): AppConfigView = appConfigRead(appConfigPath)

    /** The environment currently recorded for the active Cloud profile. */
    fun currentCloudEnvironment(): String =
        appConfig().instances.firstOrNull { it.id == activeInstance.value }?.environment
            ?: "production"

    /**
     * Debug-build profile switch. Persists the selected environment and, for
     * the local environment, the imported root bundle before the graph is
     * rebuilt. Release builds never expose this path.
     */
    suspend fun applyCloudEnvironment(
        environment: String,
        cloudCaPem: String?,
    ): Outcome<Unit> = withContext(Dispatchers.IO) {
        if (environment !in CLOUD_ENVIRONMENTS) {
            return@withContext Outcome.Failure(
                CoreFailure("invalid_environment", "Unsupported Cloud environment."),
            )
        }
        val document = try {
            appConfigRead(appConfigPath)
        } catch (_: Throwable) {
            return@withContext Outcome.Failure(
                CoreFailure("profile_unavailable", "The active profile could not be read."),
            )
        }
        val record = document.instances.firstOrNull { it.id == activeInstance.value }
            ?: return@withContext Outcome.Failure(
                CoreFailure("profile_missing", "The active profile no longer exists."),
            )
        if (record.service != "whalerein_cloud") {
            return@withContext Outcome.Failure(
                CoreFailure("not_cloud", "The active profile is not a Cloud profile."),
            )
        }
        val ca = if (environment == "local") {
            cloudCaPem
                ?: record.cloudCaPath?.let { path ->
                    File(path).takeIf(File::isFile)?.readText()
                }
                ?: return@withContext Outcome.Failure(
                    CoreFailure("local_ca_required", "Local Cloud requires the development CA."),
                )
        } else {
            null
        }
        try {
            appConfigSaveInstance(
                appConfigPath,
                InstanceInput(
                    id = record.id,
                    service = "whalerein_cloud",
                    environment = environment,
                    dataDir = "instances/${record.id}/data",
                    standaloneCaPem = null,
                    cloudCaPem = ca,
                ),
            )
            Outcome.Success(Unit)
        } catch (error: Throwable) {
            Outcome.Failure(
                CoreFailure(
                    "environment_update_failed",
                    error.message?.takeIf { it.isNotBlank() } ?: "The environment could not be updated.",
                ),
            )
        }
    }

    /**
     * Reopen the active instance after its persisted configuration changed,
     * for example when a Standalone CA was just applied. A failure leaves the
     * previous, still-valid handle in place.
     */
    internal fun reloadActive(instance: InstanceId) {
        synchronized(lock) {
            if (closed.get()) return
            val next = openBridge(instance)
            val previous = installed
            installed = next
            activeInstance = instance
            previous.close()
        }
    }

    /** Commit-then-swap profile activation. */
    internal fun switchTo(instance: InstanceId) {
        if (instance == activeInstance) return
        synchronized(lock) {
            if (closed.get()) return
            val next = openBridge(instance)
            val previous = installed
            installed = next
            activeInstance = instance
            previous.close()
        }
    }

    private fun openBridge(instance: InstanceId): CoreBridge {
        var document = try {
            appConfigRead(appConfigPath)
        } catch (_: Throwable) {
            appConfigBootstrap(appConfigPath)
        }
        var view = document.instances.firstOrNull { it.id == instance.value }
            ?: document.instances.firstOrNull { it.id == document.app.activeInstanceId }
            ?: throw IllegalArgumentException("no such profile instance")
        if (!File(view.configPath).isFile) {
            // Bootstrap records entries but only materializes the active one.
            // Selecting the requested profile creates its validated TOML
            // before Core opens, and keeps a failed write from changing state.
            document = appConfigSetActive(appConfigPath, view.id)
            view = document.instances.firstOrNull { it.id == instance.value } ?: view
        }
        val receiver = journalSink?.let { sink ->
            CoreDiagnosticReceiver { event ->
                StateDiagnosticSinkAdapter(sink).onEvent(event)
            }
        }
        // Core validates data_dir and rejects unknown entries in it. Secrets
        // live beside the materialized instance document, never inside Core's
        // data_dir, so the Keystore store can be created before Core opens.
        val instanceRoot = File(view.configPath).parentFile
            ?: File(view.dataDir)
        return CoreBridge.open(
            configPath = view.configPath,
            instanceDirectory = instanceRoot,
            filesDir = context.filesDir,
            diagnosticReceiver = receiver,
            appVersion = appVersion,
        )
    }

    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        synchronized(lock) {
            installed.close()
        }
    }

    companion object {
        private val CLOUD_ENVIRONMENTS = setOf("production", "staging", "local")
        /**
         * Open the composition root.
         *
         * [appConfigPath] is the application's authoritative document; [instance]
         * selects the starting profile. [journalSink] receives Core's redacted
         * events through the existing `:state` diagnostic port.
         */
        fun open(
            context: Context,
            appConfigPath: String,
            instance: InstanceId,
            journalSink: DiagnosticSink? = null,
            appVersion: String = CoreProvenance.appVersion,
            dispatcher: CoroutineDispatcher = Dispatchers.IO,
        ): AndroidCore {
            try {
                appConfigRead(appConfigPath)
            } catch (_: Throwable) {
                appConfigBootstrap(appConfigPath)
            }
            return AndroidCore(context, appConfigPath, journalSink, appVersion, dispatcher, instance)
        }
    }
}
