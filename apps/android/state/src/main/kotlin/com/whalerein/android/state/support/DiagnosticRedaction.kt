package com.whalerein.android.state.support

import com.whalerein.android.state.core.DiagnosticEvent

/**
 * Allowlist redaction for diagnostic payloads.
 *
 * Nothing here attempts to understand user data. Instead every identifier and
 * field value must be a short, path-free, whitespace-free token. Field keys must
 * come from [ALLOWED_FIELD_KEYS]. A payload that cannot be proven safe is
 * dropped, and a sensitive-looking key is dropped even when its value looks
 * harmless. The journal therefore never serializes content, reasoning, drafts,
 * tool arguments, question text, raw paths, callback URLs, credentials, invite
 * material, tokens or keys.
 */
object DiagnosticRedaction {
    private val SAFE_TOKEN = Regex("^[A-Za-z0-9][A-Za-z0-9._:@+\\-]{0,127}$")

    private val SENSITIVE_KEY_FRAGMENTS = listOf(
        "token",
        "secret",
        "credential",
        "password",
        "passwd",
        "key",
        "invite",
        "pairing",
        "callback",
        "path",
        "content",
        "message",
        "reasoning",
        "draft",
        "tool",
        "question",
        "answer",
        "url",
        "uri",
        "header",
        "cookie",
        "fingerprint",
    )

    /**
     * Small deterministic field names. Values are still validated separately;
     * membership here is necessary but not sufficient.
     */
    val ALLOWED_FIELD_KEYS: Set<String> = setOf(
        "stage",
        "code",
        "detail",
        "reason",
        "status",
        "method",
        "source",
        "scope",
        "operation_kind",
        "duration_ms",
        "elapsed_ms",
        "deadline_ms",
        "bytes",
        "size_bytes",
        "count",
        "attempt",
        "queue_depth",
        "progress",
        "status_code",
        "http_status",
        "retryable",
        "trace_id",
        "action_id",
        "session_id",
        "host_id",
        "account_id",
        "attachment_id",
        "operation_id",
        "build_id",
        "core_version",
    )

    private val ALLOWED_FIELD_KEYS_LOWER: Set<String> =
        ALLOWED_FIELD_KEYS.map { it.lowercase() }.toSet()

    /**
     * Returns a redacted copy, or `null` when the event name/stage cannot be
     * proven safe and the whole record must be dropped.
     */
    fun sanitize(event: DiagnosticEvent): DiagnosticEvent? {
        val name = event.event.trim()
        val stage = event.stage.trim()
        if (!isSafeToken(name) || !isSafeToken(stage)) return null
        if (event.event in DiagnosticEvents.EXCLUDED_EVENTS) return null
        if (event.event.contains(".delta")) return null

        return event.copy(
            event = name,
            stage = stage,
            code = event.code?.trim()?.takeIf { isSafeToken(it) },
            traceId = event.traceId?.trim()?.takeIf { isSafeToken(it) },
            actionId = event.actionId?.trim()?.takeIf { isSafeToken(it) },
            instanceId = event.instanceId?.trim()?.takeIf { isSafeToken(it) },
            fields = sanitizeFields(event.fields),
        )
    }

    fun sanitizeFields(fields: Map<String, String>): Map<String, String> {
        if (fields.isEmpty()) return emptyMap()
        val safe = LinkedHashMap<String, String>(fields.size)
        for ((rawKey, rawValue) in fields) {
            val key = rawKey.trim()
            if (key.isEmpty() || key.length > 64) continue
            if (key.lowercase() !in ALLOWED_FIELD_KEYS_LOWER) continue
            if (isSensitiveKey(key)) continue
            val value = rawValue.trim()
            if (!isSafeToken(value)) continue
            safe[key] = value
        }
        return safe
    }

    fun isSafeToken(value: String): Boolean = SAFE_TOKEN.matches(value)

    private fun isSensitiveKey(key: String): Boolean {
        val lower = key.lowercase()
        return SENSITIVE_KEY_FRAGMENTS.any { lower.contains(it) }
    }
}
