package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.IdGenerator
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.ScreenModel
import com.whalerein.android.state.core.SendId
import com.whalerein.android.state.core.SessionRef

data class SendRequest(
    val instanceId: InstanceId,
    val sendId: SendId,
    val session: SessionRef,
    val text: String,
)

enum class SendReceiptStatus { Confirmed, Unconfirmed }

data class ReplyRef(
    val session: SessionRef,
    val sendId: SendId,
)

data class SendReceipt(
    val sendId: SendId,
    val status: SendReceiptStatus,
    val replyRef: ReplyRef? = null,
)

/**
 * Minimal Core seam for the focused send reproduction. [readReply] is a content
 * read only; it can never submit the original intent again.
 */
interface SendRecoveryPort {
    suspend fun submit(request: SendRequest): Outcome<SendReceipt>

    suspend fun readReply(ref: ReplyRef): Outcome<String>
}

sealed interface ReplyReadState {
    data object NotRequested : ReplyReadState

    data object Reading : ReplyReadState

    data class Available(val text: String) : ReplyReadState

    data class Unavailable(val failure: CoreFailure) : ReplyReadState
}

enum class SendPhase {
    Idle,
    Submitting,
    Confirmed,
    Unconfirmed,
    Failed,
}

data class SendRecoveryUiState(
    val instanceId: InstanceId,
    val phase: SendPhase = SendPhase.Idle,
    val sendId: String? = null,
    val session: SessionRef? = null,
    val reply: ReplyReadState = ReplyReadState.NotRequested,
    val failure: CoreFailure? = null,
    /** The stage of the most recent failure; `reply_read` for an unread reply. */
    val failureStage: String? = null,
    val replyReadAttempts: Int = 0,
) {
    val confirmed: Boolean get() = phase == SendPhase.Confirmed

    val replyUnread: Boolean get() = reply is ReplyReadState.Unavailable
}

/**
 * Deterministic screen used by AC-24. A successful submission is recorded once;
 * a later content-read failure is a distinct `reply_read` failure and the retry
 * passes only the original reply reference.
 */
class SendRecoveryViewModel(
    instanceId: InstanceId,
    private val port: SendRecoveryPort,
    private val diagnostics: DiagnosticSink,
    private val idGenerator: IdGenerator,
) : ScreenModel<SendRecoveryUiState>(SendRecoveryUiState(instanceId = instanceId)) {
    private var replyRef: ReplyRef? = null

    suspend fun send(session: SessionRef, text: String) {
        if (state.value.phase == SendPhase.Submitting) return
        val sendId = SendId(idGenerator.newId())
        val request = SendRequest(
            instanceId = state.value.instanceId,
            sendId = sendId,
            session = session,
            text = text,
        )
        setState(
            state.value.copy(
                phase = SendPhase.Submitting,
                sendId = sendId.value,
                session = session,
                reply = ReplyReadState.NotRequested,
                failure = null,
                failureStage = null,
                replyReadAttempts = 0,
            ),
        )
        replyRef = null
        when (val outcome = port.submit(request)) {
            is Outcome.Success -> {
                val receipt = outcome.value
                if (receipt.status == SendReceiptStatus.Confirmed) {
                    setState(
                        state.value.copy(
                            phase = SendPhase.Confirmed,
                            sendId = receipt.sendId.value,
                        ),
                    )
                    emit(
                        DiagnosticLevel.Info,
                        DiagnosticEvents.COMMAND_RECEIPT_CONFIRMED,
                        DiagnosticStages.COMMAND,
                        fields = mapOf("operation_id" to receipt.sendId.value),
                    )
                    val ref = receipt.replyRef
                    if (ref == null) {
                        setState(
                            state.value.copy(
                                reply = ReplyReadState.Unavailable(
                                    CoreFailure(
                                        code = "reply_reference_missing",
                                        message = "No reply reference was returned.",
                                    ),
                                ),
                                failureStage = DiagnosticStages.REPLY_READ,
                            ),
                        )
                    } else {
                        replyRef = ref
                        readReply(ref, treatAsRetry = false)
                    }
                } else {
                    setState(
                        state.value.copy(
                            phase = SendPhase.Unconfirmed,
                            failure = CoreFailure(
                                code = "receipt_unconfirmed",
                                message = "The submission receipt is unconfirmed.",
                                retryable = true,
                            ),
                            failureStage = DiagnosticStages.COMMAND,
                        ),
                    )
                    emit(
                        DiagnosticLevel.Warn,
                        DiagnosticEvents.COMMAND_UNCONFIRMED,
                        DiagnosticStages.COMMAND,
                    )
                }
            }
            is Outcome.Failure -> {
                setState(
                    state.value.copy(
                        phase = SendPhase.Failed,
                        failure = outcome.failure,
                        failureStage = DiagnosticStages.COMMAND,
                    ),
                )
                emit(
                    DiagnosticLevel.Error,
                    DiagnosticEvents.COMMAND_FAILED,
                    DiagnosticStages.COMMAND,
                    code = outcome.failure.code,
                )
            }
            is Outcome.Unconfirmed -> {
                setState(
                    state.value.copy(
                        phase = SendPhase.Unconfirmed,
                        failure = outcome.failure,
                        failureStage = DiagnosticStages.COMMAND,
                    ),
                )
            }
            is Outcome.Cancelled -> setState(state.value.copy(phase = SendPhase.Idle))
        }
    }

    /** Reads content for the original reference only. Never resubmits. */
    suspend fun retryRead() {
        val ref = replyRef ?: return
        readReply(ref, treatAsRetry = true)
    }

    private suspend fun readReply(ref: ReplyRef, treatAsRetry: Boolean) {
        setState(
            state.value.copy(
                reply = ReplyReadState.Reading,
                replyReadAttempts = state.value.replyReadAttempts + 1,
            ),
        )
        when (val outcome = port.readReply(ref)) {
            is Outcome.Success -> {
                setState(
                    state.value.copy(
                        reply = ReplyReadState.Available(outcome.value),
                        failure = null,
                        failureStage = null,
                    ),
                )
            }
            is Outcome.Failure -> {
                setState(
                    state.value.copy(
                        reply = ReplyReadState.Unavailable(outcome.failure),
                        failure = outcome.failure,
                        failureStage = DiagnosticStages.REPLY_READ,
                    ),
                )
                emit(
                    DiagnosticLevel.Warn,
                    DiagnosticEvents.REPLY_READ_FAILED,
                    DiagnosticStages.REPLY_READ,
                    code = outcome.failure.code,
                    fields = mapOf(
                        "operation_id" to ref.sendId.value,
                        "attempt" to if (treatAsRetry) "retry" else "initial",
                    ),
                )
            }
            is Outcome.Unconfirmed -> {
                setState(
                    state.value.copy(
                        reply = ReplyReadState.Unavailable(outcome.failure),
                        failure = outcome.failure,
                        failureStage = DiagnosticStages.REPLY_READ,
                    ),
                )
            }
            is Outcome.Cancelled -> setState(state.value.copy(reply = ReplyReadState.NotRequested))
        }
    }

    private fun emit(
        level: DiagnosticLevel,
        event: String,
        stage: String,
        code: String? = null,
        fields: Map<String, String> = emptyMap(),
    ) {
        DiagnosticDelivery.deliver(
            diagnostics,
            DiagnosticEvent(
                level = level,
                event = event,
                stage = stage,
                code = code,
                instanceId = state.value.instanceId.value,
                fields = fields,
            ),
        )
    }
}
