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
import com.whalerein.android.state.access.DirectoryHostView
import com.whalerein.android.state.access.DirectoryUiState
import com.whalerein.android.state.access.HostAvailability
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.Route

@Composable
fun DirectoryScreen(
    state: DirectoryUiState,
    onConnect: (HostId) -> Unit,
    onSelectDefault: (HostId) -> Unit,
    onNavigate: (Route) -> Unit,
    modifier: Modifier = Modifier,
) {
    AccessScaffold(
        title = stringResource(R.string.access_directory_title),
        modifier = modifier,
    ) {
        Text(
            text = stringResource(R.string.access_directory_body),
            style = MaterialTheme.typography.bodyLarge,
        )
        if (state.loading) {
            CircularProgressIndicator()
        }
        if (state.needsSignIn && !state.loading) {
            Text(
                text = stringResource(R.string.access_directory_sign_in_body),
                style = MaterialTheme.typography.bodyMedium,
            )
            Button(
                onClick = { onNavigate(Route.SignIn) },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_directory_sign_in))
            }
        } else if (state.hosts.isEmpty() && !state.loading) {
            Text(
                text = stringResource(R.string.access_directory_empty),
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            for (host in state.hosts) {
                HostCard(
                    host = host,
                    isCurrent = state.activeHost?.hostId == host.hostId,
                    isDefault = state.defaultHostId == host.hostId,
                    onConnect = onConnect,
                    onSelectDefault = onSelectDefault,
                )
            }
        }
        state.destination?.let { destination ->
            Button(
                onClick = { onNavigate(destination) },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_continue))
            }
        }
        // The signed-out path lands here, not on Welcome, so Standalone setup
        // and the support surfaces must be reachable from this screen.
        Text(
            text = stringResource(R.string.access_directory_more),
            style = MaterialTheme.typography.titleMedium,
        )
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(
                onClick = { onNavigate(Route.StandaloneSetup) },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_directory_standalone_entry))
            }
            OutlinedButton(
                onClick = { onNavigate(Route.Account) },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_directory_account_entry))
            }
            OutlinedButton(
                onClick = { onNavigate(Route.Operations) },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_directory_operations_entry))
            }
            OutlinedButton(
                onClick = { onNavigate(Route.Diagnostics) },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = stringResource(R.string.access_directory_diagnostics_entry))
            }
        }
        state.error?.let { failure ->
            Text(
                text = failure.message,
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        if (state.deniedHostId != null) {
            Text(
                text = stringResource(R.string.access_directory_denied),
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}

@Composable
private fun HostCard(
    host: DirectoryHostView,
    isCurrent: Boolean,
    isDefault: Boolean,
    onConnect: (HostId) -> Unit,
    onSelectDefault: (HostId) -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(text = host.label, style = MaterialTheme.typography.titleMedium)
                if (isCurrent) {
                    Text(
                        text = stringResource(R.string.access_directory_current),
                        style = MaterialTheme.typography.labelMedium,
                    )
                }
                if (isDefault) {
                    Text(
                        text = stringResource(R.string.access_directory_default),
                        style = MaterialTheme.typography.labelMedium,
                    )
                }
            }
            Text(
                text = availabilityLabel(host.availability),
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(
                text = if (host.online) {
                    stringResource(R.string.access_directory_online)
                } else {
                    stringResource(R.string.access_directory_offline)
                },
                style = MaterialTheme.typography.bodyMedium,
            )
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { onConnect(host.hostId) },
                    enabled = host.connectable,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(text = stringResource(R.string.access_directory_connect))
                }
                OutlinedButton(
                    onClick = { onSelectDefault(host.hostId) },
                    enabled = host.connectable && host.availability == HostAvailability.Ready,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(text = stringResource(R.string.access_directory_set_default))
                }
            }
        }
    }
}

@Composable
private fun availabilityLabel(availability: HostAvailability): String = when (availability) {
    HostAvailability.Ready -> stringResource(R.string.access_directory_ready)
    HostAvailability.NoGrant -> stringResource(R.string.access_directory_no_grant)
    HostAvailability.Offline -> stringResource(R.string.access_directory_offline)
    HostAvailability.AuthorizationUnavailable ->
        stringResource(R.string.access_directory_authorization_unavailable)
    HostAvailability.Unknown -> stringResource(R.string.access_directory_unknown)
}
