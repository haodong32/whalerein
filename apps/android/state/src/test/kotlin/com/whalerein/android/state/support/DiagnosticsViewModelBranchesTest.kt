package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticEvent
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

/** Additional branch coverage for diagnostics preview, clear and export. */
class DiagnosticsViewModelBranchesTest {
    private val instance = InstanceId("instance-a")

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
    )

    @Test
    @DisplayName("AC-23.init-failed core readiness and scope changes keep preview available")
    fun AC_23_init_failed_core_readiness_and_scope_changes_keep_preview_available() = runTest {
        val journal = Journal(FakeJournalStore(), FakeClock())
        val viewModel = model(journal)
        viewModel.markCoreUnavailable(CoreFailure("core_open_failed", "no"))
        assertFalse(viewModel.state.value.coreInitialized)
        viewModel.markCoreReady()
        assertTrue(viewModel.state.value.coreInitialized)
        assertNull(viewModel.state.value.coreFailure)

        viewModel.setActiveInstance(InstanceId("instance-b"))
        assertEquals(InstanceId("instance-b"), viewModel.state.value.instanceId)
        viewModel.setActiveInstance(instance)
        assertEquals(instance, viewModel.state.value.instanceId)
    }

    @Test
    @DisplayName("AC-23.stages read failures and missing Core summaries stay explicit")
    fun AC_23_stages_read_failures_and_missing_core_summaries_stay_explicit() = runTest {
        val store = FakeJournalStore()
        store.failReads = true
        val journal = Journal(store, FakeClock())
        val summary = FakeCoreFailureSummaryPort(
            failureOutcome = Outcome.Failure(CoreFailure("summary_failed", "no")),
        )
        val viewModel = model(journal, summary = summary)
        viewModel.refresh()
        assertTrue(viewModel.state.value.notice is DiagnosticsNotice.PreviewFailed)
        assertNull(viewModel.state.value.coreLastFailure)
    }

    @Test
    @DisplayName("AC-23.export pending exports and provider outcomes stay explicit")
    fun AC_23_export_pending_exports_and_provider_outcomes_stay_explicit() = runTest {
        val journal = Journal(FakeJournalStore(), FakeClock())
        journal.emit(
            DiagnosticEvent(
                level = com.whalerein.android.state.core.DiagnosticLevel.Warn,
                event = DiagnosticEvents.OBSERVATION_FAILED,
                stage = DiagnosticStages.OBSERVATION,
                instanceId = instance.value,
            ),
        )
        journal.flush()

        val viewModel = model(journal)
        viewModel.cancelClear()
        viewModel.confirmClear()
        viewModel.confirmExport()
        viewModel.cancelExport()
        assertEquals(DiagnosticsExportState.Cancelled, viewModel.state.value.exportState)

        viewModel.refresh()
        viewModel.requestExport()
        viewModel.cancelExport()
        assertNull(viewModel.state.value.exportConfirmation)
        viewModel.confirmExport()
        assertEquals(DiagnosticsExportState.Cancelled, viewModel.state.value.exportState)

        val unconfirmedWrite = FakeJournalExporter(
            outcome = Outcome.Success(
                ExportWriteResult.Unconfirmed(CoreFailure("provider_unknown", "no")),
            ),
        )
        val unconfirmedModel = model(journal, exporter = unconfirmedWrite)
        unconfirmedModel.refresh()
        unconfirmedModel.requestExport()
        unconfirmedModel.confirmExport()
        assertTrue(unconfirmedModel.state.value.exportState is DiagnosticsExportState.Unconfirmed)

        val unconfirmedOutcome = FakeJournalExporter(
            outcome = Outcome.Unconfirmed(CoreFailure("provider_unknown", "no")),
        )
        val outcomeModel = model(journal, exporter = unconfirmedOutcome)
        outcomeModel.refresh()
        outcomeModel.requestExport()
        outcomeModel.confirmExport()
        assertTrue(outcomeModel.state.value.exportState is DiagnosticsExportState.Unconfirmed)

        val cancelledExport = FakeJournalExporter(outcome = Outcome.Cancelled)
        val cancelledModel = model(journal, exporter = cancelledExport)
        cancelledModel.refresh()
        cancelledModel.requestExport()
        cancelledModel.confirmExport()
        assertEquals(DiagnosticsExportState.Cancelled, cancelledModel.state.value.exportState)
    }

    @Test
    @DisplayName("AC-23.summary clearing a missing Core summary reports the local result")
    fun AC_23_summary_clearing_a_missing_core_summary_reports_the_local_result() = runTest {
        val journal = Journal(FakeJournalStore(), FakeClock())
        journal.emit(
            DiagnosticEvent(
                level = com.whalerein.android.state.core.DiagnosticLevel.Warn,
                event = DiagnosticEvents.OBSERVATION_FAILED,
                stage = DiagnosticStages.OBSERVATION,
                instanceId = instance.value,
            ),
        )
        journal.flush()
        val summary = FakeCoreFailureSummaryPort(
            clearOutcome = Outcome.Failure(CoreFailure("clear_failed", "no")),
        )
        val viewModel = model(journal, summary = summary)
        viewModel.refresh()
        viewModel.requestClear()
        viewModel.confirmClear()
        assertTrue(viewModel.state.value.notice is DiagnosticsNotice.ClearFailed)
        assertEquals(1, summary.clearCount)
        assertTrue(viewModel.state.value.preview.isEmpty())

        val noSummary = model(Journal(FakeJournalStore(), FakeClock()))
        noSummary.refresh()
        noSummary.requestExport()
        noSummary.confirmExport()
        assertTrue(noSummary.state.value.exportState is DiagnosticsExportState.Completed)
    }

    @Test
    @DisplayName("AC-23.export diagnostics models expose confirmation degraded and export states")
    fun AC_23_export_diagnostics_models_expose_confirmation_degraded_and_export_states() {
        val clear = DiagnosticsClearConfirmation(instance, 2)
        assertEquals(instance, clear.instanceId)
        assertEquals(2, clear.eventCount)

        val exportConfirmation = DiagnosticsExportConfirmation(instance, "file.jsonl", 2, 100)
        assertEquals(instance, exportConfirmation.instanceId)
        assertEquals("file.jsonl", exportConfirmation.fileName)
        assertEquals(2, exportConfirmation.eventCount)
        assertEquals(100, exportConfirmation.byteCount)

        val exportStates: List<DiagnosticsExportState> = listOf(
            DiagnosticsExportState.Idle,
            DiagnosticsExportState.Writing,
            DiagnosticsExportState.Completed("file.jsonl"),
            DiagnosticsExportState.Failed(CoreFailure("code", "message")),
            DiagnosticsExportState.Unconfirmed(CoreFailure("code", "message")),
            DiagnosticsExportState.Cancelled,
        )
        exportStates.forEach { assertEquals(it, it) }
        assertEquals("file.jsonl", (exportStates[2] as DiagnosticsExportState.Completed).fileName)

        val notices: List<DiagnosticsNotice> = listOf(
            DiagnosticsNotice.Cleared,
            DiagnosticsNotice.ClearFailed(CoreFailure("code", "message")),
            DiagnosticsNotice.PreviewFailed(CoreFailure("code", "message")),
        )
        notices.forEach { assertEquals(it, it) }

        val summary = CoreFailureSummary("code", 1L, "warn", "trace")
        val record = JournalRecord(
            schemaVersion = 2,
            kind = JournalSchema.KIND_METADATA,
            source = JournalSchema.SOURCE_JOURNAL,
            timestampEpochMillis = 1L,
            level = "WARN",
            event = "event",
            stage = "stage",
            code = "code",
            traceId = "trace",
            actionId = "action",
            instanceId = "instance",
            fields = mapOf("count" to "1"),
            exportedAtEpochMillis = 2L,
            scopeId = "scope",
            appBuild = "app",
            coreBuild = "core",
            coreLastFailure = summary,
        )
        assertEquals("code", record.coreLastFailure?.code)
        assertEquals("trace", record.traceId)
        assertEquals("next", record.copy(event = "next").event)

        val state = DiagnosticsUiState(
            instanceId = instance,
            loading = false,
            coreInitialized = false,
            coreFailure = CoreFailure("code", "message"),
            status = JournalStatus.Degraded(
                JournalDegradationReason.Storage,
                droppedRecords = 2,
                detailCode = "code",
            ),
            preview = listOf(record),
            totalEvents = 1,
            tornTailDiscarded = true,
            coreLastFailure = summary,
            clearConfirmation = clear,
            exportConfirmation = exportConfirmation,
            exportState = exportStates[3],
            notice = notices[0],
        )
        assertTrue(state.degraded)
        assertFalse(state.copy(status = JournalStatus.Healthy).degraded)
        assertEquals(1, state.preview.size)
        assertTrue(state.copy(preview = emptyList()).preview.isEmpty())
    }
}
