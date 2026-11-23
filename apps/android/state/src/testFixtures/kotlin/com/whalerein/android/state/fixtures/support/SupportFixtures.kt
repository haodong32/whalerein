package com.whalerein.android.state.fixtures.support

import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SendId
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import com.whalerein.android.state.support.AccountConnection
import com.whalerein.android.state.support.AccountPort
import com.whalerein.android.state.support.AccountRevocableTargets
import com.whalerein.android.state.support.AccountStatus
import com.whalerein.android.state.support.AppSessionInfo
import com.whalerein.android.state.support.CloudAccountState
import com.whalerein.android.state.support.CoreFailureSummary
import com.whalerein.android.state.support.CoreFailureSummaryPort
import com.whalerein.android.state.support.ExportWriteResult
import com.whalerein.android.state.support.ForgetMode
import com.whalerein.android.state.support.ForgetReport
import com.whalerein.android.state.support.JournalExporter
import com.whalerein.android.state.support.JournalStore
import com.whalerein.android.state.support.JournalStoreException
import com.whalerein.android.state.support.OperationAuthorization
import com.whalerein.android.state.support.OperationAuthorizationState
import com.whalerein.android.state.support.OperationInventory
import com.whalerein.android.state.support.OperationRecoveryRequest
import com.whalerein.android.state.support.OperationScope
import com.whalerein.android.state.support.OperationsPort
import com.whalerein.android.state.support.PairingHistoryEntry
import com.whalerein.android.state.support.RecoveredOperation
import com.whalerein.android.state.support.ReplyRef
import com.whalerein.android.state.support.RetainedOperation
import com.whalerein.android.state.support.RetainedOperationKind
import com.whalerein.android.state.support.OperationStatus
import com.whalerein.android.state.support.RevocationReport
import com.whalerein.android.state.support.SendReceipt
import com.whalerein.android.state.support.SendReceiptStatus
import com.whalerein.android.state.support.SendRecoveryPort
import com.whalerein.android.state.support.SendRequest
import com.whalerein.android.state.support.SignOutReport

class FakeClock(
    var nowMillis: Long = 1_700_000_000_000L,
    var elapsed: Long = 0L,
) : Clock {
    override fun epochMillis(): Long = nowMillis

    override fun elapsedMillis(): Long = elapsed

    fun advance(millis: Long) {
        nowMillis += millis
        elapsed += millis
    }
}

class FakeJournalStore(initial: Map<String, String> = emptyMap()) : JournalStore {
    private val files = LinkedHashMap<String, String>()
    private var modified = 0L

    var failReads: Boolean = false
    var failAppends: Boolean = false
    var failReplaces: Boolean = false
    var failDeletes: Boolean = false
    var appendCount: Int = 0
        private set

    init {
        files.putAll(initial)
    }

    override suspend fun list(): List<com.whalerein.android.state.support.JournalStoredFile> {
        modified += 1
        return files.map { (fileId, text) ->
            com.whalerein.android.state.support.JournalStoredFile(
                fileId = fileId,
                scopeId = scopeOf(fileId),
                modifiedEpochMillis = modified,
                sizeBytes = text.toByteArray(Charsets.UTF_8).size.toLong(),
            )
        }
    }

    override suspend fun read(fileId: String): String? {
        if (failReads) throw JournalStoreException("read failed")
        return files[fileId]
    }

    override suspend fun append(fileId: String, text: String) {
        appendCount += 1
        if (failAppends) throw JournalStoreException("append failed")
        files[fileId] = (files[fileId] ?: "") + text
    }

    override suspend fun replace(fileId: String, text: String) {
        if (failReplaces) throw JournalStoreException("replace failed")
        files[fileId] = text
    }

    override suspend fun delete(fileId: String) {
        if (failDeletes) throw JournalStoreException("delete failed")
        files.remove(fileId)
    }

    fun raw(fileId: String): String? = files[fileId]

    fun put(fileId: String, text: String) {
        files[fileId] = text
    }

    fun fileIds(): Set<String> = files.keys.toSet()

    companion object {
        fun fileId(scopeId: String): String = "journal-$scopeId.jsonl"

        fun scopeOf(fileId: String): String =
            fileId.removePrefix("journal-").removeSuffix(".jsonl")
    }
}

class FakeJournalExporter(
    var outcome: Outcome<ExportWriteResult> = Outcome.Success(
        ExportWriteResult.Completed("whalerein-diagnostics.jsonl"),
    ),
) : JournalExporter {
    val writes = mutableListOf<Pair<String, ByteArray>>()

    override suspend fun write(fileName: String, jsonl: ByteArray): Outcome<ExportWriteResult> {
        writes += fileName to jsonl.copyOf()
        return outcome
    }
}

class FakeCoreFailureSummaryPort(
    var summary: CoreFailureSummary? = null,
    var failureOutcome: Outcome<CoreFailureSummary?>? = null,
    var clearOutcome: Outcome<Unit> = Outcome.Success(Unit),
) : CoreFailureSummaryPort {
    var clearCount: Int = 0
        private set

    override suspend fun lastFailure(instance: InstanceId): Outcome<CoreFailureSummary?> =
        failureOutcome ?: Outcome.Success(summary)

    override suspend fun clear(instance: InstanceId): Outcome<Unit> {
        clearCount += 1
        return clearOutcome
    }
}

class FakeOperationsPort(
    var inventoryOutcome: Outcome<OperationInventory> = Outcome.Success(OperationInventory()),
    var authorizationOutcome: Outcome<OperationAuthorization> =
        Outcome.Success(OperationAuthorization(OperationAuthorizationState.Authorized)),
    val recoverOutcomes: ArrayDeque<Outcome<RecoveredOperation>> = ArrayDeque(),
) : OperationsPort {
    var inventoryCalls: Int = 0
        private set
    var duringRecover: (suspend () -> Unit)? = null
    val authorizationScopes = mutableListOf<OperationScope>()
    val recoveryRequests = mutableListOf<OperationRecoveryRequest>()

    override suspend fun inventory(instance: InstanceId): Outcome<OperationInventory> {
        inventoryCalls += 1
        return inventoryOutcome
    }

    override suspend fun authorization(scope: OperationScope): Outcome<OperationAuthorization> {
        authorizationScopes += scope
        return authorizationOutcome
    }

    override suspend fun recover(request: OperationRecoveryRequest): Outcome<RecoveredOperation> {
        recoveryRequests += request
        duringRecover?.invoke()
        return recoverOutcomes.removeFirstOrNull()
            ?: Outcome.Success(RecoveredOperation.Confirmed(request.operation.operationId))
    }
}

class FakeAccountPort(
    var statusOutcome: Outcome<AccountStatus> = Outcome.Success(
        AccountStatus(
            state = CloudAccountState.SignedIn,
            accountId = "account-a",
            workspace = "workspace-a",
            connection = AccountConnection.Online,
        ),
    ),
    var signOutOutcome: Outcome<SignOutReport> = Outcome.Success(
        SignOutReport(
            localSignedOut = true,
            remotePending = false,
            connection = AccountConnection.Online,
        ),
    ),
    var sessionsOutcome: Outcome<List<AppSessionInfo>> = Outcome.Success(emptyList()),
    var targetsOutcome: Outcome<AccountRevocableTargets> = Outcome.Success(
        AccountRevocableTargets(),
    ),
    var revokeOutcome: Outcome<RevocationReport>? = null,
    var forgetOutcome: Outcome<ForgetReport> = Outcome.Success(
        ForgetReport(ForgetMode.Ordinary, forgotten = true, remoteAuthorityRetained = false),
    ),
) : AccountPort {
    var signOutCalls: Int = 0
        private set
    val signOutConnections = mutableListOf<AccountConnection>()
    var listSessionsCalls: Int = 0
        private set
    var listTargetsCalls: Int = 0
        private set
    val revokeSessionIds = mutableListOf<String?>()
    val revokedHostIds = mutableListOf<HostId>()
    val revokedDeviceIds = mutableListOf<String>()
    var currentDeviceRevocations: Int = 0
        private set
    val forgetModes = mutableListOf<ForgetMode>()

    override suspend fun status(instance: InstanceId): Outcome<AccountStatus> = statusOutcome

    override suspend fun signOut(
        instance: InstanceId,
        connection: AccountConnection,
    ): Outcome<SignOutReport> {
        signOutCalls += 1
        signOutConnections += connection
        return signOutOutcome
    }

    override suspend fun listAppSessions(instance: InstanceId): Outcome<List<AppSessionInfo>> {
        listSessionsCalls += 1
        return sessionsOutcome
    }

    override suspend fun revocableTargets(
        instance: InstanceId,
    ): Outcome<AccountRevocableTargets> {
        listTargetsCalls += 1
        return targetsOutcome
    }

    override suspend fun revokeAppSession(
        instance: InstanceId,
        sessionId: String?,
    ): Outcome<RevocationReport> {
        revokeSessionIds += sessionId
        return revokeOutcome ?: Outcome.Success(
            RevocationReport(
                scope = sessionId?.let { com.whalerein.android.state.support.RevocationScope.AppSession(it) }
                    ?: com.whalerein.android.state.support.RevocationScope.AllAppSessions,
            ),
        )
    }

    override suspend fun revokeCloudHostGrant(
        instance: InstanceId,
        hostId: HostId,
    ): Outcome<RevocationReport> {
        revokedHostIds += hostId
        return revokeOutcome ?: Outcome.Success(
            RevocationReport(
                scope = com.whalerein.android.state.support.RevocationScope.CloudHostGrant(hostId),
            ),
        )
    }

    override suspend fun revokeCurrentDevice(
        instance: InstanceId,
    ): Outcome<RevocationReport> {
        currentDeviceRevocations += 1
        return revokeOutcome ?: Outcome.Success(
            RevocationReport(
                scope = com.whalerein.android.state.support.RevocationScope.CurrentDevice,
            ),
        )
    }

    override suspend fun revokeStandaloneDevice(
        instance: InstanceId,
        deviceId: String,
    ): Outcome<RevocationReport> {
        revokedDeviceIds += deviceId
        return revokeOutcome ?: Outcome.Success(
            RevocationReport(
                scope = com.whalerein.android.state.support.RevocationScope.StandaloneDevice(deviceId),
            ),
        )
    }

    override suspend fun forget(
        instance: InstanceId,
        mode: ForgetMode,
    ): Outcome<ForgetReport> {
        forgetModes += mode
        return forgetOutcome
    }
}

class FakeSendRecoveryPort(
    var submitOutcome: Outcome<SendReceipt> = Outcome.Failure(
        CoreFailure("not_scripted", "No submission scripted."),
    ),
    val replyOutcomes: ArrayDeque<Outcome<String>> = ArrayDeque(),
    var duringSubmit: (suspend () -> Unit)? = null,
) : SendRecoveryPort {
    var submitCount: Int = 0
        private set
    var replyReadCount: Int = 0
        private set
    val submitRequests = mutableListOf<SendRequest>()
    val replyRequests = mutableListOf<ReplyRef>()

    override suspend fun submit(request: SendRequest): Outcome<SendReceipt> {
        submitCount += 1
        submitRequests += request
        duringSubmit?.invoke()
        return submitOutcome
    }

    override suspend fun readReply(ref: ReplyRef): Outcome<String> {
        replyReadCount += 1
        replyRequests += ref
        return replyOutcomes.removeFirstOrNull()
            ?: Outcome.Failure(CoreFailure("reply_not_scripted", "No reply scripted."))
    }
}

fun testSessionRef(instance: String = "instance-a", host: String = "host-a"): SessionRef =
    SessionRef(
        host = com.whalerein.android.state.core.HostRef(
            instance = InstanceId(instance),
            hostId = HostId(host),
        ),
        sessionId = SessionId("session-a"),
    )

fun retainedOperation(
    operationId: String,
    kind: RetainedOperationKind = RetainedOperationKind.Send,
    status: OperationStatus = OperationStatus.Pending,
    scope: OperationScope = OperationScope(instanceId = InstanceId("instance-a")),
    label: String = "operation",
): RetainedOperation = RetainedOperation(
    operationId = operationId,
    kind = kind,
    status = status,
    scope = scope,
    createdAtEpochMillis = 1_000L,
    updatedAtEpochMillis = 1_000L,
    label = label,
)

fun pairingHistory(
    pairingId: String,
    kind: RetainedOperationKind = RetainedOperationKind.StandalonePairing,
): PairingHistoryEntry = PairingHistoryEntry(
    pairingId = pairingId,
    kind = kind,
    outcome = OperationStatus.Confirmed,
    scope = OperationScope(instanceId = InstanceId("instance-a")),
    recordedAtEpochMillis = 900L,
    hostLabel = "host-a",
)

fun confirmedSendReceipt(
    ref: ReplyRef,
    sendId: SendId = ref.sendId,
): SendReceipt = SendReceipt(
    sendId = sendId,
    status = SendReceiptStatus.Confirmed,
    replyRef = ref,
)

fun warnEvent(
    event: String,
    stage: String,
    code: String? = null,
    fields: Map<String, String> = emptyMap(),
    instanceId: String = "instance-a",
) = com.whalerein.android.state.core.DiagnosticEvent(
    level = DiagnosticLevel.Warn,
    event = event,
    stage = stage,
    code = code,
    instanceId = instanceId,
    fields = fields,
)
