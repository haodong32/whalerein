package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionRef

/** Model catalog and pending-queue contract. Every call is Host-scoped. */
data class ModelInfo(
    val modelId: String,
    val displayName: String,
    val current: Boolean = false,
)

data class ModelCatalog(
    val models: List<ModelInfo>,
    val currentModelId: String?,
    val guard: Guard? = null,
)

data class ModelSelection(val modelId: String)

data class PendingQueueItem(
    val itemId: String,
    val text: String,
    val guard: Guard,
    val steerable: Boolean = true,
)

data class QueueSnapshot(
    val items: List<PendingQueueItem>,
    val capable: Boolean = true,
)

sealed interface QueueMutation {
    data class Edited(val itemId: String) : QueueMutation

    data class Removed(val itemId: String) : QueueMutation

    data class Steered(val itemId: String) : QueueMutation
}

interface ModelsQueuePort {
    suspend fun modelCatalog(scope: SessionRef): Outcome<ModelCatalog>

    suspend fun selectModel(
        scope: SessionRef,
        modelId: String,
        guard: Guard?,
    ): Outcome<ModelSelection>

    suspend fun pendingQueue(scope: SessionRef): Outcome<QueueSnapshot>

    suspend fun editQueueItem(
        scope: SessionRef,
        itemId: String,
        text: String,
        guard: Guard,
    ): Outcome<QueueMutation>

    suspend fun removeQueueItem(
        scope: SessionRef,
        itemId: String,
        guard: Guard,
    ): Outcome<QueueMutation>

    suspend fun steerQueueItem(
        scope: SessionRef,
        itemId: String,
        guard: Guard,
    ): Outcome<QueueMutation>
}
