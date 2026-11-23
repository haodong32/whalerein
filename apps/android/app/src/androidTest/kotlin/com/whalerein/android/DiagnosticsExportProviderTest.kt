package com.whalerein.android

import android.net.Uri
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.whalerein.android.platform.journal.AndroidJournalStore
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.support.DiagnosticsExportState
import com.whalerein.android.state.support.DiagnosticsViewModel
import com.whalerein.android.state.support.ExportWriteResult
import com.whalerein.android.state.support.Journal
import com.whalerein.android.state.support.JournalExporter
import com.whalerein.android.state.support.JournalSource
import java.io.File
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Confirmed export writes the captured snapshot through the real ContentResolver
 * output path and keeps redaction intact.
 */
@RunWith(AndroidJUnit4::class)
class DiagnosticsExportProviderTest {
    @get:Rule
    val rule = createAndroidComposeRule<SupportTestActivity>()

    private val context =
        androidx.test.platform.app.InstrumentationRegistry.getInstrumentation().targetContext
    private val directory = File(
        context.cacheDir,
        "diagnostics-export-${System.nanoTime()}",
    )

    @After
    fun cleanup() {
        directory.deleteRecursively()
        TestDocumentProvider.reset(context)
    }

    @Test
    fun `AC-28_confirmed_export_writes_a_redacted_snapshot_to_the_provider`() = runBlocking {
        TestDocumentProvider.reset(context)
        val journal = Journal(AndroidJournalStore(directory), FixedClock())
        journal.record(
            JournalSource.Android,
            DiagnosticEvent(
                level = DiagnosticLevel.Warn,
                event = "transport.host_online",
                stage = "transport",
                code = "ready",
                instanceId = "instance-a",
                fields = mapOf(
                    "host_id" to "host-a",
                    "token" to "must-not-leak",
                    "content" to "private message",
                ),
            ),
        )
        journal.flush()

        val gateway = rule.activity.documents
        val uri = Uri.parse("content://com.whalerein.android.test.documents/export")
        val exporter = object : JournalExporter {
            override suspend fun write(fileName: String, jsonl: ByteArray): Outcome<ExportWriteResult> =
                if (gateway.write(uri, jsonl)) {
                    Outcome.Success(ExportWriteResult.Completed(fileName))
                } else {
                    Outcome.Success(
                        ExportWriteResult.Unconfirmed(
                            com.whalerein.android.state.core.CoreFailure(
                                "provider_write_unconfirmed",
                                "provider_write_unconfirmed",
                                retryable = true,
                            ),
                        ),
                    )
                }
        }
        val viewModel = DiagnosticsViewModel(
            instanceId = InstanceId("instance-a"),
            journal = journal,
            exporter = exporter,
            diagnostics = journal,
            appBuild = "app-test",
            coreBuild = "core-test",
            coreFailureSummary = null,
        )
        viewModel.refresh()
        assertEquals(1, viewModel.state.value.totalEvents)

        viewModel.requestExport()
        assertTrue(viewModel.state.value.exportConfirmation != null)
        viewModel.confirmExport()
        assertTrue(viewModel.state.value.exportState is DiagnosticsExportState.Completed)

        val written = TestDocumentProvider.read(context, uri)
        assertTrue(written.contains("\"kind\":\"metadata\""))
        assertTrue(written.contains("transport.host_online"))
        assertTrue(written.contains("host-a"))
        assertTrue(written.contains("\"appBuild\":\"app-test\""))
        assertFalse(written.contains("must-not-leak"))
        assertFalse(written.contains("private message"))
    }
}
