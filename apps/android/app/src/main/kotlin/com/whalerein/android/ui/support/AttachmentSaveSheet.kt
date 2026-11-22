package com.whalerein.android.ui.support

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.whalerein.android.R
import com.whalerein.android.state.attachments.AttachmentNotice
import com.whalerein.android.state.attachments.AttachmentPhase
import com.whalerein.android.state.attachments.AttachmentSaveUiState
import com.whalerein.android.state.attachments.SaveOutcome
import com.whalerein.android.state.attachments.TransferOutcome

private const val FIVE_MINUTES_MILLIS = 5L * 60L * 1000L
private const val FIFTEEN_MINUTES_MILLIS = 15L * 60L * 1000L

/**
 * Stateless attachment save sheet. Transfer and destination results are shown
 * separately; the deadline override is explicit and the save action stays
 * available only while staging and authorization allow a retry.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AttachmentSaveSheet(
    state: AttachmentSaveUiState,
    onStartTransfer: () -> Unit,
    onRetryTransfer: () -> Unit,
    onBeginSave: () -> Unit,
    onCancelTransfer: () -> Unit,
    onCancelSave: () -> Unit,
    onDeadlineOverride: (Long?) -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    ModalBottomSheet(onDismissRequest = onDismiss, modifier = modifier) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                text = stringResource(R.string.support_attachment_title),
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold,
            )
            state.descriptor?.let { descriptor ->
                Text(descriptor.displayName, style = MaterialTheme.typography.titleMedium)
                descriptor.sizeBytes?.let { size ->
                    Text(
                        text = stringResource(R.string.support_attachment_size, size),
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
            }
            Text(
                text = stringResource(phaseLabel(state.phase)),
                style = MaterialTheme.typography.bodyMedium,
            )
            if (state.phase == AttachmentPhase.Transferring || state.phase == AttachmentPhase.Saving) {
                val progress = state.progress
                if (progress == null) {
                    LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
                } else {
                    LinearProgressIndicator(
                        progress = { progress },
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
            Text(
                text = stringResource(R.string.support_attachment_deadline),
                style = MaterialTheme.typography.titleSmall,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                FilterChip(
                    selected = state.deadlineOverrideMillis == null,
                    onClick = { onDeadlineOverride(null) },
                    label = { Text(stringResource(R.string.support_attachment_deadline_default)) },
                )
                FilterChip(
                    selected = state.deadlineOverrideMillis == FIVE_MINUTES_MILLIS,
                    onClick = { onDeadlineOverride(FIVE_MINUTES_MILLIS) },
                    label = { Text(stringResource(R.string.support_attachment_deadline_five)) },
                )
                FilterChip(
                    selected = state.deadlineOverrideMillis == FIFTEEN_MINUTES_MILLIS,
                    onClick = { onDeadlineOverride(FIFTEEN_MINUTES_MILLIS) },
                    label = { Text(stringResource(R.string.support_attachment_deadline_fifteen)) },
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                when (state.phase) {
                    AttachmentPhase.Transferring -> OutlinedButton(onClick = onCancelTransfer) {
                        Text(stringResource(R.string.support_attachment_cancel_transfer))
                    }
                    AttachmentPhase.Saving -> OutlinedButton(onClick = onCancelSave) {
                        Text(stringResource(R.string.support_attachment_cancel_save))
                    }
                    AttachmentPhase.Staged -> Button(onClick = onBeginSave) {
                        Text(stringResource(R.string.support_attachment_save))
                    }
                    AttachmentPhase.Failed, AttachmentPhase.Unconfirmed,
                    AttachmentPhase.Cancelled,
                    -> if (state.stagingAvailable) {
                        Button(onClick = onBeginSave) {
                            Text(stringResource(R.string.support_attachment_retry_save))
                        }
                    } else {
                        Button(onClick = onRetryTransfer) {
                            Text(stringResource(R.string.support_attachment_download_again))
                        }
                    }
                    else -> Button(
                        onClick = onStartTransfer,
                        enabled = state.phase == AttachmentPhase.Idle ||
                            state.phase == AttachmentPhase.Unavailable,
                    ) {
                        Text(stringResource(R.string.support_attachment_download))
                    }
                }
            }
            TransferResult(state.transfer)
            SaveResult(state.save)
            state.notice?.let { notice -> AttachmentNoticeCard(notice) }
        }
    }
}

@Composable
private fun TransferResult(outcome: TransferOutcome) {
    val text = when (outcome) {
        TransferOutcome.NotStarted -> return
        TransferOutcome.Running -> stringResource(R.string.support_attachment_transfer_running)
        is TransferOutcome.Succeeded -> stringResource(
            R.string.support_attachment_transfer_succeeded,
            outcome.sizeBytes,
        )
        is TransferOutcome.Failed -> stringResource(
            R.string.support_attachment_transfer_failed,
            outcome.failure.code,
        )
        TransferOutcome.Cancelled -> stringResource(R.string.support_attachment_transfer_cancelled)
    }
    Card(modifier = Modifier.fillMaxWidth()) {
        Text(text = text, modifier = Modifier.padding(12.dp))
    }
}

@Composable
private fun SaveResult(outcome: SaveOutcome) {
    val text = when (outcome) {
        SaveOutcome.NotStarted -> return
        SaveOutcome.Running -> stringResource(R.string.support_attachment_save_running)
        is SaveOutcome.Succeeded -> stringResource(
            R.string.support_attachment_save_succeeded,
            outcome.fileName,
        )
        is SaveOutcome.Failed -> stringResource(
            R.string.support_attachment_save_failed,
            outcome.failure.code,
        )
        is SaveOutcome.Unconfirmed -> stringResource(
            R.string.support_attachment_save_unconfirmed,
            outcome.failure.code,
        )
        SaveOutcome.Cancelled -> stringResource(R.string.support_attachment_save_cancelled)
    }
    Card(modifier = Modifier.fillMaxWidth()) {
        Text(text = text, modifier = Modifier.padding(12.dp))
    }
}

@Composable
private fun AttachmentNoticeCard(notice: AttachmentNotice) {
    val text = when (notice) {
        AttachmentNotice.InvalidDeadline ->
            stringResource(R.string.support_attachment_invalid_deadline)
        is AttachmentNotice.Unavailable -> stringResource(
            R.string.support_attachment_unavailable,
            notice.reasonCode ?: stringResource(R.string.support_unknown_reason),
        )
        is AttachmentNotice.IntegrityFailure -> stringResource(
            R.string.support_attachment_integrity_failed,
            notice.failure.code,
        )
        is AttachmentNotice.TransferDeadlineExceeded -> stringResource(
            R.string.support_attachment_deadline_exceeded,
        )
        is AttachmentNotice.ProviderFailure -> stringResource(
            R.string.support_attachment_provider_failed,
            notice.failure.code,
        )
        is AttachmentNotice.SaveUnconfirmed -> stringResource(
            R.string.support_attachment_save_unconfirmed,
            notice.failure.code,
        )
        AttachmentNotice.AuthorizationLost ->
            stringResource(R.string.support_attachment_authorization_lost)
        AttachmentNotice.InstanceChanged ->
            stringResource(R.string.support_attachment_instance_changed)
        AttachmentNotice.StagingDiscarded ->
            stringResource(R.string.support_attachment_staging_discarded)
        is AttachmentNotice.StagingFailed -> stringResource(
            R.string.support_attachment_staging_failed,
            notice.failure.code,
        )
    }
    Card(modifier = Modifier.fillMaxWidth()) {
        Text(text = text, modifier = Modifier.padding(12.dp))
    }
}

private fun phaseLabel(phase: AttachmentPhase): Int = when (phase) {
    AttachmentPhase.Idle -> R.string.support_attachment_phase_idle
    AttachmentPhase.Transferring -> R.string.support_attachment_phase_transferring
    AttachmentPhase.Staged -> R.string.support_attachment_phase_staged
    AttachmentPhase.Saving -> R.string.support_attachment_phase_saving
    AttachmentPhase.Saved -> R.string.support_attachment_phase_saved
    AttachmentPhase.Failed -> R.string.support_attachment_phase_failed
    AttachmentPhase.Unconfirmed -> R.string.support_attachment_phase_unconfirmed
    AttachmentPhase.Cancelled -> R.string.support_attachment_phase_cancelled
    AttachmentPhase.Unavailable -> R.string.support_attachment_phase_unavailable
    AttachmentPhase.Discarded -> R.string.support_attachment_phase_discarded
}
