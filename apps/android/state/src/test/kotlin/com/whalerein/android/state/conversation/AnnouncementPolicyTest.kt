package com.whalerein.android.state.conversation

import com.whalerein.android.state.fixtures.conversation.ConversationFixtures
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

class AnnouncementPolicyTest : ConversationTestBase() {
    @Test
    @DisplayName("AC-13.announcement only meaningful milestones are announced")
    fun ac13Announcement() {
        val scope = ConversationFixtures.session()
        val attempt = ConversationFixtures.attempt()
        val idle = ConversationState.begin(scope, 1)
        val running = idle.copy(phase = RunPhase.Running)
        assertEquals(
            listOf(AnnouncementMilestone.RunStarted),
            AnnouncementPolicy.milestones(idle, running),
        )

        val reasoningOnly = running.copy(
            previews = listOf(
                ConversationFixtures.preview(attempt, "reason-1", MessageBlockKind.Reasoning, "why"),
            ),
        )
        assertTrue(AnnouncementPolicy.milestones(running, reasoningOnly).isEmpty())

        val firstText = reasoningOnly.copy(
            previews = reasoningOnly.previews + ConversationFixtures.preview(
                attempt,
                "answer-1",
                MessageBlockKind.Text,
                "visible",
            ),
        )
        assertEquals(
            listOf(AnnouncementMilestone.FirstVisibleOutput),
            AnnouncementPolicy.milestones(reasoningOnly, firstText),
        )

        val moreDeltas = firstText.copy(
            previews = firstText.previews.map {
                if (it.blockId == "answer-1") it.copy(text = "visible more") else it
            },
        )
        assertTrue(AnnouncementPolicy.milestones(firstText, moreDeltas).isEmpty())

        val approval = ConversationFixtures.approvalItem()
        val withApproval = moreDeltas.copy(pendingItems = listOf(approval))
        assertEquals(
            listOf(AnnouncementMilestone.AwaitingApproval),
            AnnouncementPolicy.milestones(moreDeltas, withApproval),
        )
        val withSecondApproval = withApproval.copy(
            pendingItems = withApproval.pendingItems + ConversationFixtures.approvalItem("approval-2"),
        )
        assertTrue(AnnouncementPolicy.milestones(withApproval, withSecondApproval).isEmpty())

        val question = ConversationFixtures.questionItem()
        val withQuestion = withSecondApproval.copy(
            pendingItems = withSecondApproval.pendingItems + question,
        )
        assertEquals(
            listOf(AnnouncementMilestone.AwaitingQuestion),
            AnnouncementPolicy.milestones(withSecondApproval, withQuestion),
        )

        val completed = withQuestion.copy(phase = RunPhase.Completed)
        assertEquals(
            listOf(AnnouncementMilestone.RunCompleted),
            AnnouncementPolicy.milestones(withQuestion, completed),
        )
        val failed = withQuestion.copy(phase = RunPhase.Failed)
        assertEquals(
            listOf(AnnouncementMilestone.RunFailed),
            AnnouncementPolicy.milestones(withQuestion, failed),
        )
    }

    @Test
    @DisplayName("AC-13.accessibility control availability and milestone projection")
    fun ac13Accessibility() {
        val approval = ConversationFixtures.approvalItem()
        val anchor = ReadingAnchor(ConversationItemKey.Committed("m1"))
        val state = ConversationUiState(
            session = ConversationFixtures.session(),
            generation = 1,
            phase = RunPhase.Running,
            composer = ComposerUiState(text = "ready"),
            wait = WaitUiState.Waiting("wait-1"),
            pendingEntry = PendingEntryUiState.Current(listOf(approval)),
            history = HistoryUiState(anchor = anchor),
            announcement = AnnouncementMilestone.FirstVisibleOutput,
        )
        val accessibility = state.accessibility()
        assertTrue(accessibility.canSend)
        assertTrue(accessibility.canStop)
        assertTrue(accessibility.canCancelWait)
        assertTrue(accessibility.canReviewPendingInput)
        assertEquals(1, accessibility.pendingInputCount)
        assertEquals(anchor, accessibility.anchor)
        assertEquals(AnnouncementMilestone.FirstVisibleOutput, accessibility.milestone)

        val unavailable = state.copy(
            phase = RunPhase.Idle,
            composer = state.composer.copy(text = "", submitting = true),
            wait = WaitUiState.Idle,
            pendingEntry = PendingEntryUiState.Loading,
        ).accessibility()
        assertFalse(unavailable.canSend)
        assertFalse(unavailable.canStop)
        assertFalse(unavailable.canCancelWait)
        assertFalse(unavailable.canReviewPendingInput)
        // TalkBack traversal and keyboard focus order are device checks; this is
        // the state projection that drives enablement and announcement policy.
    }
}
