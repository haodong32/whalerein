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
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.support.AccountConfirmation
import com.whalerein.android.state.support.AccountConnection
import com.whalerein.android.state.support.AccountNotice
import com.whalerein.android.state.support.AccountScopeKind
import com.whalerein.android.state.support.AccountUiState
import com.whalerein.android.state.support.AppSessionInfo
import com.whalerein.android.state.support.CloudAccountState
import com.whalerein.android.state.support.ForgetMode

/**
 * Stateless account and device screen. Destructive callbacks only request a
 * confirmation; the ViewModel applies them after the user confirms the scope.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AccountScreen(
    state: AccountUiState,
    cloudHostGrants: List<HostId>,
    standaloneDevices: List<String>,
    onSignOut: () -> Unit,
    onRevokeSession: (String) -> Unit,
    onRevokeAllSessions: () -> Unit,
    onRevokeCloudGrant: (HostId) -> Unit,
    onRevokeStandaloneDevice: (String) -> Unit,
    onRequestForget: (ForgetMode) -> Unit,
    onConfirm: () -> Unit,
    onCancelConfirmation: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.support_account_title)) },
                navigationIcon = {
                    TextButton(onClick = onBack) {
                        Text(stringResource(R.string.support_back))
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
            item { AccountStatusCard(state) }
            state.notice?.let { notice -> item { AccountNoticeCard(notice) } }
            item {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = onSignOut) {
                        Text(stringResource(R.string.support_account_sign_out))
                    }
                    OutlinedButton(onClick = { onRequestForget(ForgetMode.Ordinary) }) {
                        Text(stringResource(R.string.support_account_forget))
                    }
                    TextButton(onClick = { onRequestForget(ForgetMode.LocalOnly) }) {
                        Text(stringResource(R.string.support_account_forget_local_only))
                    }
                }
            }
            item {
                Text(
                    text = stringResource(R.string.support_account_local_only_hint),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            item { SectionTitle(stringResource(R.string.support_account_app_sessions)) }
            if (state.appSessions.isEmpty()) {
                item { MutedText(stringResource(R.string.support_account_no_sessions)) }
            }
            items(state.appSessions, key = { it.sessionId }) { session ->
                AppSessionRow(session = session, onRevoke = onRevokeSession)
            }
            item {
                OutlinedButton(
                    onClick = onRevokeAllSessions,
                    enabled = state.appSessions.isNotEmpty(),
                ) {
                    Text(stringResource(R.string.support_account_revoke_all))
                }
            }
            item { SectionTitle(stringResource(R.string.support_account_cloud_grants)) }
            if (cloudHostGrants.isEmpty()) {
                item { MutedText(stringResource(R.string.support_account_no_grants)) }
            }
            items(cloudHostGrants) { host ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    Text(host.value, style = MaterialTheme.typography.bodyMedium)
                    TextButton(onClick = { onRevokeCloudGrant(host) }) {
                        Text(stringResource(R.string.support_account_revoke))
                    }
                }
            }
            item { SectionTitle(stringResource(R.string.support_account_standalone_devices)) }
            if (standaloneDevices.isEmpty()) {
                item { MutedText(stringResource(R.string.support_account_no_devices)) }
            }
            items(standaloneDevices) { device ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    Text(device, style = MaterialTheme.typography.bodyMedium)
                    TextButton(onClick = { onRevokeStandaloneDevice(device) }) {
                        Text(stringResource(R.string.support_account_revoke))
                    }
                }
            }
        }
    }

    state.confirmation?.let { confirmation ->
        ConfirmationDialog(
            confirmation = confirmation,
            onConfirm = onConfirm,
            onDismiss = onCancelConfirmation,
        )
    }
}

@Composable
private fun AccountStatusCard(state: AccountUiState) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(12.dp)) {
            Text(
                text = stringResource(accountStateLabel(state.status?.state)),
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            state.status?.accountId?.let { account ->
                Text(account, style = MaterialTheme.typography.bodyMedium)
            }
            state.status?.workspace?.let { workspace ->
                Text(
                    text = stringResource(R.string.support_account_workspace, workspace),
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            Text(
                text = stringResource(
                    when (state.status?.connection ?: AccountConnection.Online) {
                        AccountConnection.Online -> R.string.support_connection_online
                        AccountConnection.Offline -> R.string.support_connection_offline
                    },
                ),
                style = MaterialTheme.typography.bodySmall,
            )
        }
    }
}

@Composable
private fun AppSessionRow(session: AppSessionInfo, onRevoke: (String) -> Unit) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(12.dp)) {
            Text(session.deviceLabel, style = MaterialTheme.typography.titleSmall)
            if (session.current) {
                Text(
                    text = stringResource(R.string.support_account_current_session),
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp))
            Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                TextButton(onClick = { onRevoke(session.sessionId) }) {
                    Text(stringResource(R.string.support_account_revoke))
                }
            }
        }
    }
}

@Composable
private fun AccountNoticeCard(notice: AccountNotice) {
    val message = when (notice) {
        is AccountNotice.LoadFailed -> stringResource(
            R.string.support_account_load_failed,
            notice.failure.code,
        )
        is AccountNotice.SessionsFailed -> stringResource(
            R.string.support_account_sessions_failed,
            notice.failure.code,
        )
        AccountNotice.ActionCancelled -> stringResource(R.string.support_account_action_cancelled)
        is AccountNotice.SignOutPendingRemote -> stringResource(
            R.string.support_account_sign_out_pending,
        )
        is AccountNotice.RevocationRetentionUncertain -> stringResource(
            R.string.support_account_retention_uncertain,
            notice.code ?: stringResource(R.string.support_unknown_reason),
        )
        is AccountNotice.ForgottenLocalOnly -> stringResource(
            R.string.support_account_forgot_local_only,
        )
        is AccountNotice.ActionFailed -> stringResource(
            R.string.support_account_action_failed,
            notice.failure.code,
        )
        is AccountNotice.ActionUnconfirmed -> stringResource(
            R.string.support_account_action_unconfirmed,
            notice.failure.code,
        )
    }
    Card(modifier = Modifier.fillMaxWidth()) {
        Text(text = message, modifier = Modifier.padding(12.dp))
    }
}

@Composable
private fun ConfirmationDialog(
    confirmation: AccountConfirmation,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    val scopeText = stringResource(scopeDescription(confirmation.scopeKind))
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.support_account_confirm_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(stringResource(R.string.support_account_confirm_scope, scopeText))
                confirmation.targetLabel?.let { target ->
                    Text(stringResource(R.string.support_account_confirm_target, target))
                }
                if (confirmation.scopeKind == AccountScopeKind.LocalIdentityWithRemoteAuthority) {
                    Text(stringResource(R.string.support_account_confirm_remote_effect))
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text(stringResource(R.string.support_confirm))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(stringResource(R.string.support_cancel))
            }
        },
    )
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
private fun MutedText(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

private fun accountStateLabel(state: CloudAccountState?): Int = when (state) {
    CloudAccountState.SignedIn -> R.string.support_account_signed_in
    CloudAccountState.SignedOut, null -> R.string.support_account_signed_out
    CloudAccountState.PendingRemoteSignOut -> R.string.support_account_pending_sign_out
}

private fun scopeDescription(scope: AccountScopeKind): Int = when (scope) {
    AccountScopeKind.Account -> R.string.support_scope_account
    AccountScopeKind.AppSession -> R.string.support_scope_app_session
    AccountScopeKind.AllAppSessions -> R.string.support_scope_all_app_sessions
    AccountScopeKind.CloudHostGrant -> R.string.support_scope_cloud_grant
    AccountScopeKind.StandaloneDevice -> R.string.support_scope_standalone_device
    AccountScopeKind.LocalIdentity -> R.string.support_scope_local_identity
    AccountScopeKind.LocalIdentityWithRemoteAuthority ->
        R.string.support_scope_local_identity_remote
}
