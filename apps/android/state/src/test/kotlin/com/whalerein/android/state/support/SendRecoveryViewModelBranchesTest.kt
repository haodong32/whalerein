package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.IdGenerator
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SendId
import com.whalerein.android.state.fixtures.support.FakeSendRecoveryPort
import com.whalerein.android.state.fixtures.support.confirmedSendReceipt
import com.whalerein.android.state.fixtures.support.testSessionRef
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/** Additional branch coverage for the focused send recovery screen. */
class SendRecoveryViewModelBranchesTest {
    private val instance = InstanceId("instance-a")
    private val session = testSessionRef()

    private fun model(core: FakeSendRecoveryPort) = SendRecoveryViewModel(
        instanceId = instance,
        port = core,
        diagnostics = DiagnosticReceiver(),
        idGenerator = IdGenerator { "send-1" },
    )

    @Test
    @DisplayName("AC-24.inspection submission outcomes stay distinct and keep one submit")
    fun AC_24_inspection_submission_outcomes_stay_distinct_and_keep_one_submit() = runTest {
        val unconfirmedReceipt = FakeSendRecoveryPort(
            submitOutcome = Outcome.Success(
                SendReceipt(SendId("send-1"), SendReceiptStatus.Unconfirmed),
            ),
        )
        val unconfirmed = model(unconfirmedReceipt)
        unconfirmed.send(session, "hello")
        assertEquals(SendPhase.Unconfirmed, unconfirmed.state.value.phase)
        assertEquals(1, unconfirmedReceipt.submitCount)

        val failureCore = FakeSendRecoveryPort(
            submitOutcome = Outcome.Failure(CoreFailure("send_rejected", "no")),
        )
        val failed = model(failureCore)
        failed.send(session, "hello")
        assertEquals(SendPhase.Failed, failed.state.value.phase)
        assertEquals("send_rejected", failed.state.value.failure?.code)
        assertEquals(DiagnosticStages.COMMAND, failed.state.value.failureStage)

        val unconfirmedCore = FakeSendRecoveryPort(
            submitOutcome = Outcome.Unconfirmed(CoreFailure("receipt_unknown", "no")),
        )
        val outcomeUnconfirmed = model(unconfirmedCore)
        outcomeUnconfirmed.send(session, "hello")
        assertEquals(SendPhase.Unconfirmed, outcomeUnconfirmed.state.value.phase)

        val cancelledCore = FakeSendRecoveryPort(submitOutcome = Outcome.Cancelled)
        val cancelled = model(cancelledCore)
        cancelled.send(session, "hello")
        assertEquals(SendPhase.Idle, cancelled.state.value.phase)
        cancelled.retryRead()
        assertEquals(0, cancelledCore.replyReadCount)

        val repeatedCore = FakeSendRecoveryPort(
            submitOutcome = Outcome.Success(confirmedSendReceipt(ReplyRef(session, SendId("send-1")))),
        )
        repeatedCore.replyOutcomes.add(Outcome.Success("reply"))
        lateinit var repeated: SendRecoveryViewModel
        repeatedCore.duringSubmit = { repeated.send(session, "duplicate") }
        repeated = model(repeatedCore)
        repeated.send(session, "hello")
        assertEquals(1, repeatedCore.submitCount)
        assertEquals(SendPhase.Confirmed, repeated.state.value.phase)
    }

    @Test
    @DisplayName("AC-24.retry missing reply references and reply read outcomes are explicit")
    fun AC_24_retry_missing_reply_references_and_reply_read_outcomes_are_explicit() = runTest {
        val missingReference = FakeSendRecoveryPort(
            submitOutcome = Outcome.Success(
                SendReceipt(SendId("send-1"), SendReceiptStatus.Confirmed),
            ),
        )
        val missing = model(missingReference)
        missing.send(session, "hello")
        assertEquals(SendPhase.Confirmed, missing.state.value.phase)
        assertTrue(missing.state.value.reply is ReplyReadState.Unavailable)
        assertEquals(0, missingReference.replyReadCount)
        missing.retryRead()
        assertEquals(0, missingReference.replyReadCount)

        val unconfirmedReply = FakeSendRecoveryPort(
            submitOutcome = Outcome.Success(
                confirmedSendReceipt(ReplyRef(session, SendId("send-1"))),
            ),
        )
        unconfirmedReply.replyOutcomes.add(Outcome.Unconfirmed(CoreFailure("read_unknown", "no")))
        val unconfirmed = model(unconfirmedReply)
        unconfirmed.send(session, "hello")
        assertTrue(unconfirmed.state.value.reply is ReplyReadState.Unavailable)

        val cancelledReply = FakeSendRecoveryPort(
            submitOutcome = Outcome.Success(
                confirmedSendReceipt(ReplyRef(session, SendId("send-1"))),
            ),
        )
        cancelledReply.replyOutcomes.add(Outcome.Cancelled)
        val cancelled = model(cancelledReply)
        cancelled.send(session, "hello")
        assertTrue(cancelled.state.value.reply is ReplyReadState.NotRequested)
        assertEquals(1, cancelledReply.submitCount)
    }

    @Test
    @DisplayName("AC-24.inspection send models expose request receipt and reply states")
    fun AC_24_inspection_send_models_expose_request_receipt_and_reply_states() {
        val request = SendRequest(instance, SendId("send-1"), session, "text")
        assertEquals(instance, request.instanceId)
        assertEquals(SendId("send-1"), request.sendId)
        assertEquals(session, request.session)
        assertEquals("text", request.text)

        val ref = ReplyRef(session, SendId("send-1"))
        assertEquals(session, ref.session)
        assertEquals(SendId("send-1"), ref.sendId)

        val receipt = SendReceipt(SendId("send-1"), SendReceiptStatus.Confirmed, ref)
        assertEquals(SendId("send-1"), receipt.sendId)
        assertEquals(SendReceiptStatus.Confirmed, receipt.status)
        assertEquals(ref, receipt.replyRef)
        assertEquals(SendReceiptStatus.Unconfirmed, receipt.copy(status = SendReceiptStatus.Unconfirmed).status)

        val replies: List<ReplyReadState> = listOf(
            ReplyReadState.NotRequested,
            ReplyReadState.Reading,
            ReplyReadState.Available("text"),
            ReplyReadState.Unavailable(CoreFailure("code", "message")),
        )
        replies.forEach { assertEquals(it, it) }
        assertEquals("text", ReplyReadState.Available("text").text)

        val state = SendRecoveryUiState(
            instanceId = instance,
            phase = SendPhase.Confirmed,
            sendId = "send-1",
            session = session,
            reply = ReplyReadState.Unavailable(CoreFailure("code", "message")),
            failure = CoreFailure("code", "message"),
            failureStage = DiagnosticStages.REPLY_READ,
            replyReadAttempts = 1,
        )
        assertTrue(state.confirmed)
        assertTrue(state.replyUnread)
        assertTrue(state.copy(phase = SendPhase.Failed).confirmed.not())
        assertNull(state.copy(sendId = null).sendId)
    }
}
