package com.whalerein.android.corebridge.ports

import com.whalerein.android.corebridge.AndroidCore
import com.whalerein.android.corebridge.AttachmentDownloadRequestView
import com.whalerein.android.corebridge.AttachmentReferenceView
import com.whalerein.android.corebridge.SessionKeyView
import com.whalerein.android.state.attachments.AttachmentAuthorization
import com.whalerein.android.state.attachments.AttachmentAuthorizationPort
import com.whalerein.android.state.attachments.AttachmentDescriptor
import com.whalerein.android.state.attachments.AttachmentFetchRequest
import com.whalerein.android.state.attachments.AttachmentPort
import com.whalerein.android.state.attachments.AttachmentScope
import com.whalerein.android.state.attachments.AttachmentStaging
import com.whalerein.android.state.attachments.StagingHandle
import com.whalerein.android.state.attachments.VerifiedAttachment
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import java.io.File
import java.security.MessageDigest
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

internal class AttachmentAdapter(private val owner: AndroidCore) : AttachmentPort {
    override suspend fun describe(scope: AttachmentScope): Outcome<AttachmentDescriptor> = outcomeOf {
        val descriptor = findDescriptor(scope)
            ?: throw com.whalerein.android.corebridge.MobileException.NotFound("attachment not found")
        descriptor
    }

    private fun findDescriptor(scope: AttachmentScope): AttachmentDescriptor? {
        val messages = owner.core().cachedMessages(
            scope.session.host.hostId.value,
            scope.session.sessionId.value,
        )
        for (message in messages) {
            for (block in message.blocks) {
                val attachment = block.attachment ?: continue
                if (attachment.attachmentRef != scope.attachmentId) continue
                if (!attachment.available) continue
                return AttachmentDescriptor(
                    attachmentId = attachment.attachmentRef,
                    displayName = attachment.name,
                    mediaType = attachment.mediaType,
                    sizeBytes = attachment.size.toLong(),
                    available = attachment.available,
                )
            }
        }
        return null
    }

    override suspend fun fetch(request: AttachmentFetchRequest): Outcome<VerifiedAttachment> = outcomeOf {
        val scope = request.scope
        val descriptor = findDescriptor(scope)
            ?: throw com.whalerein.android.corebridge.MobileException.NotFound("attachment not found")
        val directory = File(
            owner.context.filesDir,
            "attachments/${scope.instanceId.value}/${scope.session.host.hostId.value}",
        ).apply { mkdirs() }
        val destination = File(directory, UUID.randomUUID().toString())
        val requestView = AttachmentDownloadRequestView(
            reference = AttachmentReferenceView(
                session = SessionKeyView(
                    scope.session.host.hostId.value,
                    scope.session.sessionId.value,
                ),
                attachmentRef = scope.attachmentId,
            ),
            destination = destination.absolutePath,
            fileName = descriptor.displayName,
            overwrite = false,
        )
        val downloadId = owner.core().beginAttachmentDownload(requestView)
        val deadline = minOf(
            request.deadlineAtEpochMillis,
            System.currentTimeMillis() + 120_000,
        )
        var terminal = owner.core().takeAttachmentResult(downloadId, 500UL)
        while (terminal.state == "running" && System.currentTimeMillis() < deadline) {
            terminal = owner.core().takeAttachmentResult(downloadId, 500UL)
        }
        val download = terminal.download
        if (terminal.state != "succeeded" || download == null || !download.verified) {
            destination.delete()
            throw com.whalerein.android.corebridge.MobileException.Unavailable(
                terminal.errorCode ?: "attachment_transfer_failed",
            )
        }
        val file = File(download.path)
        val bytes = file.readBytes()
        val digest = MessageDigest.getInstance("SHA-256").digest(bytes)
            .joinToString("") { "%02x".format(it) }
        file.delete()
        if (digest != download.sha256.lowercase()) {
            throw com.whalerein.android.corebridge.MobileException.Security("attachment integrity failed")
        }
        VerifiedAttachment(
            descriptor = descriptor.copy(sizeBytes = bytes.size.toLong()),
            bytes = bytes,
            sha256 = digest,
        )
    }
}

internal class AttachmentStagingAdapter(private val owner: AndroidCore) : AttachmentStaging {
    private fun root(instance: InstanceId): File =
        File(owner.context.filesDir, "attachments/staging/${instance.value}").apply { mkdirs() }

    private fun file(handle: StagingHandle): File =
        File(root(handle.instanceId), "${handle.operationId}-${handle.displayName.replace('/', '_')}")

    override suspend fun stage(
        scope: AttachmentScope,
        operationId: String,
        descriptor: AttachmentDescriptor,
        bytes: ByteArray,
    ): Outcome<StagingHandle> = withContext(Dispatchers.IO) {
        outcomeOf {
            val handle = StagingHandle(
                instanceId = scope.instanceId,
                operationId = operationId,
                processId = owner.processId,
                token = UUID.randomUUID().toString(),
                displayName = descriptor.displayName,
                sizeBytes = bytes.size.toLong(),
            )
            file(handle).writeBytes(bytes)
            handle
        }
    }

    override suspend fun read(handle: StagingHandle): Outcome<ByteArray> = withContext(Dispatchers.IO) {
        outcomeOf {
            val file = file(handle)
            if (!file.isFile) throw com.whalerein.android.corebridge.MobileException.NotFound("staged file")
            file.readBytes()
        }
    }

    override suspend fun exists(handle: StagingHandle): Boolean = file(handle).isFile

    override suspend fun discard(handle: StagingHandle) {
        file(handle).delete()
    }

    override suspend fun discardInstance(instanceId: InstanceId): Int = withContext(Dispatchers.IO) {
        var removed = 0
        root(instanceId).listFiles()?.forEach { if (it.delete()) removed += 1 }
        removed
    }

    override suspend fun discardProcessesBefore(processId: String): Int = withContext(Dispatchers.IO) {
        var removed = 0
        File(owner.context.filesDir, "attachments/staging").listFiles()?.forEach { instance ->
            instance.listFiles()?.forEach { file ->
                if (!file.name.startsWith(processId) && file.delete()) removed += 1
            }
        }
        removed
    }
}

internal class AttachmentAuthorizationAdapter(private val owner: AndroidCore) :
    AttachmentAuthorizationPort {
    override suspend fun current(scope: AttachmentScope): Outcome<AttachmentAuthorization> = outcomeOf {
        val capabilities = owner.core().capabilities(
            scope.session.host.hostId.value,
            scope.session.sessionId.value,
        )
        AttachmentAuthorization(
            authorized = capabilities.attachments,
            reasonCode = if (capabilities.attachments) null else "capability_missing",
        )
    }
}
