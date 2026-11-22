package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.IdGenerator
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SendId
import com.whalerein.android.state.fixtures.support.FakeSendRecoveryPort
import com.whalerein.android.state.fixtures.support.confirmedSendReceipt
import com.whalerein.android.state.fixtures.support.testSessionRef
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

class SendRecoveryViewModelTest {
    private val instance = InstanceId("instance-a")
    private val session = testSessionRef()
    private val receiver = DiagnosticReceiver()

    private fun core(): FakeSendRecoveryPort {
        val ref = ReplyRef(session, SendId("send-original"))
        val core = FakeSendRecoveryPort(
            submitOutcome = Outcome.Success(confirmedSendReceipt(ref)),
        )
        core.replyOutcomes.add(
            Outcome.Failure(CoreFailure("reply_read_failed", "Reply content is unreadable.")),
        )
        return core
    }

    private fun model(core: FakeSendRecoveryPort) = SendRecoveryViewModel(
        instanceId = instance,
        port = core,
        diagnostics = receiver,
        idGenerator = IdGenerator { "send-original" },
    )

    @Test
    @DisplayName("AC-24.inspection saved result exposes the captured stage and one original send")
    fun AC_24_inspection_saved_result_exposes_the_captured_stage_and_one_original_send() = runTest {
        val core = core()
        val viewModel = model(core)

        viewModel.send(session, "synthetic request")

        val state = viewModel.state.value
        assertEquals(SendPhase.Confirmed, state.phase)
        assertEquals("send-original", state.sendId)
        assertEquals(DiagnosticStages.REPLY_READ, state.failureStage)
        assertEquals(1, core.submitCount)
        assertEquals(1, core.submitRequests.size)
        assertEquals(SendId("send-original"), core.submitRequests.single().sendId)
        assertEquals(session, core.submitRequests.single().session)
        assertNotNull(receiver.snapshot().firstOrNull())
    }

    @Test
    @DisplayName("AC-24.retry passes only the original reply reference for a content read")
    fun AC_24_retry_passes_only_the_original_reply_reference_for_a_content_read() = runTest {
        val core = core()
        val viewModel = model(core)
        viewModel.send(session, "synthetic request")
        val originalReference = core.replyRequests.single()

        core.replyOutcomes.add(
            Outcome.Failure(CoreFailure("reply_read_failed", "Still unreadable.")),
        )
        viewModel.retryRead()
        core.replyOutcomes.add(Outcome.Success("now readable"))
        viewModel.retryRead()

        assertEquals(3, core.replyReadCount)
        assertEquals(1, core.submitCount)
        assertEquals(3, core.replyRequests.size)
        assertTrue(core.replyRequests.all { it == originalReference })
        assertEquals(ReplyReadState.Available("now readable"), viewModel.state.value.reply)
    }
}
