package com.whalerein.android.ui.access

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.whalerein.android.R
import com.whalerein.android.state.access.Profile
import com.whalerein.android.state.access.ProfileActivationState
import com.whalerein.android.state.access.ProfilesUiState
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.InstanceKind
import com.whalerein.android.state.core.Route

@Composable
fun ProfileSettingsScreen(
    state: ProfilesUiState,
    profile: Profile,
    onActivate: (InstanceId) -> Unit,
    onRemove: (InstanceId) -> Unit,
    onRefresh: () -> Unit,
    onNavigate: (Route) -> Unit,
    modifier: Modifier = Modifier,
) {
    val scope = state.storageScopes[profile.instance]
    AccessScaffold(
        title = stringResource(R.string.access_settings_title),
        modifier = modifier,
    ) {
        Card(modifier = Modifier.fillMaxWidth()) {
            Column(
                modifier = Modifier.padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(text = profile.label, style = MaterialTheme.typography.titleLarge)
                Text(
                    text = "${stringResource(R.string.access_settings_kind)}: " +
                        when (profile.kind) {
                            InstanceKind.Cloud -> stringResource(R.string.access_profile_cloud)
                            InstanceKind.Standalone -> stringResource(R.string.access_profile_standalone)
                        },
                    style = MaterialTheme.typography.bodyMedium,
                )
                Text(
                    text = "${stringResource(R.string.access_settings_scope)}: ${profile.instance.value}",
                    style = MaterialTheme.typography.bodyMedium,
                )
                Text(
                    text = "${stringResource(R.string.access_settings_backup)}: " +
                        if (profile.backupExcluded && scope?.backupExcluded == true) {
                            stringResource(R.string.access_profile_backup_excluded)
                        } else {
                            stringResource(R.string.access_unavailable)
                        },
                    style = MaterialTheme.typography.bodyMedium,
                )
                Text(
                    text = "${stringResource(R.string.access_settings_credential)}: " +
                        credentialLabel(state.credentials[profile.instance]),
                    style = MaterialTheme.typography.bodyMedium,
                )
                Text(
                    text = "${stringResource(R.string.access_profile_generation)}: ${state.generation}",
                    style = MaterialTheme.typography.bodyMedium,
                )
                if (profile.active) {
                    Text(
                        text = stringResource(R.string.access_profile_active),
                        style = MaterialTheme.typography.labelLarge,
                    )
                }
            }
        }
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = { onActivate(profile.instance) },
                enabled = !profile.active &&
                    state.activation !is ProfileActivationState.Activating,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_activate))
            }
            OutlinedButton(
                onClick = onRefresh,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_refresh))
            }
            OutlinedButton(
                onClick = { onRemove(profile.instance) },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_profile_remove))
            }
            OutlinedButton(
                onClick = { onNavigate(Route.Profiles) },
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
