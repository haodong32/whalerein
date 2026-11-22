package com.whalerein.android.ui.access

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.whalerein.android.R
import com.whalerein.android.state.access.WelcomeUiState
import com.whalerein.android.state.core.Route

@Composable
fun WelcomeScreen(
    state: WelcomeUiState,
    onContinue: () -> Unit,
    onNavigate: (Route) -> Unit,
    modifier: Modifier = Modifier,
) {
    AccessScaffold(
        title = stringResource(R.string.access_welcome_title),
        modifier = modifier,
    ) {
        Text(
            text = stringResource(R.string.access_welcome_body),
            style = MaterialTheme.typography.bodyLarge,
        )
        if (state.loading) {
            CircularProgressIndicator()
        }
        state.activeProfile?.let { profile ->
            Text(
                text = "${stringResource(R.string.access_profile_active)}: ${profile.label}",
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = onContinue,
                enabled = !state.loading,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(text = routeTitle(state.defaultRoute))
            }
            if (state.choices.contains(Route.StandaloneSetup)) {
                OutlinedButton(
                    onClick = { onNavigate(Route.StandaloneSetup) },
                    enabled = !state.loading,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(text = stringResource(R.string.access_standalone_setup))
                }
            }
            if (state.choices.contains(Route.Profiles)) {
                OutlinedButton(
                    onClick = { onNavigate(Route.Profiles) },
                    enabled = !state.loading,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(text = stringResource(R.string.access_profiles))
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
private fun routeTitle(route: Route): String = when (route) {
    Route.SignIn, Route.Welcome -> stringResource(R.string.access_cloud_sign_in)
    Route.Directory -> stringResource(R.string.access_directory_title)
    Route.StandaloneSetup -> stringResource(R.string.access_standalone_setup)
    Route.Profiles -> stringResource(R.string.access_profiles)
    else -> stringResource(R.string.access_continue)
}
