package com.whalerein.android.state.attachments

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionRef

data class AttachmentScope(
    val instanceId: InstanceId,
    val session: SessionRef,
    val attachmentId: String,
)

data class AttachmentDescriptor(
    val attachmentId: String,
    val displayName: String,
    val mediaType: String? = null,
    val sizeBytes: Long? = null,
    val available: Boolean = true,
    /** Redaction-safe reason when [available] is false. */
    val unavailableReason: String? = null,
)

data class AttachmentLimits(
    val defaultDeadlineMillis: Long = DEFAULT_DEADLINE_MILLIS,
    val maxOverrideMillis: Long = MAX_OVERRIDE_MILLIS,
) {
    /** Returns null when [overrideMillis] is not a valid positive override. */
    fun effectiveDeadlineMillis(overrideMillis: Long?): Long? = when {
        overrideMillis == null -> defaultDeadlineMillis
        overrideMillis <= 0L -> null
        overrideMillis > maxOverrideMillis -> null
        else -> overrideMillis
    }

    companion object {
        const val DEFAULT_DEADLINE_MILLIS: Long = 120_000L
        const val MAX_OVERRIDE_MILLIS: Long = 15L * 60L * 1000L
    }
}

data class AttachmentFetchRequest(
    val scope: AttachmentScope,
    val deadlineAtEpochMillis: Long,
)

/** Bytes returned only after Core verified the complete transfer. */
data class VerifiedAttachment(
    val descriptor: AttachmentDescriptor,
    val bytes: ByteArray,
    val sha256: String,
)

/**
 * Core attachment port. [fetch] applies Core's total transfer deadline, which
 * includes verification; it never returns unverified bytes.
 */
interface AttachmentPort {
    suspend fun describe(scope: AttachmentScope): Outcome<AttachmentDescriptor>

    suspend fun fetch(request: AttachmentFetchRequest): Outcome<VerifiedAttachment>
}

data class StagingHandle(
    val instanceId: InstanceId,
    val operationId: String,
    val processId: String,
    val token: String,
    val displayName: String,
    val sizeBytes: Long,
)

/**
 * Private, instance-scoped staging.
 *
 * [discardProcessesBefore] removes handles left by earlier processes at startup.
 * [discardInstance] removes every handle for an instance on instance switch or
 * authorization loss. Completed user-owned destination files are never touched.
 */
interface AttachmentStaging {
    suspend fun stage(
        scope: AttachmentScope,
        operationId: String,
        descriptor: AttachmentDescriptor,
        bytes: ByteArray,
    ): Outcome<StagingHandle>

    suspend fun read(handle: StagingHandle): Outcome<ByteArray>

    suspend fun exists(handle: StagingHandle): Boolean

    suspend fun discard(handle: StagingHandle)

    suspend fun discardInstance(instanceId: InstanceId): Int

    suspend fun discardProcessesBefore(processId: String): Int
}

data class AttachmentAuthorization(
    val authorized: Boolean,
    /** Redaction-safe reason when [authorized] is false. */
    val reasonCode: String? = null,
)

/** Re-checked before saving and before a current-process retry. */
interface AttachmentAuthorizationPort {
    suspend fun current(scope: AttachmentScope): Outcome<AttachmentAuthorization>
}

sealed interface DestinationWriteResult {
    data class Completed(val fileName: String) : DestinationWriteResult

    /** The provider may have accepted bytes but completion is inconclusive. */
    data class Unconfirmed(val failure: CoreFailure) : DestinationWriteResult
}

/** Android document-picker writer. Implementations create a new document. */
interface DestinationWriter {
    suspend fun write(requestedName: String, bytes: ByteArray): Outcome<DestinationWriteResult>
}

/**
 * Create-new naming that preserves an existing document. A collision produces a
 * separate copy instead of replacing the original, matching the accepted D-13
 * difference from CLI overwrite.
 */
object DestinationNaming {
    /** Strips any path portion and unsafe characters from a suggested name. */
    fun safeFileName(requested: String): String {
        val name = requested.substringAfterLast('/').substringAfterLast('\\').trim()
        val cleaned = name.map { character ->
            if (character.code < 0x20 || character in INVALID_NAME_CHARACTERS) '_' else character
        }.joinToString("")
        return cleaned.trim('.').ifEmpty { DEFAULT_NAME }
    }

    fun uniqueName(requested: String, existingNames: Collection<String>): String {
        val baseName = safeFileName(requested)
        val existing = existingNames.map { it.lowercase() }.toSet()
        if (baseName.lowercase() !in existing) return baseName
        val dot = baseName.lastIndexOf('.')
        val stem = if (dot > 0) baseName.substring(0, dot) else baseName
        val extension = if (dot > 0) baseName.substring(dot) else ""
        var index = 1
        while (true) {
            val candidate = "$stem ($index)$extension"
            if (candidate.lowercase() !in existing) return candidate
            index += 1
        }
    }

    private const val DEFAULT_NAME = "attachment"
    private val INVALID_NAME_CHARACTERS = setOf(':', '*', '?', '"', '<', '>', '|', '\u0000')
}

enum class AttachmentPhase {
    Idle,
    Transferring,
    Staged,
    Saving,
    Saved,
    Failed,
    Unconfirmed,
    Cancelled,
    Unavailable,
    Discarded,
}

sealed interface TransferOutcome {
    data object NotStarted : TransferOutcome

    data object Running : TransferOutcome

    data class Succeeded(val sizeBytes: Long) : TransferOutcome

    data class Failed(val failure: CoreFailure) : TransferOutcome

    data object Cancelled : TransferOutcome
}

sealed interface SaveOutcome {
    data object NotStarted : SaveOutcome

    data object Running : SaveOutcome

    data class Succeeded(val fileName: String) : SaveOutcome

    data class Failed(val failure: CoreFailure) : SaveOutcome

    data class Unconfirmed(val failure: CoreFailure) : SaveOutcome

    data object Cancelled : SaveOutcome
}

sealed interface AttachmentNotice {
    data object InvalidDeadline : AttachmentNotice

    data class Unavailable(val reasonCode: String?) : AttachmentNotice

    data class IntegrityFailure(val failure: CoreFailure) : AttachmentNotice

    data class TransferDeadlineExceeded(val failure: CoreFailure) : AttachmentNotice

    data class ProviderFailure(val failure: CoreFailure) : AttachmentNotice

    data class SaveUnconfirmed(val failure: CoreFailure) : AttachmentNotice

    data object AuthorizationLost : AttachmentNotice

    data object InstanceChanged : AttachmentNotice

    data object StagingDiscarded : AttachmentNotice

    data class StagingFailed(val failure: CoreFailure) : AttachmentNotice
}

data class AttachmentSaveUiState(
    val scope: AttachmentScope,
    val phase: AttachmentPhase = AttachmentPhase.Idle,
    val descriptor: AttachmentDescriptor? = null,
    val transfer: TransferOutcome = TransferOutcome.NotStarted,
    val save: SaveOutcome = SaveOutcome.NotStarted,
    val progress: Float? = null,
    val deadlineOverrideMillis: Long? = null,
    val deadlineAtEpochMillis: Long? = null,
    val stagingAvailable: Boolean = false,
    val savedName: String? = null,
    val notice: AttachmentNotice? = null,
) {
    val saved: Boolean get() = phase == AttachmentPhase.Saved

    val unconfirmed: Boolean get() = phase == AttachmentPhase.Unconfirmed
}
