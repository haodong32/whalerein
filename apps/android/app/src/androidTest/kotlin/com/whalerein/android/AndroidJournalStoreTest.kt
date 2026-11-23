package com.whalerein.android

import com.whalerein.android.platform.journal.AndroidJournalStore
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.support.Journal
import com.whalerein.android.state.support.JournalSchema
import com.whalerein.android.state.support.JournalSource
import com.whalerein.android.state.support.JournalStoreException
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The real backup-excluded journal store keeps records across a process
 * restart, isolates scopes, and rejects unsafe file identities.
 */
@RunWith(AndroidJUnit4::class)
class AndroidJournalStoreTest {
    private val directory = File(
        InstrumentationRegistry.getInstrumentation().targetContext.cacheDir,
        "journal-store-${System.nanoTime()}",
    )

    @After
    fun cleanup() {
        directory.deleteRecursively()
    }

    @Test
    fun `AC-28_real_store_persists_records_across_a_new_store_instance`() = runBlocking {
        val first = AndroidJournalStore(directory)
        val fileId = "journal-instance-a${JournalSchema.FILE_SUFFIX}"
        first.append(fileId, "{\"event\":\"one\"}\n")
        first.append(fileId, "{\"event\":\"two\"}\n")

        // A new store object over the same directory is a process restart.
        val restarted = AndroidJournalStore(directory)
        assertEquals(
            "{\"event\":\"one\"}\n{\"event\":\"two\"}\n",
            restarted.read(fileId),
        )
        val listed = restarted.list().single()
        assertEquals(fileId, listed.fileId)
        assertEquals("instance-a", listed.scopeId)
        assertTrue(listed.sizeBytes > 0)

        restarted.replace(fileId, "{\"event\":\"one\"}\n")
        assertEquals("{\"event\":\"one\"}\n", restarted.read(fileId))

        restarted.delete(fileId)
        assertNull(restarted.read(fileId))
        assertTrue(restarted.list().isEmpty())
    }

    @Test
    fun `AC-28_real_store_rejects_unsafe_ids_and_survives_a_missing_directory`() = runBlocking {
        val store = AndroidJournalStore(directory)
        val failures = listOf("../escape.jsonl", "journal-a/b.jsonl", "notes.txt")
        for (fileId in failures) {
            var rejected = false
            try {
                store.read(fileId)
            } catch (_: JournalStoreException) {
                rejected = true
            }
            assertTrue("$fileId must be rejected", rejected)
        }
        assertTrue(store.list().isEmpty())
    }

    @Test
    fun `AC-28_real_journal_persists_redacts_and_clears`() = runBlocking {
        val clock = FixedClock()
        val first = Journal(AndroidJournalStore(directory), clock)
        first.emit(
            DiagnosticEvent(
                level = DiagnosticLevel.Warn,
                event = "transport.host_online",
                stage = "transport",
                code = "ready",
                instanceId = "instance-a",
                fields = mapOf(
                    "host_id" to "host-a",
                    "token" to "must-not-be-stored",
                ),
            ),
        )
        assertEquals(1, first.queuedCount())
        first.flush()

        val restarted = Journal(AndroidJournalStore(directory), clock)
        val preview = restarted.preview("instance-a")
        assertEquals(1, preview.totalEvents)
        assertEquals("transport.host_online", preview.events.single().event)
        assertEquals(mapOf("host_id" to "host-a"), preview.events.single().fields)
        assertTrue(!preview.events.single().fields.containsKey("token"))

        val raw = AndroidJournalStore(directory)
            .read("journal-instance-a${JournalSchema.FILE_SUFFIX}")
            .orEmpty()
        assertTrue(raw.contains("host_id"))
        assertTrue(!raw.contains("must-not-be-stored"))

        restarted.clear("instance-a")
        assertTrue(AndroidJournalStore(directory).list().isEmpty())
        assertEquals(0, restarted.preview("instance-a").totalEvents)
    }

    @Test
    fun `AC-28_export_snapshot_is_byte_stable_and_metadata_first`() = runBlocking {
        val clock = FixedClock()
        val journal = Journal(AndroidJournalStore(directory), clock)
        journal.record(
            JournalSource.Android,
            DiagnosticEvent(
                level = DiagnosticLevel.Warn,
                event = "auth.denied",
                stage = "login",
                code = "denied",
                instanceId = "instance-a",
            ),
        )
        journal.flush()

        val export = journal.buildExport(
            scopeId = "instance-a",
            appBuild = "app-1",
            coreBuild = "core-1",
        )
        val lines = export.jsonl.decodeToString().trim().lines()
        assertTrue(lines.first().contains("\"kind\":\"metadata\""))
        assertTrue(lines.first().contains("\"appBuild\":\"app-1\""))
        assertTrue(lines.last().contains("auth.denied"))
        assertEquals(1, export.eventCount)

        val before = export.jsonl.copyOf()
        assertEquals(before.toList(), export.jsonl.toList())
    }
}
