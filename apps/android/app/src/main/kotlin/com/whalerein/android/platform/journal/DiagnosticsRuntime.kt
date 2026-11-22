package com.whalerein.android.platform.journal

import android.content.Context
import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticSink
import com.whalerein.android.state.support.Journal
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * App-scoped diagnostic writer.
 *
 * The journal exists before the native bridge is opened so startup, sign-in and
 * Core-initialization failures are reviewable even when Core cannot load. Two
 * sinks share the one writer: [sink] records Android state events and [coreSink]
 * records Core's already-redacted events (the bridge supplies the receiver and
 * passes the raw Core event here).
 *
 * Flushes are coalesced off the UI thread; [DiagnosticSink.emit] never blocks,
 * never throws and never changes an operation outcome.
 */
class DiagnosticsRuntime(
    context: Context,
    clock: Clock,
) : AutoCloseable {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val wake = Channel<Unit>(capacity = Channel.CONFLATED)

    val store: AndroidJournalStore = AndroidJournalStore(
        File(context.noBackupFilesDir, JOURNAL_DIRECTORY),
    )

    val journal: Journal = Journal(store = store, clock = clock)

    /** Sink injected into every `:state` view model and platform component. */
    val sink: DiagnosticSink = object : DiagnosticSink {
        override fun emit(event: DiagnosticEvent) {
            journal.emit(event)
            wake.trySend(Unit)
        }
    }

    /** Sink given to `:core-bridge`; Core events keep their own journal source. */
    val coreSink: DiagnosticSink = object : DiagnosticSink {
        override fun emit(event: DiagnosticEvent) {
            journal.emitCore(event)
            wake.trySend(Unit)
        }
    }

    private val flushJob: Job = scope.launch {
        while (true) {
            wake.receive()
            delay(FLUSH_DEBOUNCE_MILLIS)
            // Drain any wakeups that arrived while flushing.
            while (wake.tryReceive().isSuccess) Unit
            runCatching { journal.flush() }
        }
    }

    private val periodicJob: Job = scope.launch {
        while (true) {
            delay(PERIODIC_FLUSH_MILLIS)
            runCatching { journal.flush() }
        }
    }

    /** Flush pending records without blocking the caller. */
    fun requestFlush() {
        wake.trySend(Unit)
    }

    override fun close() {
        flushJob.cancel()
        periodicJob.cancel()
        scope.cancel()
    }

    private companion object {
        const val JOURNAL_DIRECTORY = "diagnostics"
        const val FLUSH_DEBOUNCE_MILLIS = 400L
        const val PERIODIC_FLUSH_MILLIS = 5_000L
    }
}
