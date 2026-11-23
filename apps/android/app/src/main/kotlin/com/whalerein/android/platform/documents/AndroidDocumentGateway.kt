package com.whalerein.android.platform.documents

import android.net.Uri
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import com.whalerein.android.state.attachments.DestinationWriteResult
import com.whalerein.android.state.attachments.DestinationWriter
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.support.ExportWriteResult
import com.whalerein.android.state.support.JournalExporter
import java.io.InputStream
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Activity-scoped bridge between suspend seams and the system document picker.
 *
 * `CreateDocument` creates a fresh document, so an existing same-name file is
 * never overwritten; the provider may still disambiguate the returned name. The
 * picker result is awaited through a suspended deferred, which lets the screen
 * model stay platform-free.
 */
class AndroidDocumentGateway(private val activity: ComponentActivity) {
    private val exportResult = PendingResult()
    private val attachmentResult = PendingResult()
    private val openResult = PendingResult()

    private val exportLauncher: ActivityResultLauncher<String> =
        activity.registerForActivityResult(
            ActivityResultContracts.CreateDocument(JOURNAL_MIME_TYPE),
            exportResult::complete,
        )
    private val attachmentLauncher: ActivityResultLauncher<String> =
        activity.registerForActivityResult(
            ActivityResultContracts.CreateDocument(ATTACHMENT_MIME_TYPE),
            attachmentResult::complete,
        )
    private val openLauncher: ActivityResultLauncher<Array<String>> =
        activity.registerForActivityResult(
            ActivityResultContracts.OpenDocument(),
            openResult::complete,
        )

    suspend fun createExport(fileName: String): Uri? =
        exportResult.launch { exportLauncher.launch(fileName) }

    suspend fun createAttachment(fileName: String): Uri? =
        attachmentResult.launch { attachmentLauncher.launch(fileName) }

    suspend fun openPem(): String? {
        val uri = openResult.launch { openLauncher.launch(arrayOf(PEM_MIME_TYPE, GENERIC_MIME_TYPE)) }
            ?: return null
        return withContext(Dispatchers.IO) {
            val stream: InputStream = activity.contentResolver.openInputStream(uri)
                ?: return@withContext null
            stream.use { input ->
                val text = input.bufferedReader(Charsets.UTF_8).use { reader ->
                    val buffer = CharArray(MAX_PEM_CHARS + 1)
                    var read = 0
                    while (read < buffer.size) {
                        val count = reader.read(buffer, read, buffer.size - read)
                        if (count < 0) break
                        read += count
                    }
                    String(buffer, 0, read)
                }
                text.takeIf { it.length <= MAX_PEM_CHARS }
            }
        }
    }

    /** True when the provider accepted the bytes without a reported failure. */
    suspend fun write(uri: Uri, bytes: ByteArray): Boolean = withContext(Dispatchers.IO) {
        try {
            val stream = activity.contentResolver.openOutputStream(uri, "w")
                ?: return@withContext false
            stream.use { output ->
                output.write(bytes)
                output.flush()
            }
            true
        } catch (_: Exception) {
            // The document provider refused or aborted the write. Callers report
            // an unconfirmed save because partial bytes may exist.
            false
        }
    }

    private class PendingResult {
        private var pending: CompletableDeferred<Uri?>? = null

        fun complete(uri: Uri?) {
            val deferred = synchronized(this) {
                val current = pending
                pending = null
                current
            }
            deferred?.complete(uri)
        }

        suspend fun launch(block: () -> Unit): Uri? {
            val deferred = CompletableDeferred<Uri?>()
            synchronized(this) {
                if (pending != null) return null
                pending = deferred
            }
            withContext(Dispatchers.Main.immediate) {
                block()
            }
            return deferred.await()
        }
    }

    private companion object {
        const val JOURNAL_MIME_TYPE = "application/json"
        const val ATTACHMENT_MIME_TYPE = "application/octet-stream"
        const val PEM_MIME_TYPE = "application/x-pem-file"
        const val GENERIC_MIME_TYPE = "text/plain"
        const val MAX_PEM_CHARS = 1_048_576
    }
}

/** Diagnostic journal export through `CreateDocument`. */
class PickerJournalExporter(
    private val gateway: () -> AndroidDocumentGateway?,
) : JournalExporter {
    override suspend fun write(fileName: String, jsonl: ByteArray): Outcome<ExportWriteResult> {
        val documents = gateway() ?: return documentUnavailable()
        val uri = documents.createExport(fileName) ?: return Outcome.Cancelled
        return try {
            if (documents.write(uri, jsonl)) {
                Outcome.Success(ExportWriteResult.Completed(fileName))
            } else {
                Outcome.Success(ExportWriteResult.Unconfirmed(providerFailure()))
            }
        } catch (_: Exception) {
            Outcome.Success(ExportWriteResult.Unconfirmed(providerFailure()))
        }
    }

    private fun documentUnavailable(): Outcome<ExportWriteResult> = Outcome.Failure(
        CoreFailure("document_picker_unavailable", "document_picker_unavailable", retryable = true),
    )
}

/** Verified attachment bytes through `CreateDocument`, one new document per save. */
class PickerDestinationWriter(
    private val gateway: () -> AndroidDocumentGateway?,
) : DestinationWriter {
    override suspend fun write(requestedName: String, bytes: ByteArray): Outcome<DestinationWriteResult> {
        val documents = gateway() ?: return Outcome.Failure(
            CoreFailure("document_picker_unavailable", "document_picker_unavailable", retryable = true),
        )
        val uri = documents.createAttachment(requestedName) ?: return Outcome.Cancelled
        return try {
            if (documents.write(uri, bytes)) {
                Outcome.Success(DestinationWriteResult.Completed(requestedName))
            } else {
                Outcome.Success(DestinationWriteResult.Unconfirmed(providerFailure()))
            }
        } catch (_: Exception) {
            Outcome.Success(DestinationWriteResult.Unconfirmed(providerFailure()))
        }
    }
}

private fun providerFailure(): CoreFailure =
    CoreFailure("provider_write_unconfirmed", "provider_write_unconfirmed", retryable = true)
