package com.whalerein.android.state.support

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * Optional background drain for [Journal].
 *
 * The journal queue accepts events synchronously and drops instead of blocking
 * when full. A single writer loop owned by `:app` flushes it periodically; the
 * journal itself remains usable without the loop (tests call [Journal.flush]).
 */
class JournalWriter(
    private val journal: Journal,
    private val intervalMillis: Long = DEFAULT_INTERVAL_MILLIS,
) {
    fun start(scope: CoroutineScope): Job = scope.launch {
        while (isActive) {
            delay(intervalMillis)
            journal.flush()
        }
    }

    suspend fun flushNow() {
        journal.flush()
    }

    companion object {
        const val DEFAULT_INTERVAL_MILLIS: Long = 1_000L
    }
}
