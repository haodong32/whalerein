package com.whalerein.android.state.fixtures.conversation

import com.whalerein.android.state.conversation.AnswerRef
import com.whalerein.android.state.conversation.ApprovalDecision
import com.whalerein.android.state.conversation.CommandPort
import com.whalerein.android.state.conversation.CreateSessionInput
import com.whalerein.android.state.conversation.CreatedSession
import com.whalerein.android.state.conversation.ForkSessionInput
import com.whalerein.android.state.conversation.ForkedSession
import com.whalerein.android.state.conversation.ComposerPort
import com.whalerein.android.state.conversation.DraftPort
import com.whalerein.android.state.conversation.Guard
import com.whalerein.android.state.conversation.HistoryCursor
import com.whalerein.android.state.conversation.HistoryPage
import com.whalerein.android.state.conversation.HistoryPort
import com.whalerein.android.state.conversation.ModelCatalog
import com.whalerein.android.state.conversation.ModelInfo
import com.whalerein.android.state.conversation.ModelSelection
import com.whalerein.android.state.conversation.ModelsQueuePort
import com.whalerein.android.state.conversation.ObservationBatch
import com.whalerein.android.state.conversation.ObservationPort
import com.whalerein.android.state.conversation.PendingActionOutcome
import com.whalerein.android.state.conversation.PendingDetailResult
import com.whalerein.android.state.conversation.PendingInputPort
import com.whalerein.android.state.conversation.PendingListResult
import com.whalerein.android.state.conversation.PendingSendRecord
import com.whalerein.android.state.conversation.QueueMutation
import com.whalerein.android.state.conversation.QueueSnapshot
import com.whalerein.android.state.conversation.QuestionAnswer
import com.whalerein.android.state.conversation.RecoveryRequest
import com.whalerein.android.state.conversation.RecoveryResult
import com.whalerein.android.state.conversation.ReplyRequest
import com.whalerein.android.state.conversation.ReplyResult
import com.whalerein.android.state.conversation.SendReceipt
import com.whalerein.android.state.conversation.SessionDetails
import com.whalerein.android.state.conversation.SessionListPort
import com.whalerein.android.state.conversation.SessionListSnapshot
import com.whalerein.android.state.conversation.SendRequest
import com.whalerein.android.state.conversation.StopOutcome
import com.whalerein.android.state.conversation.WaitPort
import com.whalerein.android.state.conversation.WaitRequest
import com.whalerein.android.state.conversation.WaitResult
import com.whalerein.android.state.conversation.WatchRequest
import com.whalerein.android.state.conversation.WatchSession
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SendId
import com.whalerein.android.state.core.SessionRef
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.channels.Channel

/**
 * Controllable observation fake.
 *
 * `pollEvents` suspends on a channel, mirroring the long-poll port. Tests push
 * batches with [send] and inspect [started]/[stopped] to assert one scoped watch.
 */
class FakeObservationPort : ObservationPort {
    val started = mutableListOf<WatchRequest>()
    val stopped = mutableListOf<WatchSession>()
    var startFailure: CoreFailure? = null
    var startThrowable: Throwable? = null
    var pollFailure: CoreFailure? = null
    var pollUnconfirmed: CoreFailure? = null
    var pollCancelled: Boolean = false
    var pollThrowable: Throwable? = null
    private var nextWatch = 0
    private val batches = Channel<ObservationBatch>(Channel.UNLIMITED)

    override suspend fun startWatch(request: WatchRequest): Outcome<WatchSession> {
        started += request
        startThrowable?.let { throw it }
        startFailure?.let { return Outcome.Failure(it) }
        nextWatch += 1
        return Outcome.Success(
            WatchSession(
                watchId = "watch-$nextWatch",
                scope = request.scope,
                generation = request.generation,
                mode = request.mode,
            ),
        )
    }

    override suspend fun pollEvents(session: WatchSession): Outcome<ObservationBatch> {
        pollThrowable?.let { throw it }
        pollFailure?.let { return Outcome.Failure(it) }
        pollUnconfirmed?.let { return Outcome.Unconfirmed(it) }
        if (pollCancelled) {
            return Outcome.Cancelled
        }
        return Outcome.Success(batches.receive())
    }

    override suspend fun stopWatch(session: WatchSession): Outcome<Unit> {
        stopped += session
        return Outcome.Success(Unit)
    }

    fun send(batch: ObservationBatch) {
        batches.trySend(batch)
    }
}

class FakeHistoryPort : HistoryPort {
    var latest: Outcome<HistoryPage> =
        Outcome.Success(ConversationFixtures.page(emptyList()))
    var older: Outcome<HistoryPage> =
        Outcome.Success(ConversationFixtures.page(emptyList()))
    var exact: Outcome<HistoryPage> = Outcome.Success(
        ConversationFixtures.page(emptyList(), exact = true),
    )
    var cached: Outcome<HistoryPage> = Outcome.Success(
        ConversationFixtures.page(emptyList(), cached = true),
    )

    var throwable: Throwable? = null

    val latestCalls = mutableListOf<SessionRef>()
    val olderCalls = mutableListOf<HistoryCursor>()
    val exactCalls = mutableListOf<Pair<SessionRef, String>>()
    val cachedCalls = mutableListOf<SessionRef>()

    override suspend fun latestPage(
        scope: SessionRef,
        projection: String,
    ): Outcome<HistoryPage> {
        latestCalls += scope
        throwable?.let { throw it }
        return latest
    }

    override suspend fun olderPage(
        scope: SessionRef,
        cursor: HistoryCursor,
    ): Outcome<HistoryPage> {
        olderCalls += cursor
        throwable?.let { throw it }
        return older
    }

    override suspend fun exactMessage(
        scope: SessionRef,
        projection: String,
        messageId: String,
    ): Outcome<HistoryPage> {
        exactCalls += scope to messageId
        throwable?.let { throw it }
        return exact
    }

    override suspend fun cachedPage(
        scope: SessionRef,
        projection: String,
    ): Outcome<HistoryPage> {
        cachedCalls += scope
        throwable?.let { throw it }
        return cached
    }
}

class FakeComposerPort : ComposerPort {
    val submits = mutableListOf<SendRequest>()
    val replyRequests = mutableListOf<ReplyRequest>()
    val recoveryRequests = mutableListOf<RecoveryRequest>()
    var submitOutcome: Outcome<SendReceipt>? = null
    var submitGate: CompletableDeferred<Unit>? = null
    val submitGates = ArrayDeque<CompletableDeferred<Unit>>()
    var submitThrowable: Throwable? = null
    var replyThrowable: Throwable? = null
    var replyGate: CompletableDeferred<Unit>? = null
    var recoveryThrowable: Throwable? = null
    var replyResult: ReplyResult = ReplyResult.TimedOut
    var recoveryResult: RecoveryResult =
        RecoveryResult.StillUnconfirmed(CoreFailure("unconfirmed", "unknown"))

    override suspend fun submit(request: SendRequest): Outcome<SendReceipt> {
        submits += request
        submitGate?.await()
        submitGates.removeFirstOrNull()?.await()
        submitThrowable?.let { throw it }
        return submitOutcome ?: Outcome.Success(
            SendReceipt(
                sendId = request.sendId,
                attempt = ConversationFixtures.attempt("attempt-${request.sendId.value}"),
            ),
        )
    }

    override suspend fun readReply(request: ReplyRequest): ReplyResult {
        replyRequests += request
        replyGate?.await()
        replyThrowable?.let { throw it }
        return replyResult
    }

    override suspend fun recover(request: RecoveryRequest): RecoveryResult {
        recoveryRequests += request
        recoveryThrowable?.let { throw it }
        return recoveryResult
    }
}

class FakeWaitPort : WaitPort {
    val requests = mutableListOf<WaitRequest>()
    val cancels = mutableListOf<String>()
    var result: WaitResult = WaitResult.Quiescent
    var gate: CompletableDeferred<Unit>? = null
    var throwable: Throwable? = null

    override suspend fun wait(request: WaitRequest): WaitResult {
        requests += request
        gate?.await()
        throwable?.let { throw it }
        return result
    }

    override suspend fun cancel(waitId: String): Outcome<Unit> {
        cancels += waitId
        return Outcome.Success(Unit)
    }
}

class FakeCommandPort : CommandPort {
    val stops = mutableListOf<com.whalerein.android.state.conversation.AttemptRef?>()
    var outcome: StopOutcome = StopOutcome.Confirmed
    var gate: CompletableDeferred<Unit>? = null
    var throwable: Throwable? = null

    override suspend fun stop(
        scope: SessionRef,
        attempt: com.whalerein.android.state.conversation.AttemptRef?,
    ): StopOutcome {
        stops += attempt
        gate?.await()
        throwable?.let { throw it }
        return outcome
    }
}

class FakeModelsQueuePort : ModelsQueuePort {
    var catalog: Outcome<ModelCatalog> = Outcome.Success(
        ModelCatalog(
            models = listOf(
                ModelInfo("model-a", "Model A", current = true),
                ModelInfo("model-b", "Model B"),
            ),
            currentModelId = "model-a",
            guard = Guard("catalog-guard"),
        ),
    )
    var queue: Outcome<QueueSnapshot> = Outcome.Success(
        QueueSnapshot(items = listOf(ConversationFixtures.queueItem())),
    )
    var selectOutcome: Outcome<ModelSelection>? = null
    var editOutcome: Outcome<QueueMutation>? = null
    var removeOutcome: Outcome<QueueMutation>? = null
    var steerOutcome: Outcome<QueueMutation>? = null
    var catalogThrowable: Throwable? = null
    var queueThrowable: Throwable? = null
    var selectThrowable: Throwable? = null
    var mutationThrowable: Throwable? = null

    val selectCalls = mutableListOf<Pair<String, Guard?>>()
    val operationScopes = mutableListOf<SessionRef>()
    val editCalls = mutableListOf<Triple<String, String, Guard>>()
    val removeCalls = mutableListOf<Pair<String, Guard>>()
    val steerCalls = mutableListOf<Pair<String, Guard>>()
    val queueCalls = mutableListOf<SessionRef>()

    override suspend fun modelCatalog(scope: SessionRef): Outcome<ModelCatalog> {
        catalogThrowable?.let { throw it }
        return catalog
    }

    override suspend fun selectModel(
        scope: SessionRef,
        modelId: String,
        guard: Guard?,
    ): Outcome<ModelSelection> {
        operationScopes += scope
        selectCalls += modelId to guard
        selectThrowable?.let { throw it }
        return selectOutcome ?: Outcome.Success(ModelSelection(modelId))
    }

    override suspend fun pendingQueue(scope: SessionRef): Outcome<QueueSnapshot> {
        queueCalls += scope
        queueThrowable?.let { throw it }
        return queue
    }

    override suspend fun editQueueItem(
        scope: SessionRef,
        itemId: String,
        text: String,
        guard: Guard,
    ): Outcome<QueueMutation> {
        operationScopes += scope
        editCalls += Triple(itemId, text, guard)
        mutationThrowable?.let { throw it }
        return editOutcome ?: Outcome.Success(QueueMutation.Edited(itemId))
    }

    override suspend fun removeQueueItem(
        scope: SessionRef,
        itemId: String,
        guard: Guard,
    ): Outcome<QueueMutation> {
        operationScopes += scope
        removeCalls += itemId to guard
        mutationThrowable?.let { throw it }
        return removeOutcome ?: Outcome.Success(QueueMutation.Removed(itemId))
    }

    override suspend fun steerQueueItem(
        scope: SessionRef,
        itemId: String,
        guard: Guard,
    ): Outcome<QueueMutation> {
        operationScopes += scope
        steerCalls += itemId to guard
        mutationThrowable?.let { throw it }
        return steerOutcome ?: Outcome.Success(QueueMutation.Steered(itemId))
    }
}

data class AnswerCall(
    val batchId: String,
    val answers: List<QuestionAnswer>,
    val guard: Guard,
    val answerRef: AnswerRef,
)

class FakePendingInputPort : PendingInputPort {
    var listResult: PendingListResult = PendingListResult.Current(emptyList())
    var detailResult: PendingDetailResult = PendingDetailResult.Missing
    var approvalOutcome: PendingActionOutcome = PendingActionOutcome.Applied("")
    var answerOutcome: PendingActionOutcome = PendingActionOutcome.Applied("")
    var cancelOutcome: PendingActionOutcome = PendingActionOutcome.Applied("")
    var answerGate: CompletableDeferred<Unit>? = null
    var listThrowable: Throwable? = null
    var getThrowable: Throwable? = null
    var resolveThrowable: Throwable? = null
    var answerThrowable: Throwable? = null
    var cancelThrowable: Throwable? = null

    val listCalls = mutableListOf<SessionRef>()
    val getCalls = mutableListOf<String>()
    val resolveCalls = mutableListOf<Triple<String, ApprovalDecision, Guard>>()
    val answerCalls = mutableListOf<AnswerCall>()
    val cancelCalls = mutableListOf<AnswerRef>()

    override suspend fun list(scope: SessionRef): PendingListResult {
        listCalls += scope
        listThrowable?.let { throw it }
        return listResult
    }

    override suspend fun get(scope: SessionRef, itemId: String): PendingDetailResult {
        getCalls += itemId
        getThrowable?.let { throw it }
        return detailResult
    }

    override suspend fun resolveApproval(
        scope: SessionRef,
        approvalId: String,
        decision: ApprovalDecision,
        guard: Guard,
    ): PendingActionOutcome {
        resolveCalls += Triple(approvalId, decision, guard)
        resolveThrowable?.let { throw it }
        return approvalOutcome
    }

    override suspend fun answerQuestion(
        scope: SessionRef,
        batchId: String,
        answers: List<QuestionAnswer>,
        guard: Guard,
        answerRef: AnswerRef,
    ): PendingActionOutcome {
        answerCalls += AnswerCall(batchId, answers, guard, answerRef)
        answerGate?.await()
        answerThrowable?.let { throw it }
        return answerOutcome
    }

    override suspend fun cancelAnswer(
        scope: SessionRef,
        answerRef: AnswerRef,
    ): PendingActionOutcome {
        cancelCalls += answerRef
        cancelThrowable?.let { throw it }
        return cancelOutcome
    }
}

class FakeDraftPort : DraftPort {
    val drafts = mutableMapOf<SessionRef, String>()
    val pendingSends = mutableMapOf<SessionRef, PendingSendRecord>()
    var loadFailure: CoreFailure? = null
    var pendingSendSaveGate: CompletableDeferred<Unit>? = null
    var pendingSendSaveFailure: CoreFailure? = null

    override suspend fun load(scope: SessionRef): Outcome<String?> {
        loadFailure?.let { return Outcome.Failure(it) }
        return Outcome.Success(drafts[scope])
    }

    override suspend fun save(scope: SessionRef, text: String): Outcome<Unit> {
        drafts[scope] = text
        return Outcome.Success(Unit)
    }

    override suspend fun clear(scope: SessionRef): Outcome<Unit> {
        drafts.remove(scope)
        return Outcome.Success(Unit)
    }

    override suspend fun savePendingSend(
        scope: SessionRef,
        record: PendingSendRecord,
    ): Outcome<Unit> {
        pendingSendSaveGate?.await()
        pendingSendSaveFailure?.let { return Outcome.Failure(it) }
        pendingSends[scope] = record
        return Outcome.Success(Unit)
    }

    override suspend fun loadPendingSend(scope: SessionRef): Outcome<PendingSendRecord?> =
        Outcome.Success(pendingSends[scope])

    override suspend fun clearPendingSend(scope: SessionRef): Outcome<Unit> {
        pendingSends.remove(scope)
        return Outcome.Success(Unit)
    }
}

class FakeSessionListPort : SessionListPort {
    var listOutcome: Outcome<SessionListSnapshot>? = null
    var detailsOutcome: Outcome<SessionDetails>? = null
    var createOutcome: Outcome<CreatedSession>? = null
    var forkOutcome: Outcome<ForkedSession>? = null
    var listThrowable: Throwable? = null
    var detailsThrowable: Throwable? = null
    var createThrowable: Throwable? = null
    var forkThrowable: Throwable? = null
    var createGate: CompletableDeferred<Unit>? = null
    var forkGate: CompletableDeferred<Unit>? = null

    val listCalls = mutableListOf<HostRef>()
    val detailsCalls = mutableListOf<SessionRef>()
    val createCalls = mutableListOf<Pair<HostRef, CreateSessionInput>>()
    val forkCalls = mutableListOf<Pair<SessionRef, ForkSessionInput>>()

    override suspend fun list(host: HostRef): Outcome<SessionListSnapshot> {
        listCalls += host
        listThrowable?.let { throw it }
        return listOutcome ?: Outcome.Success(
            SessionListSnapshot(
                host = host,
                sessions = listOf(
                    ConversationFixtures.sessionSummary("session-a", "Alpha"),
                    ConversationFixtures.sessionSummary("session-b", "Beta"),
                ),
            ),
        )
    }

    override suspend fun details(session: SessionRef): Outcome<SessionDetails> {
        detailsCalls += session
        detailsThrowable?.let { throw it }
        return detailsOutcome ?: Outcome.Success(
            ConversationFixtures.sessionDetails(session),
        )
    }

    override suspend fun create(
        host: HostRef,
        input: CreateSessionInput,
    ): Outcome<CreatedSession> {
        createCalls += host to input
        createGate?.await()
        createThrowable?.let { throw it }
        return createOutcome ?: Outcome.Success(
            ConversationFixtures.created(
                session = SessionRef(host, com.whalerein.android.state.core.SessionId("created-1")),
                details = ConversationFixtures.sessionDetails(
                    SessionRef(host, com.whalerein.android.state.core.SessionId("created-1")),
                ),
            ),
        )
    }

    override suspend fun fork(
        session: SessionRef,
        input: ForkSessionInput,
    ): Outcome<ForkedSession> {
        forkCalls += session to input
        forkGate?.await()
        forkThrowable?.let { throw it }
        return forkOutcome ?: Outcome.Success(
            ConversationFixtures.forked(
                child = SessionRef(
                    session.host,
                    com.whalerein.android.state.core.SessionId("child-1"),
                ),
                details = ConversationFixtures.sessionDetails(
                    SessionRef(
                        session.host,
                        com.whalerein.android.state.core.SessionId("child-1"),
                    ),
                ),
            ),
        )
    }
}

/** Receipt helper for tests that assert the original operation identity. */
fun receiptFor(sendId: SendId): SendReceipt = SendReceipt(sendId, null)
