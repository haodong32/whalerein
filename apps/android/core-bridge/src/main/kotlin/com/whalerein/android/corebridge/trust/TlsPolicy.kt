package com.whalerein.android.corebridge.trust

import java.net.URI

/**
 * Production-like transport policy for a Standalone target.
 *
 * Cleartext is rejected before Core sees the target. A private CA is scoped to
 * the instance by Core's configuration; this adapter never installs a
 * process-wide trust anchor.
 */
object TlsPolicy {
    sealed interface Result {
        data object Accepted : Result

        data class Rejected(val code: String) : Result
    }

    fun validateStandaloneTarget(url: String): Result {
        val parsed = try {
            URI(url)
        } catch (_: Exception) {
            return Result.Rejected("invalid_target")
        }
        if (parsed.scheme != "https") return Result.Rejected("cleartext_target")
        if (parsed.host.isNullOrEmpty()) return Result.Rejected("invalid_target")
        if (parsed.userInfo != null || parsed.fragment != null) {
            return Result.Rejected("invalid_target")
        }
        return Result.Accepted
    }
}
