package com.whalerein.android.state.fixtures.support

import com.whalerein.android.state.attachments.AttachmentAuthorization
import com.whalerein.android.state.attachments.AttachmentAuthorizationPort
import com.whalerein.android.state.attachments.AttachmentDescriptor
import com.whalerein.android.state.attachments.AttachmentFetchRequest
import com.whalerein.android.state.attachments.AttachmentPort
import com.whalerein.android.state.attachments.AttachmentScope
import com.whalerein.android.state.attachments.AttachmentStaging
import com.whalerein.android.state.attachments.DestinationNaming
import com.whalerein.android.state.attachments.DestinationWriteResult
import com.whalerein.android.state.attachments.DestinationWriter
import com.whalerein.android.state.attachments.StagingHandle
import com.whalerein.android.state.attachments.VerifiedAttachment
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome

class FakeAttachmentPort(
    var descriptor: AttachmentDescriptor = AttachmentDescriptor(
        attachmentId = "attachment-a",
        displayName = "report.pdf",
        mediaType = "application/pdf",
        sizeBytes = 11L,
    ),
    var describeOutcome: Outcome<AttachmentDescriptor>? = null,
    val fetchOutcomes: ArrayDeque<Outcome<VerifiedAttachment>> = ArrayDeque(),
) : AttachmentPort {
    val requests = mutableListOf<AttachmentFetchRequest>()
    var describeCalls: Int = 0
        private set
    var duringFetch: (() -> Unit)? = null

    override suspend fun describe(scope: AttachmentScope): Outcome<AttachmentDescriptor> {
        describeCalls += 1
        return describeOutcome ?: Outcome.Success(descriptor)
    }

    override suspend fun fetch(request: AttachmentFetchRequest): Outcome<VerifiedAttachment> {
        requests += request
        duringFetch?.invoke()
        return fetchOutcomes.removeFirstOrNull()
            ?: Outcome.Failure(CoreFailure("fetch_not_scripted", "No fetch scripted."))
    }
}

class FakeAttachmentStaging : AttachmentStaging {
    private val handles = LinkedHashMap<String, StagingHandle>()
    private val bytesByToken = LinkedHashMap<String, ByteArray>()
    private var tokenCounter = 0

    var failNextStage: Boolean = false
    var failReads: Boolean = false
    var processId: String = "process-1"
    var instanceOverride: InstanceId? = null
    var duringStage: (() -> Unit)? = null
    var stageOutcome: Outcome<StagingHandle>? = null
    var readOutcome: Outcome<ByteArray>? = null
    val allBytes: Map<String, ByteArray> get() = bytesByToken

    override suspend fun stage(
        scope: AttachmentScope,
        operationId: String,
        descriptor: AttachmentDescriptor,
        bytes: ByteArray,
    ): Outcome<StagingHandle> {
        if (failNextStage) {
            failNextStage = false
            return Outcome.Failure(CoreFailure("staging_failed", "Staging write failed."))
        }
        stageOutcome?.let { return it }
        tokenCounter += 1
        val handle = StagingHandle(
            instanceId = instanceOverride ?: scope.instanceId,
            operationId = operationId,
            processId = processId,
            token = "staging-$tokenCounter",
            displayName = descriptor.displayName,
            sizeBytes = bytes.size.toLong(),
        )
        handles[handle.token] = handle
        bytesByToken[handle.token] = bytes.copyOf()
        duringStage?.invoke()
        return Outcome.Success(handle)
    }

    override suspend fun read(handle: StagingHandle): Outcome<ByteArray> {
        if (failReads) throw IllegalStateException("staging read failed")
        readOutcome?.let { return it }
        val bytes = bytesByToken[handle.token]
            ?: return Outcome.Failure(CoreFailure("staging_lost", "Staging copy is gone."))
        return Outcome.Success(bytes.copyOf())
    }

    override suspend fun exists(handle: StagingHandle): Boolean =
        bytesByToken.containsKey(handle.token)

    override suspend fun discard(handle: StagingHandle) {
        handles.remove(handle.token)
        bytesByToken.remove(handle.token)
    }

    override suspend fun discardInstance(instanceId: InstanceId): Int {
        val matching = handles.values.filter { it.instanceId == instanceId }
        matching.forEach {
            handles.remove(it.token)
            bytesByToken.remove(it.token)
        }
        return matching.size
    }

    override suspend fun discardProcessesBefore(processId: String): Int {
        val matching = handles.values.filter { it.processId != processId }
        matching.forEach {
            handles.remove(it.token)
            bytesByToken.remove(it.token)
        }
        return matching.size
    }

    fun stagedCount(): Int = handles.size
}

class FakeAttachmentAuthorizationPort(
    var authorized: Boolean = true,
    var reasonCode: String? = null,
    var outcome: Outcome<AttachmentAuthorization>? = null,
) : AttachmentAuthorizationPort {
    var checks: Int = 0
        private set

    override suspend fun current(scope: AttachmentScope): Outcome<AttachmentAuthorization> {
        checks += 1
        return outcome ?: Outcome.Success(AttachmentAuthorization(authorized, reasonCode))
    }
}

/**
 * In-memory document destination with create-new semantics: a collision selects a
 * new name and never replaces the existing document.
 */
class FakeDestinationWriter(
    initial: Map<String, ByteArray> = emptyMap(),
    var outcome: Outcome<DestinationWriteResult>? = null,
) : DestinationWriter {
    val documents = LinkedHashMap<String, ByteArray>()
    val requests = mutableListOf<Pair<String, ByteArray>>()
    var duringWrite: (() -> Unit)? = null

    init {
        documents.putAll(initial)
    }

    override suspend fun write(
        requestedName: String,
        bytes: ByteArray,
    ): Outcome<DestinationWriteResult> {
        requests += requestedName to bytes.copyOf()
        duringWrite?.invoke()
        outcome?.let { return it }
        val fileName = DestinationNaming.uniqueName(requestedName, documents.keys)
        documents[fileName] = bytes.copyOf()
        return Outcome.Success(DestinationWriteResult.Completed(fileName))
    }

    fun names(): Set<String> = documents.keys.toSet()
}
