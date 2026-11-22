package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.SendId
import com.whalerein.android.state.core.SessionRef

/** Immutable conversation screen state; Compose only renders it. */
data class ConversationUiState(
    val session: SessionRef? = null,
    /** Observation epoch; every event is validated against it. */
    val generation: Long = 0,
    val status: ConversationStatus = ConversationStatus.Loading,
    val items: List<ConversationItem> = emptyList(),
    val phase: RunPhase = RunPhase.Unknown,
    val history: HistoryUiState = HistoryUiState(),
    val composer: ComposerUiState = ComposerUiState(),
    val send: SendUiState = SendUiState.Idle,
    val wait: WaitUiState = WaitUiState.Idle,
    val stop: StopUiState = StopUiState.Idle,
    val pendingEntry: PendingEntryUiState = PendingEntryUiState.Loading,
    val pendingQueue: List<PendingQueueItem> = emptyList(),
    val watch: WatchUiState = WatchUiState.Idle,
    val announcement: AnnouncementMilestone? = null,
    val announcementToken: Long = 0,
)

enum class ConversationStatus {
    Loading,
    Ready,
    Cached,
    Resyncing,
    Invalidated,
    Failed,
}

data class ComposerUiState(
    val text: String = "",
    val mode: DeliveryMode = DeliveryMode.Queue,
    val follow: FollowMode = FollowMode.ReceiptOnly,
    val availability: ComposerAvailability = ComposerAvailability.Available,
    val submitting: Boolean = false,
)

enum class ComposerAvailability {
    Available,
    /** No transport, draft retained. */
    Offline,

    /** The Source cannot accept input right now, draft retained. */
    Unavailable,

    /** The selected mode is not advertised by this Source. */
    Unsupported,
}

data class HistoryUiState(
    val status: HistoryPagerStatus = HistoryPagerStatus.Idle,
    val cached: Boolean = false,
    val hasOlder: Boolean = false,
    val anchor: ReadingAnchor? = null,
    val lastInsertion: PageInsertion? = null,
    val exactMessageId: String? = null,
    val failure: CoreFailure? = null,
)

/** Send, follow and recovery states are deliberately distinct types. */
sealed interface SendUiState {
    data object Idle : SendUiState

    data class Submitting(
        val sendId: SendId,
        val scope: SessionRef,
    ) : SendUiState

    data class ReceiptOnly(val sendId: SendId, val receipt: SendReceipt) : SendUiState

    data class FollowPending(
        val sendId: SendId,
        val receipt: SendReceipt,
    ) : SendUiState

    data class FollowedReply(
        val sendId: SendId,
        val receipt: SendReceipt,
        val reply: Message,
    ) : SendUiState

    data class FollowPendingInput(
        val sendId: SendId,
        val receipt: SendReceipt,
        val item: PendingItem,
    ) : SendUiState

    data class FollowTimedOut(val sendId: SendId, val receipt: SendReceipt) : SendUiState

    data class FollowFailed(val sendId: SendId, val failure: CoreFailure) : SendUiState

    /** The Host may have accepted; recover by original identity, never resubmit. */
    data class Unconfirmed(val sendId: SendId, val failure: CoreFailure) : SendUiState

    data class DefiniteFailure(val failure: CoreFailure) : SendUiState

    /** Confirmed reply whose content cannot be read; retry reads only content. */
    data class ConfirmedUnread(
        val sendId: SendId,
        val receipt: SendReceipt,
        val failure: CoreFailure,
        val canRetry: Boolean = true,
    ) : SendUiState

    data class Recovering(val sendId: SendId) : SendUiState

    data class Recovered(
        val sendId: SendId,
        val receipt: SendReceipt,
        val reply: Message? = null,
    ) : SendUiState

    data class Cancelled(val sendId: SendId) : SendUiState
}

sealed interface WaitUiState {
    data object Idle : WaitUiState

    data class Waiting(val waitId: String) : WaitUiState

    data object Quiescent : WaitUiState

    data class ApprovalInput(val item: PendingItem.Approval) : WaitUiState

    data class QuestionInput(val item: PendingItem.Questions) : WaitUiState

    data object TimedOut : WaitUiState

    data object Cancelled : WaitUiState

    data class Failed(val failure: CoreFailure) : WaitUiState
}

sealed interface StopUiState {
    data object Idle : StopUiState

    data object InFlight : StopUiState

    /** The Host accepted the request but has not confirmed the run stopped. */
    data object Accepted : StopUiState

    /** The only state that may be reported as stopped. */
    data object Confirmed : StopUiState

    data class Failed(val failure: CoreFailure) : StopUiState

    data class Unconfirmed(val failure: CoreFailure) : StopUiState
}

/** Fixed pending-input entry. Stale/unavailable/unsupported are not empty. */
sealed interface PendingEntryUiState {
    data object Loading : PendingEntryUiState

    data class Current(val items: List<PendingItem>) : PendingEntryUiState

    data object CurrentEmpty : PendingEntryUiState

    data class Stale(val failure: CoreFailure) : PendingEntryUiState

    data class Unavailable(val failure: CoreFailure) : PendingEntryUiState

    data object Unsupported : PendingEntryUiState
}

sealed interface WatchUiState {
    data object Idle : WatchUiState

    data object Watching : WatchUiState

    /** Waiting for the first visible answer prefix, not hidden reasoning. */
    data object FirstOutputWaiting : WatchUiState

    data object FirstOutputObserved : WatchUiState

    data class Ended(val reason: WatchEndReason) : WatchUiState

    data object TimedOut : WatchUiState

    data class Failed(val failure: CoreFailure) : WatchUiState
}
