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
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/** Save-path behavior shared by the Android document-provider writer. */
class AttachmentScopedSaveTest {
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

    private fun port(): FakeAttachmentPort {
        val port = FakeAttachmentPort(descriptor = descriptor)
        port.fetchOutcomes.add(
            Outcome.Success(
                VerifiedAttachment(descriptor = descriptor, bytes = bytes, sha256 = "sha-verified"),
            ),
        )
        return port
    }

    private fun model(writer: FakeDestinationWriter) = AttachmentSaveViewModel(
        attachmentScope = scope,
        operationId = "operation-a",
        port = port(),
        staging = FakeAttachmentStaging(),
        authorization = FakeAttachmentAuthorizationPort(),
        writer = writer,
        clock = FakeClock(),
        diagnostics = DiagnosticReceiver(),
        processId = "process-1",
    )

    @Test
    @DisplayName("AC-26.provider-write-failure-keeps-the-staged-copy-and-retry-writes-exact-bytes")
    fun ac26ProviderWriteFailureKeepsTheStagedCopyAndRetryWritesExactBytes() = runTest {
        val writer = FakeDestinationWriter()
        val viewModel = model(writer)
        viewModel.load()
        viewModel.startTransfer()
        assertTrue(viewModel.state.value.stagingAvailable)

        writer.outcome = Outcome.Failure(
            CoreFailure("provider_write_unconfirmed", "The provider refused the write.", retryable = true),
        )
        viewModel.beginSave()
        assertEquals(AttachmentPhase.Failed, viewModel.state.value.phase)
        assertTrue(viewModel.state.value.notice is AttachmentNotice.ProviderFailure)
        assertTrue(viewModel.state.value.stagingAvailable)

        writer.outcome = null
        viewModel.beginSave()
        assertEquals(AttachmentPhase.Saved, viewModel.state.value.phase)
        assertEquals("report.pdf", viewModel.state.value.savedName)
        assertArrayEquals(bytes, writer.documents.getValue("report.pdf"))
    }

    @Test
    @DisplayName("AC-26.provider-cancellation-is-a-cancel-not-a-write-failure")
    fun ac26ProviderCancellationIsACancelNotAWriteFailure() = runTest {
        val writer = FakeDestinationWriter().apply { outcome = Outcome.Cancelled }
        val viewModel = model(writer)
        viewModel.load()
        viewModel.startTransfer()

        viewModel.beginSave()

        assertEquals(AttachmentPhase.Cancelled, viewModel.state.value.phase)
        assertEquals(SaveOutcome.Cancelled, viewModel.state.value.save)
        assertFalse(viewModel.state.value.saved)
    }

    @Test
    @DisplayName("AC-26.a-name-conflict-creates-another-copy-and-keeps-the-original")
    fun ac26ANameConflictCreatesAnotherCopyAndKeepsTheOriginal() = runTest {
        val writer = FakeDestinationWriter(initial = mapOf("report.pdf" to "original".toByteArray()))
        val viewModel = model(writer)
        viewModel.load()
        viewModel.startTransfer()

        viewModel.beginSave()

        assertEquals(AttachmentPhase.Saved, viewModel.state.value.phase)
        assertEquals("report (1).pdf", viewModel.state.value.savedName)
        assertEquals("original", writer.documents.getValue("report.pdf").decodeToString())
        assertArrayEquals(bytes, writer.documents.getValue("report (1).pdf"))
        assertEquals("report (1).pdf", DestinationNaming.uniqueName("report.pdf", listOf("report.pdf")))
    }
}
