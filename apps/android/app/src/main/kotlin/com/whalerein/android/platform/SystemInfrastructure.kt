package com.whalerein.android.platform

import android.os.SystemClock as AndroidSystemClock
import com.whalerein.android.state.core.Clock
import com.whalerein.android.state.core.IdGenerator
import java.util.UUID

/** Wall-clock and monotonic time for the installed app. */
object AndroidClock : Clock {
    override fun epochMillis(): Long = System.currentTimeMillis()

    override fun elapsedMillis(): Long = AndroidSystemClock.elapsedRealtime()
}

/** Opaque, collision-resistant operation identities. */
object UuidIdGenerator : IdGenerator {
    override fun newId(): String = UUID.randomUUID().toString()
}
