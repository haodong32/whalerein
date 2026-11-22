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

private fun guardParts(guard: Guard): Pair<Long, String>? {
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
    ): Outcome<QueueMutation> = outcomeOf {
        val view = owner.core().updatePendingQueue(
            scope.host.hostId.value,
            scope.sessionId.value,
            itemId,
            action,
            text,
        )
        when (action) {
            QueueAction.EDIT -> QueueMutation.Edited(view.itemId)
            QueueAction.REMOVE -> QueueMutation.Removed(view.itemId)
            QueueAction.PROMOTE -> QueueMutation.Steered(view.itemId)
        }
    }
}

internal class PendingInputAdapter(private val owner: AndroidCore) : PendingInputPort {
    private val batches = mutableMapOf<String, QuestionBatchView>()

    override suspend fun list(scope: SessionRef): PendingListResult = try {
        val items = mutableListOf<PendingItem>()
        owner.core().approvals(scope.host.hostId.value, scope.sessionId.value).forEach {
            items += it.toPendingItem()
        }
        var cursor = ""
        do {
            val page = owner.core().listQuestions(scope.host.hostId.value, scope.sessionId.value, cursor)
            page.batches.forEach { batch ->
                batches[batch.batchId] = batch
                items += batch.toPendingItem(scope)
            }
            cursor = page.nextCursor
        } while (cursor.isNotEmpty())
        PendingListResult.Current(items)
    } catch (error: Throwable) {
        val failure = error.toCoreFailure()
        if (failure.code == "capability_missing" || failure.code == "not_found") {
            PendingListResult.Unsupported
        } else if (failure.retryable) {
            PendingListResult.Unavailable(failure)
        } else {
            PendingListResult.Stale(failure)
        }
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
        when (receipt.status) {
            "succeeded", "host_accepted", "received" -> PendingActionOutcome.Applied(approvalId)
            "host_offline", "unknown" -> PendingActionOutcome.Unconfirmed(
                CoreFailure("uncertain", "The decision result is unknown.", retryable = true),
            )
            else -> PendingActionOutcome.Failed(
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
        val batch = batches[batchId] ?: owner.core().getQuestion(
            scope.host.hostId.value,
            scope.sessionId.value,
            batchId,
        ).also { batches[batchId] = it }
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
        when (receipt.status) {
            "succeeded", "host_accepted", "received" -> PendingActionOutcome.Applied(batchId)
            "host_offline", "unknown" -> PendingActionOutcome.Unconfirmed(
                CoreFailure("uncertain", "The answer result is unknown.", retryable = true),
            )
            else -> PendingActionOutcome.Failed(
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
        val receipt = owner.core().requestQuestionAnswerCancel(
            scope.host.hostId.value,
            scope.sessionId.value,
            answerRef.answerId,
        )
        if (receipt.cancellationRequested) PendingActionOutcome.Applied(answerRef.answerId)
        else PendingActionOutcome.ResolvedElsewhere
    } catch (error: Throwable) {
        PendingActionOutcome.Failed(error.toCoreFailure())
    }
}

private fun QuestionBatchView.toPendingItem(scope: SessionRef): PendingItem.Questions =
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
}
