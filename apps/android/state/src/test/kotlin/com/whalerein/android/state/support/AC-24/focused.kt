package com.whalerein.android.state.support.`AC-24`

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.IdGenerator
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SendId
import com.whalerein.android.state.fixtures.support.FakeSendRecoveryPort
import com.whalerein.android.state.fixtures.support.confirmedSendReceipt
import com.whalerein.android.state.fixtures.support.testSessionRef
import com.whalerein.android.state.support.DiagnosticEvents
import com.whalerein.android.state.support.DiagnosticReceiver
import com.whalerein.android.state.support.DiagnosticStages
import com.whalerein.android.state.support.ReplyReadState
import com.whalerein.android.state.support.ReplyRef
import com.whalerein.android.state.support.SendPhase
import com.whalerein.android.state.support.SendRecoveryViewModel
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/**
 * Dedicated focused reproduction.
 *
 * The package segment and class name intentionally produce the JUnit identity
 * `...support.AC-24.focused`, so the repository's literal `--only AC-24.focused`
 * selector can match a JVM test name (JVM method names cannot contain `.`).
 */
class focused {
    @Test
    @DisplayName("AC-24.focused confirmed send then failed reply read retries content only")
    fun focusedConfirmedSendThenFailedReplyRead() = runTest(StandardTestDispatcher()) {
        val instance = InstanceId("instance-a")
        val session = testSessionRef()
        val receiver = DiagnosticReceiver()
        val ref = ReplyRef(session, SendId("send-original"))
        val core = FakeSendRecoveryPort(
            submitOutcome = Outcome.Success(confirmedSendReceipt(ref)),
        )
        core.replyOutcomes.add(
            Outcome.Failure(CoreFailure("reply_read_failed", "Reply content is unreadable.")),
        )
        val viewModel = SendRecoveryViewModel(
            instanceId = instance,
            port = core,
            diagnostics = receiver,
            idGenerator = IdGenerator { "send-original" },
        )

        viewModel.send(session, "synthetic request")

        assertEquals(SendPhase.Confirmed, viewModel.state.value.phase)
        assertEquals("send-original", viewModel.state.value.sendId)
        assertTrue(viewModel.state.value.reply is ReplyReadState.Unavailable)
        assertEquals(DiagnosticStages.REPLY_READ, viewModel.state.value.failureStage)
        assertEquals(1, core.submitCount)
        assertEquals(1, core.replyReadCount)
        val replyEvent = receiver.snapshot().last {
            it.event == DiagnosticEvents.REPLY_READ_FAILED
        }
        assertEquals(DiagnosticStages.REPLY_READ, replyEvent.stage)
        assertEquals("reply_read_failed", replyEvent.code)

        core.replyOutcomes.add(Outcome.Success("synthetic answer"))
        viewModel.retryRead()

        assertEquals(ReplyReadState.Available("synthetic answer"), viewModel.state.value.reply)
        assertEquals(1, core.submitCount)
        assertEquals(2, core.replyReadCount)
    }
}
