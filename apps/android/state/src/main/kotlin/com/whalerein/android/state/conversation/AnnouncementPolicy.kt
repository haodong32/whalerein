package com.whalerein.android.state.conversation

/**
 * Meaningful stream milestones announced to accessibility services.
 *
 * This is a pure transition policy: preview deltas, token ticks and reasoning
 * prefixes never produce an announcement. The app maps a milestone to a
 * localized resource string once, so TalkBack hears state changes rather than
 * every update.
 */
enum class AnnouncementMilestone {
    RunStarted,
    FirstVisibleOutput,
    AwaitingApproval,
    AwaitingQuestion,
    RunCompleted,
    RunFailed,
    ObservationLost,
    ObservationResumed,
    StopConfirmed,
}

object AnnouncementPolicy {
    fun milestones(
        previous: ConversationState?,
        next: ConversationState,
    ): List<AnnouncementMilestone> {
        if (previous == null || previous.scope != next.scope) {
            return emptyList()
        }
        val milestones = mutableListOf<AnnouncementMilestone>()
        if (!previous.phase.isActive && next.phase.isActive) {
            milestones += AnnouncementMilestone.RunStarted
        }
        if (previous.phase.isActive && next.phase == RunPhase.Completed) {
            milestones += AnnouncementMilestone.RunCompleted
        }
        if (previous.phase != RunPhase.Failed && next.phase == RunPhase.Failed) {
            milestones += AnnouncementMilestone.RunFailed
        }
        if (previous.pendingItems.countApprovals() == 0 &&
            next.pendingItems.countApprovals() > 0
        ) {
            milestones += AnnouncementMilestone.AwaitingApproval
        }
        if (previous.pendingItems.countQuestions() == 0 &&
            next.pendingItems.countQuestions() > 0
        ) {
            milestones += AnnouncementMilestone.AwaitingQuestion
        }
        if (!previous.hasVisibleLiveOutput() && next.hasVisibleLiveOutput()) {
            milestones += AnnouncementMilestone.FirstVisibleOutput
        }
        return milestones.distinct()
    }
}

internal fun List<PendingItem>.countApprovals(): Int = count { it is PendingItem.Approval }

internal fun List<PendingItem>.countQuestions(): Int = count { it is PendingItem.Questions }

internal fun ConversationState.hasVisibleLiveOutput(): Boolean = previews.any { preview ->
    preview.kind.isVisibleOutputBlock && preview.text.isNotBlank()
}

/**
 * State projection consumed by accessibility and keyboard navigation.
 *
 * A control stays reachable in the semantic tree even when its action is not
 * currently enabled; the flags describe availability, not removal.
 */
data class ConversationAccessibility(
    val canSend: Boolean,
    val canStop: Boolean,
    val canCancelWait: Boolean,
    val canReviewPendingInput: Boolean,
    val pendingInputCount: Int,
    val anchor: ReadingAnchor?,
    val milestone: AnnouncementMilestone?,
)

fun ConversationUiState.accessibility(): ConversationAccessibility = ConversationAccessibility(
    canSend = session != null && composer.text.isNotBlank() && !composer.submitting,
    canStop = phase.isActive && stop !is StopUiState.InFlight,
    canCancelWait = wait is WaitUiState.Waiting,
    canReviewPendingInput = pendingEntry !is PendingEntryUiState.Loading,
    pendingInputCount = when (val entry = pendingEntry) {
        is PendingEntryUiState.Current -> entry.items.size
        else -> 0
    },
    anchor = history.anchor,
    milestone = announcement,
)
