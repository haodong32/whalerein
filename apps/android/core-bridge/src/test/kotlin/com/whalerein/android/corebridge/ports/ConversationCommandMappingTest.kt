package com.whalerein.android.corebridge.ports

import com.whalerein.android.corebridge.SessionDetailView
import com.whalerein.android.corebridge.WaitOutcome
import com.whalerein.android.state.conversation.CONVERSATION_PROJECTION_ANDROID_V1
import com.whalerein.android.state.conversation.Guard
import com.whalerein.android.state.conversation.PendingItem
import com.whalerein.android.state.conversation.PendingSendRecord
import com.whalerein.android.state.conversation.PendingListResult
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.SessionRef
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/**
 * The receipt/identity half of AC-27. The adapters cannot be instantiated
 * without a native Core; these mappings are the exact decision points they use.
 */
class ConversationCommandMappingTest {
    @Test
    @DisplayName("AC-27.send-receipt a missing or ambiguous receipt preserves the operation")
    fun `send receipt classification`() {
        assertEquals(SendDisposition.Unconfirmed, sendDisposition(null))
        assertEquals(SendDisposition.Unconfirmed, sendDisposition("host_offline"))
        assertEquals(SendDisposition.Unconfirmed, sendDisposition("unknown"))
        assertEquals(SendDisposition.Accepted, sendDisposition("succeeded"))
        assertEquals(SendDisposition.Accepted, sendDisposition("host_accepted"))
        assertEquals(SendDisposition.Accepted, sendDisposition("received"))
        assertEquals(SendDisposition.Rejected, sendDisposition("failed"))
        assertEquals(SendDisposition.Rejected, sendDisposition("invalid_input"))
    }

    @Test
    @DisplayName("AC-27.wait-question a question-only input-required wait asks the user")
    fun `wait settlement keeps question input`() {
        assertEquals(
            WaitSettlement.Question,
            waitSettlement(
                WaitOutcome.INPUT_REQUIRED,
                pendingInput = listOf("question"),
                hasApproval = false,
                hasQuestion = true,
            ),
        )
        assertEquals(
            WaitSettlement.Approval,
            waitSettlement(
                WaitOutcome.INPUT_REQUIRED,
                pendingInput = listOf("approval", "question"),
                hasApproval = true,
                hasQuestion = true,
            ),
        )
        assertEquals(
            WaitSettlement.InputUnavailable,
            waitSettlement(
                WaitOutcome.INPUT_REQUIRED,
                pendingInput = listOf("question"),
                hasApproval = false,
                hasQuestion = false,
            ),
        )
        assertEquals(
            WaitSettlement.Question,
            waitSettlement(
                WaitOutcome.INPUT_REQUIRED,
                pendingInput = emptyList(),
                hasApproval = false,
                hasQuestion = true,
            ),
        )
        assertEquals(
            WaitSettlement.Quiescent,
            waitSettlement(WaitOutcome.IDLE, emptyList(), false, false),
        )
        assertEquals(
            WaitSettlement.TimedOut,
            waitSettlement(WaitOutcome.TIMEOUT, emptyList(), false, false),
        )
        assertEquals(
            WaitSettlement.Cancelled,
            waitSettlement(WaitOutcome.CANCELLED, emptyList(), false, false),
        )
        assertEquals(
            WaitSettlement.Failed,
            waitSettlement(WaitOutcome.FAILED, emptyList(), false, false),
        )
    }

    @Test
    @DisplayName("AC-27.wait-cancel cancel releases exactly the bound native attempt")
    fun `wait identity maps app id to native id`() {
        val identities = WaitIdentityRegistry()
        identities.bind("app-wait-1", "native-wait-1")
        identities.bind("app-wait-2", "native-wait-2")

        assertEquals("native-wait-1", identities.nativeFor("app-wait-1"))
        assertEquals("native-wait-1", identities.release("app-wait-1"))
        assertNull(identities.release("app-wait-1"), "a released wait is not cancellable twice")
        assertNull(identities.nativeFor("app-wait-1"))
        // Releasing one attempt never touches another.
        assertEquals("native-wait-2", identities.nativeFor("app-wait-2"))
        assertNull(identities.release("unknown-app-wait"))
    }

    @Test
    @DisplayName("AC-27.pending-approvals approvals survive a question-only Host failure")
    fun `pending list keeps approvals on question failure`() {
        val approval = PendingItem.Approval(
            com.whalerein.android.state.conversation.PendingApproval(
                approvalId = "approval-1",
                title = "Run",
                body = null,
                guard = Guard("1:${"a".repeat(64)}"),
            ),
        )
        val unsupported = pendingListAfterQuestionFailure(
            listOf(approval),
            CoreFailure("capability_missing", "no questions"),
        )
        assertEquals(PendingListResult.Current(listOf(approval)), unsupported)

        assertEquals(
            PendingListResult.Unsupported,
            pendingListAfterQuestionFailure(emptyList(), CoreFailure("capability_missing", "no")),
        )
        assertEquals(
            PendingListResult.Unsupported,
            pendingListAfterQuestionFailure(emptyList(), CoreFailure("not_found", "none")),
        )
        assertEquals(
            PendingListResult.Unavailable(CoreFailure("host_offline", "offline", retryable = true)),
            pendingListAfterQuestionFailure(
                emptyList(),
                CoreFailure("host_offline", "offline", retryable = true),
            ),
        )
        assertEquals(
            PendingListResult.Stale(CoreFailure("conflict", "changed")),
            pendingListAfterQuestionFailure(emptyList(), CoreFailure("conflict", "changed")),
        )
    }

    @Test
    @DisplayName("AC-27.answer-cancel cancellation carries the original Core command id")
    fun `answer command registry keeps the original command identity`() {
        val commands = AnswerCommandRegistry()
        commands.bind("app-answer-1", AnswerCommand("core-command-1", "batch-1"))
        commands.bind("app-answer-empty", AnswerCommand("", "batch-1"))

        assertEquals("core-command-1", commands.commandFor("app-answer-1"))
        assertNull(commands.commandFor("app-answer-empty"))
        assertNull(commands.commandFor("unknown-answer"))
        commands.clear("app-answer-1")
        assertNull(commands.commandFor("app-answer-1"))
    }

    @Test
    @DisplayName("AC-27.answer-guard a changed guard requires a fresh review")
    fun `reviewed guard must match the exact batch version and digest`() {
        val digest = "b".repeat(64)
        assertTrue(reviewedGuardMatches(Guard("7:$digest"), 7uL, digest))
        assertFalse(reviewedGuardMatches(Guard("8:$digest"), 7uL, digest))
        assertFalse(reviewedGuardMatches(Guard("7:${"c".repeat(64)}"), 7uL, digest))
        assertFalse(reviewedGuardMatches(Guard("invalid"), 7uL, digest))
        assertEquals("7:$digest", batchGuard(7uL, digest).value)
    }

    @Test
    @DisplayName("AC-27.queue-receipt an ambiguous queue receipt is not an applied change")
    fun `queue receipt classification`() {
        assertEquals(CommandDisposition.Applied, commandDisposition("succeeded"))
        assertEquals(CommandDisposition.Unconfirmed, commandDisposition("host_accepted"))
        assertEquals(CommandDisposition.Unconfirmed, commandDisposition("received"))
        assertEquals(CommandDisposition.Unconfirmed, commandDisposition("host_offline"))
        assertEquals(CommandDisposition.Unconfirmed, commandDisposition("unknown"))
        assertEquals(CommandDisposition.Rejected, commandDisposition("failed"))
        assertEquals(CommandDisposition.Rejected, commandDisposition("conflict"))
    }

    @Test
    @DisplayName("AC-27.send-restart the durable send record round-trips in scoped storage")
    fun `pending send codec`() {
        val record = PendingSendRecord(
            sendId = "send-9",
            text = "message content stays here",
            follow = "ReceiptOnly",
            submittedAtMillis = 42,
        )
        val encoded = encodePendingSend(record)
        assertEquals(record, decodePendingSend(encoded))
        assertEquals(
            "whalerein.pending-send.v1/session-1",
            pendingSendStorageKey("session-1"),
        )
        assertTrue(pendingSendStorageKey("a") != pendingSendStorageKey("b"))
    }

    @Test
    @DisplayName("AC-27.history-projection stale exact projections are rejected")
    fun `exact history projection fence`() {
        assertTrue(cacheExactProjectionEligible(CONVERSATION_PROJECTION_ANDROID_V1))
        assertFalse(cacheExactProjectionEligible("legacy-projection"))
        assertTrue(historyProjectionMatches("android-v1", "android-v1"))
        assertTrue(historyProjectionMatches("", "android-v1"))
        assertFalse(historyProjectionMatches("legacy-projection", "android-v1"))
    }

    @Test
    @DisplayName("AC-27.session-fork a committed session exposes a fork target")
    fun `latest completed turn mapping`() {
        assertEquals("cursor-9", latestCompletedTurn("cursor-9", blank = false))
        assertNull(latestCompletedTurn("cursor-9", blank = true))
        assertNull(latestCompletedTurn("", blank = false))

        val session = SessionRef(
            HostRef(InstanceId("instance-1"), HostId("host-1")),
            com.whalerein.android.state.core.SessionId("session-1"),
        )
        val committed = SessionDetailView(
            hostId = "host-1",
            sessionId = "session-1",
            title = "Session",
            cwd = "/workspace",
            createdAtUnixMs = 1,
            updatedAtUnixMs = 2,
            running = false,
            agentAvailable = true,
            blank = false,
            parentSessionId = "",
            origin = "app",
            historyCursor = "cursor-9",
            queuedPrompts = 0u,
            capabilities = null,
        )
        assertEquals("cursor-9", committed.toDetails(session).latestCompletedTurnId)
        assertEquals(
            null,
            committed.copy(blank = true).toDetails(session).latestCompletedTurnId,
        )
    }
}
