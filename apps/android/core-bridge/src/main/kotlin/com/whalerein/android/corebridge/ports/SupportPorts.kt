package com.whalerein.android.corebridge.ports

import com.whalerein.android.corebridge.AndroidCore
import com.whalerein.android.corebridge.CloudAccountSessionView
import com.whalerein.android.corebridge.RetainedOperationView
import com.whalerein.android.corebridge.MobileException
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SendId
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import com.whalerein.android.state.support.AccountAction
import com.whalerein.android.state.support.AccountConnection
import com.whalerein.android.state.support.AccountPort
import com.whalerein.android.state.support.AccountStatus
import com.whalerein.android.state.support.AppSessionInfo
import com.whalerein.android.state.support.CloudAccountState
import com.whalerein.android.state.support.CoreFailureSummary
import com.whalerein.android.state.support.CoreFailureSummaryPort
import com.whalerein.android.state.support.ForgetMode
import com.whalerein.android.state.support.ForgetReport
import com.whalerein.android.state.support.OperationAuthorization
import com.whalerein.android.state.support.OperationAuthorizationState
import com.whalerein.android.state.support.OperationInventory
import com.whalerein.android.state.support.OperationRecoveryRequest
import com.whalerein.android.state.support.OperationScope
import com.whalerein.android.state.support.OperationStatus
import com.whalerein.android.state.support.OperationsPort
import com.whalerein.android.state.support.PairingHistoryEntry
import com.whalerein.android.state.support.RecoveredOperation
import com.whalerein.android.state.support.RetainedOperation
import com.whalerein.android.state.support.RetainedOperationKind
import com.whalerein.android.state.support.RetentionCertainty
import com.whalerein.android.state.support.RevocationReport
import com.whalerein.android.state.support.RevocationScope
import com.whalerein.android.state.support.SendReceipt as RecoverySendReceipt
import com.whalerein.android.state.support.SendReceiptStatus
import com.whalerein.android.state.support.SendRecoveryPort
import com.whalerein.android.state.support.SendRequest as RecoverySendRequest
import com.whalerein.android.state.support.ReplyRef
import com.whalerein.android.state.support.SignOutReport
import java.io.File
import java.security.MessageDigest

private fun RetainedOperationView.kind(): RetainedOperationKind = when (kind) {
    "send" -> RetainedOperationKind.Send
    "pairing" -> RetainedOperationKind.CloudPairing
    else -> RetainedOperationKind.Control
}

private fun RetainedOperationView.status(): OperationStatus = when (status) {
    "confirmed" -> OperationStatus.Confirmed
    "failed" -> OperationStatus.Failed
    "uncertain" -> OperationStatus.Unconfirmed
    else -> OperationStatus.Pending
}

private fun RetainedOperationView.toModel(instance: InstanceId): RetainedOperation =
    RetainedOperation(
        operationId = operationId,
        kind = kind(),
        status = status(),
        scope = OperationScope(
            instanceId = instance,
            hostId = hostId?.let(::HostId),
            sessionId = sessionId?.let(::SessionId),
        ),
        createdAtEpochMillis = 0,
        updatedAtEpochMillis = 0,
        label = kind,
        attemptId = null,
    )

internal class OperationsAdapter(private val owner: AndroidCore) : OperationsPort {
    override suspend fun inventory(instance: InstanceId): Outcome<OperationInventory> = outcomeOf {
        val operations = (owner.core().listSendOperations() + owner.core().listPendingCommands())
            .map { it.toModel(instance) }
        OperationInventory(operations = operations)
    }

    override suspend fun authorization(scope: OperationScope): Outcome<OperationAuthorization> =
        Outcome.Success(
            OperationAuthorization(
                state = if (scope.instanceId == owner.activeInstance) {
                    OperationAuthorizationState.Authorized
                } else {
                    OperationAuthorizationState.ForeignAccount
                },
            ),
        )

    override suspend fun recover(
        request: OperationRecoveryRequest,
    ): Outcome<RecoveredOperation> = outcomeOf {
        val operation = request.operation
        val view = if (operation.kind == RetainedOperationKind.Send) {
            owner.core().recoverSendWithContent(operation.operationId, true, 10_000uL)
        } else {
            owner.core().recoverPendingCommand(operation.operationId)
        }
        when (view.status) {
            "confirmed" -> RecoveredOperation.Confirmed(operation.operationId)
            "failed" -> throw MobileException.Unavailable("operation failed")
            else -> RecoveredOperation.Unconfirmed(
                operation.operationId,
                CoreFailure("uncertain", "The operation is still unconfirmed.", retryable = true),
            )
        }
    }
}

internal class AccountAdapter(private val owner: AndroidCore) : AccountPort {
    override suspend fun status(instance: InstanceId): Outcome<AccountStatus> = outcomeOf {
        val status = owner.core().cloudStatus()
        val account = status.account
        AccountStatus(
            state = when {
                account != null -> CloudAccountState.SignedIn
                status.logoutPending -> CloudAccountState.PendingRemoteSignOut
                else -> CloudAccountState.SignedOut
            },
            accountId = account?.accountId,
            workspace = account?.workspaceId,
            connection = AccountConnection.Online,
        )
    }

    override suspend fun signOut(
        instance: InstanceId,
        connection: AccountConnection,
    ): Outcome<SignOutReport> = outcomeOf {
        val remoteCleared = owner.core().cloudSignOut()
        SignOutReport(
            localSignedOut = true,
            remotePending = !remoteCleared,
            connection = connection,
        )
    }

    override suspend fun listAppSessions(instance: InstanceId): Outcome<List<AppSessionInfo>> =
        outcomeOf {
            val current = owner.core().currentCloudSessionId()
            owner.core().cloudAccountSessions().map { session ->
                AppSessionInfo(
                    sessionId = session.sessionId,
                    deviceLabel = session.instanceId,
                    createdAtEpochMillis = session.expiresAt.toLong(),
                    current = session.sessionId == current,
                )
            }
        }

    override suspend fun revokeAppSession(
        instance: InstanceId,
        sessionId: String?,
    ): Outcome<RevocationReport> = outcomeOf {
        val state = if (sessionId == null) {
            owner.core().cloudRevokeAllSessions()
        } else {
            owner.core().cloudRevokeSession(sessionId)
        }
        RevocationReport(
            scope = if (sessionId == null) RevocationScope.AllAppSessions
            else RevocationScope.AppSession(sessionId),
            retention = if (state == "revoked") RetentionCertainty.Certain
            else RetentionCertainty.Uncertain,
            retentionCode = state,
        )
    }

    override suspend fun revokeCloudHostGrant(
        instance: InstanceId,
        hostId: HostId,
    ): Outcome<RevocationReport> = outcomeOf {
        owner.core().revoke()
        RevocationReport(
            scope = RevocationScope.CloudHostGrant(hostId),
            retention = RetentionCertainty.Certain,
        )
    }

    override suspend fun revokeStandaloneDevice(
        instance: InstanceId,
        deviceId: String,
    ): Outcome<RevocationReport> = outcomeOf {
        owner.core().revoke()
        RevocationReport(
            scope = RevocationScope.StandaloneDevice(deviceId),
            retention = RetentionCertainty.Certain,
        )
    }

    override suspend fun forget(
        instance: InstanceId,
        mode: ForgetMode,
    ): Outcome<ForgetReport> = outcomeOf {
        owner.core().forgetDevice()
        ForgetReport(
            mode = mode,
            forgotten = true,
            remoteAuthorityRetained = mode == ForgetMode.LocalOnly,
        )
    }
}

internal class CoreFailureSummaryAdapter(private val owner: AndroidCore) : CoreFailureSummaryPort {
    private fun file(instance: InstanceId): File? {
        val view = owner.appConfig().instances.firstOrNull { it.id == instance.value } ?: return null
        return File(view.dataDir, "last-failure.json")
    }

    override suspend fun lastFailure(instance: InstanceId): Outcome<CoreFailureSummary?> = outcomeOf {
        val file = file(instance) ?: return@outcomeOf null
        if (!file.isFile) return@outcomeOf null
        val text = runCatching { file.readText() }.getOrNull() ?: return@outcomeOf null
        val code = Regex("\"code\"\\s*:\\s*\"([^\"]+)\"").find(text)?.groupValues?.get(1)
        val time = Regex("\"time_unix_ms\"\\s*:\\s*(\\d+)").find(text)?.groupValues?.get(1)?.toLongOrNull()
        CoreFailureSummary(code = code ?: "unknown", occurredAtEpochMillis = time ?: 0L)
    }

    override suspend fun clear(instance: InstanceId): Outcome<Unit> = outcomeOf {
        file(instance)?.delete()
        Unit
    }
}

internal class SendRecoveryAdapter(private val owner: AndroidCore) : SendRecoveryPort {
    override suspend fun submit(request: RecoverySendRequest): Outcome<RecoverySendReceipt> = outcomeOf {
        val operationId = MessageDigest.getInstance("SHA-256")
            .digest(request.sendId.value.toByteArray(Charsets.UTF_8))
            .take(16)
            .joinToString("") { "%02x".format(it) }
        val view = owner.core().sendWithOperationId(
            request.session.host.hostId.value,
            request.session.sessionId.value,
            request.text,
            com.whalerein.android.corebridge.DeliveryChoice.QUEUE,
            operationId,
        )
        RecoverySendReceipt(
            sendId = request.sendId,
            status = if (view.receipt == null) SendReceiptStatus.Unconfirmed
            else SendReceiptStatus.Confirmed,
        )
    }

    override suspend fun readReply(ref: ReplyRef): Outcome<String> = outcomeOf {
        val operationId = MessageDigest.getInstance("SHA-256")
            .digest(ref.sendId.value.toByteArray(Charsets.UTF_8))
            .take(16)
            .joinToString("") { "%02x".format(it) }
        val view = owner.core().recoverSendWithContent(operationId, true, 5_000uL)
        val message = view.content ?: throw MobileException.NotFound("reply not readable")
        message.blocks.filter { it.kind == "text" }.joinToString("\n") { it.text }
    }
}
