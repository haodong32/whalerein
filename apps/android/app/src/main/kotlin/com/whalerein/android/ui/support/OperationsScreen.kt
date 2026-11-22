package com.whalerein.android.ui.support

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.whalerein.android.R
import com.whalerein.android.state.support.OperationStatus
import com.whalerein.android.state.support.OperationsNotice
import com.whalerein.android.state.support.OperationsUiState
import com.whalerein.android.state.support.PairingHistoryEntry
import com.whalerein.android.state.support.RetainedOperation
import com.whalerein.android.state.support.RetainedOperationKind

/**
 * Stateless operations screen: retained sends/controls, Cloud and Standalone
 * pairings, and pairing history. Recovery always routes through the ViewModel.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun OperationsScreen(
    state: OperationsUiState,
    onRefresh: () -> Unit,
    onRecover: (String) -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.support_operations_title)) },
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
            item {
                OperationCounts(state)
            }
            state.notice?.let { notice ->
                item { OperationsNoticeCard(notice) }
            }
            if (state.loading && state.operations.isEmpty()) {
                item {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.Center,
                    ) {
                        CircularProgressIndicator()
                    }
                }
            }
            item { SectionTitle(stringResource(R.string.support_operations_retained)) }
            if (state.operations.isEmpty() && !state.loading) {
                item { EmptyText(stringResource(R.string.support_operations_empty)) }
            }
            items(state.operations, key = { it.operationId }) { operation ->
                OperationRow(
                    operation = operation,
                    recovering = state.recoveringOperationId == operation.operationId,
                    onRecover = onRecover,
                )
            }
            item { SectionTitle(stringResource(R.string.support_operations_pairing_history)) }
            if (state.pairingHistory.isEmpty()) {
                item { EmptyText(stringResource(R.string.support_operations_history_empty)) }
            }
            items(state.pairingHistory, key = { it.pairingId }) { entry ->
                PairingHistoryRow(entry)
            }
        }
    }
}

@Composable
private fun OperationCounts(state: OperationsUiState) {
    val counts = state.counts
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        CountChip(stringResource(R.string.support_status_pending), counts.pending)
        CountChip(stringResource(R.string.support_status_confirmed), counts.confirmed)
        CountChip(stringResource(R.string.support_status_failed), counts.failed)
        CountChip(stringResource(R.string.support_status_unconfirmed), counts.unconfirmed)
    }
}

@Composable
private fun CountChip(label: String, count: Int) {
    AssistChip(
        onClick = {},
        enabled = false,
        label = { Text(stringResource(R.string.support_count_label, label, count)) },
    )
}

@Composable
private fun OperationRow(
    operation: RetainedOperation,
    recovering: Boolean,
    onRecover: (String) -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(12.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = stringResource(kindLabel(operation.kind)),
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    text = stringResource(statusLabel(operation.status)),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Text(
                text = operation.label,
                style = MaterialTheme.typography.bodyMedium,
            )
            HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp))
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.End,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (recovering) {
                    CircularProgressIndicator(modifier = Modifier.padding(end = 12.dp))
                }
                Button(
                    onClick = { onRecover(operation.operationId) },
                    enabled = !recovering,
                ) {
                    Text(stringResource(R.string.support_operations_recover))
                }
            }
        }
    }
}

@Composable
private fun PairingHistoryRow(entry: PairingHistoryEntry) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(12.dp)) {
            Text(
                text = stringResource(kindLabel(entry.kind)),
                style = MaterialTheme.typography.titleSmall,
            )
            Text(
                text = stringResource(
                    R.string.support_operations_history_detail,
                    entry.hostLabel,
                    stringResource(statusLabel(entry.outcome)),
                ),
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}

@Composable
private fun OperationsNoticeCard(notice: OperationsNotice) {
    val message = when (notice) {
        is OperationsNotice.LoadFailed -> stringResource(
            R.string.support_operations_load_failed,
            notice.failure.code,
        )
        is OperationsNotice.RecoveryRefused -> stringResource(
            R.string.support_operations_recovery_refused,
            notice.reason.name,
        )
        is OperationsNotice.RecoveryFailed -> stringResource(
            R.string.support_operations_recovery_failed,
            notice.failure.code,
        )
        is OperationsNotice.RecoveryUnconfirmed -> stringResource(
            R.string.support_operations_recovery_unconfirmed,
            notice.failure.code,
        )
        is OperationsNotice.RecoveryConfirmed -> stringResource(
            R.string.support_operations_recovery_confirmed,
        )
        is OperationsNotice.RecoveryCancelled -> stringResource(
            R.string.support_operations_recovery_cancelled,
        )
    }
    Card(modifier = Modifier.fillMaxWidth()) {
        Text(text = message, modifier = Modifier.padding(12.dp))
    }
}

@Composable
private fun SectionTitle(title: String) {
    Text(
        text = title,
        style = MaterialTheme.typography.titleMedium,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier.padding(top = 12.dp),
    )
}

@Composable
private fun EmptyText(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

private fun kindLabel(kind: RetainedOperationKind): Int = when (kind) {
    RetainedOperationKind.Send -> R.string.support_operations_kind_send
    RetainedOperationKind.Control -> R.string.support_operations_kind_control
    RetainedOperationKind.CloudPairing -> R.string.support_operations_kind_cloud_pairing
    RetainedOperationKind.StandalonePairing -> R.string.support_operations_kind_standalone_pairing
}

private fun statusLabel(status: OperationStatus): Int = when (status) {
    OperationStatus.Pending -> R.string.support_status_pending
    OperationStatus.Confirmed -> R.string.support_status_confirmed
    OperationStatus.Failed -> R.string.support_status_failed
    OperationStatus.Unconfirmed -> R.string.support_status_unconfirmed
}
