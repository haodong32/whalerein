package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.Outcome
import com.whalerein.android.state.core.SessionRef
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * Serialized draft owner per session. Conversation models take a lease; a new
 * claim invalidates the previous lease, so delayed saves and final releases from
 * a replaced conversation cannot overwrite the replacement's newer text.
 */
class DraftStore(private val port: DraftPort) {
    data class Lease(
        val session: SessionRef,
        val generation: Long,
    )

    data class Claim(val lease: Lease, val text: String?)

    private class Entry(
        var generation: Long = 0,
        var text: String = "",
        var revision: Long = 0,
        val mutex: Mutex = Mutex(),
    )

    private val entries = mutableMapOf<SessionRef, Entry>()

    private fun entry(session: SessionRef): Entry =
        synchronized(entries) { entries.getOrPut(session) { Entry() } }

    suspend fun claim(session: SessionRef): Outcome<Claim> {
        val entry = entry(session)
        return entry.mutex.withLock {
            entry.generation += 1
            entry.revision += 1
            when (val loaded = port.load(session)) {
                is Outcome.Success -> {
                    val text = loaded.value.orEmpty()
                    entry.text = text
                    Outcome.Success(Claim(Lease(session, entry.generation), loaded.value))
                }

                is Outcome.Failure -> loaded
                is Outcome.Unconfirmed -> loaded
                Outcome.Cancelled -> Outcome.Cancelled
            }
        }
    }

    fun update(lease: Lease, text: String): Long = synchronized(entries) {
        val entry = entry(lease.session)
        if (entry.generation != lease.generation) return entry.revision
        entry.text = text
        entry.revision += 1
        entry.revision
    }

    suspend fun setAndFlush(lease: Lease, text: String, revision: Long): Outcome<Unit> {
        val entry = entry(lease.session)
        return entry.mutex.withLock {
            if (entry.generation != lease.generation || entry.revision != revision) {
                return@withLock Outcome.Success(Unit)
            }
            entry.text = text
            when (val saved = port.save(lease.session, text)) {
                is Outcome.Success -> Outcome.Success(Unit)

                is Outcome.Failure -> saved
                is Outcome.Unconfirmed -> saved
                Outcome.Cancelled -> Outcome.Cancelled
            }
        }
    }

    suspend fun clearIfCurrent(lease: Lease, matching: String): Outcome<Unit> {
        val entry = entry(lease.session)
        return entry.mutex.withLock {
            if (entry.generation != lease.generation || entry.text != matching) {
                return@withLock Outcome.Success(Unit)
            }
            entry.revision += 1
            when (val cleared = port.clear(lease.session)) {
                is Outcome.Success -> {
                    entry.text = ""
                    Outcome.Success(Unit)
                }

                is Outcome.Failure -> cleared
                is Outcome.Unconfirmed -> cleared
                Outcome.Cancelled -> Outcome.Cancelled
            }
        }
    }

    suspend fun release(lease: Lease, text: String): Outcome<Unit> {
        val revision = update(lease, text)
        return setAndFlush(lease, text, revision)
    }
}
