package com.whalerein.android.state.support

import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.fixtures.support.FakeClock
import com.whalerein.android.state.fixtures.support.FakeJournalStore
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

/** Journal contract behind the diagnostics export surface. */
class JournalScopedContractTest {
    private val scope = "instance-a"
    private val fileId = FakeJournalStore.fileId(scope)

    private fun event(fields: Map<String, String> = emptyMap()) = DiagnosticEvent(
        level = DiagnosticLevel.Warn,
        event = "transport.host_online",
        stage = "transport",
        code = "ready",
        instanceId = scope,
        fields = fields,
    )

    @Test
    @DisplayName("AC-26.journal-persists-across-restart-and-keeps-redaction")
    fun ac26JournalPersistsAcrossRestartAndKeepsRedaction() = runTest {
        val store = FakeJournalStore()
        val first = Journal(store, FakeClock())
        first.emit(
            event(
                mapOf(
                    "host_id" to "host-a",
                    "token" to "must-not-be-written",
                    "content" to "private message",
                ),
            ),
        )
        first.flush()

        val restarted = Journal(store, FakeClock())
        val preview = restarted.preview(scope)
        assertEquals(1, preview.totalEvents)
        assertEquals(mapOf("host_id" to "host-a"), preview.events.single().fields)
        val raw = store.raw(fileId).orEmpty()
        assertTrue(raw.contains("host_id"))
        assertFalse(raw.contains("must-not-be-written"))
        assertFalse(raw.contains("private message"))
    }

    @Test
    @DisplayName("AC-26.clear-removes-only-the-selected-scope")
    fun ac26ClearRemovesOnlyTheSelectedScope() = runTest {
        val store = FakeJournalStore()
        val journal = Journal(store, FakeClock())
        journal.emit(event(mapOf("host_id" to "host-a")))
        journal.record(
            JournalSource.Android,
            event().copy(instanceId = "instance-b"),
        )
        journal.flush()

        journal.clear(scope)

        assertEquals(0, journal.preview(scope).totalEvents)
        assertEquals(1, journal.preview("instance-b").totalEvents)
        assertTrue(store.fileIds().contains(FakeJournalStore.fileId("instance-b")))
        assertFalse(store.fileIds().contains(fileId))
    }

    @Test
    @DisplayName("AC-26.export-snapshot-is-frozen-with-metadata-first")
    fun ac26ExportSnapshotIsFrozenWithMetadataFirst() = runTest {
        val store = FakeJournalStore()
        val journal = Journal(store, FakeClock())
        journal.emit(event(mapOf("host_id" to "host-a")))
        journal.flush()

        val export = journal.buildExport(scope, appBuild = "app-1", coreBuild = "core-1")
        val lines = export.jsonl.decodeToString().trim().lines()
        assertTrue(lines.first().contains("\"kind\":\"metadata\""))
        assertTrue(lines.first().contains("\"appBuild\":\"app-1\""))
        assertTrue(lines.last().contains("transport.host_online"))
        assertEquals(1, export.eventCount)

        // Later writes do not mutate the captured export bytes.
        journal.emit(event(mapOf("host_id" to "host-b")))
        journal.flush()
        val captured = export.jsonl.copyOf()
        assertEquals(captured.toList(), export.jsonl.toList())
        assertEquals(2, journal.preview(scope).totalEvents)
    }
}
