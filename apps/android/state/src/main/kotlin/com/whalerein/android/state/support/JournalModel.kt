package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticLevel
import kotlinx.serialization.Serializable

/** Stable schema identity for stored and exported journal records. */
object JournalSchema {
    const val VERSION: Int = 1
    const val KIND_EVENT: String = "event"
    const val KIND_METADATA: String = "metadata"
    const val SOURCE_ANDROID: String = "android"
    const val SOURCE_CORE: String = "core"
    const val SOURCE_JOURNAL: String = "journal"
    const val FILE_SUFFIX: String = ".jsonl"
}

enum class JournalSource(val wireName: String) {
    Android(JournalSchema.SOURCE_ANDROID),
    Core(JournalSchema.SOURCE_CORE),
}

/** Optional metadata sourced from Core's separate last-failure summary. */
@Serializable
data class CoreFailureSummary(
    val code: String,
    val occurredAtEpochMillis: Long,
    val level: String = "ERROR",
    val traceId: String? = null,
)

/**
 * One JSONL object.
 *
 * Event rows use kind = event; an export's first row uses kind = metadata and
 * carries the versioned App/Core build identity. Null fields are omitted from
 * the serialized line.
 */
@Serializable
data class JournalRecord(
    val schemaVersion: Int = JournalSchema.VERSION,
    val kind: String = JournalSchema.KIND_EVENT,
    val source: String? = null,
    val timestampEpochMillis: Long? = null,
    val level: String? = null,
    val event: String? = null,
    val stage: String? = null,
    val code: String? = null,
    val traceId: String? = null,
    val actionId: String? = null,
    val instanceId: String? = null,
    val fields: Map<String, String> = emptyMap(),
    // Metadata-only rows.
    val exportedAtEpochMillis: Long? = null,
    val scopeId: String? = null,
    val appBuild: String? = null,
    val coreBuild: String? = null,
    val coreLastFailure: CoreFailureSummary? = null,
)

/** A store entry. [fileId] is opaque to callers and is never logged. */
data class JournalStoredFile(
    val fileId: String,
    val scopeId: String,
    val modifiedEpochMillis: Long,
    val sizeBytes: Long,
)

/**
 * Storage seam for the journal. `:app` supplies a private, backup-excluded
 * file implementation; JVM tests supply an in-memory or fault-injecting one.
 *
 * Implementations may throw [JournalStoreException] (or any exception) on I/O.
 * The journal converts those into degraded status instead of propagating them.
 */
interface JournalStore {
    suspend fun list(): List<JournalStoredFile>

    suspend fun read(fileId: String): String?

    suspend fun append(fileId: String, text: String)

    suspend fun replace(fileId: String, text: String)

    suspend fun delete(fileId: String)
}

class JournalStoreException(message: String, cause: Throwable? = null) : Exception(message, cause)

/** Journal retention and filtering policy. Defaults are the accepted values. */
data class JournalPolicy(
    val retentionMillis: Long = SEVEN_DAYS_MILLIS,
    val maxTotalBytes: Long = FOUR_MEBIBYTES,
    val queueCapacity: Int = 512,
    val infoMilestones: Set<String> = DiagnosticEvents.INFO_MILESTONES,
    val excludedEvents: Set<String> = DiagnosticEvents.EXCLUDED_EVENTS,
) {
    fun retains(level: DiagnosticLevel, event: String): Boolean {
        if (event in excludedEvents || event.contains(".delta")) return false
        return when (level) {
            DiagnosticLevel.Debug -> false
            DiagnosticLevel.Info -> event in infoMilestones
            DiagnosticLevel.Warn, DiagnosticLevel.Error -> true
        }
    }

    companion object {
        const val SEVEN_DAYS_MILLIS: Long = 7L * 24L * 60L * 60L * 1000L
        const val FOUR_MEBIBYTES: Long = 4L * 1024L * 1024L

        fun defaults(): JournalPolicy = JournalPolicy()
    }
}

enum class JournalDegradationReason { Saturation, Storage, ReadFailure }

sealed interface JournalStatus {
    data object Healthy : JournalStatus

    data class Degraded(
        val reason: JournalDegradationReason,
        val droppedRecords: Int = 0,
        val detailCode: String? = null,
    ) : JournalStatus
}

data class JournalReadResult(
    val records: List<JournalRecord> = emptyList(),
    val tornTailDiscarded: Boolean = false,
    val discardedLines: Int = 0,
    val error: CoreFailure? = null,
) {
    val events: List<JournalRecord> get() = records.filter { it.kind == JournalSchema.KIND_EVENT }
}

/** A frozen, byte-stable export captured before the confirmation prompt. */
data class JournalExport(
    val scopeId: String,
    val fileName: String,
    val jsonl: ByteArray,
    val eventCount: Int,
    val metadata: JournalRecord,
)
