package com.whalerein.android.ui.access

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
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
import com.whalerein.android.state.access.CloudSignInPhase
import com.whalerein.android.state.access.CloudSignInUiState
import com.whalerein.android.state.core.Route

@Composable
fun CloudSignInScreen(
    state: CloudSignInUiState,
    onStart: () -> Unit,
    onCancel: () -> Unit,
    onRecover: () -> Unit,
    onNavigate: (Route) -> Unit,
    modifier: Modifier = Modifier,
) {
    AccessScaffold(
        title = stringResource(R.string.access_sign_in_title),
        modifier = modifier,
    ) {
        Text(
            text = stringResource(R.string.access_sign_in_body),
            style = MaterialTheme.typography.bodyLarge,
        )
        if (state.previousSession != null) {
            Card(modifier = Modifier.fillMaxWidth()) {
                Text(
                    text = stringResource(R.string.access_previous_session),
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(16.dp),
                )
            }
        }
        when (state.phase) {
            CloudSignInPhase.Starting,
            CloudSignInPhase.Completing,
            CloudSignInPhase.Recovering -> CircularProgressIndicator()

            else -> Unit
        }
        Text(
            text = phaseLabel(state.phase),
            style = MaterialTheme.typography.bodyMedium,
        )
        state.attempt?.let {
            Text(
                text = stringResource(R.string.access_sign_in_waiting),
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            when (state.phase) {
                CloudSignInPhase.Idle,
                CloudSignInPhase.Failed,
                CloudSignInPhase.Expired,
                CloudSignInPhase.Cancelled -> Button(
                    onClick = onStart,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(text = stringResource(R.string.access_sign_in_start))
                }

                CloudSignInPhase.Unconfirmed -> Button(
                    onClick = onRecover,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(text = stringResource(R.string.access_sign_in_recover))
                }

                CloudSignInPhase.Succeeded -> Button(
                    onClick = { onNavigate(Route.Directory) },
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(text = stringResource(R.string.access_sign_in_open_directory))
                }

                else -> Unit
            }
            if (state.phase == CloudSignInPhase.AwaitingBrowser ||
                state.phase == CloudSignInPhase.Unconfirmed
            ) {
                OutlinedButton(
                    onClick = onCancel,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(text = stringResource(R.string.access_sign_in_cancel))
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
private fun phaseLabel(phase: CloudSignInPhase): String = when (phase) {
    CloudSignInPhase.Idle,
    CloudSignInPhase.Failed,
    CloudSignInPhase.Cancelled -> stringResource(R.string.access_sign_in_title)

    CloudSignInPhase.Starting,
    CloudSignInPhase.AwaitingBrowser -> stringResource(R.string.access_sign_in_waiting)

    CloudSignInPhase.Completing -> stringResource(R.string.access_sign_in_completing)
    CloudSignInPhase.Recovering -> stringResource(R.string.access_sign_in_recovering)
    CloudSignInPhase.Succeeded -> stringResource(R.string.access_sign_in_succeeded)
    CloudSignInPhase.Unconfirmed -> stringResource(R.string.access_sign_in_unconfirmed)
    CloudSignInPhase.Expired -> stringResource(R.string.access_sign_in_expired)
}
