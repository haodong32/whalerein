package com.whalerein.android

import android.content.Context
import com.whalerein.android.corebridge.AndroidCore
import com.whalerein.android.platform.PlatformServices
import com.whalerein.android.platform.lan.LanAwarePairingPort
import com.whalerein.android.state.access.CloudAuthPort
import com.whalerein.android.state.access.DirectoryPort
import com.whalerein.android.state.access.ExternalEntryPort
import com.whalerein.android.state.access.PairingPort
import com.whalerein.android.state.access.ProfilePort
import com.whalerein.android.state.access.StartupPort
import com.whalerein.android.state.attachments.AttachmentAuthorizationPort
import com.whalerein.android.state.attachments.AttachmentPort
import com.whalerein.android.state.attachments.AttachmentStaging
import com.whalerein.android.state.attachments.DestinationWriter
import com.whalerein.android.state.conversation.CommandPort
import com.whalerein.android.state.conversation.ComposerPort
import com.whalerein.android.state.conversation.DraftPort
import com.whalerein.android.state.conversation.HistoryPort
import com.whalerein.android.state.conversation.ModelsQueuePort
import com.whalerein.android.state.conversation.ObservationPort
import com.whalerein.android.state.conversation.PendingInputPort
import com.whalerein.android.state.conversation.SessionListPort
import com.whalerein.android.state.conversation.WaitPort
import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.IdGenerator
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.support.AccountPort
import com.whalerein.android.state.support.CoreFailureSummaryPort
import com.whalerein.android.state.support.Journal
import com.whalerein.android.state.support.JournalExporter
import com.whalerein.android.state.support.OperationsPort
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Production [AppDependencies] over the `:core-bridge` facade.
 *
 * `:core-bridge` owns the installed Core handle, its single event consumer and
 * every port adapter over generated UniFFI. `:app` supplies only platform
 * seams: the picker-backed journal exporter and destination writer, the LAN
 * permission gateway and the app-scoped journal.
 *
 * Expected `:core-bridge` facade API (implemented by `AndroidCore`):
 * - `AndroidCore.open(context, appConfigPath, instance, journalSink, appVersion)`
 * - `accessPorts { cloudAuth, profiles, directory, pairing, startup, externalEntry }`
 * - `conversationPorts { observation, history, composer, wait, command,
 *   modelsQueue, pendingInput, draft, sessionList }`
 * - `supportPorts { operations, account, failureSummary }`
 * - `attachmentPorts { attachments, staging, authorization }`
 * - `activeInstance`, `provenance { coreRevision, coreVersion, appVersion }`,
 *   `close()`
 */
class BridgeAppDependencies(
    private val core: AndroidCore,
    override val journal: Journal,
    override val journalExporter: JournalExporter,
    override val destinationWriter: DestinationWriter,
    override val clock: Clock,
    override val ids: IdGenerator,
    override val diagnosticSink: DiagnosticSink,
    private val platform: PlatformServices,
) : AppDependencies {
    override val cloudAuth: CloudAuthPort get() = core.accessPorts.cloudAuth
    override val profiles: ProfilePort get() = core.accessPorts.profiles
    override val directory: DirectoryPort get() = core.accessPorts.directory
    override val pairing: PairingPort =
        LanAwarePairingPort(core.accessPorts.pairing, platform.lan())
    override val startup: StartupPort get() = core.accessPorts.startup
    override val externalEntry: ExternalEntryPort get() = core.accessPorts.externalEntry

    override val observation: ObservationPort get() = core.conversationPorts.observation
    override val history: HistoryPort get() = core.conversationPorts.history
    override val composer: ComposerPort get() = core.conversationPorts.composer
    override val wait: WaitPort get() = core.conversationPorts.wait
    override val commands: CommandPort get() = core.conversationPorts.command
    override val pendingInput: PendingInputPort get() = core.conversationPorts.pendingInput
    override val drafts: DraftPort get() = core.conversationPorts.draft
    override val modelsQueue: ModelsQueuePort get() = core.conversationPorts.modelsQueue
    override val sessionList: SessionListPort get() = core.conversationPorts.sessionList

    override val operations: OperationsPort get() = core.supportPorts.operations
    override val account: AccountPort get() = core.supportPorts.account
    override val coreFailureSummary: CoreFailureSummaryPort
        get() = core.supportPorts.failureSummary

    override val attachments: AttachmentPort get() = core.attachmentPorts.attachments
    override val attachmentStaging: AttachmentStaging get() = core.attachmentPorts.staging
    override val attachmentAuthorization: AttachmentAuthorizationPort
        get() = core.attachmentPorts.authorization

    override val coreRevision: String get() = core.provenance.coreRevision
    override val coreVersion: String get() = core.provenance.coreVersion
    override val appVersion: String get() = core.provenance.appVersion
    override val instance: InstanceId get() = core.activeInstance

    override fun close() {
        core.close()
    }
}

/** Opens the bridge facade off the main thread without logging Core paths. */
suspend fun openBridgeFacade(
    context: Context,
    coreSink: DiagnosticSink,
    appVersion: String,
): Outcome<AndroidCore> = withContext(Dispatchers.IO) {
    try {
        val document = File(context.noBackupFilesDir, APP_CONFIG_FILE)
        document.parentFile?.mkdirs()
        Outcome.Success(
            AndroidCore.open(
                context = context,
                appConfigPath = document.absolutePath,
                instance = InstanceId(DEFAULT_INSTANCE_ID),
                journalSink = coreSink,
                appVersion = appVersion,
            ),
        )
    } catch (_: Throwable) {
        Outcome.Failure(
            CoreFailure(
                code = "core_open_failed",
                message = "core_open_failed",
                retryable = true,
            ),
        )
    }
}

private const val APP_CONFIG_FILE = "client/app-config.json"
private const val DEFAULT_INSTANCE_ID = "cloud-production"
