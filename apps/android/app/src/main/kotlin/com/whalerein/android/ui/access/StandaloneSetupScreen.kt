package com.whalerein.android.ui.access

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.whalerein.android.R
import com.whalerein.android.state.access.CaImportState
import com.whalerein.android.state.access.LanPermissionState
import com.whalerein.android.state.access.StandalonePhase
import com.whalerein.android.state.access.StandaloneSetupUiState
import com.whalerein.android.state.core.Route

@Composable
fun StandaloneSetupScreen(
    state: StandaloneSetupUiState,
    onLoadInvitation: (String) -> Unit,
    onCheckFingerprint: (String) -> Unit,
    onImportCa: (String) -> Unit,
    onRequestLanPermission: () -> Unit,
    onValidateTarget: (String) -> Unit,
    onAccept: () -> Unit,
    onReject: () -> Unit,
    onNavigate: (Route) -> Unit,
    modifier: Modifier = Modifier,
) {
    var invitationText by remember { mutableStateOf("") }
    var fingerprintText by remember { mutableStateOf("") }
    var caText by remember { mutableStateOf("") }
    var targetText by remember { mutableStateOf("") }

    AccessScaffold(
        title = stringResource(R.string.access_standalone_title),
        modifier = modifier,
    ) {
        Text(
            text = stringResource(R.string.access_standalone_body),
            style = MaterialTheme.typography.bodyLarge,
        )
        OutlinedTextField(
            value = invitationText,
            onValueChange = { invitationText = it },
            label = { Text(text = stringResource(R.string.access_invitation_input)) },
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedButton(
            onClick = { onLoadInvitation(invitationText) },
            enabled = invitationText.isNotBlank(),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(text = stringResource(R.string.access_invitation_load))
        }

        state.invitation?.let { invitation ->
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(
                    modifier = Modifier.padding(16.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Text(
                        text = "${stringResource(R.string.access_invitation_host)}: " +
                            invitation.hostLabel,
                        style = MaterialTheme.typography.titleMedium,
                    )
                    Text(
                        text = "${stringResource(R.string.access_invitation_fingerprint)}: " +
                            invitation.fingerprint,
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    Text(
                        text = if (invitation.caRequired) {
                            stringResource(R.string.access_ca_required)
                        } else {
                            stringResource(R.string.access_ca_not_required)
                        },
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
            OutlinedTextField(
                value = fingerprintText,
                onValueChange = { fingerprintText = it },
                label = { Text(text = stringResource(R.string.access_fingerprint_input)) },
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedButton(
                onClick = { onCheckFingerprint(fingerprintText) },
                enabled = fingerprintText.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_fingerprint_check))
            }
            state.fingerprintMatches?.let { matches ->
                Text(
                    text = if (matches) {
                        stringResource(R.string.access_fingerprint_match)
                    } else {
                        stringResource(R.string.access_fingerprint_mismatch)
                    },
                    color = if (matches) {
                        MaterialTheme.colorScheme.primary
                    } else {
                        MaterialTheme.colorScheme.error
                    },
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            if (invitation.caRequired) {
                OutlinedTextField(
                    value = caText,
                    onValueChange = { caText = it },
                    label = { Text(text = stringResource(R.string.access_ca_input)) },
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedButton(
                    onClick = { onImportCa(caText) },
                    enabled = caText.isNotBlank(),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(text = stringResource(R.string.access_ca_import))
                }
            }
        }

        Text(
            text = caLabel(state.ca),
            style = MaterialTheme.typography.bodyMedium,
        )
        Text(
            text = lanLabel(state.lan),
            style = MaterialTheme.typography.bodyMedium,
        )
        if (state.lan != LanPermissionState.NotRequired &&
            state.lan != LanPermissionState.Granted
        ) {
            OutlinedButton(
                onClick = onRequestLanPermission,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_lan_request))
            }
        }
        OutlinedTextField(
            value = targetText,
            onValueChange = { targetText = it },
            label = { Text(text = stringResource(R.string.access_target_input)) },
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedButton(
            onClick = { onValidateTarget(targetText) },
            enabled = targetText.isNotBlank(),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(text = stringResource(R.string.access_target_validate))
        }
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = onAccept,
                enabled = state.phase != StandalonePhase.Pairing &&
                    state.fingerprintMatches == true,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_accept))
            }
            OutlinedButton(
                onClick = onReject,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_reject))
            }
            if (state.phase == StandalonePhase.Paired) {
                state.destination?.let { destination ->
                    Button(
                        onClick = { onNavigate(destination) },
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(text = stringResource(R.string.access_continue))
                    }
                }
            }
        }
        state.error?.let { failure ->
            Text(
                text = failure.message,
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}

@Composable
private fun caLabel(state: CaImportState): String = when (state) {
    CaImportState.NotRequired -> stringResource(R.string.access_ca_not_required)
    CaImportState.AwaitingSelection -> stringResource(R.string.access_ca_input)
    is CaImportState.Ready -> stringResource(R.string.access_ca_ready)
    is CaImportState.Rejected -> stringResource(R.string.access_ca_rejected)
}

@Composable
private fun lanLabel(state: LanPermissionState): String = when (state) {
    LanPermissionState.Unknown -> stringResource(R.string.access_lan_unknown)
    LanPermissionState.Granted -> stringResource(R.string.access_lan_granted)
    LanPermissionState.Denied -> stringResource(R.string.access_lan_denied)
    LanPermissionState.Revoked -> stringResource(R.string.access_lan_revoked)
    LanPermissionState.NotRequired -> stringResource(R.string.access_lan_not_required)
}
