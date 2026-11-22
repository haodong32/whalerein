package com.whalerein.android.ui.conversation

import androidx.annotation.StringRes
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import com.whalerein.android.R
import com.whalerein.android.state.conversation.AnnouncementMilestone
import com.whalerein.android.state.conversation.ConversationUiState

/**
 * Accessibility announcement policy for the conversation.
 *
 * Only meaningful state milestones produce an announcement. Preview token
 * deltas and progress ticks are never announced; the state model already
 * collapses them into [AnnouncementMilestone].
 */
@Composable
fun AnnouncementPolicy(
    state: ConversationUiState,
    onConsumed: () -> Unit,
) {
    val view = LocalView.current
    val context = LocalContext.current
    LaunchedEffect(state.announcementToken) {
        val milestone = state.announcement ?: return@LaunchedEffect
        if (state.announcementToken == 0L) {
            return@LaunchedEffect
        }
        view.announceForAccessibility(context.getString(milestone.labelRes()))
        onConsumed()
    }
}

@StringRes
fun AnnouncementMilestone.labelRes(): Int = when (this) {
    AnnouncementMilestone.RunStarted -> R.string.conversation_announcement_run_started
    AnnouncementMilestone.FirstVisibleOutput ->
        R.string.conversation_announcement_first_visible_output

    AnnouncementMilestone.AwaitingApproval ->
        R.string.conversation_announcement_awaiting_approval

    AnnouncementMilestone.AwaitingQuestion ->
        R.string.conversation_announcement_awaiting_question

    AnnouncementMilestone.RunCompleted -> R.string.conversation_announcement_run_completed
    AnnouncementMilestone.RunFailed -> R.string.conversation_announcement_run_failed
    AnnouncementMilestone.ObservationLost ->
        R.string.conversation_announcement_observation_lost

    AnnouncementMilestone.ObservationResumed ->
        R.string.conversation_announcement_observation_resumed

    AnnouncementMilestone.StopConfirmed -> R.string.conversation_announcement_stop_confirmed
}
