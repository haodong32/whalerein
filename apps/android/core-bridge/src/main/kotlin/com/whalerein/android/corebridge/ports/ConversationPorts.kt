package com.whalerein.android.corebridge.ports

import com.whalerein.android.corebridge.AndroidCore
import com.whalerein.android.corebridge.ApprovalView
import com.whalerein.android.corebridge.AttemptRefView
import com.whalerein.android.corebridge.ConversationEvent
import com.whalerein.android.corebridge.DeliveryChoice
import com.whalerein.android.corebridge.MobileException
import com.whalerein.android.corebridge.WaitOutcome
import com.whalerein.android.state.conversation.AttemptEnded
import com.whalerein.android.state.conversation.CommandPort
import com.whalerein.android.state.conversation.ComposerPort
import com.whalerein.android.state.conversation.ConversationEvent as StateEvent
import com.whalerein.android.state.conversation.ConversationFailureCodes
import com.whalerein.android.state.conversation.DeliveryMode
import com.whalerein.android.state.conversation.Guard
import com.whalerein.android.state.conversation.HistoryCursor
import com.whalerein.android.state.conversation.HistoryPage
import com.whalerein.android.state.conversation.HistoryPort
import com.whalerein.android.state.conversation.HistoryReplaced
import com.whalerein.android.state.conversation.MessageCommitted
import com.whalerein.android.state.conversation.MessageBlockKind
import com.whalerein.android.state.conversation.ObservationBatch
import com.whalerein.android.state.conversation.ObservationPort
import com.whalerein.android.state.conversation.PendingItem
import com.whalerein.android.state.conversation.PreviewAppended
import com.whalerein.android.state.conversation.PreviewReplaced
import com.whalerein.android.state.conversation.ReplyResult
import com.whalerein.android.state.conversation.RecoveryRequest
import com.whalerein.android.state.conversation.RecoveryResult
import com.whalerein.android.state.conversation.ReplyRequest
import com.whalerein.android.state.conversation.RunPhaseChanged
import com.whalerein.android.state.conversation.RunPhase
import com.whalerein.android.state.conversation.SendReceipt
import com.whalerein.android.state.conversation.SendRequest
import com.whalerein.android.state.conversation.StopOutcome
import com.whalerein.android.state.conversation.ToolUpdated
import com.whalerein.android.state.conversation.WaitPort
import com.whalerein.android.state.conversation.WaitRequest
import com.whalerein.android.state.conversation.WaitResult
import com.whalerein.android.state.conversation.WatchEndReason
import com.whalerein.android.state.conversation.WatchMode
import com.whalerein.android.state.conversation.WatchRequest
import com.whalerein.android.state.conversation.WatchSession
import com.whalerein.android.state.conversation.PendingInputsChanged
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionRef
import java.security.MessageDigest
import java.util.UUID
import kotlinx.coroutines.delay
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

internal fun AttemptRefView.toStateAttempt() =
    com.whalerein.android.state.conversation.AttemptRef(
        liveEpoch = liveEpochId(liveEpoch),
        attemptId = com.whalerein.android.state.core.AttemptId(attemptId),
    )

private fun runPhase(value: String): RunPhase = when (value) {
    "idle" -> RunPhase.Idle
    "running" -> RunPhase.Running
    "stopping" -> RunPhase.WaitingForInput
    "source_unavailable" -> RunPhase.Failed
    else -> RunPhase.Unknown
}

private fun operationIdFor(sendId: String): String =
    MessageDigest.getInstance("SHA-256")
        .digest(sendId.toByteArray(Charsets.UTF_8))
        .take(16)
        .joinToString("") { "%02x".format(it) }

internal class ObservationAdapter(private val owner: AndroidCore) : ObservationPort {
    private val mutex = Mutex()
    private var current: WatchSession? = null
    private var mode = WatchMode.Live
    private var deadline = 0L
    private var sawOutput = false

    override suspend fun startWatch(request: WatchRequest): Outcome<WatchSession> = mutex.withLock {
        onIoOutcome {
            val existing = current
            if (existing != null && existing.scope == request.scope &&
                existing.generation == request.generation
            ) {
                return@onIoOutcome existing
            }
            owner.core().stopWatch()
            owner.core().startWatch(request.scope.hostId(), request.scope.sessionId())
            val session = WatchSession(
                watchId = UUID.randomUUID().toString(),
                scope = request.scope,
                generation = request.generation,
                mode = request.mode,
            )
            mode = request.mode
            deadline = request.timeoutMillis?.let { System.currentTimeMillis() + it } ?: 0L
            sawOutput = false
            current = session
            session
        }
    }

    override suspend fun pollEvents(session: WatchSession): Outcome<ObservationBatch> = mutex.withLock {
        val active = current
        if (active == null || active.watchId != session.watchId) {
            return@withLock Outcome.Success(
                ObservationBatch(
                    scope = session.scope,
                    generation = session.generation,
                    ended = WatchEndReason.ScopeChanged,
                ),
            )
        }
        onIoOutcome {
            val raw = owner.core().pollEvents(500UL)
            var dropped = 0
            var ended: WatchEndReason? = null
            val events = mutableListOf<StateEvent>()
            for (event in raw) {
                when (event) {
                    is ConversationEvent.DroppedEvents -> {
                        dropped += event.count.toInt()
                        ended = WatchEndReason.DroppedEvents
                    }
                    ConversationEvent.WatchEnded -> ended = WatchEndReason.Requested
                    ConversationEvent.HistoryReset -> events += HistoryReplaced(
                        session.scope,
                        session.generation,
                        emptyList(),
                    )
                    is ConversationEvent.State -> {
                        events += RunPhaseChanged(
                            session.scope,
                            session.generation,
                            runPhase(event.runPhase),
                        )
                        events += PendingInputsChanged(
                            session.scope,
                            session.generation,
                            event.approvals.map { it.toPendingItem() },
                        )
                        if (mode == WatchMode.UntilQuiescent && !event.running &&
                            event.approvals.isEmpty()
                        ) {
                            ended = WatchEndReason.Quiescent
                        }
                    }
                    is ConversationEvent.Preview -> {
                        val attempt = event.attempt.toStateAttempt()
                        val kind = coreBlockKind(event.kind)
                        val target = if (event.replace) {
                            PreviewReplaced(
                                session.scope, session.generation, attempt,
                                event.blockId, kind, event.text, event.incomplete,
                            )
                        } else {
                            PreviewAppended(
                                session.scope, session.generation, attempt,
                                event.blockId, kind, event.text, event.incomplete,
                            )
                        }
                        events += target
                        if (kind == MessageBlockKind.Text && event.text.isNotEmpty()) {
                            sawOutput = true
                            if (mode == WatchMode.FirstVisibleOutput) {
                                ended = WatchEndReason.FirstVisibleOutput
                            }
                        }
                    }
                    is ConversationEvent.Message -> {
                        val origin = event.origin?.toStateAttempt()
                        events += MessageCommitted(
                            session.scope,
                            session.generation,
                            event.message.toStateMessage(origin),
                        )
                        if (mode == WatchMode.FirstVisibleOutput &&
                            event.message.blocks.any { it.kind == "text" }
                        ) {
                            sawOutput = true
                            ended = WatchEndReason.FirstVisibleOutput
                        }
                    }
                    is ConversationEvent.ToolActivity -> events += ToolUpdated(
                        session.scope,
                        session.generation,
                        event.activity.toStateActivity(),
                    )
                    is ConversationEvent.AttemptEnded -> events += AttemptEnded(
                        session.scope,
                        session.generation,
                        event.attempt.toStateAttempt(),
                    )
                    else -> Unit
                }
            }
            if (ended == null && deadline > 0 && System.currentTimeMillis() > deadline) {
                ended = WatchEndReason.Timeout
            }
            if (ended != null) {
                owner.core().stopWatch()
                current = null
            }
            ObservationBatch(
                scope = session.scope,
                generation = session.generation,
                events = events,
                droppedCount = dropped,
                ended = ended,
            )
        }
    }

    override suspend fun stopWatch(session: WatchSession): Outcome<Unit> = mutex.withLock {
        onIoOutcome {
            if (current?.watchId == session.watchId) {
                owner.core().stopWatch()
                current = null
            }
        }
    }
}

private inline fun <T> onIoOutcome(block: () -> T): Outcome<T> = try {
    Outcome.Success(block())
} catch (cancelled: kotlinx.coroutines.CancellationException) {
    throw cancelled
} catch (error: Throwable) {
    Outcome.Failure(error.toCoreFailure())
}

private fun SessionRef.hostId(): String = host.hostId.value
private fun SessionRef.sessionId(): String = sessionId.value

internal fun ApprovalView.toPendingItem(): PendingItem = PendingItem.Approval(
    com.whalerein.android.state.conversation.PendingApproval(
        approvalId = approvalId,
        title = toolName,
        body = reason,
        guard = Guard("$version:$digestHex"),
        expiresAtMillis = expiresAtUnixMs,
    ),
)

private const val EXACT_REREAD_MAX_PAGES = 8

internal class HistoryAdapter(private val owner: AndroidCore) : HistoryPort {
    override suspend fun latestPage(scope: SessionRef, projection: String): Outcome<HistoryPage> =
        page(scope, projection, cache = false, continuation = null)

    override suspend fun olderPage(scope: SessionRef, cursor: HistoryCursor): Outcome<HistoryPage> =
        page(scope, cursor.projection, cache = false, continuation = cursor.continuation)

    override suspend fun cachedPage(scope: SessionRef, projection: String): Outcome<HistoryPage> =
        page(scope, projection, cache = true, continuation = null)

    override suspend fun exactMessage(
        scope: SessionRef,
        projection: String,
        messageId: String,
    ): Outcome<HistoryPage> = outcomeOf {
        // The cache only holds the live projection; a different requested
        // projection must be resolved online instead of mislabelled exact.
        if (cacheExactProjectionEligible(projection)) {
            val cached = owner.core().cachedMessages(scope.hostId(), scope.sessionId())
            val message = cached.firstOrNull { it.messageId == messageId }
            if (message != null) {
                return@outcomeOf HistoryPage(
                    session = scope,
                    projection = projection,
                    messages = listOf(message.toStateMessage()),
                    exact = true,
                )
            }
        }
        // Cache miss: Core has no message-reference locator for a page entry,
        // so the exact binding reread cannot reconstruct one. Re-read the
        // scoped online projection instead of reporting a false not-found.
        var continuation: String? = null
        var pages = 0
        while (pages < EXACT_REREAD_MAX_PAGES) {
            val page = owner.core().historyPage(
                scope.hostId(),
                scope.sessionId(),
                false,
                continuation,
                100u,
                4u * 1024u * 1024u,
            )
            if (!historyProjectionMatches(page.projection, projection)) {
                throw MobileException.StaleHistory("content projection changed")
            }
            val message = page.messages.firstOrNull { it.messageId == messageId }
            if (message != null) {
                return@outcomeOf HistoryPage(
                    session = scope,
                    projection = page.projection.ifEmpty { projection },
                    messages = listOf(message.toStateMessage()),
                    exact = true,
                )
            }
            val next = page.continuation
            if (page.complete || next.isNullOrEmpty()) {
                throw MobileException.NotFound("message not found")
            }
            continuation = next
            pages += 1
        }
        throw MobileException.NotFound("message not found")
    }

    private suspend fun page(
        scope: SessionRef,
        projection: String,
        cache: Boolean,
        continuation: String?,
    ): Outcome<HistoryPage> = outcomeOf {
        val page = owner.core().historyPage(
            scope.hostId(),
            scope.sessionId(),
            cache,
            continuation,
            100u,
            4u * 1024u * 1024u,
        )
        HistoryPage(
            session = scope,
            projection = if (page.projection.isNotEmpty()) page.projection else projection,
            messages = page.messages.mapIndexed { index, message ->
                message.toStateMessage(sequence = index.toLong())
            },
            continuation = page.continuation?.let {
                HistoryCursor(
                    continuation = it,
                    session = scope,
                    projection = projection,
                    expiresAtMillis = System.currentTimeMillis() + 5 * 60_000,
                )
            },
            cached = cache || page.source == "cache",
            exact = false,
        )
    }
}

internal class ComposerAdapter(private val owner: AndroidCore) : ComposerPort {
    override suspend fun submit(request: SendRequest): Outcome<SendReceipt> = try {
        val id = operationIdFor(request.sendId.value)
        val result = owner.core().sendWithOperationId(
            request.scope.hostId(),
            request.scope.sessionId(),
            request.text,
            request.mode.toCore(),
            id,
        )
        when (sendDisposition(result.receipt?.status)) {
            SendDisposition.Accepted -> Outcome.Success(
                SendReceipt(
                    sendId = request.sendId,
                    attempt = result.reply?.let { null },
                    acceptedAtMillis = System.currentTimeMillis(),
                ),
            )

            // A missing/ambiguous receipt keeps the original operation
            // recoverable; it is never a definite failure.
            SendDisposition.Unconfirmed -> Outcome.Unconfirmed(
                CoreFailure(
                    "uncertain",
                    "The send result is unknown; recover the original operation.",
                    retryable = true,
                ),
            )

            SendDisposition.Rejected -> Outcome.Failure(
                CoreFailure(
                    result.receipt?.status ?: "send_rejected",
                    "The Host rejected the send.",
                    retryable = false,
                ),
            )
        }
    } catch (cancelled: kotlinx.coroutines.CancellationException) {
        throw cancelled
    } catch (error: Throwable) {
        val failure = error.toCoreFailure()
        if (failure.retryable) Outcome.Unconfirmed(failure) else Outcome.Failure(failure)
    }

    override suspend fun readReply(request: ReplyRequest): ReplyResult = try {
        val id = operationIdFor(request.sendId.value)
        val view = onIoBlocking {
            owner.core().recoverSendWithContent(
                id,
                true,
                request.timeoutMillis.coerceIn(1, 55_000).toULong(),
            )
        }
        view.toReplyResult()
    } catch (error: Throwable) {
        val failure = error.toCoreFailure()
        if (failure.code == "timeout") ReplyResult.TimedOut else ReplyResult.Failed(failure)
    }

    override suspend fun recover(request: RecoveryRequest): RecoveryResult = try {
        val id = operationIdFor(request.sendId.value)
        val view = onIoBlocking { owner.core().recoverSendWithContent(id, true, 10_000uL) }
        val receipt = SendReceipt(request.sendId, null)
        when {
            view.content != null -> RecoveryResult.Confirmed(receipt, view.content!!.toStateMessage())
            view.status == "confirmed" || view.status == "accepted" ->
                RecoveryResult.ConfirmedUnread(
                    receipt,
                    CoreFailure(ConversationFailureCodes.CONTENT_UNREADABLE, "Reply is unreadable."),
                )
            view.status == "failed" -> RecoveryResult.Failed(
                CoreFailure("failed", "The send operation failed."),
            )
            else -> RecoveryResult.StillUnconfirmed(
                CoreFailure("uncertain", "The send result is still unknown.", retryable = true),
            )
        }
    } catch (error: Throwable) {
        RecoveryResult.Failed(error.toCoreFailure())
    }
}

internal fun com.whalerein.android.corebridge.RetainedOperationView.toReplyResult(): ReplyResult {
    val content = content
    if (content != null) return ReplyResult.Reply(content.toStateMessage())
    val reply = reply
    return when (reply) {
        is com.whalerein.android.corebridge.MessageReplyView.Committed -> if (readError != null) {
            ReplyResult.ConfirmedUnread(
                CoreFailure(ConversationFailureCodes.CONTENT_UNREADABLE, "Reply is unreadable."),
            )
        } else {
            ReplyResult.ConfirmedUnread(
                CoreFailure(ConversationFailureCodes.CONTENT_UNREADABLE, "Reply content was not read."),
            )
        }
        is com.whalerein.android.corebridge.MessageReplyView.InputRequired ->
            ReplyResult.ConfirmedUnread(
                CoreFailure("input_required", "The reply reached pending Host input."),
            )
        com.whalerein.android.corebridge.MessageReplyView.Pending,
        com.whalerein.android.corebridge.MessageReplyView.Uncertain,
        null -> ReplyResult.Failed(
            CoreFailure("uncertain", "The reply could not be read.", retryable = true),
        )
    }
}

internal fun DeliveryMode.toCore(): DeliveryChoice = when (this) {
    DeliveryMode.Queue -> DeliveryChoice.QUEUE
    DeliveryMode.Steer -> DeliveryChoice.STEER
    DeliveryMode.StartIfIdle, DeliveryMode.Send -> DeliveryChoice.SEND
}

private suspend fun <T> onIoBlocking(block: () -> T): T =
    kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { block() }

internal class WaitAdapter(private val owner: AndroidCore) : WaitPort {
    private val identities = WaitIdentityRegistry()
    private val identityMutex = Mutex()

    override suspend fun wait(request: WaitRequest): WaitResult {
        val nativeId = try {
            onIoBlocking {
                owner.core().beginWait(
                    request.scope.hostId(),
                    request.scope.sessionId(),
                    request.timeoutMillis.coerceIn(1, 60 * 60_000).toULong(),
                )
            }
        } catch (cancelled: kotlinx.coroutines.CancellationException) {
            throw cancelled
        } catch (error: Throwable) {
            return WaitResult.Failed(error.toCoreFailure())
        }
        identityMutex.withLock { identities.bind(request.waitId, nativeId) }
        val deadline = System.currentTimeMillis() + request.timeoutMillis
        var result: com.whalerein.android.corebridge.WaitResultView? = null
        try {
            while (System.currentTimeMillis() < deadline) {
                result = onIoBlocking { owner.core().takeWaitResult(nativeId, 250UL) }
                if (result != null) break
                delay(50)
            }
        } catch (cancelled: kotlinx.coroutines.CancellationException) {
            // Keep the app/native mapping so an explicit cancel can still
            // release exactly this native attempt.
            throw cancelled
        } catch (error: Throwable) {
            identityMutex.withLock { identities.release(request.waitId) }
            return WaitResult.Failed(error.toCoreFailure())
        }
        identityMutex.withLock { identities.release(request.waitId) }
        if (result == null) {
            // The local bounded wait expired; release the native attempt so it
            // cannot settle into a later, unrelated wait state.
            try {
                onIoBlocking { owner.core().cancelWait(nativeId) }
            } catch (_: Throwable) {
                // The attempt may have settled between the take and the cancel.
            }
            return WaitResult.TimedOut
        }
        return when (result.outcome) {
            WaitOutcome.IDLE -> WaitResult.Quiescent
            WaitOutcome.INPUT_REQUIRED -> inputWaitResult(request.scope, result)
            WaitOutcome.TIMEOUT -> WaitResult.TimedOut
            WaitOutcome.CANCELLED -> WaitResult.Cancelled
            WaitOutcome.FAILED -> WaitResult.Failed(
                CoreFailure(result.errorCode ?: "failed", "Wait failed."),
            )
        }
    }

    /**
     * Releases only the native attempt behind [waitId]. The app id never reaches
     * Core; a wait that already settled is a no-op, not a host interruption.
     */
    override suspend fun cancel(waitId: String): Outcome<Unit> {
        val nativeId = identityMutex.withLock { identities.release(waitId) }
            ?: return Outcome.Success(Unit)
        return outcomeOf { owner.core().cancelWait(nativeId) }
    }

    private fun inputWaitResult(
        scope: SessionRef,
        result: com.whalerein.android.corebridge.WaitResultView,
    ): WaitResult {
        val pending = result.state?.pendingInput.orEmpty()
        val kind = pendingInputKind(pending)
        var approval = approvalItem(scope)
        var question = questionItem(scope)
        when (kind) {
            PendingInputKind.Approval -> if (approval == null) question = questionItem(scope)
            PendingInputKind.Question -> if (question == null) approval = approvalItem(scope)
            PendingInputKind.None -> Unit
        }
        return when (waitSettlement(result.outcome, pending, approval != null, question != null)) {
            WaitSettlement.Approval -> WaitResult.ApprovalInput(approval!!)
            WaitSettlement.Question -> WaitResult.QuestionInput(question!!)
            WaitSettlement.Quiescent -> WaitResult.Quiescent
            WaitSettlement.TimedOut -> WaitResult.TimedOut
            WaitSettlement.Cancelled -> WaitResult.Cancelled
            WaitSettlement.Failed,
            WaitSettlement.InputUnavailable,
            -> WaitResult.Failed(
                CoreFailure(
                    result.errorCode ?: "input_unavailable",
                    "The pending input could not be read.",
                    retryable = true,
                ),
            )
        }
    }

    private fun approvalItem(scope: SessionRef): PendingItem.Approval? = try {
        owner.core().approvals(scope.hostId(), scope.sessionId()).firstOrNull()?.toPendingItem()
            as? PendingItem.Approval
    } catch (_: Throwable) {
        null
    }

    private fun questionItem(scope: SessionRef): PendingItem.Questions? = try {
        owner.core()
            .listQuestions(scope.hostId(), scope.sessionId(), "")
            .batches
            .firstOrNull()
            ?.toPendingItem(scope)
    } catch (_: Throwable) {
        null
    }
}

internal class CommandAdapter(private val owner: AndroidCore) : CommandPort {
    override suspend fun stop(scope: SessionRef, attempt: com.whalerein.android.state.conversation.AttemptRef?): StopOutcome =
        try {
            val command = owner.core().stop(scope.hostId(), scope.sessionId())
            when (command.status) {
                "succeeded" -> StopOutcome.Confirmed
                "received", "host_accepted" -> StopOutcome.Accepted
                "host_offline", "unknown" -> StopOutcome.Unconfirmed(
                    CoreFailure("uncertain", "The stop result is unknown.", retryable = true),
                )
                else -> StopOutcome.Failed(CoreFailure(command.status, "Stop failed."))
            }
        } catch (error: Throwable) {
            val failure = error.toCoreFailure()
            if (failure.retryable) StopOutcome.Unconfirmed(failure) else StopOutcome.Failed(failure)
        }
}
