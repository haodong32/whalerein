package com.whalerein.android.corebridge.ports

import com.whalerein.android.state.access.CloudAuthPort
import com.whalerein.android.state.access.DirectoryPort
import com.whalerein.android.state.access.ExternalEntryPort
import com.whalerein.android.state.access.PairingPort
import com.whalerein.android.state.access.ProfilePort
import com.whalerein.android.state.access.StartupPort
import com.whalerein.android.state.attachments.AttachmentAuthorizationPort
import com.whalerein.android.state.attachments.AttachmentPort
import com.whalerein.android.state.attachments.AttachmentStaging
import com.whalerein.android.state.conversation.CommandPort
import com.whalerein.android.state.conversation.ComposerPort
import com.whalerein.android.state.conversation.DraftPort
import com.whalerein.android.state.conversation.HistoryPort
import com.whalerein.android.state.conversation.ModelsQueuePort
import com.whalerein.android.state.conversation.ObservationPort
import com.whalerein.android.state.conversation.PendingInputPort
import com.whalerein.android.state.conversation.SessionListPort
import com.whalerein.android.state.conversation.WaitPort
import com.whalerein.android.state.support.AccountPort
import com.whalerein.android.state.support.CoreFailureSummaryPort
import com.whalerein.android.state.support.OperationsPort
import com.whalerein.android.state.support.SendRecoveryPort

data class AccessPorts(
    val cloudAuth: CloudAuthPort,
    val profiles: ProfilePort,
    val directory: DirectoryPort,
    val pairing: PairingPort,
    val startup: StartupPort,
    val externalEntry: ExternalEntryPort,
)

data class ConversationPorts(
    val observation: ObservationPort,
    val history: HistoryPort,
    val composer: ComposerPort,
    val wait: WaitPort,
    val command: CommandPort,
    val modelsQueue: ModelsQueuePort,
    val pendingInput: PendingInputPort,
    val draft: DraftPort,
    val sessionList: SessionListPort,
)

data class SupportPorts(
    val operations: OperationsPort,
    val account: AccountPort,
    val failureSummary: CoreFailureSummaryPort,
    val sendRecovery: SendRecoveryPort,
)

data class AttachmentPorts(
    val attachments: AttachmentPort,
    val staging: AttachmentStaging,
    val authorization: AttachmentAuthorizationPort,
)
