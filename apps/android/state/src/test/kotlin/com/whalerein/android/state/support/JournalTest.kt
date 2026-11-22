package com.whalerein.android.state.support

import com.whalerein.android.state.core.CoreFailure
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.core.IdGenerator
import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SendId
import com.whalerein.android.state.fixtures.support.FakeClock
import com.whalerein.android.state.fixtures.support.FakeJournalStore
import com.whalerein.android.state.fixtures.support.FakeSendRecoveryPort
import com.whalerein.android.state.fixtures.support.confirmedSendReceipt
import com.whalerein.android.state.fixtures.support.testSessionRef
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test
import java.nio.charset.StandardCharsets

/**
 * JVM contract tests for the journal. The fault-injecting in-memory store stands
 * in for the Android private file store; rotation, torn-tail and I/O behavior are
 * asserted at this seam, while on-device restart and export evidence remains
 * native scope.
 */
class JournalTest {
    private val instance = "instance-a"
    private val session = testSessionRef()

    private fun event(
        level: DiagnosticLevel = DiagnosticLevel.Warn,
        name: String = DiagnosticEvents.OBSERVATION_FAILED,
        stage: String = DiagnosticStages.OBSERVATION,
        code: String? = null,
        fields: Map<String, String> = emptyMap(),
        instanceId: String = "instance-a",
    ) = DiagnosticEvent(
        level = level,
        event = name,
        stage = stage,
        code = code,
        instanceId = instanceId,
        fields = fields,
    )

    @Test
    @DisplayName("AC-23.bounded saturation drops oldest queued records without blocking")
    fun AC_23_bounded_saturation_drops_oldest_queued_records_without_blocking() = runTest {
        val store = FakeJournalStore()
        val clock = FakeClock()
        val journal = Journal(
            store = store,
            clock = clock,
            policy = JournalPolicy(queueCapacity = 4),
        )

        repeat(10) {
            journal.emit(event(fields = mapOf("count" to it.toString())))
        }

        assertEquals(4, journal.queuedCount())
        val status = journal.status.value
        assertTrue(status is JournalStatus.Degraded)
        assertEquals(JournalDegradationReason.Saturation, (status as JournalStatus.Degraded).reason)
        assertEquals(6, status.droppedRecords)

        journal.flush()
        assertEquals(4, journal.read(instance).events.size)
    }

    @Test
    @DisplayName("AC-23.restart reads complete earlier records")
    fun AC_23_restart_reads_complete_earlier_records() = runTest {
        val store = FakeJournalStore()
        val clock = FakeClock()
        val first = Journal(store, clock)
        first.emit(event(code = "transport_lost"))
        first.flush()

        val restarted = Journal(store, clock)
        val result = restarted.read(instance)

        assertEquals(1, result.events.size)
        assertEquals("transport_lost", result.events.single().code)
        assertFalse(result.tornTailDiscarded)
        assertNull(result.error)
        assertTrue(store.raw(FakeJournalStore.fileId(instance))!!.endsWith("\n"))
    }

    @Test
    @DisplayName("AC-23.rotation enforces age and app-wide size while scope clear stays isolated")
    fun AC_23_rotation_enforces_age_and_app_wide_size_while_scope_clear_stays_isolated() = runTest {
        assertEquals(
            7L * 24L * 60L * 60L * 1000L,
            JournalPolicy.defaults().retentionMillis,
        )
        assertEquals(4L * 1024L * 1024L, JournalPolicy.defaults().maxTotalBytes)
        val store = FakeJournalStore()
        val clock = FakeClock()
        val journal = Journal(
            store = store,
            clock = clock,
            policy = JournalPolicy(
                retentionMillis = 1_000L,
                maxTotalBytes = 600L,
            ),
        )
        journal.emit(event(name = DiagnosticEvents.TRANSPORT_FAILED, stage = DiagnosticStages.TRANSPORT))
        journal.flush()

        clock.advance(2_000L)
        repeat(5) {
            journal.emit(
                event(
                    name = DiagnosticEvents.OBSERVATION_FAILED,
                    fields = mapOf("count" to "1234567890", "detail" to "retentionprobe"),
                    instanceId = "instance-a",
                ),
            )
        }
        journal.emit(
            event(
                name = DiagnosticEvents.REPLY_READ_FAILED,
                stage = DiagnosticStages.REPLY_READ,
                instanceId = "instance-b",
            ),
        )
        journal.flush()

        val scopeA = journal.read("instance-a").events
        assertFalse(scopeA.any { it.event == DiagnosticEvents.TRANSPORT_FAILED })
        assertTrue(scopeA.isNotEmpty())
        assertTrue(scopeA.any { it.event == DiagnosticEvents.OBSERVATION_FAILED })
        assertEquals(
            1,
            journal.read("instance-b").events.count {
                it.event == DiagnosticEvents.REPLY_READ_FAILED
            },
        )

        val totalBytes = store.fileIds().sumOf { fileId ->
            store.raw(fileId)!!.toByteArray(StandardCharsets.UTF_8).size.toLong()
        }
        assertTrue(totalBytes <= 600L, "journal total $totalBytes exceeds the configured cap")

        journal.clear("instance-a")
        assertTrue(journal.read("instance-a").events.isEmpty())
        assertTrue(journal.read("instance-b").events.isNotEmpty())
    }

    @Test
    @DisplayName("AC-23.levels retain warnings errors and allowlisted milestones only")
    fun AC_23_levels_retain_warnings_errors_and_allowlisted_milestones_only() = runTest {
        val store = FakeJournalStore()
        val journal = Journal(store, FakeClock())
        journal.emit(event(level = DiagnosticLevel.Debug))
        journal.emit(event(level = DiagnosticLevel.Info, name = DiagnosticEvents.TRANSPORT_CONNECTED))
        journal.emit(event(level = DiagnosticLevel.Info, name = DiagnosticEvents.OBSERVATION_STARTED))
        journal.emit(event(level = DiagnosticLevel.Warn))
        journal.emit(event(level = DiagnosticLevel.Error))
        journal.emit(
            event(
                level = DiagnosticLevel.Warn,
                name = "content.delta",
            ),
        )
        journal.flush()

        val names = journal.read(instance).events.map { it.event }
        assertEquals(
            listOf(
                DiagnosticEvents.OBSERVATION_STARTED,
                DiagnosticEvents.OBSERVATION_FAILED,
                DiagnosticEvents.OBSERVATION_FAILED,
            ),
            names,
        )
        assertTrue(journal.read(instance).events.any { it.level == "ERROR" })
    }

    @Test
    @DisplayName("AC-23.torn-tail keeps earlier records and resumes appending")
    fun AC_23_torn_tail_keeps_earlier_records_and_resumes_appending() = runTest {
        val store = FakeJournalStore()
        val clock = FakeClock()
        val journal = Journal(store, clock)
        journal.emit(event(code = "first"))
        journal.flush()
        val fileId = FakeJournalStore.fileId(instance)
        val complete = store.raw(fileId)!!
        store.put(fileId, complete + "{\"schemaVersion\":1,\"kind\":\"event\"")

        val reopened = Journal(store, clock)
        val torn = reopened.read(instance)
        assertEquals(1, torn.events.size)
        assertTrue(torn.tornTailDiscarded)

        reopened.emit(event(code = "second"))
        reopened.flush()
        val resumed = reopened.read(instance)
        assertEquals(2, resumed.events.size)
        assertFalse(resumed.tornTailDiscarded)
        assertEquals(
            2,
            store.raw(fileId)!!.split('\n').count { it.isNotBlank() },
        )
    }

    @Test
    @DisplayName("AC-23.io-failure leaves operation outcomes unchanged and surfaces degradation")
    fun AC_23_io_failure_leaves_operation_outcomes_unchanged_and_surfaces_degradation() = runTest {
        val store = FakeJournalStore()
        store.failAppends = true
        val journal = Journal(store, FakeClock())
        val ref = ReplyRef(session, SendId("send-1"))
        val core = FakeSendRecoveryPort(
            submitOutcome = Outcome.Success(confirmedSendReceipt(ref)),
        )
        core.replyOutcomes.add(Outcome.Failure(CoreFailure("reply_unavailable", "Unreadable.")))
        val viewModel = SendRecoveryViewModel(
            instanceId = com.whalerein.android.state.core.InstanceId(instance),
            port = core,
            diagnostics = journal,
            idGenerator = IdGenerator { "send-1" },
        )

        viewModel.send(session, "hello")
        journal.flush()

        assertEquals(SendPhase.Confirmed, viewModel.state.value.phase)
        assertTrue(viewModel.state.value.reply is ReplyReadState.Unavailable)
        val status = journal.status.value
        assertTrue(status is JournalStatus.Degraded)
        assertEquals(JournalDegradationReason.Storage, (status as JournalStatus.Degraded).reason)

        // A throwing diagnostic sink must also leave the operation outcome intact.
        val guardedCore = FakeSendRecoveryPort(
            submitOutcome = Outcome.Success(confirmedSendReceipt(ref)),
        )
        guardedCore.replyOutcomes.add(Outcome.Success("visible reply"))
        val guarded = SendRecoveryViewModel(
            instanceId = com.whalerein.android.state.core.InstanceId(instance),
            port = guardedCore,
            diagnostics = DiagnosticSink { error("diagnostic sink failure") },
            idGenerator = IdGenerator { "send-1" },
        )
        guarded.send(session, "hello")
        assertEquals(SendPhase.Confirmed, guarded.state.value.phase)
        assertEquals(1, guardedCore.submitCount)
    }

    @Test
    @DisplayName("AC-23.redaction drops sentinel content paths callbacks tokens and keys")
    fun AC_23_redaction_drops_sentinel_content_paths_callbacks_tokens_and_keys() = runTest {
        val sentinel = "SENTINEL-message-body /Users/secret/callback?invite=abc&token=sk-live"
        val store = FakeJournalStore()
        val journal = Journal(store, FakeClock())
        journal.emit(
            event(
                level = DiagnosticLevel.Warn,
                fields = mapOf(
                    "message" to sentinel,
                    "path" to sentinel,
                    "callback" to sentinel,
                    "token" to "sk_live_SENTINEL",
                    "invite" to sentinel,
                    "content" to sentinel,
                    "reason" to sentinel,
                    "count" to "3",
                    "operation_id" to "send-1",
                ),
            ),
        )
        journal.emit(
            DiagnosticEvent(
                level = DiagnosticLevel.Error,
                event = "observation.failed_$sentinel",
                stage = DiagnosticStages.OBSERVATION,
                instanceId = instance,
            ),
        )
        journal.emit(
            DiagnosticEvent(
                level = DiagnosticLevel.Warn,
                event = DiagnosticEvents.TRANSPORT_FAILED,
                stage = DiagnosticStages.TRANSPORT,
                code = sentinel,
                traceId = sentinel,
                actionId = sentinel,
                instanceId = instance,
            ),
        )
        journal.flush()

        val result = journal.read(instance)
        assertEquals(2, result.events.size)
        assertEquals(mapOf("count" to "3", "operation_id" to "send-1"), result.events.first().fields)
        assertNull(result.events.last().code)
        assertNull(result.events.last().traceId)

        val export = journal.buildExport(
            scopeId = instance,
            appBuild = sentinel,
            coreBuild = sentinel,
            coreLastFailure = CoreFailureSummary(code = sentinel, occurredAtEpochMillis = 1L),
        )
        val serialized = String(export.jsonl, StandardCharsets.UTF_8) +
            result.events.joinToString("\n") { it.toString() }
        assertFalse(serialized.contains("SENTINEL"))
        assertFalse(serialized.contains("/Users/secret"))
        assertFalse(serialized.contains("sk_live"))
        assertFalse(serialized.contains("\"message\""))
        assertNotNull(export.jsonl)
    }
}
