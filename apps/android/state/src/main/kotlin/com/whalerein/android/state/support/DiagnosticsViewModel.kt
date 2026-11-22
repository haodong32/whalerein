package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.ScreenModel

data class DiagnosticsClearConfirmation(
    val instanceId: InstanceId,
    val eventCount: Int,
)

data class DiagnosticsExportConfirmation(
    val instanceId: InstanceId,
    val fileName: String,
    val eventCount: Int,
    val byteCount: Int,
)

sealed interface DiagnosticsExportState {
    data object Idle : DiagnosticsExportState

    data object Writing : DiagnosticsExportState

    data class Completed(val fileName: String) : DiagnosticsExportState

    data class Failed(val failure: CoreFailure) : DiagnosticsExportState

    data class Unconfirmed(val failure: CoreFailure) : DiagnosticsExportState

    data object Cancelled : DiagnosticsExportState
}

sealed interface DiagnosticsNotice {
    data object Cleared : DiagnosticsNotice

    data class ClearFailed(val failure: CoreFailure) : DiagnosticsNotice

    data class PreviewFailed(val failure: CoreFailure) : DiagnosticsNotice
}

data class DiagnosticsUiState(
    val instanceId: InstanceId,
    val loading: Boolean = false,
    /** False when Core could not open; the journal preview/export stays usable. */
    val coreInitialized: Boolean = true,
    val coreFailure: CoreFailure? = null,
    val status: JournalStatus = JournalStatus.Healthy,
    val preview: List<JournalRecord> = emptyList(),
    val totalEvents: Int = 0,
    val tornTailDiscarded: Boolean = false,
    val coreLastFailure: CoreFailureSummary? = null,
    val clearConfirmation: DiagnosticsClearConfirmation? = null,
    val exportConfirmation: DiagnosticsExportConfirmation? = null,
    val exportState: DiagnosticsExportState = DiagnosticsExportState.Idle,
    val notice: DiagnosticsNotice? = null,
) {
    val degraded: Boolean get() = status is JournalStatus.Degraded
}

/**
 * Diagnostics screen contract.
 *
 * The model never requires a live Core handle: startup records and a degraded
 * banner remain reviewable when Core initialization failed. An export is frozen
 * at confirmation-request time, so concurrent writes or a profile switch cannot
 * change the reviewed bytes.
 */
class DiagnosticsViewModel(
    instanceId: InstanceId,
    private val journal: Journal,
    private val exporter: JournalExporter,
    private val diagnostics: DiagnosticSink,
    private val coreFailureSummary: CoreFailureSummaryPort? = null,
    private val appBuild: String = "unknown",
    private val coreBuild: String = "unknown",
    private val previewLimit: Int = 200,
) : ScreenModel<DiagnosticsUiState>(DiagnosticsUiState(instanceId = instanceId)) {
    private var pendingExport: JournalExport? = null

    /** Core failed before opening; the local journal stays available. */
    fun markCoreUnavailable(failure: CoreFailure) {
        setState(
            state.value.copy(
                coreInitialized = false,
                coreFailure = failure,
            ),
        )
        DiagnosticDelivery.deliver(
            diagnostics,
            DiagnosticEvent(
                level = DiagnosticLevel.Error,
                event = DiagnosticEvents.STARTUP_CORE_FAILED,
                stage = DiagnosticStages.STARTUP,
                code = failure.code,
                instanceId = state.value.instanceId.value,
            ),
        )
    }

    fun markCoreReady() {
        setState(state.value.copy(coreInitialized = true, coreFailure = null))
    }

    /** Updates the visible scope without disturbing a captured export snapshot. */
    fun setActiveInstance(instance: InstanceId) {
        setState(
            state.value.copy(
                instanceId = instance,
                preview = emptyList(),
                totalEvents = 0,
                clearConfirmation = null,
            ),
        )
    }

    suspend fun refresh() {
        setState(state.value.copy(loading = true))
        journal.flush()
        val scopeId = state.value.instanceId.value
        val preview = journal.preview(scopeId, previewLimit)
        val summary = coreFailureSummary?.lastFailure(state.value.instanceId)?.valueOrNull()
        val notice = preview.error?.let { DiagnosticsNotice.PreviewFailed(it) }
        setState(
            state.value.copy(
                loading = false,
                status = journal.status.value,
                preview = preview.events,
                totalEvents = preview.totalEvents,
                tornTailDiscarded = preview.tornTailDiscarded,
                coreLastFailure = summary,
                notice = notice ?: state.value.notice,
            ),
        )
    }

    fun requestClear() {
        setState(
            state.value.copy(
                clearConfirmation = DiagnosticsClearConfirmation(
                    instanceId = state.value.instanceId,
                    eventCount = state.value.totalEvents,
                ),
            ),
        )
    }

    fun cancelClear() {
        setState(state.value.copy(clearConfirmation = null))
    }

    suspend fun confirmClear() {
        val confirmation = state.value.clearConfirmation ?: return
        journal.clear(confirmation.instanceId.value)
        val coreClear = coreFailureSummary?.clear(confirmation.instanceId)
        setState(
            state.value.copy(
                clearConfirmation = null,
                preview = emptyList(),
                totalEvents = 0,
                coreLastFailure = null,
                notice = if (coreClear is Outcome.Failure) {
                    DiagnosticsNotice.ClearFailed(coreClear.failure)
                } else {
                    DiagnosticsNotice.Cleared
                },
            ),
        )
        DiagnosticDelivery.deliver(
            diagnostics,
            DiagnosticEvent(
                level = DiagnosticLevel.Info,
                event = DiagnosticEvents.DIAGNOSTICS_CLEARED,
                stage = DiagnosticStages.CONFIGURATION,
                instanceId = confirmation.instanceId.value,
                fields = mapOf("count" to "0"),
            ),
        )
    }

    suspend fun requestExport() {
        journal.flush()
        val scopeId = state.value.instanceId.value
        val summary = coreFailureSummary?.lastFailure(state.value.instanceId)?.valueOrNull()
        val export = journal.buildExport(
            scopeId = scopeId,
            appBuild = appBuild,
            coreBuild = coreBuild,
            coreLastFailure = summary,
        )
        pendingExport = export
        setState(
            state.value.copy(
                exportConfirmation = DiagnosticsExportConfirmation(
                    instanceId = state.value.instanceId,
                    fileName = export.fileName,
                    eventCount = export.eventCount,
                    byteCount = export.jsonl.size,
                ),
                exportState = DiagnosticsExportState.Idle,
                coreLastFailure = summary,
            ),
        )
    }

    fun cancelExport() {
        pendingExport = null
        setState(
            state.value.copy(
                exportConfirmation = null,
                exportState = DiagnosticsExportState.Cancelled,
            ),
        )
    }

    suspend fun confirmExport() {
        val export = pendingExport ?: return
        setState(
            state.value.copy(
                exportConfirmation = null,
                exportState = DiagnosticsExportState.Writing,
            ),
        )
        val outcome = exporter.write(export.fileName, export.jsonl)
        val next = when (outcome) {
            is Outcome.Success -> when (val result = outcome.value) {
                is ExportWriteResult.Completed -> {
                    emitExport(
                        level = DiagnosticLevel.Info,
                        event = DiagnosticEvents.EXPORT_COMPLETED,
                        code = null,
                    )
                    DiagnosticsExportState.Completed(result.fileName)
                }
                is ExportWriteResult.Unconfirmed -> {
                    emitExport(
                        level = DiagnosticLevel.Warn,
                        event = DiagnosticEvents.EXPORT_UNCONFIRMED,
                        code = result.failure.code,
                    )
                    DiagnosticsExportState.Unconfirmed(result.failure)
                }
            }
            is Outcome.Failure -> {
                emitExport(
                    level = DiagnosticLevel.Error,
                    event = DiagnosticEvents.EXPORT_FAILED,
                    code = outcome.failure.code,
                )
                DiagnosticsExportState.Failed(outcome.failure)
            }
            is Outcome.Unconfirmed -> {
                emitExport(
                    level = DiagnosticLevel.Warn,
                    event = DiagnosticEvents.EXPORT_UNCONFIRMED,
                    code = outcome.failure.code,
                )
                DiagnosticsExportState.Unconfirmed(outcome.failure)
            }
            is Outcome.Cancelled -> DiagnosticsExportState.Cancelled
        }
        pendingExport = null
        setState(state.value.copy(exportState = next))
    }

    private fun emitExport(level: DiagnosticLevel, event: String, code: String?) {
        DiagnosticDelivery.deliver(
            diagnostics,
            DiagnosticEvent(
                level = level,
                event = event,
                stage = DiagnosticStages.EXPORT,
                code = code,
                instanceId = state.value.instanceId.value,
            ),
        )
    }
}
