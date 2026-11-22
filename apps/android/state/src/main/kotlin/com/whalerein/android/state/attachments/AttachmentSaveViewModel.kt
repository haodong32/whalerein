package com.whalerein.android.state.attachments

import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.ScreenModel
import com.whalerein.android.state.support.DiagnosticDelivery
import com.whalerein.android.state.support.DiagnosticEvents
import com.whalerein.android.state.support.DiagnosticStages

/**
 * Attachment transfer and save contract.
 *
 * The verified staging copy is the only retry source. A save retry is allowed
 * only in the current process while staging and the original authorization are
 * still valid; otherwise the staging copy is discarded and the screen reports an
 * explicit unavailable/refresh state. Transfer and destination outcomes are
 * kept separate, and an inconclusive provider completion is [AttachmentPhase.Unconfirmed].
 */
class AttachmentSaveViewModel(
    private val attachmentScope: AttachmentScope,
    private val operationId: String,
    private val port: AttachmentPort,
    private val staging: AttachmentStaging,
    private val authorization: AttachmentAuthorizationPort,
    private val writer: DestinationWriter,
    private val clock: Clock,
    private val diagnostics: DiagnosticSink,
    private val limits: AttachmentLimits = AttachmentLimits(),
    private val processId: String = DEFAULT_PROCESS_ID,
) : ScreenModel<AttachmentSaveUiState>(AttachmentSaveUiState(scope = attachmentScope)) {
    private var handle: StagingHandle? = null
    private var transferGeneration = 0L
    private var saveGeneration = 0L

    suspend fun load() {
        when (val outcome = port.describe(attachmentScope)) {
            is Outcome.Success -> {
                val descriptor = outcome.value
                if (!descriptor.available) {
                    setState(
                        state.value.copy(
                            descriptor = descriptor,
                            phase = AttachmentPhase.Unavailable,
                            notice = AttachmentNotice.Unavailable(descriptor.unavailableReason),
                        ),
                    )
                } else {
                    setState(
                        state.value.copy(
                            descriptor = descriptor,
                            phase = AttachmentPhase.Idle,
                            notice = null,
                        ),
                    )
                }
            }
            is Outcome.Failure -> setState(
                state.value.copy(
                    phase = AttachmentPhase.Unavailable,
                    notice = AttachmentNotice.Unavailable(outcome.failure.code),
                ),
            )
            is Outcome.Unconfirmed -> setState(
                state.value.copy(
                    phase = AttachmentPhase.Unavailable,
                    notice = AttachmentNotice.Unavailable(outcome.failure.code),
                ),
            )
            is Outcome.Cancelled -> Unit
        }
    }

    /** Returns false (and leaves prior state) when the override is not positive. */
    fun setDeadlineOverrideMillis(value: Long?): Boolean {
        if (value != null && (value <= 0L || value > limits.maxOverrideMillis)) {
            setState(state.value.copy(notice = AttachmentNotice.InvalidDeadline))
            return false
        }
        setState(
            state.value.copy(
                deadlineOverrideMillis = value,
                notice = null,
            ),
        )
        return true
    }

    fun reportProgress(fraction: Float) {
        setState(state.value.copy(progress = fraction.coerceIn(0f, 1f)))
    }

    fun clearNotice() {
        setState(state.value.copy(notice = null))
    }

    suspend fun startTransfer() {
        if (state.value.phase == AttachmentPhase.Transferring ||
            state.value.phase == AttachmentPhase.Saving ||
            state.value.phase == AttachmentPhase.Staged ||
            state.value.phase == AttachmentPhase.Saved
        ) {
            return
        }
        val effectiveDeadline = limits.effectiveDeadlineMillis(state.value.deadlineOverrideMillis)
        if (effectiveDeadline == null) {
            setState(state.value.copy(notice = AttachmentNotice.InvalidDeadline))
            return
        }
        if (!authorizationValid()) {
            markAuthorizationLost()
            return
        }

        discardHandle()
        val generation = ++transferGeneration
        val deadlineAt = clock.epochMillis() + effectiveDeadline
        setState(
            state.value.copy(
                phase = AttachmentPhase.Transferring,
                transfer = TransferOutcome.Running,
                save = SaveOutcome.NotStarted,
                progress = 0f,
                deadlineAtEpochMillis = deadlineAt,
                stagingAvailable = false,
                savedName = null,
                notice = null,
            ),
        )
        emit(DiagnosticLevel.Info, DiagnosticEvents.TRANSFER_STARTED, DiagnosticStages.TRANSFER)

        val request = AttachmentFetchRequest(scope = attachmentScope, deadlineAtEpochMillis = deadlineAt)
        when (val outcome = port.fetch(request)) {
            is Outcome.Success -> {
                if (generation != transferGeneration) return
                val verified = outcome.value
                if (clock.epochMillis() > deadlineAt) {
                    val failure = deadlineFailure()
                    failTransfer(failure, AttachmentNotice.TransferDeadlineExceeded(failure))
                    return
                }
                when (
                    val staged = staging.stage(
                        scope = attachmentScope,
                        operationId = operationId,
                        descriptor = verified.descriptor,
                        bytes = verified.bytes,
                    )
                ) {
                    is Outcome.Success -> {
                        if (generation != transferGeneration) {
                            staging.discard(staged.value)
                            return
                        }
                        if (clock.epochMillis() > deadlineAt) {
                            staging.discard(staged.value)
                            val failure = deadlineFailure()
                            failTransfer(failure, AttachmentNotice.TransferDeadlineExceeded(failure))
                            return
                        }
                        handle = staged.value
                        setState(
                            state.value.copy(
                                phase = AttachmentPhase.Staged,
                                descriptor = verified.descriptor,
                                transfer = TransferOutcome.Succeeded(verified.bytes.size.toLong()),
                                progress = 1f,
                                stagingAvailable = true,
                                save = SaveOutcome.NotStarted,
                            ),
                        )
                        emit(
                            DiagnosticLevel.Info,
                            DiagnosticEvents.TRANSFER_COMPLETED,
                            DiagnosticStages.TRANSFER,
                            fields = mapOf("bytes" to verified.bytes.size.toString()),
                        )
                    }
                    is Outcome.Failure -> failTransfer(
                        staged.failure,
                        AttachmentNotice.StagingFailed(staged.failure),
                    )
                    is Outcome.Unconfirmed -> failTransfer(
                        staged.failure,
                        AttachmentNotice.StagingFailed(staged.failure),
                    )
                    is Outcome.Cancelled -> {
                        setState(
                            state.value.copy(
                                phase = AttachmentPhase.Cancelled,
                                transfer = TransferOutcome.Cancelled,
                            ),
                        )
                    }
                }
            }
            is Outcome.Failure -> {
                if (generation != transferGeneration) return
                val notice = if (outcome.failure.code == CODE_INTEGRITY) {
                    AttachmentNotice.IntegrityFailure(outcome.failure)
                } else {
                    null
                }
                failTransfer(outcome.failure, notice)
            }
            is Outcome.Unconfirmed -> {
                if (generation != transferGeneration) return
                failTransfer(outcome.failure, null)
            }
            is Outcome.Cancelled -> {
                if (generation != transferGeneration) return
                setState(
                    state.value.copy(
                        phase = AttachmentPhase.Cancelled,
                        transfer = TransferOutcome.Cancelled,
                    ),
                )
            }
        }
    }

    suspend fun beginSave() {
        if (state.value.phase != AttachmentPhase.Staged &&
            state.value.phase != AttachmentPhase.Failed &&
            state.value.phase != AttachmentPhase.Unconfirmed &&
            state.value.phase != AttachmentPhase.Cancelled
        ) {
            return
        }
        val currentHandle = handle
        if (!state.value.stagingAvailable || currentHandle == null) {
            setState(
                state.value.copy(
                    phase = AttachmentPhase.Unavailable,
                    notice = AttachmentNotice.StagingDiscarded,
                ),
            )
            return
        }
        if (currentHandle.instanceId != attachmentScope.instanceId ||
            currentHandle.processId != processId
        ) {
            discardHandle()
            setState(
                state.value.copy(
                    phase = AttachmentPhase.Unavailable,
                    stagingAvailable = false,
                    notice = AttachmentNotice.StagingDiscarded,
                ),
            )
            return
        }
        if (!authorizationValid()) {
            markAuthorizationLost()
            return
        }
        val generation = ++saveGeneration
        when (val staged = staging.read(currentHandle)) {
            is Outcome.Success -> saveBytes(staged.value, generation)
            is Outcome.Failure -> {
                handle = null
                setState(
                    state.value.copy(
                        phase = AttachmentPhase.Failed,
                        stagingAvailable = false,
                        save = SaveOutcome.Failed(staged.failure),
                    ),
                )
            }
            is Outcome.Unconfirmed -> {
                handle = null
                setState(
                    state.value.copy(
                        phase = AttachmentPhase.Failed,
                        stagingAvailable = false,
                        save = SaveOutcome.Failed(staged.failure),
                    ),
                )
            }
            is Outcome.Cancelled -> Unit
        }
    }

    private suspend fun saveBytes(bytes: ByteArray, generation: Long) {
        val descriptor = state.value.descriptor ?: return
        val requestedName = DestinationNaming.safeFileName(descriptor.displayName)
        setState(
            state.value.copy(
                phase = AttachmentPhase.Saving,
                save = SaveOutcome.Running,
                notice = null,
            ),
        )
        emit(DiagnosticLevel.Info, DiagnosticEvents.SAVE_STARTED, DiagnosticStages.SAVE)
        when (val outcome = writer.write(requestedName, bytes)) {
            is Outcome.Success -> {
                if (generation != saveGeneration) return
                when (val result = outcome.value) {
                    is DestinationWriteResult.Completed -> {
                        discardHandle()
                        setState(
                            state.value.copy(
                                phase = AttachmentPhase.Saved,
                                save = SaveOutcome.Succeeded(result.fileName),
                                savedName = result.fileName,
                                stagingAvailable = false,
                            ),
                        )
                        emit(
                            DiagnosticLevel.Info,
                            DiagnosticEvents.SAVE_COMPLETED,
                            DiagnosticStages.SAVE,
                        )
                    }
                    is DestinationWriteResult.Unconfirmed -> {
                        setState(
                            state.value.copy(
                                phase = AttachmentPhase.Unconfirmed,
                                save = SaveOutcome.Unconfirmed(result.failure),
                                notice = AttachmentNotice.SaveUnconfirmed(result.failure),
                            ),
                        )
                        emit(
                            DiagnosticLevel.Warn,
                            DiagnosticEvents.SAVE_UNCONFIRMED,
                            DiagnosticStages.SAVE,
                            code = result.failure.code,
                        )
                    }
                }
            }
            is Outcome.Failure -> {
                if (generation != saveGeneration) return
                setState(
                    state.value.copy(
                        phase = AttachmentPhase.Failed,
                        save = SaveOutcome.Failed(outcome.failure),
                        notice = AttachmentNotice.ProviderFailure(outcome.failure),
                    ),
                )
                emit(
                    DiagnosticLevel.Error,
                    DiagnosticEvents.SAVE_FAILED,
                    DiagnosticStages.SAVE,
                    code = outcome.failure.code,
                )
            }
            is Outcome.Unconfirmed -> {
                if (generation != saveGeneration) return
                setState(
                    state.value.copy(
                        phase = AttachmentPhase.Unconfirmed,
                        save = SaveOutcome.Unconfirmed(outcome.failure),
                        notice = AttachmentNotice.SaveUnconfirmed(outcome.failure),
                    ),
                )
            }
            is Outcome.Cancelled -> {
                if (generation != saveGeneration) return
                setState(
                    state.value.copy(
                        phase = AttachmentPhase.Cancelled,
                        save = SaveOutcome.Cancelled,
                    ),
                )
            }
        }
    }

    /** Separate cancellation while the destination is being written. */
    fun cancelSave() {
        if (state.value.phase != AttachmentPhase.Saving) return
        saveGeneration += 1
        setState(
            state.value.copy(
                phase = AttachmentPhase.Cancelled,
                save = SaveOutcome.Cancelled,
            ),
        )
    }

    fun cancelTransfer() {
        if (state.value.phase != AttachmentPhase.Transferring) return
        transferGeneration += 1
        setState(
            state.value.copy(
                phase = AttachmentPhase.Cancelled,
                transfer = TransferOutcome.Cancelled,
                progress = null,
            ),
        )
    }

    suspend fun onAuthorizationLost() {
        markAuthorizationLost()
    }

    suspend fun onInstanceChanged(newInstance: InstanceId) {
        if (newInstance == attachmentScope.instanceId) return
        staging.discardInstance(attachmentScope.instanceId)
        handle = null
        setState(
            state.value.copy(
                phase = AttachmentPhase.Discarded,
                stagingAvailable = false,
                savedName = null,
                notice = AttachmentNotice.InstanceChanged,
            ),
        )
    }

    /** Startup cleanup: discard everything staged by an earlier process. */
    suspend fun prepareAfterRestart() {
        val current = handle
        if (current != null && current.processId != processId) {
            staging.discard(current)
            handle = null
            setState(
                state.value.copy(
                    phase = AttachmentPhase.Discarded,
                    stagingAvailable = false,
                    notice = AttachmentNotice.StagingDiscarded,
                ),
            )
        }
        staging.discardProcessesBefore(processId)
    }

    suspend fun discardStaging() {
        discardHandle()
        setState(
            state.value.copy(
                stagingAvailable = false,
                notice = AttachmentNotice.StagingDiscarded,
            ),
        )
    }

    private suspend fun authorizationValid(): Boolean =
        when (val outcome = authorization.current(attachmentScope)) {
            is Outcome.Success -> outcome.value.authorized
            else -> false
        }

    private suspend fun markAuthorizationLost() {
        discardHandle()
        setState(
            state.value.copy(
                phase = AttachmentPhase.Unavailable,
                stagingAvailable = false,
                notice = AttachmentNotice.AuthorizationLost,
            ),
        )
    }

    private suspend fun discardHandle() {
        val current = handle ?: return
        staging.discard(current)
        handle = null
    }

    private fun failTransfer(failure: CoreFailure, notice: AttachmentNotice?) {
        setState(
            state.value.copy(
                phase = AttachmentPhase.Failed,
                transfer = TransferOutcome.Failed(failure),
                progress = null,
                notice = notice,
            ),
        )
        emit(
            DiagnosticLevel.Error,
            DiagnosticEvents.TRANSFER_FAILED,
            DiagnosticStages.TRANSFER,
            code = failure.code,
        )
    }

    private fun deadlineFailure(): CoreFailure = CoreFailure(
        code = CODE_DEADLINE,
        message = "The attachment transfer deadline was exceeded.",
        retryable = true,
    )

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
                instanceId = attachmentScope.instanceId.value,
                actionId = operationId,
                fields = fields,
            ),
        )
    }

    companion object {
        const val DEFAULT_PROCESS_ID: String = "current-process"
        const val CODE_INTEGRITY: String = "attachment_integrity_failed"
        const val CODE_DEADLINE: String = "attachment_transfer_deadline_exceeded"
    }
}
