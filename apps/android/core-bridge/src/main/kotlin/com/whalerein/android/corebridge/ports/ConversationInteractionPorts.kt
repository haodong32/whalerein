package com.whalerein.android.corebridge.ports

import com.whalerein.android.corebridge.AndroidCore
import com.whalerein.android.corebridge.QuestionBatchView
import com.whalerein.android.corebridge.QuestionAnswerView
import com.whalerein.android.corebridge.QueueAction
import com.whalerein.android.state.conversation.AnswerRef
import com.whalerein.android.state.conversation.ApprovalDecision
import com.whalerein.android.state.conversation.DraftPort
import com.whalerein.android.state.conversation.Guard
import com.whalerein.android.state.conversation.ModelCatalog
import com.whalerein.android.state.conversation.ModelInfo
import com.whalerein.android.state.conversation.ModelSelection
import com.whalerein.android.state.conversation.ModelsQueuePort
import com.whalerein.android.state.conversation.PendingActionOutcome
import com.whalerein.android.state.conversation.PendingDetailResult
import com.whalerein.android.state.conversation.PendingInputPort
import com.whalerein.android.state.conversation.PendingItem
import com.whalerein.android.state.conversation.PendingListResult
import com.whalerein.android.state.conversation.PendingSendRecord
import com.whalerein.android.state.conversation.QueueMutation
import com.whalerein.android.state.conversation.QueueSnapshot
import com.whalerein.android.state.conversation.Question
import com.whalerein.android.state.conversation.QuestionAnswer
import com.whalerein.android.state.conversation.QuestionBatch
import com.whalerein.android.state.conversation.QuestionKind
import com.whalerein.android.state.conversation.QuestionOption
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionRef

internal fun guardParts(guard: Guard): Pair<Long, String>? {
    val separator = guard.value.indexOf(':')
    if (separator <= 0) return null
    val version = guard.value.substring(0, separator).toLongOrNull() ?: return null
    val digest = guard.value.substring(separator + 1)
    if (digest.length != 64) return null
    return version to digest
}

internal class ModelsQueueAdapter(private val owner: AndroidCore) : ModelsQueuePort {
    override suspend fun modelCatalog(scope: SessionRef): Outcome<ModelCatalog> = outcomeOf {
        val catalog = owner.core().modelCatalog(scope.host.hostId.value)
        val models = catalog.providers.flatMap { provider ->
            provider.models.map { model ->
                ModelInfo(
                    modelId = "${provider.id}/${model.id}",
                    displayName = model.name.ifEmpty { model.id },
                    current = catalog.defaultSelection?.let {
                        it.provider == provider.id && it.model == model.id
                    } == true,
                )
            }
        }
        ModelCatalog(
            models = models,
            currentModelId = catalog.defaultSelection?.let { "${it.provider}/${it.model}" },
        )
    }

    override suspend fun selectModel(
        scope: SessionRef,
        modelId: String,
        guard: Guard?,
    ): Outcome<ModelSelection> = outcomeOf {
        val parts = modelId.split('/', limit = 2)
        require(parts.size == 2)
        val result = owner.core().selectSessionModel(
            scope.host.hostId.value,
            scope.sessionId.value,
            parts[0],
            parts[1],
            null,
        )
        check(result.receipt.status != "failed")
        ModelSelection(modelId)
    }

    override suspend fun pendingQueue(scope: SessionRef): Outcome<QueueSnapshot> = outcomeOf {
        val queue = owner.core().pendingQueue(scope.host.hostId.value, scope.sessionId.value)
        QueueSnapshot(
            items = queue.items.map { item ->
                val text = item.blocks.filterIsInstance<com.whalerein.android.corebridge.PendingQueueBlockView.Text>()
                    .joinToString("\n") { it.text }
                com.whalerein.android.state.conversation.PendingQueueItem(
                    itemId = item.itemId,
                    text = text,
                    guard = Guard(item.itemId),
                    steerable = item.actions.contains("promote"),
                )
            },
            capable = queue.available,
        )
    }

    override suspend fun editQueueItem(
        scope: SessionRef,
        itemId: String,
        text: String,
        guard: Guard,
    ): Outcome<QueueMutation> = mutate(scope, itemId, QueueAction.EDIT, text)

    override suspend fun removeQueueItem(
        scope: SessionRef,
        itemId: String,
        guard: Guard,
    ): Outcome<QueueMutation> = mutate(scope, itemId, QueueAction.REMOVE, "")

    override suspend fun steerQueueItem(
        scope: SessionRef,
        itemId: String,
        guard: Guard,
    ): Outcome<QueueMutation> = mutate(scope, itemId, QueueAction.PROMOTE, "")

    private suspend fun mutate(
        scope: SessionRef,
        itemId: String,
        action: QueueAction,
        text: String,
    ): Outcome<QueueMutation> {
        val view = try {
            owner.core().updatePendingQueue(
                scope.host.hostId.value,
                scope.sessionId.value,
                itemId,
                action,
                text,
            )
        } catch (cancelled: kotlinx.coroutines.CancellationException) {
            throw cancelled
        } catch (error: Throwable) {
            val failure = error.toCoreFailure()
            return if (failure.retryable) Outcome.Unconfirmed(failure) else Outcome.Failure(failure)
        }
        val mutation = when (action) {
            QueueAction.EDIT -> QueueMutation.Edited(view.itemId)
            QueueAction.REMOVE -> QueueMutation.Removed(view.itemId)
            QueueAction.PROMOTE -> QueueMutation.Steered(view.itemId)
        }
        return when (commandDisposition(view.receipt.status)) {
            CommandDisposition.Applied -> Outcome.Success(mutation)
            CommandDisposition.Unconfirmed -> Outcome.Unconfirmed(
                CoreFailure(
                    "uncertain",
                    "The queue change is not confirmed; refresh the Host queue.",
                    retryable = true,
                ),
            )

            CommandDisposition.Rejected -> Outcome.Failure(
                CoreFailure(
                    view.receipt.status,
                    "The Host rejected the queue change.",
                    retryable = false,
                ),
            )
        }
    }
}

internal class PendingInputAdapter(private val owner: AndroidCore) : PendingInputPort {
    private val batches = mutableMapOf<String, QuestionBatchView>()
    private val commands = AnswerCommandRegistry()

    override suspend fun list(scope: SessionRef): PendingListResult = try {
        val approvals = owner.core()
            .approvals(scope.host.hostId.value, scope.sessionId.value)
            .map { it.toPendingItem() }
        val items = approvals.toMutableList()
        try {
            var cursor = ""
            do {
                val page = owner.core().listQuestions(
                    scope.host.hostId.value,
                    scope.sessionId.value,
                    cursor,
                )
                page.batches.forEach { batch ->
                    batches[batch.batchId] = batch
                    items += batch.toPendingItem(scope)
                }
                cursor = page.nextCursor
            } while (cursor.isNotEmpty())
        } catch (error: Throwable) {
            // A Host without question support still has a usable approval
            // list; a question failure must not discard retrieved approvals.
            return pendingListAfterQuestionFailure(approvals, error.toCoreFailure())
        }
        PendingListResult.Current(items)
    } catch (error: Throwable) {
        val failure = error.toCoreFailure()
        pendingListAfterQuestionFailure(emptyList(), failure)
    }

    override suspend fun get(scope: SessionRef, itemId: String): PendingDetailResult = try {
        val approval = runCatching { owner.core().getApproval(scope.host.hostId.value, itemId) }.getOrNull()
        if (approval != null) {
            PendingDetailResult.Found(approval.toPendingItem())
        } else {
            val batch = owner.core().getQuestion(
                scope.host.hostId.value,
                scope.sessionId.value,
                itemId,
            )
            batches[batch.batchId] = batch
            PendingDetailResult.Found(batch.toPendingItem(scope))
        }
    } catch (error: Throwable) {
        val failure = error.toCoreFailure()
        if (failure.code == "not_found") PendingDetailResult.Missing
        else PendingDetailResult.Unavailable(failure)
    }

    override suspend fun resolveApproval(
        scope: SessionRef,
        approvalId: String,
        decision: ApprovalDecision,
        guard: Guard,
    ): PendingActionOutcome = try {
        val (version, digest) = guardParts(guard)
            ?: return PendingActionOutcome.GuardChanged(null)
        val receipt = owner.core().resolveApproval(
            scope.host.hostId.value,
            scope.sessionId.value,
            approvalId,
            version,
            digest,
            decision == ApprovalDecision.Approve,
        )
        when (commandDisposition(receipt.status)) {
            CommandDisposition.Applied -> PendingActionOutcome.Applied(approvalId)
            CommandDisposition.Unconfirmed -> PendingActionOutcome.Unconfirmed(
                CoreFailure("uncertain", "The decision result is unknown.", retryable = true),
            )

            CommandDisposition.Rejected -> PendingActionOutcome.Failed(
                CoreFailure(receipt.status, "The decision was rejected."),
            )
        }
    } catch (error: Throwable) {
        val failure = error.toCoreFailure()
        when (failure.code) {
            "conflict", "stale_history" -> PendingActionOutcome.GuardChanged(null)
            "not_found" -> PendingActionOutcome.Expired
            "capability_missing" -> PendingActionOutcome.Unsupported
            else -> if (failure.retryable) PendingActionOutcome.Unconfirmed(failure)
            else PendingActionOutcome.Failed(failure)
        }
    }

    override suspend fun answerQuestion(
        scope: SessionRef,
        batchId: String,
        answers: List<QuestionAnswer>,
        guard: Guard,
        answerRef: AnswerRef,
    ): PendingActionOutcome = try {
        // A cached batch is only usable when the reviewed guard still matches
        // it; otherwise the answer must be reviewed again against fresh state.
        var batch = batches[batchId]
        if (batch == null || !reviewedGuardMatches(guard, batch.version, batch.digestHex)) {
            batch = owner.core().getQuestion(
                scope.host.hostId.value,
                scope.sessionId.value,
                batchId,
            ).also { batches[batchId] = it }
        }
        if (!reviewedGuardMatches(guard, batch.version, batch.digestHex)) {
            return PendingActionOutcome.GuardChanged(batchGuard(batch.version, batch.digestHex))
        }
        val views = answers.map { answer ->
            QuestionAnswerView(
                id = answer.questionId,
                selected = answer.selectedOptionIds,
                custom = answer.text,
            )
        }
        val receipt = owner.core().answerQuestion(
            scope.host.hostId.value,
            scope.sessionId.value,
            batch,
            views,
        )
        // Keep Core's command identity even when the outcome is still unknown,
        // so an explicit cancellation targets the exact original command.
        commands.bind(answerRef.answerId, AnswerCommand(receipt.commandId, batchId))
        when (commandDisposition(receipt.status)) {
            CommandDisposition.Applied -> PendingActionOutcome.Applied(batchId)
            CommandDisposition.Unconfirmed -> PendingActionOutcome.Unconfirmed(
                CoreFailure("uncertain", "The answer result is unknown.", retryable = true),
            )

            CommandDisposition.Rejected -> PendingActionOutcome.Failed(
                CoreFailure(receipt.status, "The answer was rejected."),
            )
        }
    } catch (error: Throwable) {
        val failure = error.toCoreFailure()
        when (failure.code) {
            "conflict", "stale_history" -> PendingActionOutcome.GuardChanged(null)
            "not_found" -> PendingActionOutcome.Expired
            else -> if (failure.retryable) PendingActionOutcome.Unconfirmed(failure)
            else PendingActionOutcome.Failed(failure)
        }
    }

    override suspend fun cancelAnswer(
        scope: SessionRef,
        answerRef: AnswerRef,
    ): PendingActionOutcome = try {
        val commandId = commands.commandFor(answerRef.answerId)
            ?: return PendingActionOutcome.ResolvedElsewhere
        val receipt = owner.core().requestQuestionAnswerCancel(
            scope.host.hostId.value,
            scope.sessionId.value,
            commandId,
        )
        commands.clear(answerRef.answerId)
        if (receipt.cancellationRequested) {
            PendingActionOutcome.Applied(receipt.commandId.ifEmpty { commandId })
        } else {
            PendingActionOutcome.ResolvedElsewhere
        }
    } catch (error: Throwable) {
        val failure = error.toCoreFailure()
        if (failure.code == "not_found") PendingActionOutcome.ResolvedElsewhere
        else if (failure.retryable) PendingActionOutcome.Unconfirmed(failure)
        else PendingActionOutcome.Failed(failure)
    }
}

internal fun QuestionBatchView.toPendingItem(scope: SessionRef): PendingItem.Questions =
    PendingItem.Questions(
        QuestionBatch(
            batchId = batchId,
            questions = questions.map { item ->
                Question(
                    questionId = item.id,
                    kind = if (item.multiSelect) QuestionKind.MultiChoice
                    else if (item.options.isEmpty()) QuestionKind.Open
                    else QuestionKind.SingleChoice,
                    prompt = item.question,
                    options = item.options.mapIndexed { index, option ->
                        QuestionOption(
                            optionId = option.label,
                            label = option.label,
                            description = option.description,
                        )
                    },
                )
            },
            guard = Guard("$version:$digestHex"),
            title = batchId,
        ),
    )

/**
 * Draft and unresolved-send storage backed by Core's encrypted, Host-scoped
 * draft table.
 *
 * The unresolved send is a separate reserved row in the same store, so it gets
 * the same encryption, scope binding and Host-clear behavior without a schema
 * change. Its JSON holds the original send id, submitted text and follow mode.
 */
internal class DraftAdapter(private val owner: AndroidCore) : DraftPort {
    override suspend fun load(scope: SessionRef): Outcome<String?> = outcomeOf {
        owner.core().loadDraft(scope.host.hostId.value, scope.sessionId.value)
    }

    override suspend fun save(scope: SessionRef, text: String): Outcome<Unit> = outcomeOf {
        owner.core().saveDraft(scope.host.hostId.value, scope.sessionId.value, text)
    }

    override suspend fun clear(scope: SessionRef): Outcome<Unit> = outcomeOf {
        owner.core().clearDraft(scope.host.hostId.value, scope.sessionId.value)
    }

    override suspend fun savePendingSend(
        scope: SessionRef,
        record: PendingSendRecord,
    ): Outcome<Unit> = outcomeOf {
        owner.core().saveDraft(
            scope.host.hostId.value,
            pendingSendStorageKey(scope.sessionId.value),
            encodePendingSend(record),
        )
    }

    override suspend fun loadPendingSend(
        scope: SessionRef,
    ): Outcome<PendingSendRecord?> = outcomeOf {
        val encoded = owner.core().loadDraft(
            scope.host.hostId.value,
            pendingSendStorageKey(scope.sessionId.value),
        ) ?: return@outcomeOf null
        decodePendingSend(encoded)
    }

    override suspend fun clearPendingSend(scope: SessionRef): Outcome<Unit> = outcomeOf {
        owner.core().clearDraft(
            scope.host.hostId.value,
            pendingSendStorageKey(scope.sessionId.value),
        )
    }
}
