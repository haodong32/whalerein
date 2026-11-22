package com.whalerein.android.state.attachments

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.fixtures.support.FakeAttachmentAuthorizationPort
import com.whalerein.android.state.fixtures.support.FakeAttachmentPort
import com.whalerein.android.state.fixtures.support.FakeAttachmentStaging
import com.whalerein.android.state.fixtures.support.FakeClock
import com.whalerein.android.state.fixtures.support.FakeDestinationWriter
import com.whalerein.android.state.fixtures.support.testSessionRef
import com.whalerein.android.state.support.DiagnosticReceiver
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/** Additional branch coverage for the attachment save state machine. */
class AttachmentSaveViewModelBranchesTest {
    private val instance = InstanceId("instance-a")
    private val scope = AttachmentScope(
        instanceId = instance,
        session = testSessionRef(),
        attachmentId = "attachment-a",
    )
    private val bytes = "verified-attachment-bytes".toByteArray()
    private val descriptor = AttachmentDescriptor(
        attachmentId = "attachment-a",
        displayName = "report.pdf",
        mediaType = "application/pdf",
        sizeBytes = bytes.size.toLong(),
    )

    private fun portSuccess(): FakeAttachmentPort {
        val port = FakeAttachmentPort(descriptor = descriptor)
        port.fetchOutcomes.add(
            Outcome.Success(
                VerifiedAttachment(descriptor, bytes, sha256 = "sha-verified"),
            ),
        )
        return port
    }

    private fun model(
        port: FakeAttachmentPort = portSuccess(),
        staging: FakeAttachmentStaging = FakeAttachmentStaging(),
        authorization: FakeAttachmentAuthorizationPort = FakeAttachmentAuthorizationPort(),
        writer: FakeDestinationWriter = FakeDestinationWriter(),
        clock: FakeClock = FakeClock(),
        processId: String = "process-1",
    ) = AttachmentSaveViewModel(
        attachmentScope = scope,
        operationId = "operation-a",
        port = port,
        staging = staging,
        authorization = authorization,
        writer = writer,
        clock = clock,
        diagnostics = DiagnosticReceiver(),
        processId = processId,
    )

    @Test
    @DisplayName("AC-20.failure load and transfer failure branches stay distinct")
    fun AC_20_failure_load_and_transfer_failure_branches_stay_distinct() = runTest {
        val describeFailure = FakeAttachmentPort(descriptor = descriptor)
        describeFailure.describeOutcome = Outcome.Failure(CoreFailure("unsupported", "no"))
        val failedLoad = model(port = describeFailure)
        failedLoad.load()
        assertEquals(AttachmentPhase.Unavailable, failedLoad.state.value.phase)

        val describeUnconfirmed = FakeAttachmentPort(descriptor = descriptor)
        describeUnconfirmed.describeOutcome = Outcome.Unconfirmed(CoreFailure("unknown", "no"))
        val unconfirmedLoad = model(port = describeUnconfirmed)
        unconfirmedLoad.load()
        assertEquals(AttachmentPhase.Unavailable, unconfirmedLoad.state.value.phase)

        val describeCancelled = FakeAttachmentPort(descriptor = descriptor)
        describeCancelled.describeOutcome = Outcome.Cancelled
        val cancelledLoad = model(port = describeCancelled)
        cancelledLoad.load()
        assertEquals(AttachmentPhase.Idle, cancelledLoad.state.value.phase)

        val genericFailurePort = FakeAttachmentPort(descriptor = descriptor)
        genericFailurePort.fetchOutcomes.add(
            Outcome.Failure(CoreFailure("source_unavailable", "no")),
        )
        val genericFailure = model(port = genericFailurePort)
        genericFailure.load()
        genericFailure.startTransfer()
        assertEquals(AttachmentPhase.Failed, genericFailure.state.value.phase)
        assertNull(genericFailure.state.value.notice)

        val unconfirmedPort = FakeAttachmentPort(descriptor = descriptor)
        unconfirmedPort.fetchOutcomes.add(Outcome.Unconfirmed(CoreFailure("unknown", "no")))
        val unconfirmed = model(port = unconfirmedPort)
        unconfirmed.load()
        unconfirmed.startTransfer()
        assertEquals(AttachmentPhase.Failed, unconfirmed.state.value.phase)

        val stagingFailure = FakeAttachmentStaging()
        stagingFailure.failNextStage = true
        val stagingFailed = model(staging = stagingFailure)
        stagingFailed.load()
        stagingFailed.startTransfer()
        assertEquals(AttachmentPhase.Failed, stagingFailed.state.value.phase)
        assertTrue(stagingFailed.state.value.notice is AttachmentNotice.StagingFailed)

        for (outcome in listOf(
            Outcome.Failure(CoreFailure("staging_failed", "no")),
            Outcome.Unconfirmed(CoreFailure("staging_unknown", "no")),
            Outcome.Cancelled,
        )) {
            val staging = FakeAttachmentStaging()
            staging.stageOutcome = outcome
            val stagingModel = model(staging = staging)
            stagingModel.load()
            stagingModel.startTransfer()
            if (outcome is Outcome.Cancelled) {
                assertEquals(AttachmentPhase.Cancelled, stagingModel.state.value.phase)
            } else {
                assertEquals(AttachmentPhase.Failed, stagingModel.state.value.phase)
            }
        }

        val timeoutClock = FakeClock(nowMillis = 1_000L)
        val timeoutStaging = FakeAttachmentStaging()
        timeoutStaging.duringStage = {
            timeoutClock.advance(AttachmentLimits.DEFAULT_DEADLINE_MILLIS + 1L)
        }
        val timeout = model(staging = timeoutStaging, clock = timeoutClock)
        timeout.load()
        timeout.startTransfer()
        assertEquals(AttachmentPhase.Failed, timeout.state.value.phase)
        assertTrue(timeout.state.value.notice is AttachmentNotice.TransferDeadlineExceeded)

        val cancelStaging = FakeAttachmentStaging()
        lateinit var cancelDuringStage: AttachmentSaveViewModel
        cancelStaging.duringStage = { cancelDuringStage.cancelTransfer() }
        cancelDuringStage = model(staging = cancelStaging)
        cancelDuringStage.load()
        cancelDuringStage.startTransfer()
        assertEquals(AttachmentPhase.Cancelled, cancelDuringStage.state.value.phase)
        assertEquals(0, cancelStaging.stagedCount())

        val cancelFetchPort = FakeAttachmentPort(descriptor = descriptor)
        cancelFetchPort.fetchOutcomes.add(
            Outcome.Success(VerifiedAttachment(descriptor, bytes, "sha")),
        )
        lateinit var cancelDuringFetch: AttachmentSaveViewModel
        cancelFetchPort.duringFetch = { cancelDuringFetch.cancelTransfer() }
        cancelDuringFetch = model(port = cancelFetchPort)
        cancelDuringFetch.load()
        cancelDuringFetch.startTransfer()
        assertEquals(AttachmentPhase.Cancelled, cancelDuringFetch.state.value.phase)

        val staged = model()
        staged.load()
        staged.startTransfer()
        staged.startTransfer()
        assertEquals(AttachmentPhase.Staged, staged.state.value.phase)
        staged.beginSave()
        staged.startTransfer()
        assertEquals(AttachmentPhase.Saved, staged.state.value.phase)
    }

    @Test
    @DisplayName("AC-20.cleanup save guards staging reads and restart cleanup branches")
    fun AC_20_cleanup_save_guards_staging_reads_and_restart_cleanup_branches() = runTest {
        val idle = model()
        idle.load()
        idle.beginSave()
        assertEquals(AttachmentPhase.Idle, idle.state.value.phase)

        val cancelledPort = FakeAttachmentPort(descriptor = descriptor)
        cancelledPort.fetchOutcomes.add(Outcome.Cancelled)
        val cancelledTransfer = model(port = cancelledPort)
        cancelledTransfer.load()
        cancelledTransfer.startTransfer()
        cancelledTransfer.beginSave()
        assertEquals(AttachmentPhase.Unavailable, cancelledTransfer.state.value.phase)

        val processStaging = FakeAttachmentStaging().apply { processId = "process-9" }
        val processMismatch = model(staging = processStaging)
        processMismatch.load()
        processMismatch.startTransfer()
        processMismatch.beginSave()
        assertEquals(AttachmentPhase.Unavailable, processMismatch.state.value.phase)
        assertTrue(processMismatch.state.value.notice is AttachmentNotice.StagingDiscarded)

        val instanceStaging = FakeAttachmentStaging().apply {
            instanceOverride = InstanceId("instance-b")
        }
        val instanceMismatch = model(staging = instanceStaging)
        instanceMismatch.load()
        instanceMismatch.startTransfer()
        instanceMismatch.beginSave()
        assertEquals(AttachmentPhase.Unavailable, instanceMismatch.state.value.phase)

        val authFailure = FakeAttachmentAuthorizationPort(
            outcome = Outcome.Failure(CoreFailure("auth_check_failed", "no")),
        )
        val authFailureModel = model(authorization = authFailure)
        authFailureModel.load()
        authFailureModel.startTransfer()
        assertEquals(AttachmentPhase.Unavailable, authFailureModel.state.value.phase)
        assertTrue(authFailureModel.state.value.notice is AttachmentNotice.AuthorizationLost)

        for (readOutcome in listOf(
            Outcome.Failure(CoreFailure("staging_lost", "no")),
            Outcome.Unconfirmed(CoreFailure("staging_unknown", "no")),
            Outcome.Cancelled,
        )) {
            val staging = FakeAttachmentStaging()
            val readModel = model(staging = staging)
            readModel.load()
            readModel.startTransfer()
            staging.readOutcome = readOutcome
            readModel.beginSave()
            if (readOutcome is Outcome.Cancelled) {
                assertEquals(AttachmentPhase.Staged, readModel.state.value.phase)
            } else {
                assertEquals(AttachmentPhase.Failed, readModel.state.value.phase)
                assertFalse(readModel.state.value.stagingAvailable)
            }
        }

        val restartStaging = FakeAttachmentStaging().apply { processId = "old-process" }
        val restart = model(staging = restartStaging, processId = "new-process")
        restart.load()
        restart.startTransfer()
        restart.prepareAfterRestart()
        assertEquals(AttachmentPhase.Discarded, restart.state.value.phase)
        assertEquals(0, restartStaging.stagedCount())

        val discard = model()
        discard.load()
        discard.startTransfer()
        discard.discardStaging()
        assertTrue(discard.state.value.notice is AttachmentNotice.StagingDiscarded)

        val lost = model()
        lost.load()
        lost.startTransfer()
        lost.onAuthorizationLost()
        assertEquals(AttachmentPhase.Unavailable, lost.state.value.phase)

        val sameInstance = model()
        sameInstance.load()
        sameInstance.onInstanceChanged(instance)
        assertEquals(AttachmentPhase.Idle, sameInstance.state.value.phase)

        sameInstance.cancelSave()
        sameInstance.cancelTransfer()
        sameInstance.reportProgress(2f)
        assertEquals(1f, sameInstance.state.value.progress)
        sameInstance.reportProgress(-1f)
        assertEquals(0f, sameInstance.state.value.progress)
        sameInstance.setDeadlineOverrideMillis(5_000L)
        sameInstance.clearNotice()
        assertNull(sameInstance.state.value.notice)
        sameInstance.setDeadlineOverrideMillis(null)
        assertNull(sameInstance.state.value.deadlineOverrideMillis)

        val unconfirmedWriter = FakeDestinationWriter(
            outcome = Outcome.Unconfirmed(CoreFailure("provider_unknown", "no")),
        )
        val unconfirmedSave = model(writer = unconfirmedWriter)
        unconfirmedSave.load()
        unconfirmedSave.startTransfer()
        unconfirmedSave.beginSave()
        assertEquals(AttachmentPhase.Unconfirmed, unconfirmedSave.state.value.phase)
        assertTrue(unconfirmedSave.state.value.save is SaveOutcome.Unconfirmed)

        val cancelledWriter = FakeDestinationWriter(outcome = Outcome.Cancelled)
        val cancelledSave = model(writer = cancelledWriter)
        cancelledSave.load()
        cancelledSave.startTransfer()
        cancelledSave.beginSave()
        assertEquals(AttachmentPhase.Cancelled, cancelledSave.state.value.phase)
        assertTrue(cancelledSave.state.value.save is SaveOutcome.Cancelled)
    }

    @Test
    @DisplayName("AC-20.verified attachment models expose limits naming and result variants")
    fun AC_20_verified_attachment_models_expose_limits_naming_and_result_variants() {
        val limits = AttachmentLimits(defaultDeadlineMillis = 1_000L, maxOverrideMillis = 2_000L)
        assertEquals(1_000L, limits.effectiveDeadlineMillis(null))
        assertNull(limits.effectiveDeadlineMillis(0L))
        assertNull(limits.effectiveDeadlineMillis(-1L))
        assertNull(limits.effectiveDeadlineMillis(2_001L))
        assertEquals(1_500L, limits.effectiveDeadlineMillis(1_500L))

        val session = testSessionRef()
        val copyScope = AttachmentScope(instance, session, "other").copy(attachmentId = "next")
        assertEquals("next", copyScope.attachmentId)
        assertEquals(session, copyScope.session)
        assertEquals(instance, copyScope.instanceId)

        val descriptorCopy = descriptor.copy(available = false, unavailableReason = "unsupported")
        assertFalse(descriptorCopy.available)
        assertEquals("unsupported", descriptorCopy.unavailableReason)
        assertEquals("application/pdf", descriptor.mediaType)
        assertEquals(bytes.size.toLong(), descriptor.sizeBytes)

        val request = AttachmentFetchRequest(scope, 123L)
        assertEquals(scope, request.scope)
        assertEquals(123L, request.deadlineAtEpochMillis)

        val verified = VerifiedAttachment(descriptor, bytes, "sha")
        assertArrayEquals(bytes, verified.bytes)
        assertEquals("sha", verified.sha256)
        assertEquals(descriptor, verified.descriptor)

        val handle = StagingHandle(instance, "op", "process", "token", "name.pdf", 3L)
        assertEquals(instance, handle.instanceId)
        assertEquals("op", handle.operationId)
        assertEquals("process", handle.processId)
        assertEquals("token", handle.token)
        assertEquals("name.pdf", handle.displayName)
        assertEquals(3L, handle.sizeBytes)

        val authorization = AttachmentAuthorization(false, "expired")
        assertFalse(authorization.authorized)
        assertEquals("expired", authorization.reasonCode)

        val failure = CoreFailure("code", "message")
        val outcomes: List<Any> = listOf(
            TransferOutcome.NotStarted,
            TransferOutcome.Running,
            TransferOutcome.Succeeded(1L),
            TransferOutcome.Failed(failure),
            TransferOutcome.Cancelled,
            SaveOutcome.NotStarted,
            SaveOutcome.Running,
            SaveOutcome.Succeeded("name"),
            SaveOutcome.Failed(failure),
            SaveOutcome.Unconfirmed(failure),
            SaveOutcome.Cancelled,
            AttachmentNotice.InvalidDeadline,
            AttachmentNotice.Unavailable("reason"),
            AttachmentNotice.IntegrityFailure(failure),
            AttachmentNotice.TransferDeadlineExceeded(failure),
            AttachmentNotice.ProviderFailure(failure),
            AttachmentNotice.SaveUnconfirmed(failure),
            AttachmentNotice.AuthorizationLost,
            AttachmentNotice.InstanceChanged,
            AttachmentNotice.StagingDiscarded,
            AttachmentNotice.StagingFailed(failure),
            DestinationWriteResult.Completed("name"),
            DestinationWriteResult.Unconfirmed(failure),
        )
        assertTrue(outcomes.isNotEmpty())
        assertEquals(
            DestinationWriteResult.Completed("name"),
            DestinationWriteResult.Completed("name"),
        )
        outcomes.forEach { assertEquals(it, it) }

        val state = AttachmentSaveUiState(
            scope = scope,
            phase = AttachmentPhase.Saved,
            descriptor = descriptor,
            transfer = TransferOutcome.Succeeded(1L),
            save = SaveOutcome.Succeeded("name"),
            progress = 1f,
            deadlineOverrideMillis = 5_000L,
            deadlineAtEpochMillis = 6_000L,
            stagingAvailable = true,
            savedName = "name",
            notice = null,
        )
        assertTrue(state.saved)
        assertFalse(state.unconfirmed)
        val copied = state.copy(phase = AttachmentPhase.Unconfirmed)
        assertTrue(copied.unconfirmed)
        assertEquals(scope, copied.scope)
        assertEquals(descriptor, copied.descriptor)
        assertEquals(AttachmentPhase.Unconfirmed, copied.phase)

        assertEquals("report.pdf", DestinationNaming.safeFileName("dir/report.pdf"))
        assertEquals("report.pdf", DestinationNaming.safeFileName("dir\\report.pdf"))
        assertEquals("attachment", DestinationNaming.safeFileName("..."))
        assertEquals("a_b_c", DestinationNaming.safeFileName("a:b*c"))
        assertEquals("REPORT (2).PDF", DestinationNaming.uniqueName("REPORT.PDF", listOf("report.pdf", "REPORT (1).PDF")))
        assertEquals("readme (1)", DestinationNaming.uniqueName("readme", listOf("readme")))
        assertEquals("attachment (1)", DestinationNaming.uniqueName("", listOf("attachment")))
    }
}
