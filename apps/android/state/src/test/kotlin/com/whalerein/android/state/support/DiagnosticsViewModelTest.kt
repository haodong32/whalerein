package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.fixtures.support.FakeClock
import com.whalerein.android.state.fixtures.support.FakeCoreFailureSummaryPort
import com.whalerein.android.state.fixtures.support.FakeJournalExporter
import com.whalerein.android.state.fixtures.support.FakeJournalStore
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test
import java.nio.charset.StandardCharsets

class DiagnosticsViewModelTest {
    private val instance = InstanceId("instance-a")

    private fun event(
        level: DiagnosticLevel = DiagnosticLevel.Warn,
        name: String = DiagnosticEvents.OBSERVATION_FAILED,
        stage: String = DiagnosticStages.OBSERVATION,
        code: String? = null,
        traceId: String? = null,
        actionId: String? = null,
    ) = DiagnosticEvent(
        level = level,
        event = name,
        stage = stage,
        code = code,
        traceId = traceId,
        actionId = actionId,
        instanceId = instance.value,
    )

    private fun model(
        journal: Journal,
        exporter: FakeJournalExporter = FakeJournalExporter(),
        summary: FakeCoreFailureSummaryPort? = null,
    ) = DiagnosticsViewModel(
        instanceId = instance,
        journal = journal,
        exporter = exporter,
        diagnostics = DiagnosticReceiver(),
        coreFailureSummary = summary,
        appBuild = "app-1",
        coreBuild = "core-1",
    )

    @Test
    @DisplayName("AC-23.init-failed preview and export stay available when Core cannot open")
    fun AC_23_init_failed_preview_and_export_stay_available_when_Core_cannot_open() = runTest {
        val store = FakeJournalStore()
        val clock = FakeClock()
        val journal = Journal(store, clock)
        journal.emit(event(code = "startup_config"))
        journal.flush()
        val exporter = FakeJournalExporter()
        val viewModel = model(journal, exporter)

        viewModel.markCoreUnavailable(CoreFailure("core_open_failed", "Core did not open."))
        viewModel.refresh()

        val failed = viewModel.state.value
        assertFalse(failed.coreInitialized)
        assertEquals("core_open_failed", failed.coreFailure?.code)
        assertEquals(1, failed.preview.size)

        viewModel.requestExport()
        viewModel.confirmExport()

        assertEquals(1, exporter.writes.size)
        assertTrue(viewModel.state.value.exportState is DiagnosticsExportState.Completed)
    }

    @Test
    @DisplayName("AC-23.stages record login transport observation and reply read failures")
    fun AC_23_stages_record_login_transport_observation_and_reply_read_failures() = runTest {
        val journal = Journal(FakeJournalStore(), FakeClock())
        journal.emit(event(name = DiagnosticEvents.LOGIN_FAILED, stage = DiagnosticStages.LOGIN, code = "invalid_state"))
        journal.emit(
            event(
                name = DiagnosticEvents.TRANSPORT_FAILED,
                stage = DiagnosticStages.TRANSPORT,
                code = "tls_rejected",
            ),
        )
        journal.emit(
            event(
                name = DiagnosticEvents.OBSERVATION_FAILED,
                stage = DiagnosticStages.OBSERVATION,
                code = "watch_lost",
            ),
        )
        journal.emit(
            event(
                name = DiagnosticEvents.REPLY_READ_FAILED,
                stage = DiagnosticStages.REPLY_READ,
                code = "content_unavailable",
            ),
        )
        journal.flush()

        val viewModel = model(journal)
        viewModel.refresh()

        val stages = viewModel.state.value.preview.mapNotNull { it.stage }
        assertEquals(
            listOf(
                DiagnosticStages.LOGIN,
                DiagnosticStages.TRANSPORT,
                DiagnosticStages.OBSERVATION,
                DiagnosticStages.REPLY_READ,
            ),
            stages,
        )
        assertTrue(viewModel.state.value.preview.all { it.code != null })
    }

    @Test
    @DisplayName("AC-23.core-sink shares one scoped writer and preserves correlation ids")
    fun AC_23_core_sink_shares_one_scoped_writer_and_preserves_correlation_ids() = runTest {
        val journal = Journal(FakeJournalStore(), FakeClock())
        journal.emit(
            event(
                name = DiagnosticEvents.OBSERVATION_STARTED,
                level = DiagnosticLevel.Info,
                stage = DiagnosticStages.OBSERVATION,
                actionId = "action-7",
            ),
        )
        journal.emitCore(
            event(
                name = DiagnosticEvents.TRANSPORT_FAILED,
                stage = DiagnosticStages.TRANSPORT,
                code = "mqtt_closed",
                traceId = "trace-7",
                actionId = "action-7",
            ),
        )
        journal.flush()

        val records = journal.read(instance.value).events
        assertEquals(2, records.size)
        assertEquals(
            listOf(JournalSchema.SOURCE_ANDROID, JournalSchema.SOURCE_CORE),
            records.map { it.source },
        )
        assertEquals("action-7", records[0].actionId)
        assertEquals("trace-7", records[1].traceId)
        assertEquals("action-7", records[1].actionId)
        assertEquals(instance.value, records[1].instanceId)
    }

    @Test
    @DisplayName("AC-23.summary adds Core last-failure metadata without duplicate rows")
    fun AC_23_summary_adds_Core_last_failure_metadata_without_duplicate_rows() = runTest {
        val journal = Journal(FakeJournalStore(), FakeClock())
        journal.emit(
            event(
                name = DiagnosticEvents.COMMAND_FAILED,
                stage = DiagnosticStages.COMMAND,
                code = "send_failed",
            ),
        )
        journal.flush()
        val summary = FakeCoreFailureSummaryPort(
            summary = CoreFailureSummary(
                code = "send_failed",
                occurredAtEpochMillis = 1_700_000_000_000L,
                traceId = "trace-9",
            ),
        )
        val exporter = FakeJournalExporter()
        val viewModel = model(journal, exporter, summary)
        viewModel.refresh()

        viewModel.requestExport()
        viewModel.confirmExport()

        val text = String(exporter.writes.single().second, StandardCharsets.UTF_8)
        val lines = text.split('\n').filter { it.isNotBlank() }
        assertEquals(2, lines.size)
        assertTrue(lines[0].contains("\"kind\":\"metadata\""))
        assertTrue(lines[0].contains("\"coreLastFailure\""))
        assertTrue(lines[0].contains("\"code\":\"send_failed\""))
        assertEquals(1, lines.count { it.contains("\"kind\":\"event\"") })
        assertEquals(1, viewModel.state.value.totalEvents)
    }

    @Test
    @DisplayName("AC-23.export freezes the reviewed snapshot and clear drops pre-clear records")
    fun AC_23_export_freezes_the_reviewed_snapshot_and_clear_drops_pre_clear_records() = runTest {
        val store = FakeJournalStore()
        val clock = FakeClock()
        val journal = Journal(store, clock)
        journal.emit(event(name = DiagnosticEvents.TRANSPORT_FAILED, code = "first"))
        journal.emit(event(name = DiagnosticEvents.OBSERVATION_FAILED, code = "second"))
        journal.flush()
        val summary = FakeCoreFailureSummaryPort()
        val exporter = FakeJournalExporter()
        val viewModel = model(journal, exporter, summary)
        viewModel.refresh()

        viewModel.requestExport()
        assertEquals(2, viewModel.state.value.exportConfirmation?.eventCount)

        // Concurrent write and an instance switch must not alter the snapshot.
        journal.emit(event(name = DiagnosticEvents.REPLY_READ_FAILED, code = "late"))
        journal.flush()
        viewModel.setActiveInstance(InstanceId("instance-b"))
        viewModel.confirmExport()

        val written = String(exporter.writes.single().second, StandardCharsets.UTF_8)
        val lines = written.split('\n').filter { it.isNotBlank() }
        assertEquals(3, lines.size)
        assertTrue(lines[0].contains("\"kind\":\"metadata\""))
        assertTrue(written.contains("first"))
        assertTrue(written.contains("second"))
        assertFalse(written.contains("late"))
        assertTrue(exporter.writes.single().first.endsWith(".jsonl"))
        assertTrue(viewModel.state.value.exportState is DiagnosticsExportState.Completed)

        // Failures are explicit, never reported as completion.
        val failedExporter = FakeJournalExporter(
            outcome = Outcome.Failure(CoreFailure("export_failed", "Provider rejected the export.")),
        )
        val failed = model(journal, failedExporter)
        failed.refresh()
        failed.requestExport()
        failed.confirmExport()
        assertTrue(failed.state.value.exportState is DiagnosticsExportState.Failed)

        // Queued pre-clear records disappear; a post-clear record remains visible.
        journal.emit(event(name = DiagnosticEvents.OBSERVATION_STARTED))
        viewModel.setActiveInstance(instance)
        viewModel.refresh()
        viewModel.requestClear()
        assertEquals(instance, viewModel.state.value.clearConfirmation?.instanceId)
        viewModel.confirmClear()
        assertEquals(1, summary.clearCount)
        assertTrue(viewModel.state.value.preview.isEmpty())
        journal.flush()
        assertTrue(journal.read(instance.value).events.isEmpty())

        journal.emit(event(name = DiagnosticEvents.OBSERVATION_FAILED, code = "after_clear"))
        viewModel.refresh()
        assertEquals(listOf("after_clear"), viewModel.state.value.preview.map { it.code })
        assertNull(viewModel.state.value.clearConfirmation)
    }
}
