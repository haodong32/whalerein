package com.whalerein.android.ui.support

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.whalerein.android.R
import com.whalerein.android.state.support.DiagnosticsExportState
import com.whalerein.android.state.support.DiagnosticsNotice
import com.whalerein.android.state.support.DiagnosticsUiState
import com.whalerein.android.state.support.JournalRecord
import com.whalerein.android.state.support.JournalStatus

/**
 * Stateless diagnostics screen.
 *
 * The preview, clear and export actions do not require a live Core handle, so
 * startup failures remain reviewable. Export always prompts for explicit
 * confirmation of the captured snapshot.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DiagnosticsScreen(
    state: DiagnosticsUiState,
    onRefresh: () -> Unit,
    onRequestClear: () -> Unit,
    onConfirmClear: () -> Unit,
    onCancelClear: () -> Unit,
    onRequestExport: () -> Unit,
    onConfirmExport: () -> Unit,
    onCancelExport: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.support_diagnostics_title)) },
                navigationIcon = {
                    TextButton(onClick = onBack) {
                        Text(stringResource(R.string.support_back))
                    }
                },
                actions = {
                    TextButton(onClick = onRefresh, enabled = !state.loading) {
                        Text(stringResource(R.string.support_refresh))
                    }
                },
            )
        },
    ) { padding ->
        LazyColumn(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            item { DiagnosticsStatusCard(state) }
            state.notice?.let { notice -> item { DiagnosticsNoticeCard(notice) } }
            item {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = onRequestExport, enabled = state.totalEvents > 0) {
                        Text(stringResource(R.string.support_diagnostics_export))
                    }
                    OutlinedButton(onClick = onRequestClear, enabled = state.totalEvents > 0) {
                        Text(stringResource(R.string.support_diagnostics_clear))
                    }
                }
            }
            item { ExportStateCard(state.exportState) }
            item {
                Text(
                    text = stringResource(
                        R.string.support_diagnostics_preview_count,
                        state.preview.size,
                        state.totalEvents,
                    ),
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.SemiBold,
                )
            }
            if (state.preview.isEmpty()) {
                item {
                    Text(
                        text = stringResource(R.string.support_diagnostics_empty),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
            items(state.preview, key = { previewKey(it) }) { record ->
                JournalRecordRow(record)
            }
        }
    }

    state.clearConfirmation?.let { confirmation ->
        AlertDialog(
            onDismissRequest = onCancelClear,
            title = { Text(stringResource(R.string.support_diagnostics_clear_title)) },
            text = {
                Text(
                    stringResource(
                        R.string.support_diagnostics_clear_body,
                        confirmation.instanceId.value,
                        confirmation.eventCount,
                    ),
                )
            },
            confirmButton = {
                TextButton(onClick = onConfirmClear) {
                    Text(stringResource(R.string.support_confirm))
                }
            },
            dismissButton = {
                TextButton(onClick = onCancelClear) {
                    Text(stringResource(R.string.support_cancel))
                }
            },
        )
    }

    state.exportConfirmation?.let { confirmation ->
        AlertDialog(
            onDismissRequest = onCancelExport,
            title = { Text(stringResource(R.string.support_diagnostics_export_title)) },
            text = {
                Text(
                    stringResource(
                        R.string.support_diagnostics_export_body,
                        confirmation.fileName,
                        confirmation.eventCount,
                    ),
                )
            },
            confirmButton = {
                TextButton(onClick = onConfirmExport) {
                    Text(stringResource(R.string.support_confirm))
                }
            },
            dismissButton = {
                TextButton(onClick = onCancelExport) {
                    Text(stringResource(R.string.support_cancel))
                }
            },
        )
    }
}

@Composable
private fun DiagnosticsStatusCard(state: DiagnosticsUiState) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                text = stringResource(R.string.support_diagnostics_scope, state.instanceId.value),
                style = MaterialTheme.typography.titleSmall,
            )
            if (!state.coreInitialized) {
                Text(
                    text = stringResource(
                        R.string.support_diagnostics_core_unavailable,
                        state.coreFailure?.code ?: stringResource(R.string.support_unknown_reason),
                    ),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.error,
                )
            }
            when (val status = state.status) {
                JournalStatus.Healthy -> Text(
                    text = stringResource(R.string.support_diagnostics_healthy),
                    style = MaterialTheme.typography.bodyMedium,
                )
                is JournalStatus.Degraded -> {
                    Text(
                        text = stringResource(
                            R.string.support_diagnostics_degraded,
                            status.reason.name,
                            status.droppedRecords,
                        ),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.error,
                    )
                    LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
                }
            }
            if (state.tornTailDiscarded) {
                Text(
                    text = stringResource(R.string.support_diagnostics_torn_tail),
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
    }
}

@Composable
private fun DiagnosticsNoticeCard(notice: DiagnosticsNotice) {
    val message = when (notice) {
        DiagnosticsNotice.Cleared -> stringResource(R.string.support_diagnostics_cleared)
        is DiagnosticsNotice.ClearFailed -> stringResource(
            R.string.support_diagnostics_clear_failed,
            notice.failure.code,
        )
        is DiagnosticsNotice.PreviewFailed -> stringResource(
            R.string.support_diagnostics_preview_failed,
            notice.failure.code,
        )
    }
    Card(modifier = Modifier.fillMaxWidth()) {
        Text(text = message, modifier = Modifier.padding(12.dp))
    }
}

@Composable
private fun ExportStateCard(exportState: DiagnosticsExportState) {
    when (exportState) {
        DiagnosticsExportState.Idle -> Unit
        DiagnosticsExportState.Writing -> {
            LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
        }
        is DiagnosticsExportState.Completed -> Card(modifier = Modifier.fillMaxWidth()) {
            Text(
                text = stringResource(
                    R.string.support_diagnostics_export_completed,
                    exportState.fileName,
                ),
                modifier = Modifier.padding(12.dp),
            )
        }
        is DiagnosticsExportState.Failed -> Card(modifier = Modifier.fillMaxWidth()) {
            Text(
                text = stringResource(
                    R.string.support_diagnostics_export_failed,
                    exportState.failure.code,
                ),
                modifier = Modifier.padding(12.dp),
                color = MaterialTheme.colorScheme.error,
            )
        }
        is DiagnosticsExportState.Unconfirmed -> Card(modifier = Modifier.fillMaxWidth()) {
            Text(
                text = stringResource(
                    R.string.support_diagnostics_export_unconfirmed,
                    exportState.failure.code,
                ),
                modifier = Modifier.padding(12.dp),
            )
        }
        DiagnosticsExportState.Cancelled -> Card(modifier = Modifier.fillMaxWidth()) {
            Text(
                text = stringResource(R.string.support_diagnostics_export_cancelled),
                modifier = Modifier.padding(12.dp),
            )
        }
    }
}

@Composable
private fun JournalRecordRow(record: JournalRecord) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(2.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(
                    text = record.event ?: stringResource(R.string.support_diagnostics_unknown_event),
                    style = MaterialTheme.typography.titleSmall,
                )
                Text(
                    text = record.level ?: "",
                    style = MaterialTheme.typography.labelMedium,
                )
            }
            Text(
                text = stringResource(
                    R.string.support_diagnostics_stage,
                    record.stage ?: stringResource(R.string.support_unknown_reason),
                    record.code ?: stringResource(R.string.support_diagnostics_no_code),
                ),
                style = MaterialTheme.typography.bodySmall,
            )
            record.traceId?.let { trace ->
                Text(trace, style = MaterialTheme.typography.bodySmall)
            }
            if (record.fields.isNotEmpty()) {
                HorizontalDivider(modifier = Modifier.padding(vertical = 4.dp))
                for ((key, value) in record.fields) {
                    Text(
                        text = stringResource(R.string.support_diagnostics_field, key, value),
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
            }
        }
    }
}

private fun previewKey(record: JournalRecord): String =
    "${record.timestampEpochMillis}:${record.event}:${record.code}:${record.traceId}"
