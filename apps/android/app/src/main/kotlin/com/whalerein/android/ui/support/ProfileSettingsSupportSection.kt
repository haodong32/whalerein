package com.whalerein.android.ui.support

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.whalerein.android.R
import com.whalerein.android.state.support.AccountUiState
import com.whalerein.android.state.support.CloudAccountState
import com.whalerein.android.state.support.ForgetMode

/**
 * Stateless support section embedded in profile settings. It surfaces the
 * account status plus entry points to account, operations and diagnostics, and
 * never constructs a ViewModel.
 */
@Composable
fun ProfileSettingsSupportSection(
    state: AccountUiState,
    onOpenAccount: () -> Unit,
    onOpenOperations: () -> Unit,
    onOpenDiagnostics: () -> Unit,
    onSignOut: () -> Unit,
    onRequestForget: (ForgetMode) -> Unit,
    modifier: Modifier = Modifier,
) {
    Card(modifier = modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                text = stringResource(R.string.support_settings_title),
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                text = stringResource(
                    when (state.status?.state ?: CloudAccountState.SignedOut) {
                        CloudAccountState.SignedIn -> R.string.support_account_signed_in
                        CloudAccountState.SignedOut -> R.string.support_account_signed_out
                        CloudAccountState.PendingRemoteSignOut ->
                            R.string.support_account_pending_sign_out
                    },
                ),
                style = MaterialTheme.typography.bodyMedium,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                TextButton(onClick = onOpenAccount) {
                    Text(stringResource(R.string.support_settings_open_account))
                }
                TextButton(onClick = onOpenOperations) {
                    Text(stringResource(R.string.support_settings_open_operations))
                }
                TextButton(onClick = onOpenDiagnostics) {
                    Text(stringResource(R.string.support_settings_open_diagnostics))
                }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(
                    onClick = onSignOut,
                    enabled = state.status?.state != CloudAccountState.SignedOut,
                ) {
                    Text(stringResource(R.string.support_account_sign_out))
                }
                TextButton(onClick = { onRequestForget(ForgetMode.Ordinary) }) {
                    Text(stringResource(R.string.support_account_forget))
                }
            }
        }
    }
}
