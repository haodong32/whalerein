package com.whalerein.android.ui.access

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.whalerein.android.R
import com.whalerein.android.state.access.CredentialState
import com.whalerein.android.state.access.Profile
import com.whalerein.android.state.access.ProfileActivationState
import com.whalerein.android.state.access.ProfilesUiState
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.InstanceKind
import com.whalerein.android.state.core.Route

@Composable
fun ProfilesScreen(
    state: ProfilesUiState,
    onActivate: (InstanceId) -> Unit,
    onRemove: (InstanceId) -> Unit,
    onEdit: (Profile) -> Unit,
    onRefresh: () -> Unit,
    onNavigate: (Route) -> Unit,
    modifier: Modifier = Modifier,
) {
    AccessScaffold(
        title = stringResource(R.string.access_profiles_title),
        modifier = modifier,
    ) {
        Text(
            text = stringResource(R.string.access_profiles_body),
            style = MaterialTheme.typography.bodyLarge,
        )
        if (state.loading) {
            CircularProgressIndicator()
        }
        if (state.profiles.isEmpty() && !state.loading) {
            Text(
                text = stringResource(R.string.access_profiles_empty),
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            for (profile in state.profiles) {
                ProfileCard(
                    profile = profile,
                    credential = state.credentials[profile.instance],
                    isActivating = (state.activation as? ProfileActivationState.Activating)
                        ?.instance == profile.instance,
                    onActivate = onActivate,
                    onRemove = onRemove,
                    onEdit = onEdit,
                )
            }
        }
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(
                onClick = onRefresh,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_refresh))
            }
            OutlinedButton(
                onClick = { onNavigate(Route.Welcome) },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_back))
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
private fun ProfileCard(
    profile: Profile,
    credential: CredentialState?,
    isActivating: Boolean,
    onActivate: (InstanceId) -> Unit,
    onRemove: (InstanceId) -> Unit,
    onEdit: (Profile) -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(text = profile.label, style = MaterialTheme.typography.titleMedium)
                if (profile.active) {
                    Text(
                        text = stringResource(R.string.access_profile_active),
                        style = MaterialTheme.typography.labelMedium,
                    )
                }
            }
            Text(
                text = when (profile.kind) {
                    InstanceKind.Cloud -> stringResource(R.string.access_profile_cloud)
                    InstanceKind.Standalone -> stringResource(R.string.access_profile_standalone)
                },
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(
                text = credentialLabel(credential),
                style = MaterialTheme.typography.bodyMedium,
            )
            if (profile.backupExcluded) {
                Text(
                    text = stringResource(R.string.access_profile_backup_excluded),
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { onActivate(profile.instance) },
                    enabled = !profile.active && !isActivating,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(text = stringResource(R.string.access_profile_activate))
                }
                OutlinedButton(
                    onClick = { onEdit(profile) },
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(text = stringResource(R.string.access_profile_edit))
                }
                OutlinedButton(
                    onClick = { onRemove(profile.instance) },
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(text = stringResource(R.string.access_profile_remove))
                }
            }
        }
    }
}

@Composable
internal fun credentialLabel(credential: CredentialState?): String = when (credential) {
    null -> stringResource(R.string.access_credential_present)
    CredentialState.Present -> stringResource(R.string.access_credential_present)
    CredentialState.Absent -> stringResource(R.string.access_credential_absent)
    CredentialState.Unavailable -> stringResource(R.string.access_credential_unavailable)
    is CredentialState.Invalid -> stringResource(R.string.access_credential_invalid)
}
