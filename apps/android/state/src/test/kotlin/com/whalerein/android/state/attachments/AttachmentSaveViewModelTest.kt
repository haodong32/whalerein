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

/**
 * JVM contract tests for attachment transfer and save.
 *
 * The Android document provider/picker and private file staging are represented
 * by injected fakes here. Their real device behavior (SAF document creation,
 * provider streams and process-death cleanup) is native scope covered by the
 * Android UI/integration suites; this class does not claim device evidence.
 */
class AttachmentSaveViewModelTest {
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

    private fun portWithSuccess(): FakeAttachmentPort {
        val port = FakeAttachmentPort(descriptor = descriptor)
        port.fetchOutcomes.add(
            Outcome.Success(
                VerifiedAttachment(
                    descriptor = descriptor,
                    bytes = bytes,
                    sha256 = "sha-verified",
                ),
            ),
        )
        return port
    }

    private fun model(
        port: FakeAttachmentPort = portWithSuccess(),
        staging: FakeAttachmentStaging = FakeAttachmentStaging(),
        authorization: FakeAttachmentAuthorizationPort = FakeAttachmentAuthorizationPort(),
        writer: FakeDestinationWriter = FakeDestinationWriter(),
        clock: FakeClock = FakeClock(),
        limits: AttachmentLimits = AttachmentLimits(),
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
        limits = limits,
        processId = processId,
    )

    @Test
    @DisplayName("AC-20.verified only verified bytes reach the destination and staging is released")
    fun AC_20_verified_only_verified_bytes_reach_the_destination_and_staging_is_released() = runTest {
        val writer = FakeDestinationWriter()
        val staging = FakeAttachmentStaging()
        val viewModel = model(staging = staging, writer = writer)

        viewModel.load()
        viewModel.startTransfer()

        assertEquals(AttachmentPhase.Staged, viewModel.state.value.phase)
        assertTrue(viewModel.state.value.stagingAvailable)
        assertEquals(TransferOutcome.Succeeded(bytes.size.toLong()), viewModel.state.value.transfer)

        viewModel.beginSave()

        assertEquals(AttachmentPhase.Saved, viewModel.state.value.phase)
        assertEquals("report.pdf", viewModel.state.value.savedName)
        assertArrayEquals(bytes, writer.documents["report.pdf"])
        assertEquals(0, staging.stagedCount())
    }

    @Test
    @DisplayName("AC-20.naming same-name copy preserves the original document")
    fun AC_20_naming_same_name_copy_preserves_the_original_document() = runTest {
        val original = "original-document".toByteArray()
        val writer = FakeDestinationWriter(initial = mapOf("report.pdf" to original))
        val viewModel = model(writer = writer)
        viewModel.load()
        viewModel.startTransfer()
        viewModel.beginSave()

        assertEquals("report (1).pdf", viewModel.state.value.savedName)
        assertArrayEquals(original, writer.documents["report.pdf"])
        assertArrayEquals(bytes, writer.documents["report (1).pdf"])
        assertEquals(setOf("report.pdf", "report (1).pdf"), writer.names())

        assertEquals(
            "report (2).pdf",
            DestinationNaming.uniqueName(
                "report.pdf",
                listOf("report.pdf", "report (1).pdf"),
            ),
        )
        assertEquals("report.pdf", DestinationNaming.safeFileName("../../private/report.pdf"))
    }

    @Test
    @DisplayName("AC-20.failure unsupported integrity cancellation and provider errors stay distinct")
    fun AC_20_failure_unsupported_integrity_cancellation_and_provider_errors_stay_distinct() = runTest {
        val unsupportedPort = FakeAttachmentPort(descriptor = descriptor)
        unsupportedPort.describeOutcome = Outcome.Success(
            descriptor.copy(available = false, unavailableReason = "unsupported_source"),
        )
        val unsupported = model(port = unsupportedPort)
        unsupported.load()
        assertEquals(AttachmentPhase.Unavailable, unsupported.state.value.phase)
        assertEquals(
            AttachmentNotice.Unavailable("unsupported_source"),
            unsupported.state.value.notice,
        )

        val integrityPort = FakeAttachmentPort(descriptor = descriptor)
        integrityPort.fetchOutcomes.add(
            Outcome.Failure(
                CoreFailure(AttachmentSaveViewModel.CODE_INTEGRITY, "Integrity mismatch."),
            ),
        )
        val integrity = model(port = integrityPort)
        integrity.load()
        integrity.startTransfer()
        assertEquals(AttachmentPhase.Failed, integrity.state.value.phase)
        assertTrue(integrity.state.value.notice is AttachmentNotice.IntegrityFailure)

        val cancelPort = FakeAttachmentPort(descriptor = descriptor)
        cancelPort.fetchOutcomes.add(Outcome.Cancelled)
        val cancelled = model(port = cancelPort)
        cancelled.load()
        cancelled.startTransfer()
        assertEquals(AttachmentPhase.Cancelled, cancelled.state.value.phase)
        assertEquals(TransferOutcome.Cancelled, cancelled.state.value.transfer)

        val providerFailure = FakeDestinationWriter(
            outcome = Outcome.Failure(CoreFailure("provider_rejected", "Provider rejected.")),
        )
        val staging = FakeAttachmentStaging()
        val failed = model(staging = staging, writer = providerFailure)
        failed.load()
        failed.startTransfer()
        failed.beginSave()
        assertEquals(AttachmentPhase.Failed, failed.state.value.phase)
        assertTrue(failed.state.value.notice is AttachmentNotice.ProviderFailure)
        assertTrue(failed.state.value.stagingAvailable)
    }

    @Test
    @DisplayName("AC-20.cleanup restart and instance switch discard staging while retry reuses it")
    fun AC_20_cleanup_restart_and_instance_switch_discard_staging_while_retry_reuses_it() = runTest {
        val staging = FakeAttachmentStaging()
        staging.stage(scope, "operation-old", descriptor, bytes)
        val restarted = model(staging = staging, processId = "process-2")
        restarted.prepareAfterRestart()
        assertEquals(0, staging.stagedCount())

        val retryStaging = FakeAttachmentStaging()
        val retryWriter = FakeDestinationWriter(
            outcome = Outcome.Failure(CoreFailure("provider_busy", "Try again.")),
        )
        val retry = model(
            staging = retryStaging,
            writer = retryWriter,
            port = portWithSuccess(),
        )
        retry.load()
        retry.startTransfer()
        retry.beginSave()
        assertEquals(AttachmentPhase.Failed, retry.state.value.phase)
        assertTrue(retry.state.value.stagingAvailable)

        retryWriter.outcome = null
        retry.beginSave()
        assertEquals(AttachmentPhase.Saved, retry.state.value.phase)
        assertEquals("report.pdf", retry.state.value.savedName)

        val switchStaging = FakeAttachmentStaging()
        val switched = model(staging = switchStaging)
        switched.load()
        switched.startTransfer()
        switched.onInstanceChanged(InstanceId("instance-b"))
        assertEquals(AttachmentPhase.Discarded, switched.state.value.phase)
        assertEquals(0, switchStaging.stagedCount())
        assertTrue(switched.state.value.notice is AttachmentNotice.InstanceChanged)
    }

    @Test
    @DisplayName("AC-20.deadline applies default valid override timeout and separate save cancellation")
    fun AC_20_deadline_applies_default_valid_override_timeout_and_separate_save_cancellation() = runTest {
        val clock = FakeClock(nowMillis = 1_000L)
        val defaultModel = model(clock = clock)
        defaultModel.load()
        defaultModel.startTransfer()

        assertEquals(
            1_000L + AttachmentLimits.DEFAULT_DEADLINE_MILLIS,
            defaultModel.state.value.deadlineAtEpochMillis,
        )

        val overrideClock = FakeClock(nowMillis = 1_000L)
        val overridePort = portWithSuccess()
        val override = model(port = overridePort, clock = overrideClock)
        override.load()
        assertTrue(override.setDeadlineOverrideMillis(5_000L))
        override.startTransfer()
        assertEquals(6_000L, override.state.value.deadlineAtEpochMillis)
        assertEquals(6_000L, overridePort.requests.single().deadlineAtEpochMillis)

        val invalidClock = FakeClock(nowMillis = 1_000L)
        val invalid = model(clock = invalidClock)
        assertFalse(invalid.setDeadlineOverrideMillis(0L))
        assertFalse(invalid.setDeadlineOverrideMillis(AttachmentLimits.MAX_OVERRIDE_MILLIS + 1L))
        assertEquals(AttachmentNotice.InvalidDeadline, invalid.state.value.notice)

        val timeoutClock = FakeClock(nowMillis = 1_000L)
        val timeoutPort = portWithSuccess()
        timeoutPort.duringFetch = { timeoutClock.advance(AttachmentLimits.DEFAULT_DEADLINE_MILLIS + 1L) }
        val timeout = model(port = timeoutPort, clock = timeoutClock)
        timeout.load()
        timeout.startTransfer()
        assertEquals(AttachmentPhase.Failed, timeout.state.value.phase)
        assertTrue(timeout.state.value.notice is AttachmentNotice.TransferDeadlineExceeded)

        val cancelClock = FakeClock(nowMillis = 1_000L)
        val writer = FakeDestinationWriter()
        val separate = model(writer = writer, clock = cancelClock)
        separate.load()
        separate.startTransfer()
        writer.duringWrite = { separate.cancelSave() }
        separate.beginSave()

        assertEquals(AttachmentPhase.Cancelled, separate.state.value.phase)
        assertEquals(TransferOutcome.Succeeded(bytes.size.toLong()), separate.state.value.transfer)
        assertEquals(SaveOutcome.Cancelled, separate.state.value.save)
    }

    @Test
    @DisplayName("AC-20.scope-loss authorization loss and instance replacement discard private staging")
    fun AC_20_scope_loss_authorization_loss_and_instance_replacement_discard_private_staging() = runTest {
        val staging = FakeAttachmentStaging()
        val authorization = FakeAttachmentAuthorizationPort()
        val viewModel = model(staging = staging, authorization = authorization)
        viewModel.load()
        viewModel.startTransfer()
        assertTrue(viewModel.state.value.stagingAvailable)

        authorization.authorized = false
        viewModel.beginSave()

        assertEquals(AttachmentPhase.Unavailable, viewModel.state.value.phase)
        assertEquals(AttachmentNotice.AuthorizationLost, viewModel.state.value.notice)
        assertFalse(viewModel.state.value.stagingAvailable)
        assertEquals(0, staging.stagedCount())

        val switchStaging = FakeAttachmentStaging()
        val switched = model(staging = switchStaging)
        switched.load()
        switched.startTransfer()
        switched.onInstanceChanged(InstanceId("instance-b"))
        assertEquals(AttachmentPhase.Discarded, switched.state.value.phase)
        assertEquals(0, switchStaging.stagedCount())
    }

    @Test
    @DisplayName("AC-20.unknown-save inconclusive provider completion is Unconfirmed not success")
    fun AC_20_unknown_save_inconclusive_provider_completion_is_Unconfirmed_not_success() = runTest {
        val writer = FakeDestinationWriter(
            outcome = Outcome.Success(
                DestinationWriteResult.Unconfirmed(
                    CoreFailure("provider_inconclusive", "Completion is unknown."),
                ),
            ),
        )
        val staging = FakeAttachmentStaging()
        val viewModel = model(staging = staging, writer = writer)
        viewModel.load()
        viewModel.startTransfer()
        viewModel.beginSave()

        assertEquals(AttachmentPhase.Unconfirmed, viewModel.state.value.phase)
        assertTrue(viewModel.state.value.unconfirmed)
        assertNull(viewModel.state.value.savedName)
        assertTrue(viewModel.state.value.save is SaveOutcome.Unconfirmed)
        assertTrue(viewModel.state.value.notice is AttachmentNotice.SaveUnconfirmed)
        assertTrue(viewModel.state.value.stagingAvailable)
    }
}
