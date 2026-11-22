package com.whalerein.android

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
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.IdGenerator
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.support.AccountPort
import com.whalerein.android.state.support.CoreFailureSummaryPort
import com.whalerein.android.state.support.Journal
import com.whalerein.android.state.support.JournalExporter
import com.whalerein.android.state.support.OperationsPort

/**
 * Composition-root contract for the whole app.
 *
 * Every property is a `:state` port or a shared `:state` type. Screens are
 * constructed only from this interface, so instrumentation fixtures can build a
 * deterministic fake graph without Core, Android Keystore or the native bridge.
 *
 * Production is supplied by [BridgeAppDependencies], which wraps the
 * `:core-bridge` facade. `:app` never imports generated UniFFI types or JNA.
 */
interface AppDependencies : AutoCloseable {
    // Access: six typed ports.
    val cloudAuth: CloudAuthPort
    val profiles: ProfilePort
    val directory: DirectoryPort
    val pairing: PairingPort
    val startup: StartupPort
    val externalEntry: ExternalEntryPort

    // Conversation: eight typed ports.
    val observation: ObservationPort
    val history: HistoryPort
    val composer: ComposerPort
    val wait: WaitPort
    val commands: CommandPort
    val pendingInput: PendingInputPort
    val drafts: DraftPort
    val modelsQueue: ModelsQueuePort
    val sessionList: SessionListPort

    // Support: operations, account, the scoped journal and optional Core summary.
    val operations: OperationsPort
    val account: AccountPort
    val journal: Journal
    val journalExporter: JournalExporter
    val coreFailureSummary: CoreFailureSummaryPort?

    // Attachments: transfer, private staging, authorization and the picker writer.
    val attachments: AttachmentPort
    val attachmentStaging: AttachmentStaging
    val attachmentAuthorization: AttachmentAuthorizationPort
    val destinationWriter: DestinationWriter

    // Shared infrastructure and provenance.
    val clock: Clock
    val ids: IdGenerator
    val diagnosticSink: DiagnosticSink
    val coreVersion: String
    val appVersion: String

    /** The Core instance this graph is currently bound to. */
    val instance: InstanceId
}
