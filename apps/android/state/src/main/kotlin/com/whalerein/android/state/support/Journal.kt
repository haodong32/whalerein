package com.whalerein.android.state.support

import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticSink
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import java.nio.charset.StandardCharsets

data class JournalPreview(
    val scopeId: String,
    val events: List<JournalRecord>,
    val totalEvents: Int,
    val tornTailDiscarded: Boolean = false,
    val error: CoreFailure? = null,
)

/**
 * Bounded, non-blocking JSONL journal.
 *
 * [emit] only appends to an in-memory bounded queue; it never suspends, never
 * touches storage and never throws. [flush] drains the queue, repairs a torn
 * tail, appends complete records and prunes by age and app-wide size. Storage
 * failures downgrade [status] instead of propagating, so a diagnostic problem
 * can never change the outcome of the operation that emitted the event.
 */
@OptIn(ExperimentalSerializationApi::class)
class Journal(
    private val store: JournalStore,
    private val clock: Clock,
    private val policy: JournalPolicy = JournalPolicy.defaults(),
) : DiagnosticSink {
    private val json = Json {
        encodeDefaults = true
        explicitNulls = false
        ignoreUnknownKeys = true
    }

    private val lock = Any()
    private val flushMutex = Mutex()
    private val queue = ArrayDeque<QueuedRecord>()
    private val clearWatermarks = mutableMapOf<String, Long>()
    private var nextSequence = 0L
    private var droppedRecords = 0
    private var writeFailureCode: String? = null
    private var readFailureCode: String? = null

    private val mutableStatus = MutableStateFlow<JournalStatus>(JournalStatus.Healthy)
    val status: StateFlow<JournalStatus> = mutableStatus.asStateFlow()

    private data class QueuedRecord(
        val sequence: Long,
        val scopeId: String,
        val source: JournalSource,
        val timestampEpochMillis: Long,
        val event: DiagnosticEvent,
    )

    private data class PruneCandidate(
        val fileId: String,
        val line: RawLine,
    )

    private data class RawLine(
        val index: Int,
        val text: String,
        val timestampEpochMillis: Long,
        val byteSize: Long,
    )

    private data class ParsedContent(
        val lines: List<String>,
        val tornTail: Boolean,
        val discardedLines: Int,
    )

    override fun emit(event: DiagnosticEvent) {
        record(JournalSource.Android, event)
    }

    /** Emits a typed Core event through the same app-scoped writer. */
    fun emitCore(event: DiagnosticEvent) {
        record(JournalSource.Core, event)
    }

    fun record(source: JournalSource, event: DiagnosticEvent) {
        val sanitized = DiagnosticRedaction.sanitize(event) ?: return
        if (!policy.retains(sanitized.level, sanitized.event)) return
        val scopeId = sanitized.instanceId ?: STARTUP_SCOPE
        val accepted = synchronized(lock) {
            if (queue.size >= policy.queueCapacity) {
                droppedRecords += 1
                false
            } else {
                nextSequence += 1
                queue.addLast(
                    QueuedRecord(
                        sequence = nextSequence,
                        scopeId = scopeId,
                        source = source,
                        timestampEpochMillis = clock.epochMillis(),
                        event = sanitized,
                    ),
                )
                true
            }
        }
        if (!accepted) updateStatus()
    }

    fun queuedCount(): Int = synchronized(lock) { queue.size }

    fun droppedRecords(): Int = synchronized(lock) { droppedRecords }

    suspend fun flush() {
        flushMutex.withLock {
            val batch = synchronized(lock) {
                val items = queue.toList()
                queue.clear()
                items.filter { item ->
                    val watermark = clearWatermarks[item.scopeId]
                    watermark == null || item.sequence > watermark
                }
            }
            batch.groupBy { it.scopeId }.forEach { (scopeId, records) ->
                val allowed = synchronized(lock) {
                    val watermark = clearWatermarks[scopeId]
                    watermark == null || records.any { it.sequence > watermark }
                }
                if (allowed) {
                    appendLines(scopeId, records.map { encodeEvent(it) })
                }
            }
            prune()
            updateStatus()
        }
    }

    suspend fun read(scopeId: String): JournalReadResult {
        val content = try {
            store.read(fileIdFor(scopeId)) ?: ""
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            readFailureCode = CODE_READ_FAILED
            updateStatus()
            return JournalReadResult(
                error = CoreFailure(
                    code = CODE_READ_FAILED,
                    message = "Diagnostics could not be read.",
                    retryable = true,
                ),
            )
        }
        readFailureCode = null
        val parsed = parse(content)
        updateStatus()
        val records = mutableListOf<JournalRecord>()
        for (line in parsed.lines) {
            decodeOrNull(line)?.let { records += it }
        }
        return JournalReadResult(
            records = records,
            tornTailDiscarded = parsed.tornTail,
            discardedLines = parsed.discardedLines,
        )
    }

    suspend fun preview(scopeId: String, limit: Int = 200): JournalPreview {
        val result = read(scopeId)
        val events = result.events
        return JournalPreview(
            scopeId = scopeId,
            events = if (limit > 0) events.takeLast(limit) else events,
            totalEvents = events.size,
            tornTailDiscarded = result.tornTailDiscarded,
            error = result.error,
        )
    }

    /**
     * Removes the selected scope's file and drops already-queued records that
     * predate the clear. Fails silently with degraded status on I/O errors.
     */
    suspend fun clear(scopeId: String) {
        synchronized(lock) {
            val mark = nextSequence
            clearWatermarks[scopeId] = maxOf(clearWatermarks[scopeId] ?: 0L, mark)
            val iterator = queue.iterator()
            while (iterator.hasNext()) {
                val item = iterator.next()
                if (item.scopeId == scopeId && item.sequence <= mark) iterator.remove()
            }
        }
        try {
            store.delete(fileIdFor(scopeId))
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            writeFailureCode = CODE_WRITE_FAILED
        }
        updateStatus()
    }

    /**
     * Captures a byte-stable export snapshot. Callers must obtain confirmation
     * before writing [JournalExport.jsonl]; later writes or scope changes do not
     * mutate the captured bytes. The versioned metadata row is always first.
     */
    suspend fun buildExport(
        scopeId: String,
        appBuild: String,
        coreBuild: String,
        coreLastFailure: CoreFailureSummary? = null,
    ): JournalExport {
        val result = read(scopeId)
        val metadata = JournalRecord(
            kind = JournalSchema.KIND_METADATA,
            source = JournalSchema.SOURCE_JOURNAL,
            exportedAtEpochMillis = clock.epochMillis(),
            scopeId = scopeId,
            appBuild = safeBuild(appBuild),
            coreBuild = safeBuild(coreBuild),
            coreLastFailure = coreLastFailure?.let { sanitizeSummary(it) },
        )
        val events = result.events
        val builder = StringBuilder()
        builder.append(json.encodeToString(metadata)).append('\n')
        for (event in events) {
            builder.append(json.encodeToString(event)).append('\n')
        }
        return JournalExport(
            scopeId = scopeId,
            fileName = "whalerein-diagnostics-${safeScope(scopeId)}-${clock.epochMillis()}" +
                JournalSchema.FILE_SUFFIX,
            jsonl = builder.toString().toByteArray(StandardCharsets.UTF_8),
            eventCount = events.size,
            metadata = metadata,
        )
    }

    private fun encodeEvent(queued: QueuedRecord): String {
        val record = JournalRecord(
            kind = JournalSchema.KIND_EVENT,
            source = queued.source.wireName,
            timestampEpochMillis = queued.timestampEpochMillis,
            level = queued.event.level.name.uppercase(),
            event = queued.event.event,
            stage = queued.event.stage,
            code = queued.event.code,
            traceId = queued.event.traceId,
            actionId = queued.event.actionId,
            instanceId = queued.event.instanceId,
            fields = queued.event.fields,
        )
        return json.encodeToString(record)
    }

    private suspend fun appendLines(scopeId: String, lines: List<String>) {
        if (lines.isEmpty()) return
        try {
            val fileId = fileIdFor(scopeId)
            val existing = store.read(fileId) ?: ""
            val repaired = repairTail(existing)
            if (repaired != existing) store.replace(fileId, repaired)
            val chunk = lines.joinToString(separator = "\n", postfix = "\n")
            store.append(fileId, chunk)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            writeFailureCode = CODE_WRITE_FAILED
        }
    }

    private suspend fun prune() {
        try {
            val cutoff = clock.epochMillis() - policy.retentionMillis
            val files = store.list()
            val candidates = mutableListOf<PruneCandidate>()
            val perFile = LinkedHashMap<String, List<RawLine>>()
            for (file in files) {
                val content = store.read(file.fileId) ?: continue
                val parsed = parse(content)
                val kept = mutableListOf<RawLine>()
                for ((index, text) in parsed.lines.withIndex()) {
                    val record = decodeOrNull(text) ?: continue
                    val timestamp = record.timestampEpochMillis ?: 0L
                    if (timestamp < cutoff) continue
                    kept += RawLine(index, text, timestamp, byteSize(text))
                }
                perFile[file.fileId] = kept
                for (line in kept) candidates += PruneCandidate(file.fileId, line)
            }

            val removed = mutableSetOf<Pair<String, Int>>()
            var total = candidates.sumOf { it.line.byteSize }
            if (total > policy.maxTotalBytes) {
                val oldestFirst = candidates.sortedWith(
                    compareBy(
                        { it.line.timestampEpochMillis },
                        { it.fileId },
                        { it.line.index },
                    ),
                )
                for (candidate in oldestFirst) {
                    if (total <= policy.maxTotalBytes) break
                    if (removed.add(candidate.fileId to candidate.line.index)) {
                        total -= candidate.line.byteSize
                    }
                }
            }

            for ((fileId, kept) in perFile) {
                val remaining = kept.filterNot { (fileId to it.index) in removed }
                val content = remaining.joinToString(separator = "\n", postfix = "\n") {
                    it.text
                }
                val existing = store.read(fileId) ?: ""
                if (remaining.isEmpty()) {
                    if (existing.isNotEmpty()) store.delete(fileId)
                } else if (content != existing) {
                    store.replace(fileId, content)
                }
            }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            writeFailureCode = CODE_WRITE_FAILED
        }
    }

    private fun parse(content: String): ParsedContent {
        if (content.isEmpty()) return ParsedContent(emptyList(), tornTail = false, discardedLines = 0)
        val segments = content.split('\n')
        val complete = segments.dropLast(1)
        val torn = !content.endsWith('\n')
        var discarded = 0
        val lines = mutableListOf<String>()
        for (segment in complete) {
            if (segment.isBlank()) continue
            if (decodeOrNull(segment) == null) {
                discarded += 1
            } else {
                lines += segment
            }
        }
        return ParsedContent(lines, tornTail = torn, discardedLines = discarded)
    }

    private fun repairTail(content: String): String {
        if (content.isEmpty()) return content
        val lastNewline = content.lastIndexOf('\n')
        return if (lastNewline < 0) "" else content.substring(0, lastNewline + 1)
    }

    private fun decodeOrNull(text: String): JournalRecord? = try {
        json.decodeFromString<JournalRecord>(text)
    } catch (_: Exception) {
        null
    }

    private fun sanitizeSummary(summary: CoreFailureSummary): CoreFailureSummary? {
        if (!DiagnosticRedaction.isSafeToken(summary.code)) return null
        val trace = summary.traceId?.takeIf { DiagnosticRedaction.isSafeToken(it) }
        val level = summary.level.uppercase().takeIf {
            it in setOf("DEBUG", "INFO", "WARN", "ERROR")
        } ?: "ERROR"
        return summary.copy(traceId = trace, level = level)
    }

    private fun safeBuild(value: String): String =
        value.takeIf { DiagnosticRedaction.isSafeToken(it) } ?: "unknown"

    private fun safeScope(scopeId: String): String =
        scopeId.replace(Regex("[^A-Za-z0-9._-]"), "_").ifEmpty { "startup" }

    private fun fileIdFor(scopeId: String): String = "journal-${safeScope(scopeId)}${JournalSchema.FILE_SUFFIX}"

    private fun byteSize(text: String): Long =
        text.toByteArray(StandardCharsets.UTF_8).size.toLong() + 1L

    private fun updateStatus() {
        mutableStatus.value = when {
            droppedRecords > 0 -> JournalStatus.Degraded(
                reason = JournalDegradationReason.Saturation,
                droppedRecords = droppedRecords,
                detailCode = CODE_SATURATED,
            )
            writeFailureCode != null -> JournalStatus.Degraded(
                reason = JournalDegradationReason.Storage,
                detailCode = writeFailureCode,
            )
            readFailureCode != null -> JournalStatus.Degraded(
                reason = JournalDegradationReason.ReadFailure,
                detailCode = readFailureCode,
            )
            else -> JournalStatus.Healthy
        }
    }

    companion object {
        const val STARTUP_SCOPE: String = "startup"
        const val CODE_READ_FAILED: String = "journal_read_failed"
        const val CODE_WRITE_FAILED: String = "journal_write_failed"
        const val CODE_SATURATED: String = "journal_saturated"
    }
}
