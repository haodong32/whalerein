package com.whalerein.android.corebridge.state

import com.whalerein.android.corebridge.DiagnosticEventView
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.DisplayName
import org.junit.jupiter.api.Test

class StateDiagnosticSinkAdapterTest {
    @Test
    @DisplayName("Core diagnostic events map onto the shared state sink vocabulary")
    fun `diagnostic mapping`() {
        val seen = mutableListOf<DiagnosticEvent>()
        val sink = DiagnosticSink { seen += it }
        val adapter = StateDiagnosticSinkAdapter(sink)
        adapter.onEvent(
            DiagnosticEventView(
                timeUnixMs = 5,
                traceId = "trace-1",
                level = "warn",
                event = "http_transport_failed",
                fields = mapOf("code" to "timeout", "elapsed_ms" to "12"),
            ),
        )
        assertEquals(1, seen.size)
        assertEquals(DiagnosticLevel.Warn, seen[0].level)
        assertEquals("transport", seen[0].stage)
        assertEquals("timeout", seen[0].code)
        assertEquals("trace-1", seen[0].traceId)
    }

    @Test
    @DisplayName("a throwing journal writer never breaks the Core callback")
    fun `throwing sink is contained`() {
        val adapter = StateDiagnosticSinkAdapter { error("writer failed") }
        adapter.onEvent(
            DiagnosticEventView(1, "trace", "error", "configuration_failed", emptyMap()),
        )
    }

    @Test
    @DisplayName("configuration and attachment stages use the shared names")
    fun `stage vocabulary`() {
        assertEquals(
            "configuration",
            com.whalerein.android.corebridge.diagnostics.coreDiagnosticStage("configuration_loaded"),
        )
        assertEquals(
            "transfer",
            com.whalerein.android.corebridge.diagnostics.coreDiagnosticStage("attachment_transfer"),
        )
        assertEquals(
            "",
            com.whalerein.android.corebridge.diagnostics.coreDiagnosticStage("custom_event"),
        )
    }
}
