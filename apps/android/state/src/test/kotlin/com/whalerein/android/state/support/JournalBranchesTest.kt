package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.fixtures.support.FakeClock
import com.whalerein.android.state.fixtures.support.FakeJournalStore
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/** Additional branch coverage for journal storage, rotation and writer plumbing. */
@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class JournalBranchesTest {
    private val instance = "instance-a"

    private fun event(
        level: DiagnosticLevel = DiagnosticLevel.Warn,
        name: String = DiagnosticEvents.OBSERVATION_FAILED,
        stage: String = DiagnosticStages.OBSERVATION,
        code: String? = null,
        instanceId: String? = "instance-a",
        fields: Map<String, String> = emptyMap(),
    ) = DiagnosticEvent(
        level = level,
        event = name,
        stage = stage,
        code = code,
        instanceId = instanceId,
        fields = fields,
    )

    @Test
    @DisplayName("AC-23.restart startup scope corrupt lines and torn tails recover safely")
    fun AC_23_restart_startup_scope_corrupt_lines_and_torn_tails_recover_safely() = runTest {
        val store = FakeJournalStore()
        val journal = Journal(store, FakeClock())
        journal.emit(event(instanceId = null))
        assertEquals(1, journal.queuedCount())
        journal.flush()
        assertEquals(1, journal.read(Journal.STARTUP_SCOPE).events.size)
        assertEquals(0, journal.droppedRecords())

        val fileId = FakeJournalStore.fileId(instance)
        store.put(
            fileId,
            "not-json\n" +
                "{\"schemaVersion\":1,\"kind\":\"event\",\"source\":\"android\",\"level\":\"WARN\",\"event\":\"observation.failed\",\"stage\":\"observation\"}\n",
        )
        val parsed = journal.read(instance)
        assertEquals(1, parsed.events.size)
        assertEquals(1, parsed.discardedLines)

        store.put(fileId, "{\"schemaVersion\":1,\"kind\":\"event\"")
        journal.emit(event(code = "recovered"))
        journal.flush()
        val repaired = journal.read(instance)
        assertEquals(1, repaired.events.size)
        assertFalse(repaired.tornTailDiscarded)

        val preview = journal.preview(instance, limit = 1)
        assertEquals(1, preview.events.size)
        assertEquals(1, preview.totalEvents)
        assertEquals(instance, preview.scopeId)
        assertEquals(preview, preview.copy(events = preview.events.toList()))
        assertEquals(preview, journal.preview(instance, limit = 0))
    }

    @Test
    @DisplayName("AC-23.io-failure read delete and prune failures degrade without throwing")
    fun AC_23_io_failure_read_delete_and_prune_failures_degrade_without_throwing() = runTest {
        val store = FakeJournalStore()
        store.failReads = true
        val journal = Journal(store, FakeClock())
        val readFailure = journal.read(instance)
        assertEquals(Journal.CODE_READ_FAILED, readFailure.error?.code)
        assertTrue(journal.status.value is JournalStatus.Degraded)
        assertEquals(
            JournalDegradationReason.ReadFailure,
            (journal.status.value as JournalStatus.Degraded).reason,
        )
        store.failReads = false
        journal.read(instance)
        assertEquals(JournalStatus.Healthy, journal.status.value)

        val deleteStore = FakeJournalStore()
        deleteStore.failDeletes = true
        val deleteJournal = Journal(deleteStore, FakeClock())
        deleteJournal.clear(instance)
        assertTrue(deleteJournal.status.value is JournalStatus.Degraded)
        assertEquals(
            JournalDegradationReason.Storage,
            (deleteJournal.status.value as JournalStatus.Degraded).reason,
        )

        val pruneStore = FakeJournalStore()
        val pruneClock = FakeClock()
        val pruneJournal = Journal(
            pruneStore,
            pruneClock,
            policy = JournalPolicy(retentionMillis = 1_000L, maxTotalBytes = 4_096L),
        )
        pruneJournal.emit(event())
        pruneJournal.flush()
        pruneStore.failReplaces = true
        pruneClock.advance(2_000L)
        pruneJournal.emit(event(code = "new"))
        pruneJournal.flush()
        assertTrue(pruneJournal.status.value is JournalStatus.Degraded)
    }

    @Test
    @DisplayName("AC-23.summary export sanitizes metadata and scope identifiers")
    fun AC_23_summary_export_sanitizes_metadata_and_scope_identifiers() = runTest {
        val store = FakeJournalStore()
        val clock = FakeClock(nowMillis = 5_000L)
        val journal = Journal(store, clock)
        journal.emit(event(code = "first"))
        journal.flush()

        val export = journal.buildExport(
            scopeId = instance,
            appBuild = "app_1.0",
            coreBuild = "core/1.0",
            coreLastFailure = CoreFailureSummary(
                code = "send_failed",
                occurredAtEpochMillis = 1L,
                level = "verbose",
                traceId = "/Users/secret",
            ),
        )
        assertEquals(instance, export.scopeId)
        assertEquals(1, export.eventCount)
        assertTrue(export.fileName.endsWith(".jsonl"))
        assertEquals("unknown", export.metadata.coreBuild)
        assertEquals("ERROR", export.metadata.coreLastFailure?.level)
        assertNull(export.metadata.coreLastFailure?.traceId)
        assertEquals(export.metadata, export.metadata.copy())

        val noSummary = journal.buildExport(instance, "app_1.0", "core_1.0")
        assertNull(noSummary.metadata.coreLastFailure)

        val unsafeSummary = journal.buildExport(
            scopeId = "instance/with space",
            appBuild = "app_1.0",
            coreBuild = "core_1.0",
            coreLastFailure = CoreFailureSummary("/secret", 1L),
        )
        assertNull(unsafeSummary.metadata.coreLastFailure)
        assertEquals("instance_with_space", unsafeSummary.fileName.substringAfter("whalerein-diagnostics-").substringBeforeLast("-"))
        assertEquals("instance/with space", unsafeSummary.scopeId)

        val emptyScope = journal.read("")
        assertNotNull(emptyScope)
        journal.clear("")
        journal.clear(instance)
        journal.clear(instance)
        assertTrue(journal.read(instance).events.isEmpty())
    }

    @Test
    @DisplayName("AC-23.bounded receiver saturation and background writer drain the queue")
    fun AC_23_bounded_receiver_saturation_and_background_writer_drain_the_queue() = runTest {
        val receiver = DiagnosticReceiver(capacity = 2, delegate = DiagnosticSink { error("delegate") })
        receiver.emit(event(code = "one"))
        receiver.emit(event(code = "two"))
        receiver.emit(event(code = "three"))
        assertEquals(1, receiver.droppedCount())
        assertEquals(2, receiver.snapshot().size)
        assertEquals(listOf("two", "three"), receiver.snapshot().mapNotNull { it.code })
        receiver.clear()
        assertTrue(receiver.snapshot().isEmpty())
        assertEquals(0, receiver.droppedCount())

        val zeroCapacity = DiagnosticReceiver(capacity = 0)
        zeroCapacity.emit(event())
        assertTrue(zeroCapacity.snapshot().isEmpty())

        val store = FakeJournalStore()
        val journal = Journal(store, FakeClock())
        val writer = JournalWriter(journal, intervalMillis = 1_000L)
        val job = writer.start(backgroundScope)
        journal.emit(event(code = "writer"))
        advanceTimeBy(1_500L)
        runCurrent()
        assertEquals(listOf("writer"), journal.read(instance).events.mapNotNull { it.code })
        journal.emit(event(code = "flush-now"))
        writer.flushNow()
        assertEquals(2, journal.read(instance).events.size)
        job.cancel()
    }

    @Test
    @DisplayName("AC-23.levels vocabulary policies and journal result models expose stable values")
    fun AC_23_levels_vocabulary_policies_and_journal_result_models_expose_stable_values() {
        assertEquals(13, DiagnosticStages.ALL.size)
        assertTrue(DiagnosticStages.ALL.contains(DiagnosticStages.REPLY_READ))
        assertTrue(DiagnosticEvents.INFO_MILESTONES.contains(DiagnosticEvents.COMMAND_RECOVERED))
        assertFalse(DiagnosticEvents.INFO_MILESTONES.contains(DiagnosticEvents.TRANSPORT_FAILED))
        assertTrue(DiagnosticEvents.EXCLUDED_EVENTS.contains("content.delta"))
        assertEquals("android", JournalSource.Android.wireName)
        assertEquals("core", JournalSource.Core.wireName)

        val policy = JournalPolicy.defaults()
        assertTrue(policy.retains(DiagnosticLevel.Warn, DiagnosticEvents.OBSERVATION_FAILED))
        assertTrue(policy.retains(DiagnosticLevel.Error, DiagnosticEvents.OBSERVATION_FAILED))
        assertTrue(policy.retains(DiagnosticLevel.Info, DiagnosticEvents.OBSERVATION_STARTED))
        assertFalse(policy.retains(DiagnosticLevel.Info, DiagnosticEvents.TRANSPORT_CONNECTED))
        assertFalse(policy.retains(DiagnosticLevel.Debug, DiagnosticEvents.OBSERVATION_FAILED))
        assertFalse(policy.retains(DiagnosticLevel.Warn, "content.delta"))
        assertFalse(policy.retains(DiagnosticLevel.Warn, "observation.delta"))
        assertEquals(policy, policy.copy())

        val stored = JournalStoredFile("file", "scope", 1L, 2L)
        assertEquals("file", stored.fileId)
        assertEquals("scope", stored.scopeId)
        assertEquals(1L, stored.modifiedEpochMillis)
        assertEquals(2L, stored.sizeBytes)

        val statuses: List<JournalStatus> = listOf(
            JournalStatus.Healthy,
            JournalStatus.Degraded(JournalDegradationReason.Saturation, 1, "code"),
            JournalStatus.Degraded(JournalDegradationReason.Storage, 0, "code"),
            JournalStatus.Degraded(JournalDegradationReason.ReadFailure, 0, "code"),
        )
        statuses.forEach { assertEquals(it, it) }
        assertEquals(1, (statuses[1] as JournalStatus.Degraded).droppedRecords)
        assertEquals("code", (statuses[2] as JournalStatus.Degraded).detailCode)

        val metadata = JournalRecord(
            schemaVersion = 1,
            kind = JournalSchema.KIND_METADATA,
            source = JournalSchema.SOURCE_JOURNAL,
            exportedAtEpochMillis = 1L,
            scopeId = instance,
            appBuild = "app",
            coreBuild = "core",
            coreLastFailure = CoreFailureSummary("code", 1L),
        )
        val eventRecord = JournalRecord(
            schemaVersion = 1,
            kind = JournalSchema.KIND_EVENT,
            source = JournalSchema.SOURCE_ANDROID,
            timestampEpochMillis = 1L,
            level = "WARN",
            event = "event",
            stage = "stage",
            code = "code",
            traceId = "trace",
            actionId = "action",
            instanceId = instance,
            fields = mapOf("count" to "1"),
        )
        assertEquals("metadata", metadata.kind)
        assertEquals("event", eventRecord.kind)
        assertEquals("code", eventRecord.code)
        assertEquals("trace", eventRecord.traceId)
        assertEquals("action", eventRecord.actionId)
        assertEquals(instance, eventRecord.instanceId)
        assertEquals(mapOf("count" to "1"), eventRecord.fields)
        assertEquals("next", eventRecord.copy(event = "next").event)

        val read = JournalReadResult(
            records = listOf(metadata, eventRecord),
            tornTailDiscarded = true,
            discardedLines = 2,
            error = CoreFailure("code", "message"),
        )
        assertEquals(1, read.events.size)
        assertEquals("event", read.events.single().event)
        assertTrue(read.tornTailDiscarded)
        assertEquals(2, read.discardedLines)
        assertEquals("code", read.error?.code)

        val export = JournalExport(instance, "file.jsonl", byteArrayOf(1), 1, metadata)
        assertEquals(instance, export.scopeId)
        assertEquals("file.jsonl", export.fileName)
        assertEquals(1, export.jsonl.size)
        assertEquals(1, export.eventCount)
        assertEquals(metadata, export.metadata)

        val summary = CoreFailureSummary("code", 1L, "warn", "trace")
        assertEquals("code", summary.code)
        assertEquals(1L, summary.occurredAtEpochMillis)
        assertEquals("warn", summary.level)
        assertEquals("trace", summary.traceId)
        assertEquals("ERROR", summary.copy(level = "ERROR").level)
    }
}
