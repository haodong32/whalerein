package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.IdGenerator
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.ScreenModel
import com.whalerein.android.state.core.SendId
import com.whalerein.android.state.core.SessionRef
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * Foreground conversation screen model.
 *
 * It owns:
 * - the current observation scope/generation and the one scoped watch;
 * - history paging with a stable reading anchor and cached-page marker;
 * - ordered live preview/commit convergence through [ConversationReducer];
 * - composer submission, queue/steer/start-if-idle modes, follow, recovery;
 * - bounded wait and explicit stop as separate operations;
 * - the fixed pending-input entry summary.
 *
 * The constructor takes only typed ports, a clock, an ID generator and a
 * diagnostic sink, so the same class runs on the JVM and in the app.
 */
class ConversationViewModel(
    private val observation: ObservationPort,
    private val history: HistoryPort,
    private val composer: ComposerPort,
    private val wait: WaitPort,
    private val commands: CommandPort,
    private val pendingInput: PendingInputPort,
    private val drafts: DraftPort,
    private val clock: Clock,
    private val ids: IdGenerator,
    diagnostics: DiagnosticSink,
    private val queue: ModelsQueuePort? = null,
    private val projection: String = CONVERSATION_PROJECTION_ANDROID_V1,
    draftStore: DraftStore? = null,
) : ScreenModel<ConversationUiState>(ConversationUiState()) {
    private val diag = ConversationDiagnostics(diagnostics)

    private var reducerState = ConversationState()
    private var pagerState = HistoryPagerState()
    private var generationCounter = 0L
    private var loadedSession: SessionRef? = null
    private var watchJob: Job? = null
    private var watchSession: WatchSession? = null
    private var watchMode: WatchMode = WatchMode.Live
    private var waitJob: Job? = null
    private var activeWaitId: String? = null
    private var resyncing = false
    private var announcementToken = 0L
    private val draftsBySession = mutableMapOf<SessionRef, String>()
    private val pendingSends = mutableMapOf<SessionRef, PendingSendRecord>()

    /**
     * Monotonic per-session draft revision. A submission captures the revision
     * it sent; an accepted receipt may clear the persisted draft only while the
     * revision is unchanged, so a newer edit is never erased by an older send.
     */
    private val draftRevisions = mutableMapOf<SessionRef, Long>()
    private val draftStore: DraftStore = draftStore ?: DraftStore(drafts)
    private var draftLease: DraftStore.Lease? = null
    private var draftSaveJob: Job? = null

    /**
     * Opens [session]. Re-opening the same session is a no-op, so rotation and
     * pane recreation reuse the active observation and history page.
     */
    fun open(session: SessionRef) {
        if (loadedSession == session) {
            return
        }
        val previous = loadedSession
        if (previous != null) {
            draftsBySession[previous] = state.value.composer.text
            stopWatch(WatchEndReason.ScopeChanged)
        }
        generationCounter += 1
        loadedSession = session
        reducerState = ConversationState.begin(session, generationCounter)
        pagerState = HistoryPagerState.begin(session, projection)
        activeWaitId = null
        waitJob?.cancel()
        waitJob = null
        resyncing = false
        setState(
            ConversationUiState(
                session = session,
                generation = generationCounter,
                status = ConversationStatus.Loading,
                composer = ComposerUiState(text = draftsBySession[session].orEmpty()),
                history = HistoryUiState(status = HistoryPagerStatus.LoadingLatest),
                pendingEntry = PendingEntryUiState.Loading,
            ),
        )
        loadDraft(session)
        loadPendingSend(session)
        loadLatestPage()
        refreshPendingInputs()
        refreshQueue()
        startWatch(WatchMode.Live)
    }

    /** Foreground/visible. Idempotent: never starts a second watch. */
    fun onVisible() {
        if (loadedSession == null) {
            return
        }
        if (watchJob?.isActive == true) {
            return
        }
        // Resuming foreground visibility always returns to a live observation;
        // a first-visible-output request is a one-shot explicit action.
        startWatch(WatchMode.Live)
    }

    /** Background/hidden. Stops only the observation; the Host run continues. */
    fun onHidden() {
        if (loadedSession == null) {
            return
        }
        stopWatch(WatchEndReason.Backgrounded)
    }

    fun onForeground() = onVisible()

    fun onBackground() = onHidden()

    // ------------------------------------------------------------------ history

    fun loadOlder() {
        val session = loadedSession ?: return
        when (val start = HistoryPager.requestOlder(pagerState, clock.epochMillis())) {
            OlderPageStart.AtEnd -> Unit
            is OlderPageStart.Invalidated -> {
                pagerState = start.result.state
                applyReducerEvent(HistoryReplaced(session, state.value.generation, emptyList()))
                edit {
                    copy(
                        items = emptyList(),
                        history = historyFromPager(),
                        status = ConversationStatus.Invalidated,
                    )
                }
                loadLatestPage()
            }

            is OlderPageStart.Request -> {
                pagerState = HistoryPager.expectOlder(pagerState)
                val anchor = state.value.history.anchor
                publishHistory()
                scope.launch {
                    val result = guardedHistoryCall {
                        history.olderPage(session, start.cursor)
                    }
                    if (loadedSession != session) {
                        return@launch
                    }
                    when (result) {
                        is Outcome.Success -> {
                            val accepted = HistoryPager.acceptOlder(
                                pagerState,
                                start.cursor,
                                result.value,
                                clock.epochMillis(),
                                anchor,
                            )
                            pagerState = accepted.state
                            if (pagerState.status is HistoryPagerStatus.Invalidated) {
                                applyReducerEvent(
                                    HistoryReplaced(session, state.value.generation, emptyList()),
                                )
                                edit {
                                    copy(
                                        items = emptyList(),
                                        history = historyFromPager(),
                                        status = ConversationStatus.Invalidated,
                                    )
                                }
                                loadLatestPage()
                                return@launch
                            }
                            applyReducerEvent(
                                MessagesLoaded(
                                    session,
                                    state.value.generation,
                                    result.value.messages,
                                ),
                            )
                            publishHistory(accepted.insertion)
                        }

                        is Outcome.Failure -> finishHistoryFailure(result.failure)
                        is Outcome.Unconfirmed -> finishHistoryFailure(result.failure)
                        Outcome.Cancelled -> Unit
                    }
                }
            }
        }
    }

    fun loadExact(messageId: String) {
        val session = loadedSession ?: return
        scope.launch {
            val result = guardedHistoryCall {
                history.exactMessage(session, projection, messageId)
            }
            if (loadedSession != session) {
                return@launch
            }
            when (result) {
                is Outcome.Success -> {
                    val accepted = HistoryPager.acceptExact(
                        pagerState,
                        result.value,
                        clock.epochMillis(),
                        messageId,
                        state.value.history.anchor,
                    )
                    pagerState = accepted.state
                    if (pagerState.status is HistoryPagerStatus.Invalidated) {
                        applyReducerEvent(
                            HistoryReplaced(session, state.value.generation, emptyList()),
                        )
                        edit {
                            copy(
                                items = emptyList(),
                                history = historyFromPager(),
                                status = ConversationStatus.Invalidated,
                            )
                        }
                        loadLatestPage()
                        return@launch
                    }
                    applyReducerEvent(
                        MessagesLoaded(session, state.value.generation, result.value.messages),
                    )
                    publishHistory(accepted.insertion)
                }

                is Outcome.Failure -> finishHistoryFailure(result.failure)
                is Outcome.Unconfirmed -> finishHistoryFailure(result.failure)
                Outcome.Cancelled -> Unit
            }
        }
    }

    fun loadCached() {
        val session = loadedSession ?: return
        scope.launch {
            val result = guardedHistoryCall {
                history.cachedPage(session, projection)
            }
            if (loadedSession != session) {
                return@launch
            }
            when (result) {
                is Outcome.Success -> {
                    val accepted = HistoryPager.acceptCached(
                        pagerState,
                        result.value,
                        clock.epochMillis(),
                    )
                    pagerState = accepted.state
                    applyReducerEvent(
                        HistoryReplaced(session, state.value.generation, result.value.messages),
                    )
                    edit {
                        copy(
                            history = historyFromPager(),
                            status = ConversationStatus.Cached,
                        )
                    }
                }

                is Outcome.Failure -> {
                    pagerState = HistoryPager.cachedUnavailable(pagerState, result.failure).state
                    edit {
                        copy(
                            history = historyFromPager(),
                            status = ConversationStatus.Cached,
                        )
                    }
                }

                is Outcome.Unconfirmed -> {
                    pagerState = HistoryPager.cachedUnavailable(pagerState, result.failure).state
                    edit {
                        copy(
                            history = historyFromPager(),
                            status = ConversationStatus.Cached,
                        )
                    }
                }

                Outcome.Cancelled -> Unit
            }
        }
    }

    fun setReadingAnchor(anchor: ReadingAnchor?) {
        pagerState = HistoryPager.reanchor(pagerState, anchor)
        edit { copy(history = historyFromPager()) }
    }

    // ----------------------------------------------------------------- composer

    fun updateDraft(text: String) {
        val session = loadedSession ?: return
        draftsBySession[session] = text
        draftRevisions[session] = (draftRevisions[session] ?: 0L) + 1
        edit { copy(composer = composer.copy(text = text)) }
        val lease = draftLease ?: return
        val revision = draftStore.update(lease, text)
        draftSaveJob?.cancel()
        draftSaveJob = scope.launch {
            delay(DRAFT_DEBOUNCE_MILLIS)
            when (val saved = draftStore.setAndFlush(lease, text, revision)) {
                is Outcome.Success -> Unit
                is Outcome.Failure -> diag.warn(
                    ConversationDiagnosticStage.COMMAND,
                    ConversationDiagnosticEventName.SEND_FAILED,
                    code = saved.failure.code,
                    fields = mapOf("operation" to "draft_save"),
                )

                is Outcome.Unconfirmed -> Unit
                Outcome.Cancelled -> Unit
            }
        }
    }

    fun selectDeliveryMode(mode: DeliveryMode) {
        edit { copy(composer = composer.copy(mode = mode)) }
    }

    fun selectFollowMode(follow: FollowMode) {
        edit { copy(composer = composer.copy(follow = follow)) }
    }

    fun submit() = submit(state.value.composer.mode)

    fun queue() = submit(DeliveryMode.Queue)

    fun steer() = submit(DeliveryMode.Steer)

    fun startIfIdle() = submit(DeliveryMode.StartIfIdle)

    private fun submit(mode: DeliveryMode) {
        val session = loadedSession ?: return
        val ui = state.value
        if (ui.composer.submitting || ui.send is SendUiState.Submitting) {
            return
        }
        if (ui.composer.availability == ComposerAvailability.Unsupported) {
            return
        }
        val text = ui.composer.text
        if (text.isBlank()) {
            return
        }
        // The stable send identity exists before submission.
        val sendId = SendId(ids.newId())
        val draftRevision = draftRevisions[session] ?: 0L
        val followMode = ui.composer.follow
        edit {
            copy(
                composer = composer.copy(
                    text = "",
                    mode = mode,
                    submitting = true,
                    availability = ComposerAvailability.Available,
                ),
                send = SendUiState.Submitting(sendId, session),
            )
        }
        // Keep the persisted draft until the Host accepts the send. Clearing it
        // here would lose the text if the process dies before the receipt.
        draftsBySession[session] = text
        // Persist the exact operation identity before touching the Host so a
        // process restart can recover this send instead of resubmitting it.
        val pending = PendingSendRecord(
            sendId = sendId.value,
            text = text,
            follow = followMode.name,
            submittedAtMillis = clock.epochMillis(),
        )
        pendingSends[session] = pending
        scope.launch {
            val saved = drafts.savePendingSend(session, pending)
            if (saved is Outcome.Failure || saved is Outcome.Unconfirmed) {
                diag.warn(
                    ConversationDiagnosticStage.COMMAND,
                    ConversationDiagnosticEventName.SEND_FAILED,
                    actionId = sendId.value,
                    code = "pending_send_not_persisted",
                )
            }
            submitInternal(session, sendId, text, mode, followMode, draftRevision)
        }
    }

    private suspend fun submitInternal(
        session: SessionRef,
        sendId: SendId,
        text: String,
        mode: DeliveryMode,
        followMode: FollowMode,
        draftRevision: Long,
    ) {
        val result = try {
            composer.submit(SendRequest(session, sendId, text, mode, followMode))
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (throwable: Throwable) {
            diag.error(
                ConversationDiagnosticStage.COMMAND,
                ConversationDiagnosticEventName.SEND_FAILED,
                actionId = sendId.value,
                code = "unexpected",
            )
            Outcome.Failure(unexpectedFailure())
        }
        when (result) {
            is Outcome.Success -> onSubmitted(
                session,
                sendId,
                followMode,
                result.value,
                text,
                draftRevision,
            )

            is Outcome.Failure -> onSendFailed(
                session,
                sendId,
                text,
                result.failure,
                definite = true,
                draftRevision = draftRevision,
            )

            is Outcome.Unconfirmed -> onSendFailed(
                session,
                sendId,
                text,
                result.failure,
                definite = false,
                draftRevision = draftRevision,
            )

            Outcome.Cancelled -> onSendCancelled(session, sendId, text, draftRevision)
        }
    }

    private fun onSubmitted(
        session: SessionRef,
        sendId: SendId,
        followMode: FollowMode,
        receipt: SendReceipt,
        submittedText: String,
        draftRevision: Long,
    ) {
        if (!isCurrentSend(sendId)) {
            return
        }
        settleAcceptedDraft(session, submittedText, draftRevision)
        clearPendingSend(session)
        diag.info(
            ConversationDiagnosticStage.COMMAND,
            ConversationDiagnosticEventName.SEND_ACCEPTED,
            actionId = sendId.value,
            fields = mapOf("follow" to followMode.name.lowercase()),
        )
        if (followMode == FollowMode.ReceiptOnly) {
            edit {
                copy(
                    composer = composer.copy(submitting = false),
                    send = SendUiState.ReceiptOnly(sendId, receipt),
                )
            }
            return
        }
        edit {
            copy(
                composer = composer.copy(submitting = false),
                send = SendUiState.FollowPending(sendId, receipt),
            )
        }
        scope.launch { followReply(session, sendId, receipt) }
    }

    private suspend fun followReply(
        session: SessionRef,
        sendId: SendId,
        receipt: SendReceipt,
    ) {
        val result = try {
            composer.readReply(
                ReplyRequest(
                    scope = session,
                    sendId = sendId,
                    attempt = receipt.attempt,
                    timeoutMillis = REPLY_FOLLOW_TIMEOUT_MILLIS,
                ),
            )
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (throwable: Throwable) {
            diag.error(
                ConversationDiagnosticStage.REPLY_READ,
                ConversationDiagnosticEventName.REPLY_UNREADABLE,
                actionId = sendId.value,
                code = "unexpected",
            )
            ReplyResult.Failed(unexpectedFailure())
        }
        if (!isCurrentSend(sendId)) {
            diag.warn(
                ConversationDiagnosticStage.COMMAND,
                ConversationDiagnosticEventName.SEND_FAILED,
                actionId = sendId.value,
                code = "late_receipt_ignored",
            )
            return
        }
        when (result) {
            is ReplyResult.Reply -> {
                applyCommittedReply(session, result.message)
                edit {
                    copy(
                        composer = composer.copy(submitting = false),
                        send = SendUiState.FollowedReply(sendId, receipt, result.message),
                    )
                }
            }

            is ReplyResult.PendingInput -> {
                mergePendingItem(result.item)
                edit {
                    copy(
                        composer = composer.copy(submitting = false),
                        send = SendUiState.FollowPendingInput(sendId, receipt, result.item),
                    )
                }
            }

            is ReplyResult.ConfirmedUnread -> {
                diag.warn(
                    ConversationDiagnosticStage.REPLY_READ,
                    ConversationDiagnosticEventName.REPLY_UNREADABLE,
                    actionId = sendId.value,
                    code = result.failure.code,
                )
                edit {
                    copy(
                        composer = composer.copy(submitting = false),
                        send = SendUiState.ConfirmedUnread(sendId, receipt, result.failure),
                    )
                }
            }

            ReplyResult.TimedOut -> edit {
                copy(
                    composer = composer.copy(submitting = false),
                    send = SendUiState.FollowTimedOut(sendId, receipt),
                )
            }

            is ReplyResult.Failed -> {
                diag.warn(
                    ConversationDiagnosticStage.REPLY_READ,
                    ConversationDiagnosticEventName.REPLY_UNREADABLE,
                    actionId = sendId.value,
                    code = result.failure.code,
                )
                edit {
                    copy(
                        composer = composer.copy(submitting = false),
                        send = SendUiState.FollowFailed(sendId, result.failure),
                    )
                }
            }
        }
    }

    private fun onSendFailed(
        session: SessionRef,
        sendId: SendId,
        submittedText: String,
        failure: CoreFailure,
        definite: Boolean,
        draftRevision: Long,
    ) {
        if (!isCurrentSend(sendId)) {
            diag.warn(
                ConversationDiagnosticStage.COMMAND,
                ConversationDiagnosticEventName.SEND_FAILED,
                actionId = sendId.value,
                code = "late_receipt_ignored",
            )
            return
        }
        restoreDraftAfterFailure(session, submittedText, draftRevision)
        if (definite) {
            clearPendingSend(session)
        }
        val levelCode = if (definite) "definite_failure" else "unconfirmed"
        diag.warn(
            ConversationDiagnosticStage.COMMAND,
            if (definite) {
                ConversationDiagnosticEventName.SEND_FAILED
            } else {
                ConversationDiagnosticEventName.SEND_UNCONFIRMED
            },
            actionId = sendId.value,
            code = failure.code,
            fields = mapOf("outcome" to levelCode),
        )
        edit {
            copy(
                composer = composer.copy(
                    submitting = false,
                    availability = if (failure.code == ConversationFailureCodes.OFFLINE) {
                        ComposerAvailability.Offline
                    } else {
                        ComposerAvailability.Unavailable
                    },
                ),
                send = if (definite) {
                    SendUiState.DefiniteFailure(failure)
                } else {
                    SendUiState.Unconfirmed(sendId, failure)
                },
            )
        }
    }

    private fun onSendCancelled(
        session: SessionRef,
        sendId: SendId,
        submittedText: String,
        draftRevision: Long,
    ) {
        if (!isCurrentSend(sendId)) {
            return
        }
        restoreDraftAfterFailure(session, submittedText, draftRevision)
        clearPendingSend(session)
        edit {
            copy(
                composer = composer.copy(submitting = false),
                send = SendUiState.Cancelled(sendId),
            )
        }
    }

    /**
     * Clears the persisted draft only after an accepted send, and only while no
     * newer edit replaced it. A newer draft is written back instead, so neither
     * an old submission nor a late receipt can erase it.
     */
    private fun settleAcceptedDraft(
        session: SessionRef,
        submittedText: String,
        submittedRevision: Long,
    ) {
        val currentRevision = draftRevisions[session] ?: 0L
        val currentText = draftsBySession[session] ?: state.value.composer.text
        val lease = draftLease ?: return
        if (currentRevision == submittedRevision && currentText == submittedText) {
            draftsBySession[session] = ""
            scope.launch {
                draftStore.clearIfCurrent(lease, submittedText)
            }
        } else {
            draftsBySession[session] = currentText
            val revision = draftStore.update(lease, currentText)
            scope.launch {
                draftStore.setAndFlush(lease, currentText, revision)
            }
        }
    }

    /**
     * Restores the submitted text for a definite failure or unknown outcome.
     * The persisted draft was never cleared, so a process restart still restores
     * it; a newer edit keeps ownership of the composer and the store.
     */
    private fun restoreDraftAfterFailure(
        session: SessionRef,
        submittedText: String,
        submittedRevision: Long,
    ) {
        if (loadedSession != session) {
            return
        }
        val currentRevision = draftRevisions[session] ?: 0L
        if (currentRevision != submittedRevision) {
            return
        }
        draftsBySession[session] = submittedText
        val lease = draftLease ?: return
        val revision = draftStore.update(lease, submittedText)
        scope.launch {
            draftStore.setAndFlush(lease, submittedText, revision)
        }
        edit {
            if (composer.text.isEmpty()) {
                copy(composer = composer.copy(text = submittedText))
            } else {
                this
            }
        }
    }

    /**
     * Restores the one unresolved send identity for this exact scope. The
     * composer stays untouched so a newer draft keeps ownership; the existing
     * recovery action reads the original operation, never a new submission.
     */
    private fun loadPendingSend(session: SessionRef) {
        scope.launch {
            val result = try {
                drafts.loadPendingSend(session)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                Outcome.Failure(unexpectedFailure())
            }
            if (loadedSession != session) {
                return@launch
            }
            if (result !is Outcome.Success) {
                diag.warn(
                    ConversationDiagnosticStage.COMMAND,
                    ConversationDiagnosticEventName.SEND_UNCONFIRMED,
                    code = "pending_send_unreadable",
                )
                return@launch
            }
            val record = result.value ?: return@launch
            if (record.sendId.isBlank() || state.value.send != SendUiState.Idle) {
                return@launch
            }
            pendingSends[session] = record
            edit {
                copy(
                    send = SendUiState.Unconfirmed(
                        SendId(record.sendId),
                        CoreFailure(
                            "uncertain",
                            "The previous send result is unknown; recover the original operation.",
                            retryable = true,
                        ),
                    ),
                )
            }
            diag.info(
                ConversationDiagnosticStage.COMMAND,
                ConversationDiagnosticEventName.SEND_UNCONFIRMED,
                actionId = record.sendId,
                code = "pending_send_restored",
            )
        }
    }

    private fun clearPendingSend(session: SessionRef) {
        pendingSends.remove(session)
        scope.launch { drafts.clearPendingSend(session) }
    }

    /**
     * Explicit dismissal of a restored unknown send: clears its durable
     * identity and restores the submitted text only when no newer draft exists.
     */
    fun discardUnconfirmedSend() {
        val send = state.value.send as? SendUiState.Unconfirmed ?: return
        val session = loadedSession ?: return
        val record = pendingSends[session]?.takeIf { it.sendId == send.sendId.value }
        clearPendingSend(session)
        record?.let { restorePendingText(session, it) }
        edit {
            copy(
                send = SendUiState.Idle,
                composer = composer.copy(submitting = false),
            )
        }
    }

    private fun restorePendingText(session: SessionRef, record: PendingSendRecord) {
        if (loadedSession != session || state.value.composer.text.isNotEmpty()) {
            return
        }
        draftsBySession[session] = record.text
        val lease = draftLease ?: return
        val revision = draftStore.update(lease, record.text)
        scope.launch {
            draftStore.setAndFlush(lease, record.text, revision)
        }
        edit { copy(composer = composer.copy(text = record.text)) }
    }

    /** Recovers the current ambiguous send by its original operation identity. */
    fun recoverSend() {
        val sendId = when (val send = state.value.send) {
            is SendUiState.Unconfirmed -> send.sendId
            is SendUiState.FollowTimedOut -> send.sendId
            else -> return
        }
        val session = loadedSession ?: return
        edit { copy(send = SendUiState.Recovering(sendId)) }
        scope.launch {
            val result = try {
                composer.recover(RecoveryRequest(session, sendId))
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                RecoveryResult.StillUnconfirmed(unexpectedFailure())
            }
            if ((state.value.send as? SendUiState.Recovering)?.sendId != sendId) {
                return@launch
            }
            when (result) {
                is RecoveryResult.Confirmed -> {
                    result.reply?.let { applyCommittedReply(session, it) }
                    clearPendingSend(session)
                    edit {
                        copy(
                            send = SendUiState.Recovered(sendId, result.receipt, result.reply),
                            composer = composer.copy(submitting = false),
                        )
                    }
                    diag.info(
                        ConversationDiagnosticStage.COMMAND,
                        ConversationDiagnosticEventName.RECOVERY_FINISHED,
                        actionId = sendId.value,
                        code = "confirmed",
                    )
                }

                is RecoveryResult.ConfirmedUnread -> {
                    clearPendingSend(session)
                    edit {
                        copy(
                            send = SendUiState.ConfirmedUnread(
                                sendId,
                                result.receipt,
                                result.failure,
                            ),
                            composer = composer.copy(submitting = false),
                        )
                    }
                }

                is RecoveryResult.StillUnconfirmed -> edit {
                    copy(
                        send = SendUiState.Unconfirmed(sendId, result.failure),
                        composer = composer.copy(submitting = false),
                    )
                }

                is RecoveryResult.Failed -> {
                    val record = pendingSends[session]?.takeIf { it.sendId == sendId.value }
                    clearPendingSend(session)
                    record?.let { restorePendingText(session, it) }
                    edit {
                        copy(
                            send = SendUiState.DefiniteFailure(result.failure),
                            composer = composer.copy(submitting = false),
                        )
                    }
                }
            }
        }
    }

    /**
     * Retries only the content read for a confirmed reply. It never submits the
     * draft again and keeps the original send identity.
     */
    fun retryContentRead() {
        val unread = state.value.send as? SendUiState.ConfirmedUnread ?: return
        val session = loadedSession ?: return
        if (!unread.canRetry) {
            return
        }
        scope.launch {
            val result = try {
                composer.readReply(
                    ReplyRequest(
                        scope = session,
                        sendId = unread.sendId,
                        attempt = unread.receipt.attempt,
                        timeoutMillis = REPLY_FOLLOW_TIMEOUT_MILLIS,
                    ),
                )
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                ReplyResult.ConfirmedUnread(unexpectedFailure())
            }
            if ((state.value.send as? SendUiState.ConfirmedUnread)?.sendId != unread.sendId) {
                return@launch
            }
            when (result) {
                is ReplyResult.Reply -> {
                    applyCommittedReply(session, result.message)
                    edit {
                        copy(
                            send = SendUiState.FollowedReply(
                                unread.sendId,
                                unread.receipt,
                                result.message,
                            ),
                            composer = composer.copy(submitting = false),
                        )
                    }
                }

                is ReplyResult.PendingInput -> {
                    mergePendingItem(result.item)
                    edit {
                        copy(
                            send = SendUiState.FollowPendingInput(
                                unread.sendId,
                                unread.receipt,
                                result.item,
                            ),
                            composer = composer.copy(submitting = false),
                        )
                    }
                }

                is ReplyResult.ConfirmedUnread -> edit {
                    copy(
                        send = unread.copy(failure = result.failure),
                        composer = composer.copy(submitting = false),
                    )
                }

                ReplyResult.TimedOut -> edit {
                    copy(
                        send = SendUiState.FollowTimedOut(unread.sendId, unread.receipt),
                        composer = composer.copy(submitting = false),
                    )
                }

                is ReplyResult.Failed -> edit {
                    copy(
                        send = unread.copy(failure = result.failure),
                        composer = composer.copy(submitting = false),
                    )
                }
            }
        }
    }

    // --------------------------------------------------------------------- wait

    fun waitForQuiescence() = beginWait(WaitTrigger.Quiescence)

    fun waitForPendingInput() = beginWait(WaitTrigger.PendingInput)

    private fun beginWait(trigger: WaitTrigger) {
        val session = loadedSession ?: return
        if (waitJob?.isActive == true) {
            return
        }
        val waitId = ids.newId()
        activeWaitId = waitId
        edit { copy(wait = WaitUiState.Waiting(waitId)) }
        waitJob = scope.launch {
            val result = try {
                wait.wait(
                    WaitRequest(
                        scope = session,
                        waitId = waitId,
                        attempt = currentAttempt(),
                        trigger = trigger,
                        timeoutMillis = WAIT_TIMEOUT_MILLIS,
                    ),
                )
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                WaitResult.Failed(unexpectedFailure())
            }
            if (activeWaitId != waitId) {
                return@launch
            }
            activeWaitId = null
            waitJob = null
            when (result) {
                WaitResult.Quiescent -> edit { copy(wait = WaitUiState.Quiescent) }
                is WaitResult.ApprovalInput -> {
                    mergePendingItem(result.item)
                    edit { copy(wait = WaitUiState.ApprovalInput(result.item)) }
                }

                is WaitResult.QuestionInput -> {
                    mergePendingItem(result.item)
                    edit { copy(wait = WaitUiState.QuestionInput(result.item)) }
                }

                WaitResult.TimedOut -> edit { copy(wait = WaitUiState.TimedOut) }
                WaitResult.Cancelled -> edit { copy(wait = WaitUiState.Cancelled) }
                is WaitResult.Failed -> {
                    diag.warn(
                        ConversationDiagnosticStage.COMMAND,
                        ConversationDiagnosticEventName.STOP_FAILED,
                        code = result.failure.code,
                    )
                    edit { copy(wait = WaitUiState.Failed(result.failure)) }
                }
            }
        }
    }

    /**
     * Cancels only the local bounded wait. It never issues Host interruption and
     * never removes queued work.
     */
    fun cancelWait() {
        val waitId = activeWaitId ?: return
        activeWaitId = null
        waitJob?.cancel()
        waitJob = null
        edit { copy(wait = WaitUiState.Cancelled) }
        diag.info(
            ConversationDiagnosticStage.COMMAND,
            ConversationDiagnosticEventName.WATCH_STOPPED,
            code = "wait_cancelled",
        )
        scope.launch { wait.cancel(waitId) }
    }

    fun stop() {
        val session = loadedSession ?: return
        if (state.value.stop is StopUiState.InFlight) {
            return
        }
        edit { copy(stop = StopUiState.InFlight) }
        scope.launch {
            val outcome = try {
                commands.stop(session, currentAttempt())
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                StopOutcome.Unconfirmed(unexpectedFailure())
            }
            if (loadedSession != session) {
                return@launch
            }
            when (outcome) {
                StopOutcome.Accepted -> edit { copy(stop = StopUiState.Accepted) }
                StopOutcome.Confirmed -> {
                    edit { copy(stop = StopUiState.Confirmed) }
                    announce(AnnouncementMilestone.StopConfirmed)
                    diag.info(
                        ConversationDiagnosticStage.COMMAND,
                        ConversationDiagnosticEventName.STOP_FAILED,
                        code = "confirmed",
                    )
                }

                is StopOutcome.Failed -> {
                    diag.warn(
                        ConversationDiagnosticStage.COMMAND,
                        ConversationDiagnosticEventName.STOP_FAILED,
                        code = outcome.failure.code,
                    )
                    edit { copy(stop = StopUiState.Failed(outcome.failure)) }
                }

                is StopOutcome.Unconfirmed -> {
                    diag.warn(
                        ConversationDiagnosticStage.COMMAND,
                        ConversationDiagnosticEventName.STOP_FAILED,
                        code = outcome.failure.code,
                    )
                    edit { copy(stop = StopUiState.Unconfirmed(outcome.failure)) }
                }
            }
        }
    }

    // ----------------------------------------------------------- pending input

    fun refreshPendingInputs() {
        val session = loadedSession ?: return
        edit { copy(pendingEntry = PendingEntryUiState.Loading) }
        scope.launch {
            val result = try {
                pendingInput.list(session)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                PendingListResult.Unavailable(unexpectedFailure())
            }
            if (loadedSession != session) {
                return@launch
            }
            val entry = when (result) {
                is PendingListResult.Current -> if (result.items.isEmpty()) {
                    PendingEntryUiState.CurrentEmpty
                } else {
                    PendingEntryUiState.Current(result.items)
                }

                PendingListResult.Unsupported -> PendingEntryUiState.Unsupported
                is PendingListResult.Stale -> {
                    diag.warn(
                        ConversationDiagnosticStage.PENDING_INPUT,
                        ConversationDiagnosticEventName.PENDING_INPUT_STALE,
                        code = result.failure.code,
                    )
                    PendingEntryUiState.Stale(result.failure)
                }

                is PendingListResult.Unavailable ->
                    PendingEntryUiState.Unavailable(result.failure)
            }
            edit { copy(pendingEntry = entry) }
            if (result is PendingListResult.Current) {
                applyReducerEvent(
                    PendingInputsChanged(
                        session,
                        state.value.generation,
                        result.items,
                    ),
                )
            }
        }
    }

    fun refreshQueue() {
        val port = queue ?: return
        val session = loadedSession ?: return
        scope.launch {
            val result = try {
                port.pendingQueue(session)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                Outcome.Failure(unexpectedFailure())
            }
            if (loadedSession != session) {
                return@launch
            }
            when (result) {
                is Outcome.Success -> edit { copy(pendingQueue = result.value.items) }
                is Outcome.Failure -> diag.warn(
                    ConversationDiagnosticStage.QUEUE,
                    ConversationDiagnosticEventName.QUEUE_OPERATION,
                    code = result.failure.code,
                )

                is Outcome.Unconfirmed -> diag.warn(
                    ConversationDiagnosticStage.QUEUE,
                    ConversationDiagnosticEventName.QUEUE_OPERATION,
                    code = result.failure.code,
                )

                Outcome.Cancelled -> Unit
            }
        }
    }

    // ----------------------------------------------------------- announcements

    fun consumeAnnouncement() {
        edit { copy(announcement = null) }
    }

    // ------------------------------------------------------------ observation

    /**
     * Requests a first-visible-output watch. Only visible answer content may
     * satisfy it; a reasoning-only prefix leaves it waiting.
     */
    fun startFirstVisibleOutput() {
        if (loadedSession == null) {
            return
        }
        stopWatch(WatchEndReason.Requested)
        startWatch(WatchMode.FirstVisibleOutput)
    }

    private fun startWatch(mode: WatchMode) {
        val session = loadedSession ?: return
        if (watchJob?.isActive == true) {
            if (watchMode == mode) {
                return
            }
            stopWatch(WatchEndReason.Requested)
        }
        watchMode = mode
        edit {
            copy(
                watch = if (mode == WatchMode.FirstVisibleOutput) {
                    WatchUiState.FirstOutputWaiting
                } else {
                    WatchUiState.Watching
                },
            )
        }
        val request = WatchRequest(session, state.value.generation, mode)
        watchJob = scope.launch {
            val started = try {
                observation.startWatch(request)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (throwable: Throwable) {
                Outcome.Failure(unexpectedFailure())
            }
            when (started) {
                is Outcome.Success -> {
                    watchSession = started.value
                    diag.info(
                        ConversationDiagnosticStage.OBSERVATION,
                        ConversationDiagnosticEventName.WATCH_STARTED,
                        fields = mapOf("mode" to mode.name.lowercase()),
                    )
                    try {
                        while (isActive) {
                            val polled = try {
                                observation.pollEvents(started.value)
                            } catch (cancellation: CancellationException) {
                                throw cancellation
                            } catch (throwable: Throwable) {
                                Outcome.Failure(unexpectedFailure())
                            }
                            when (polled) {
                                is Outcome.Success -> {
                                    handleBatch(polled.value)
                                    if (polled.value.ended != null) {
                                        break
                                    }
                                }

                                is Outcome.Failure -> {
                                    failWatch(polled.failure)
                                    break
                                }

                                is Outcome.Unconfirmed -> {
                                    failWatch(polled.failure)
                                    break
                                }

                                Outcome.Cancelled -> break
                            }
                        }
                    } finally {
                        if (watchSession?.watchId == started.value.watchId) {
                            watchSession = null
                        }
                    }
                }

                is Outcome.Failure -> failWatch(started.failure)
                is Outcome.Unconfirmed -> failWatch(started.failure)
                Outcome.Cancelled -> Unit
            }
        }
    }

    private fun stopWatch(reason: WatchEndReason) {
        val job = watchJob
        val session = watchSession
        watchJob = null
        watchSession = null
        job?.cancel()
        if (session != null) {
            diag.info(
                ConversationDiagnosticStage.OBSERVATION,
                ConversationDiagnosticEventName.WATCH_STOPPED,
                fields = mapOf("reason" to reason.name.lowercase()),
            )
            scope.launch { observation.stopWatch(session) }
        }
        val current = state.value.watch
        if (current != WatchUiState.Idle && current !is WatchUiState.Failed) {
            edit { copy(watch = WatchUiState.Ended(reason)) }
        }
    }

    private fun failWatch(failure: CoreFailure) {
        diag.warn(
            ConversationDiagnosticStage.OBSERVATION,
            ConversationDiagnosticEventName.WATCH_FAILED,
            code = failure.code,
        )
        edit { copy(watch = WatchUiState.Failed(failure)) }
    }

    private fun handleBatch(batch: ObservationBatch) {
        val session = loadedSession ?: return
        if (batch.scope != session || batch.generation != state.value.generation) {
            diag.warn(
                ConversationDiagnosticStage.OBSERVATION,
                ConversationDiagnosticEventName.WATCH_FAILED,
                code = "stale_generation",
                fields = mapOf("events" to batch.events.size.toString()),
            )
            return
        }
        batch.events.forEach { applyReducerEvent(it) }
        if (batch.droppedCount > 0) {
            diag.warn(
                ConversationDiagnosticStage.OBSERVATION,
                ConversationDiagnosticEventName.EVENTS_DROPPED,
                code = "dropped_events",
                fields = mapOf("count" to batch.droppedCount.toString()),
            )
            announce(AnnouncementMilestone.ObservationLost)
            resync()
            return
        }
        batch.ended?.let { handleWatchEnded(it) }
    }

    private fun handleWatchEnded(reason: WatchEndReason) {
        when (reason) {
            WatchEndReason.FirstVisibleOutput -> {
                if (watchMode != WatchMode.FirstVisibleOutput) {
                    return
                }
                val visible = reducerState.previews.any {
                    it.kind.isVisibleOutputBlock && it.text.isNotBlank()
                }
                if (visible) {
                    edit { copy(watch = WatchUiState.FirstOutputObserved) }
                    announce(AnnouncementMilestone.FirstVisibleOutput)
                } else {
                    // Hidden reasoning alone never satisfies the observation.
                    diag.warn(
                        ConversationDiagnosticStage.OBSERVATION,
                        ConversationDiagnosticEventName.WATCH_FAILED,
                        code = "hidden_reasoning_only",
                    )
                    edit { copy(watch = WatchUiState.FirstOutputWaiting) }
                }
            }

            WatchEndReason.Timeout -> edit { copy(watch = WatchUiState.TimedOut) }
            WatchEndReason.DroppedEvents -> {
                announce(AnnouncementMilestone.ObservationLost)
                resync()
            }

            WatchEndReason.Failed -> edit {
                copy(watch = WatchUiState.Failed(unexpectedFailure()))
            }

            WatchEndReason.Requested,
            WatchEndReason.Quiescent,
            WatchEndReason.ScopeChanged,
            WatchEndReason.Backgrounded,
            -> edit { copy(watch = WatchUiState.Ended(reason)) }
        }
    }

    /**
     * Reconciles the affected target through Core after loss: new generation,
     * authoritative latest page and a fresh observation. The old generation can
     * no longer mutate the view.
     */
    private fun resync() {
        if (resyncing) {
            return
        }
        val session = loadedSession ?: return
        resyncing = true
        stopWatch(WatchEndReason.DroppedEvents)
        generationCounter += 1
        val generation = generationCounter
        reducerState = ConversationState.begin(session, generation)
        pagerState = HistoryPagerState.begin(session, projection)
        diag.warn(
            ConversationDiagnosticStage.OBSERVATION,
            ConversationDiagnosticEventName.RESYNC_STARTED,
            code = "resync",
        )
        edit {
            copy(
                generation = generation,
                status = ConversationStatus.Resyncing,
                items = emptyList(),
            )
        }
        loadLatestPage()
        startWatch(WatchMode.Live)
    }

    // ------------------------------------------------------------------ shared

    private fun applyCommittedReply(session: SessionRef, message: Message) {
        if (loadedSession != session) {
            return
        }
        applyReducerEvent(
            MessageCommitted(session, state.value.generation, message),
        )
    }

    private fun mergePendingItem(item: PendingItem) {
        val session = loadedSession ?: return
        val items = reducerState.pendingItems.filterNot { it.itemId == item.itemId } + item
        applyReducerEvent(PendingInputsChanged(session, state.value.generation, items))
    }

    private fun applyReducerEvent(event: ConversationEvent): Boolean {
        val previous = reducerState
        val next = ConversationReducer.reduce(previous, event)
        if (next == previous) {
            return false
        }
        reducerState = next
        edit {
            copy(
                items = next.toItems(),
                phase = next.phase,
                pendingEntry = when {
                    event is PendingInputsChanged && next.pendingItems.isEmpty() ->
                        PendingEntryUiState.CurrentEmpty

                    event is PendingInputsChanged -> PendingEntryUiState.Current(next.pendingItems)
                    else -> pendingEntry
                },
            )
        }
        AnnouncementPolicy.milestones(previous, next).forEach(::announce)
        return true
    }

    private fun announce(milestone: AnnouncementMilestone) {
        announcementToken += 1
        val token = announcementToken
        edit { copy(announcement = milestone, announcementToken = token) }
    }

    private fun loadLatestPage() {
        val session = loadedSession ?: return
        val generation = state.value.generation
        pagerState = HistoryPager.expectLatest(pagerState)
        publishHistory()
        scope.launch {
            val result = guardedHistoryCall {
                history.latestPage(session, projection)
            }
            if (loadedSession != session || state.value.generation != generation) {
                return@launch
            }
            when (result) {
                is Outcome.Success -> {
                    val accepted = HistoryPager.acceptLatest(
                        pagerState,
                        result.value,
                        clock.epochMillis(),
                    )
                    pagerState = accepted.state
                    applyReducerEvent(
                        HistoryReplaced(session, generation, result.value.messages),
                    )
                    resyncing = false
                    edit {
                        copy(
                            history = historyFromPager(),
                            status = statusFromPager(),
                        )
                    }
                }

                is Outcome.Failure -> finishHistoryFailure(result.failure)
                is Outcome.Unconfirmed -> finishHistoryFailure(result.failure)
                Outcome.Cancelled -> Unit
            }
        }
    }

    private fun finishHistoryFailure(failure: CoreFailure) {
        pagerState = HistoryPager.fail(pagerState, failure).state
        edit {
            copy(
                history = historyFromPager(),
                status = statusFromPager(),
            )
        }
        diag.warn(
            ConversationDiagnosticStage.REPLY_READ,
            ConversationDiagnosticEventName.REPLY_UNREADABLE,
            code = failure.code,
        )
    }

    private fun publishHistory(insertion: PageInsertion? = null) {
        edit {
            copy(
                history = historyFromPager().copy(lastInsertion = insertion),
                status = statusFromPager(),
            )
        }
    }

    private fun historyFromPager(): HistoryUiState = HistoryUiState(
        status = pagerState.status,
        cached = pagerState.cached,
        hasOlder = pagerState.hasOlder,
        anchor = pagerState.anchor,
        exactMessageId = pagerState.exactMessageId,
        failure = when (val status = pagerState.status) {
            is HistoryPagerStatus.CachedUnavailable -> status.failure
            is HistoryPagerStatus.Failed -> status.failure
            else -> null
        },
    )

    private fun statusFromPager(): ConversationStatus = when (pagerState.status) {
        is HistoryPagerStatus.Ready -> if (pagerState.cached) {
            ConversationStatus.Cached
        } else {
            ConversationStatus.Ready
        }

        is HistoryPagerStatus.Cached,
        is HistoryPagerStatus.CachedUnavailable,
        -> ConversationStatus.Cached

        is HistoryPagerStatus.Invalidated -> ConversationStatus.Invalidated
        is HistoryPagerStatus.Failed -> ConversationStatus.Failed
        HistoryPagerStatus.Idle,
        HistoryPagerStatus.LoadingLatest,
        HistoryPagerStatus.LoadingOlder,
        -> ConversationStatus.Loading
    }

    private fun loadDraft(session: SessionRef) {
        scope.launch {
            when (val result = draftStore.claim(session)) {
                is Outcome.Success -> {
                    draftLease = result.value.lease
                    val loaded = result.value.text ?: return@launch
                    if (loadedSession == session && state.value.composer.text.isEmpty()) {
                        edit { copy(composer = composer.copy(text = loaded)) }
                    }
                }

                is Outcome.Failure -> diag.warn(
                    ConversationDiagnosticStage.COMMAND,
                    ConversationDiagnosticEventName.SEND_FAILED,
                    code = result.failure.code,
                    fields = mapOf("operation" to "draft_load"),
                )

                is Outcome.Unconfirmed -> Unit
                Outcome.Cancelled -> Unit
            }
        }
    }

    private fun currentAttempt(): AttemptRef? =
        reducerState.previews.lastOrNull()?.attempt ?: reducerState.committed.lastOrNull()?.origin

    private fun isCurrentSend(sendId: SendId): Boolean = when (val send = state.value.send) {
        is SendUiState.Submitting -> send.sendId == sendId
        is SendUiState.FollowPending -> send.sendId == sendId
        is SendUiState.FollowTimedOut -> send.sendId == sendId
        is SendUiState.Unconfirmed -> send.sendId == sendId
        is SendUiState.ConfirmedUnread -> send.sendId == sendId
        is SendUiState.Recovering -> send.sendId == sendId
        else -> false
    }

    private suspend fun guardedHistoryCall(
        call: suspend () -> Outcome<HistoryPage>,
    ): Outcome<HistoryPage> = try {
        call()
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (throwable: Throwable) {
        Outcome.Failure(unexpectedFailure())
    }

    private fun unexpectedFailure(): CoreFailure =
        CoreFailure("unexpected", "Unexpected failure", retryable = true)

    override fun onCleared() {
        watchJob?.cancel()
        waitJob?.cancel()
        draftSaveJob?.cancel()
        val lease = draftLease
        val session = loadedSession
        if (lease != null && session != null) {
            val text = draftsBySession[session] ?: state.value.composer.text
            CoroutineScope(Dispatchers.Main.immediate).launch {
                draftStore.release(lease, text)
            }
        }
        super.onCleared()
    }

    private companion object {
        const val REPLY_FOLLOW_TIMEOUT_MILLIS = 60_000L
        const val DRAFT_DEBOUNCE_MILLIS = 120L
        const val WAIT_TIMEOUT_MILLIS = 120_000L
    }

    private fun edit(transform: ConversationUiState.() -> ConversationUiState) {
        update { transform(it) }
    }


}
