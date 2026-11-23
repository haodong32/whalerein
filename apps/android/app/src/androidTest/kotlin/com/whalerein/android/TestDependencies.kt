package com.whalerein.android

import com.whalerein.android.state.access.CaImportState
import com.whalerein.android.state.access.CloudAttempt
import com.whalerein.android.state.access.CloudAuthPort
import com.whalerein.android.state.access.CloudAuthResult
import com.whalerein.android.state.access.CloudSession
import com.whalerein.android.state.access.CredentialState
import com.whalerein.android.state.access.DirectoryHostView
import com.whalerein.android.state.access.DirectoryPort
import com.whalerein.android.state.access.ExternalAuthorization
import com.whalerein.android.state.access.ExternalEntryPort
import com.whalerein.android.state.access.ExternalTarget
import com.whalerein.android.state.access.FingerprintCheck
import com.whalerein.android.state.access.HostConnection
import com.whalerein.android.state.access.HostSelection
import com.whalerein.android.state.access.LanPermissionState
import com.whalerein.android.state.access.PairingPort
import com.whalerein.android.state.access.PairingResult
import com.whalerein.android.state.access.Profile
import com.whalerein.android.state.access.ProfileActivation
import com.whalerein.android.state.access.ProfilePort
import com.whalerein.android.state.access.ProfileStorageScope
import com.whalerein.android.state.access.ProfileValidation
import com.whalerein.android.state.access.StandaloneInvitation
import com.whalerein.android.state.access.StandaloneTarget
import com.whalerein.android.state.access.StartupPort
import com.whalerein.android.state.access.StartupSnapshot
import com.whalerein.android.state.access.TransportValidation
import com.whalerein.android.state.attachments.AttachmentAuthorization
import com.whalerein.android.state.attachments.AttachmentAuthorizationPort
import com.whalerein.android.state.attachments.AttachmentDescriptor
import com.whalerein.android.state.attachments.AttachmentFetchRequest
import com.whalerein.android.state.attachments.AttachmentPort
import com.whalerein.android.state.attachments.AttachmentScope
import com.whalerein.android.state.attachments.AttachmentStaging
import com.whalerein.android.state.attachments.DestinationWriteResult
import com.whalerein.android.state.attachments.DestinationWriter
import com.whalerein.android.state.attachments.StagingHandle
import com.whalerein.android.state.attachments.VerifiedAttachment
import com.whalerein.android.state.conversation.ApprovalDecision
import com.whalerein.android.state.conversation.AnswerRef
import com.whalerein.android.state.conversation.CommandPort
import com.whalerein.android.state.conversation.ComposerPort
import com.whalerein.android.state.conversation.DraftPort
import com.whalerein.android.state.conversation.Guard
import com.whalerein.android.state.conversation.HistoryCursor
import com.whalerein.android.state.conversation.HistoryPage
import com.whalerein.android.state.conversation.HistoryPort
import com.whalerein.android.state.conversation.ModelCatalog
import com.whalerein.android.state.conversation.ModelSelection
import com.whalerein.android.state.conversation.ModelsQueuePort
import com.whalerein.android.state.conversation.ObservationBatch
import com.whalerein.android.state.conversation.ObservationPort
import com.whalerein.android.state.conversation.PendingActionOutcome
import com.whalerein.android.state.conversation.PendingDetailResult
import com.whalerein.android.state.conversation.PendingInputPort
import com.whalerein.android.state.conversation.PendingItem
import com.whalerein.android.state.conversation.PendingListResult
import com.whalerein.android.state.conversation.QueueMutation
import com.whalerein.android.state.conversation.QueueSnapshot
import com.whalerein.android.state.conversation.RecoveryRequest
import com.whalerein.android.state.conversation.RecoveryResult
import com.whalerein.android.state.conversation.ReplyRequest
import com.whalerein.android.state.conversation.ReplyResult
import com.whalerein.android.state.conversation.SendReceipt
import com.whalerein.android.state.conversation.SendRequest
import com.whalerein.android.state.conversation.StopOutcome
import com.whalerein.android.state.conversation.WaitPort
import com.whalerein.android.state.conversation.WaitRequest
import com.whalerein.android.state.conversation.WaitResult
import com.whalerein.android.state.conversation.WatchRequest
import com.whalerein.android.state.conversation.WatchSession
import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.IdGenerator
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import com.whalerein.android.state.conversation.SessionListPort
import com.whalerein.android.state.support.AccountPort
import com.whalerein.android.state.support.AccountStatus
import com.whalerein.android.state.support.AppSessionInfo
import com.whalerein.android.state.support.CoreFailureSummary
import com.whalerein.android.state.support.CoreFailureSummaryPort
import com.whalerein.android.state.support.ForgetMode
import com.whalerein.android.state.support.ForgetReport
import com.whalerein.android.state.support.Journal
import com.whalerein.android.state.support.JournalExporter
import com.whalerein.android.state.support.JournalStore
import com.whalerein.android.state.support.JournalStoredFile
import com.whalerein.android.state.support.OperationInventory
import com.whalerein.android.state.support.OperationRecoveryRequest
import com.whalerein.android.state.support.OperationScope
import com.whalerein.android.state.support.OperationsPort
import com.whalerein.android.state.support.RecoveredOperation
import com.whalerein.android.state.support.RevocationReport
import com.whalerein.android.state.support.SignOutReport
import com.whalerein.android.state.support.ExportWriteResult

/**
 * Deterministic composition-root fake.
 *
 * Tests use the exact `:state` interfaces the production graph supplies; no
 * Core, Keystore, JNA or network is involved.
 */
class FakeAppDependencies : AppDependencies {
    val fakeObservation = FakeObservationPort()
    val fakeHistory = FakeHistoryPort()
    val fakePendingInput = FakePendingInputPort()
    val fakeAttachment = FakeAttachmentPort()
    val fakeStaging = FakeAttachmentStaging()
    val fakeAuthorization = FakeAttachmentAuthorization()
    val fakeWriter = FakeDestinationWriter()
    val fakeJournal = Journal(InMemoryJournalStore(), FixedClock())
    val fakeSink = RecordingDiagnosticSink()

    override val cloudAuth: CloudAuthPort = FakeCloudAuthPort()
    override val profiles: ProfilePort = FakeProfilePort()
    val fakeDirectory = FakeDirectoryPort()
    override val directory: DirectoryPort = fakeDirectory
    override val pairing: PairingPort = FakePairingPort()
    var startupOutcome: Outcome<StartupSnapshot> = Outcome.Success(StartupSnapshot())
    val startupPort: StartupPort = object : StartupPort {
        override suspend fun restore(instance: InstanceId): Outcome<StartupSnapshot> =
            startupOutcome
    }
    override val startup: StartupPort = startupPort
    override val externalEntry: ExternalEntryPort = object : ExternalEntryPort {
        override suspend fun currentProfile(): Outcome<Profile?> = Outcome.Success(null)

        override suspend fun authorize(target: ExternalTarget): ExternalAuthorization =
            ExternalAuthorization.Unauthorized
    }

    override val observation: ObservationPort = fakeObservation
    override val history: HistoryPort = fakeHistory
    override val composer: ComposerPort = FakeComposerPort()
    override val wait: WaitPort = object : WaitPort {
        override suspend fun wait(request: WaitRequest): WaitResult = WaitResult.TimedOut

        override suspend fun cancel(waitId: String): Outcome<Unit> = Outcome.Success(Unit)
    }
    override val commands: CommandPort = object : CommandPort {
        override suspend fun stop(scope: SessionRef, attempt: com.whalerein.android.state.conversation.AttemptRef?): StopOutcome =
            StopOutcome.Accepted
    }
    override val pendingInput: PendingInputPort = fakePendingInput
    override val drafts: DraftPort = object : DraftPort {
        override suspend fun load(scope: SessionRef): Outcome<String?> = Outcome.Success(null)
        override suspend fun save(scope: SessionRef, text: String): Outcome<Unit> = Outcome.Success(Unit)
        override suspend fun clear(scope: SessionRef): Outcome<Unit> = Outcome.Success(Unit)
        override suspend fun savePendingSend(
            scope: SessionRef,
            record: com.whalerein.android.state.conversation.PendingSendRecord,
        ): Outcome<Unit> = Outcome.Success(Unit)
        override suspend fun loadPendingSend(
            scope: SessionRef,
        ): Outcome<com.whalerein.android.state.conversation.PendingSendRecord?> =
            Outcome.Success(null)
        override suspend fun clearPendingSend(scope: SessionRef): Outcome<Unit> =
            Outcome.Success(Unit)
    }
    override val modelsQueue: ModelsQueuePort = FakeModelsQueuePort()
    override val sessionList: SessionListPort = FakeSessionListPort()

    override val operations: OperationsPort = object : OperationsPort {
        override suspend fun inventory(instance: InstanceId): Outcome<OperationInventory> =
            Outcome.Success(OperationInventory())

        override suspend fun authorization(scope: OperationScope): Outcome<com.whalerein.android.state.support.OperationAuthorization> =
            Outcome.Failure(CoreFailure("unsupported", "unsupported"))

        override suspend fun recover(request: OperationRecoveryRequest): Outcome<RecoveredOperation> =
            Outcome.Failure(CoreFailure("unsupported", "unsupported"))
    }
    override val account: AccountPort = FakeAccountPort()
    override val journal: Journal = fakeJournal
    override val journalExporter: JournalExporter = object : JournalExporter {
        override suspend fun write(fileName: String, jsonl: ByteArray): Outcome<ExportWriteResult> =
            Outcome.Success(ExportWriteResult.Completed(fileName))
    }
    override val coreFailureSummary: CoreFailureSummaryPort? = object : CoreFailureSummaryPort {
        override suspend fun lastFailure(instance: InstanceId): Outcome<CoreFailureSummary?> =
            Outcome.Success(null)

        override suspend fun clear(instance: InstanceId): Outcome<Unit> = Outcome.Success(Unit)
    }

    override val attachments: AttachmentPort = fakeAttachment
    override val attachmentStaging: AttachmentStaging = fakeStaging
    override val attachmentAuthorization: AttachmentAuthorizationPort = fakeAuthorization
    override val destinationWriter: DestinationWriter = fakeWriter

    override val clock: Clock = FixedClock()
    override val ids: IdGenerator = SequentialIds()
    override val diagnosticSink: DiagnosticSink = fakeSink
    override val coreVersion: String = "test-core"
    override val appVersion: String = "test-app"
    override val instance: InstanceId = InstanceId("test-instance")

    override fun close() = Unit
}

val TEST_SESSION = SessionRef(
    host = com.whalerein.android.state.core.HostRef(
        InstanceId("test-instance"),
        HostId("test-host"),
    ),
    sessionId = SessionId("test-session"),
)

class FixedClock(
    private var now: Long = 1_000L,
) : Clock {
    override fun epochMillis(): Long = now

    override fun elapsedMillis(): Long = now
}

class SequentialIds : IdGenerator {
    private var next = 0

    override fun newId(): String = "id-${next++}"
}

class RecordingDiagnosticSink : DiagnosticSink {
    val events = mutableListOf<com.whalerein.android.state.core.DiagnosticEvent>()

    override fun emit(event: com.whalerein.android.state.core.DiagnosticEvent) {
        events += event
    }
}

class InMemoryJournalStore : JournalStore {
    private val files = mutableMapOf<String, String>()

    override suspend fun list(): List<JournalStoredFile> = files.map { (id, text) ->
        JournalStoredFile(id, id.removePrefix("journal-").removeSuffix(".jsonl"), 0, text.length.toLong())
    }

    override suspend fun read(fileId: String): String? = files[fileId]

    override suspend fun append(fileId: String, text: String) {
        files[fileId] = (files[fileId] ?: "") + text
    }

    override suspend fun replace(fileId: String, text: String) {
        files[fileId] = text
    }

    override suspend fun delete(fileId: String) {
        files.remove(fileId)
    }
}

class FakeObservationPort : ObservationPort {
    var startCount = 0
    var stopCount = 0
    var lastSession: WatchSession? = null
    private val watches = mutableMapOf<String, kotlinx.coroutines.CompletableDeferred<Unit>>()

    override suspend fun startWatch(request: WatchRequest): Outcome<WatchSession> {
        startCount += 1
        val session = WatchSession(
            watchId = "watch-${startCount}",
            scope = request.scope,
            generation = request.generation,
            mode = request.mode,
        )
        watches[session.watchId] = kotlinx.coroutines.CompletableDeferred()
        lastSession = session
        return Outcome.Success(session)
    }

    override suspend fun pollEvents(session: WatchSession): Outcome<ObservationBatch> {
        // Stay suspended until the scoped watch is stopped: an active watch
        // must not spin the main looper or change the watch identity.
        watches[session.watchId]?.await()
        return Outcome.Success(
            ObservationBatch(
                scope = session.scope,
                generation = session.generation,
                ended = com.whalerein.android.state.conversation.WatchEndReason.Requested,
            ),
        )
    }

    override suspend fun stopWatch(session: WatchSession): Outcome<Unit> {
        stopCount += 1
        watches.remove(session.watchId)?.complete(Unit)
        return Outcome.Success(Unit)
    }
}

class FakeHistoryPort : HistoryPort {
    override suspend fun latestPage(scope: SessionRef, projection: String): Outcome<HistoryPage> =
        Outcome.Success(HistoryPage(scope, projection, emptyList()))

    override suspend fun olderPage(scope: SessionRef, cursor: HistoryCursor): Outcome<HistoryPage> =
        Outcome.Failure(CoreFailure("no_page", "no_page"))

    override suspend fun exactMessage(
        scope: SessionRef,
        projection: String,
        messageId: String,
    ): Outcome<HistoryPage> = Outcome.Failure(CoreFailure("no_page", "no_page"))

    override suspend fun cachedPage(scope: SessionRef, projection: String): Outcome<HistoryPage> =
        Outcome.Failure(CoreFailure("no_cache", "no_cache"))
}

class FakeComposerPort : ComposerPort {
    override suspend fun submit(request: SendRequest): Outcome<SendReceipt> =
        Outcome.Failure(CoreFailure("unsupported", "unsupported"))

    override suspend fun readReply(request: ReplyRequest): ReplyResult =
        ReplyResult.TimedOut

    override suspend fun recover(request: RecoveryRequest): RecoveryResult =
        RecoveryResult.StillUnconfirmed(CoreFailure("unsupported", "unsupported"))
}

class FakePendingInputPort : PendingInputPort {
    var items: List<PendingItem> = emptyList()

    override suspend fun list(scope: SessionRef): PendingListResult =
        PendingListResult.Current(items)

    override suspend fun get(scope: SessionRef, itemId: String): PendingDetailResult {
        val item = items.firstOrNull { it.itemId == itemId } ?: return PendingDetailResult.Missing
        return PendingDetailResult.Found(item)
    }

    override suspend fun resolveApproval(
        scope: SessionRef,
        approvalId: String,
        decision: ApprovalDecision,
        guard: Guard,
    ): PendingActionOutcome = PendingActionOutcome.Applied(approvalId)

    override suspend fun answerQuestion(
        scope: SessionRef,
        batchId: String,
        answers: List<com.whalerein.android.state.conversation.QuestionAnswer>,
        guard: Guard,
        answerRef: AnswerRef,
    ): PendingActionOutcome = PendingActionOutcome.Applied(batchId)

    override suspend fun cancelAnswer(scope: SessionRef, answerRef: AnswerRef): PendingActionOutcome =
        PendingActionOutcome.Applied(answerRef.answerId)
}

class FakeSessionListPort : SessionListPort {
    var snapshot: com.whalerein.android.state.conversation.SessionListSnapshot? = null
    var detail: com.whalerein.android.state.conversation.SessionDetails? = null
    var createResult: com.whalerein.android.state.conversation.CreatedSession? = null
    var forkResult: com.whalerein.android.state.conversation.ForkedSession? = null

    override suspend fun list(
        host: com.whalerein.android.state.core.HostRef,
    ): Outcome<com.whalerein.android.state.conversation.SessionListSnapshot> =
        Outcome.Success(
            snapshot ?: com.whalerein.android.state.conversation.SessionListSnapshot(
                host = host,
                sessions = emptyList(),
            ),
        )

    override suspend fun details(
        session: com.whalerein.android.state.core.SessionRef,
    ): Outcome<com.whalerein.android.state.conversation.SessionDetails> =
        Outcome.Success(
            detail ?: com.whalerein.android.state.conversation.SessionDetails(session = session),
        )

    override suspend fun create(
        host: com.whalerein.android.state.core.HostRef,
        input: com.whalerein.android.state.conversation.CreateSessionInput,
    ): Outcome<com.whalerein.android.state.conversation.CreatedSession> =
        createResult?.let { Outcome.Success(it) }
            ?: Outcome.Failure(CoreFailure("unsupported", "unsupported"))

    override suspend fun fork(
        session: com.whalerein.android.state.core.SessionRef,
        input: com.whalerein.android.state.conversation.ForkSessionInput,
    ): Outcome<com.whalerein.android.state.conversation.ForkedSession> =
        forkResult?.let { Outcome.Success(it) }
            ?: Outcome.Failure(CoreFailure("unsupported", "unsupported"))
}

class FakeModelsQueuePort : ModelsQueuePort {
    override suspend fun modelCatalog(scope: SessionRef): Outcome<ModelCatalog> =
        Outcome.Success(ModelCatalog(emptyList(), null))

    override suspend fun selectModel(
        scope: SessionRef,
        modelId: String,
        guard: Guard?,
    ): Outcome<ModelSelection> = Outcome.Success(ModelSelection(modelId))

    override suspend fun pendingQueue(scope: SessionRef): Outcome<QueueSnapshot> =
        Outcome.Success(QueueSnapshot(emptyList()))

    override suspend fun editQueueItem(
        scope: SessionRef,
        itemId: String,
        text: String,
        guard: Guard,
    ): Outcome<QueueMutation> = Outcome.Success(QueueMutation.Edited(itemId))

    override suspend fun removeQueueItem(
        scope: SessionRef,
        itemId: String,
        guard: Guard,
    ): Outcome<QueueMutation> = Outcome.Success(QueueMutation.Removed(itemId))

    override suspend fun steerQueueItem(
        scope: SessionRef,
        itemId: String,
        guard: Guard,
    ): Outcome<QueueMutation> = Outcome.Success(QueueMutation.Steered(itemId))
}

class FakeAttachmentPort : AttachmentPort {
    override suspend fun describe(scope: AttachmentScope): Outcome<AttachmentDescriptor> =
        Outcome.Success(
            AttachmentDescriptor(
                attachmentId = scope.attachmentId,
                displayName = "notes.txt",
                mediaType = "text/plain",
                sizeBytes = 5,
            ),
        )

    override suspend fun fetch(request: AttachmentFetchRequest): Outcome<VerifiedAttachment> =
        Outcome.Success(
            VerifiedAttachment(
                descriptor = AttachmentDescriptor(
                    attachmentId = request.scope.attachmentId,
                    displayName = "notes.txt",
                    mediaType = "text/plain",
                    sizeBytes = 5,
                ),
                bytes = "hello".toByteArray(),
                sha256 = "test",
            ),
        )
}

class FakeAttachmentStaging : AttachmentStaging {
    private val staged = mutableMapOf<String, ByteArray>()

    override suspend fun stage(
        scope: AttachmentScope,
        operationId: String,
        descriptor: AttachmentDescriptor,
        bytes: ByteArray,
    ): Outcome<StagingHandle> {
        val handle = StagingHandle(
            instanceId = scope.instanceId,
            operationId = operationId,
            processId = "test-process",
            token = "test-token",
            displayName = descriptor.displayName,
            sizeBytes = bytes.size.toLong(),
        )
        staged[handle.token] = bytes
        return Outcome.Success(handle)
    }

    override suspend fun read(handle: StagingHandle): Outcome<ByteArray> =
        staged[handle.token]?.let { Outcome.Success(it) }
            ?: Outcome.Failure(CoreFailure("staging_missing", "staging_missing"))

    override suspend fun exists(handle: StagingHandle): Boolean = staged.containsKey(handle.token)

    override suspend fun discard(handle: StagingHandle) {
        staged.remove(handle.token)
    }

    override suspend fun discardInstance(instanceId: InstanceId): Int = 0

    override suspend fun discardProcessesBefore(processId: String): Int = 0
}

class FakeAttachmentAuthorization : AttachmentAuthorizationPort {
    override suspend fun current(scope: AttachmentScope): Outcome<AttachmentAuthorization> =
        Outcome.Success(AttachmentAuthorization(authorized = true))
}

class FakeDestinationWriter : DestinationWriter {
    var written: Pair<String, ByteArray>? = null

    override suspend fun write(requestedName: String, bytes: ByteArray): Outcome<DestinationWriteResult> {
        written = requestedName to bytes
        return Outcome.Success(DestinationWriteResult.Completed(requestedName))
    }
}

class FakeCloudAuthPort : CloudAuthPort {
    override suspend fun pendingAttempt(instance: InstanceId): Outcome<CloudAttempt?> =
        Outcome.Success(null)

    override suspend fun start(instance: InstanceId, environment: String): Outcome<CloudAttempt> =
        Outcome.Failure(CoreFailure("unsupported", "unsupported"))

    override suspend fun complete(
        instance: InstanceId,
        attemptId: com.whalerein.android.state.core.AttemptId,
        callbackUri: String,
    ): CloudAuthResult = CloudAuthResult.Cancelled

    override suspend fun recoverLostExchange(
        instance: InstanceId,
        attemptId: com.whalerein.android.state.core.AttemptId,
    ): CloudAuthResult = CloudAuthResult.Cancelled

    override suspend fun cancel(
        instance: InstanceId,
        attemptId: com.whalerein.android.state.core.AttemptId,
    ): Outcome<Unit> = Outcome.Success(Unit)

    override suspend fun currentSession(instance: InstanceId): Outcome<CloudSession?> =
        Outcome.Success(null)
}

class FakeProfilePort : ProfilePort {
    override suspend fun list(): Outcome<List<Profile>> = Outcome.Success(emptyList())

    override suspend fun active(): Outcome<Profile?> = Outcome.Success(null)

    override suspend fun validate(profile: Profile): Outcome<ProfileValidation> =
        Outcome.Success(ProfileValidation.Valid(profile))

    override suspend fun save(profile: Profile): Outcome<Profile> = Outcome.Success(profile)

    override suspend fun activate(instance: InstanceId): Outcome<ProfileActivation> =
        Outcome.Failure(CoreFailure("unsupported", "unsupported"))

    override suspend fun remove(instance: InstanceId): Outcome<Unit> = Outcome.Success(Unit)

    override suspend fun credentialState(instance: InstanceId): CredentialState =
        CredentialState.Absent

    override suspend fun storageScope(instance: InstanceId): Outcome<ProfileStorageScope> =
        Outcome.Success(ProfileStorageScope(instance, backupExcluded = true))
}

class FakeDirectoryPort : DirectoryPort {
    var listOutcome: Outcome<List<DirectoryHostView>> = Outcome.Success(emptyList())
    var sessionOutcome: Outcome<CloudSession?> = Outcome.Success(null)

    override suspend fun list(instance: InstanceId): Outcome<List<DirectoryHostView>> =
        listOutcome

    override suspend fun connect(instance: InstanceId, hostId: HostId): Outcome<HostConnection> =
        Outcome.Failure(CoreFailure("unsupported", "unsupported"))

    override suspend fun selectDefault(instance: InstanceId, hostId: HostId): Outcome<HostSelection> =
        Outcome.Success(HostSelection(hostId))

    override suspend fun currentSession(instance: InstanceId): Outcome<CloudSession?> =
        sessionOutcome
}

class FakePairingPort : PairingPort {
    override suspend fun reviewInvitation(encoded: String): Outcome<StandaloneInvitation> =
        Outcome.Failure(CoreFailure("unsupported", "unsupported"))

    override suspend fun checkFingerprint(
        instance: InstanceId,
        presented: String,
    ): Outcome<FingerprintCheck> = Outcome.Success(FingerprintCheck.Match)

    override suspend fun importCa(instance: InstanceId, pem: String): Outcome<CaImportState> =
        Outcome.Success(CaImportState.NotRequired)

    override suspend fun decide(instance: InstanceId, accept: Boolean): Outcome<PairingResult> =
        Outcome.Failure(CoreFailure("unsupported", "unsupported"))

    override suspend fun lanPermission(): LanPermissionState = LanPermissionState.NotRequired

    override suspend fun requestLanPermission(): LanPermissionState = LanPermissionState.NotRequired

    override suspend fun validateTarget(
        instance: InstanceId,
        target: StandaloneTarget,
    ): Outcome<TransportValidation> = Outcome.Success(TransportValidation.Valid)
}

class FakeAccountPort : AccountPort {
    override suspend fun status(instance: InstanceId): Outcome<AccountStatus> =
        Outcome.Success(AccountStatus(com.whalerein.android.state.support.CloudAccountState.SignedOut))

    override suspend fun signOut(
        instance: InstanceId,
        connection: com.whalerein.android.state.support.AccountConnection,
    ): Outcome<SignOutReport> = Outcome.Failure(CoreFailure("unsupported", "unsupported"))

    override suspend fun listAppSessions(instance: InstanceId): Outcome<List<AppSessionInfo>> =
        Outcome.Success(emptyList())

    override suspend fun revokeAppSession(
        instance: InstanceId,
        sessionId: String?,
    ): Outcome<RevocationReport> = Outcome.Failure(CoreFailure("unsupported", "unsupported"))

    override suspend fun revokeCloudHostGrant(
        instance: InstanceId,
        hostId: HostId,
    ): Outcome<RevocationReport> = Outcome.Failure(CoreFailure("unsupported", "unsupported"))

    override suspend fun revokeStandaloneDevice(
        instance: InstanceId,
        deviceId: String,
    ): Outcome<RevocationReport> = Outcome.Failure(CoreFailure("unsupported", "unsupported"))

    override suspend fun forget(instance: InstanceId, mode: ForgetMode): Outcome<ForgetReport> =
        Outcome.Failure(CoreFailure("unsupported", "unsupported"))
}
