package com.whalerein.android.state.core

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

class CoreTypesTest {
    @Test
    @DisplayName("AC-24.inspection typed outcomes keep success failure and unconfirmed distinct")
    fun outcomesKeepDistinct() {
        val value = Outcome.Success("ok")
        assertEquals("ok", value.valueOrNull())
        assertNull(Outcome.Failure(CoreFailure("c", "m")).valueOrNull())
        assertNull(Outcome.Unconfirmed(CoreFailure("c", "m")).valueOrNull())
        assertNull(Outcome.Cancelled.valueOrNull())

        assertNull(value.failureOrNull())
        assertEquals(CoreFailure("timeout", "timed out"), Outcome.Failure(CoreFailure("timeout", "timed out")).failureOrNull())
        assertEquals(CoreFailure("unconfirmed", "unknown"), Outcome.Unconfirmed(CoreFailure("unconfirmed", "unknown")).failureOrNull())
        assertNull(Outcome.Cancelled.failureOrNull())
        assertTrue(Outcome.Failure(CoreFailure("c", "m", retryable = true)).failureOrNull()?.retryable == true)
    }

    @Test
    @DisplayName("AC-23.levels diagnostic events carry only allowlisted scalars")
    fun diagnosticsCarryAllowlistedScalars() {
        val event = DiagnosticEvent(
            level = DiagnosticLevel.Warn,
            event = "command.send.unconfirmed",
            stage = "command",
            code = "unconfirmed",
            traceId = "trace-1",
            actionId = "action-1",
            instanceId = "instance-1",
            fields = mapOf("count" to "1"),
        )
        assertEquals(DiagnosticLevel.Warn, event.level)
        assertEquals("command", event.stage)
        assertEquals(mapOf("count" to "1"), event.fields)

        val sink = DiagnosticSink { }
        sink.emit(event)
    }
}
