package com.whalerein.android.corebridge.ports

import com.whalerein.android.state.access.CloudAttempt
import com.whalerein.android.state.core.AttemptId
import com.whalerein.android.state.core.HostId
import com.whalerein.android.state.core.HostRef
import com.whalerein.android.state.core.InstanceId
import com.whalerein.android.state.core.SessionId
import com.whalerein.android.state.core.SessionRef
import java.io.File
import java.util.Properties

/**
 * Durable, app-owned access metadata beside the authoritative app document.
 *
 * Only non-secret routing and attempt metadata is retained: the PKCE verifier,
 * callback code, credential and token never reach this store. Each profile gets
 * its own file, so two instances can never observe or overwrite each other's
 * pending attempt or last target. A missing or malformed record is the same as
 * no record, which keeps first launch and upgrades safe.
 */
internal class AccessRecordStore(private val directory: File) {
    fun readPending(instance: InstanceId): CloudAttempt? {
        val values = read(file(instance, PENDING_PREFIX)) ?: return null
        val attemptId = values.getProperty(KEY_ATTEMPT_ID).orEmpty()
        val authorizationUrl = values.getProperty(KEY_AUTHORIZATION_URL).orEmpty()
        val redirectUri = values.getProperty(KEY_REDIRECT_URI).orEmpty()
        val environment = values.getProperty(KEY_ENVIRONMENT).orEmpty()
        val expiresAt = values.getProperty(KEY_EXPIRES_AT)?.toLongOrNull() ?: return null
        if (attemptId.isBlank() || authorizationUrl.isBlank() ||
            redirectUri.isBlank() || environment.isBlank()
        ) {
            return null
        }
        return CloudAttempt(
            attemptId = AttemptId(attemptId),
            authorizationUrl = authorizationUrl,
            state = values.getProperty(KEY_STATE).orEmpty(),
            redirectUri = redirectUri,
            environment = environment,
            expiresAtEpochMillis = expiresAt,
            expectedAccountId = values.getProperty(KEY_EXPECTED_ACCOUNT)?.takeIf { it.isNotBlank() },
            expectedWorkspaceId = values.getProperty(KEY_EXPECTED_WORKSPACE)?.takeIf { it.isNotBlank() },
        )
    }

    fun writePending(instance: InstanceId, attempt: CloudAttempt) {
        val values = Properties()
        values.setProperty(VERSION, RECORD_VERSION.toString())
        values.setProperty(KEY_ATTEMPT_ID, attempt.attemptId.value)
        values.setProperty(KEY_AUTHORIZATION_URL, attempt.authorizationUrl)
        values.setProperty(KEY_STATE, attempt.state)
        values.setProperty(KEY_REDIRECT_URI, attempt.redirectUri)
        values.setProperty(KEY_ENVIRONMENT, attempt.environment)
        values.setProperty(KEY_EXPIRES_AT, attempt.expiresAtEpochMillis.toString())
        attempt.expectedAccountId?.let { values.setProperty(KEY_EXPECTED_ACCOUNT, it) }
        attempt.expectedWorkspaceId?.let { values.setProperty(KEY_EXPECTED_WORKSPACE, it) }
        write(file(instance, PENDING_PREFIX), values)
    }

    /** Consume the pending attempt. A later callback must not exchange again. */
    fun clearPending(instance: InstanceId) {
        delete(file(instance, PENDING_PREFIX))
    }

    fun readTarget(instance: InstanceId): SessionRef? {
        val values = read(file(instance, TARGET_PREFIX)) ?: return null
        val hostId = values.getProperty(KEY_HOST_ID).orEmpty()
        val sessionId = values.getProperty(KEY_SESSION_ID).orEmpty()
        if (hostId.isBlank() || sessionId.isBlank()) return null
        return SessionRef(HostRef(instance, HostId(hostId)), SessionId(sessionId))
    }

    fun writeTarget(instance: InstanceId, target: SessionRef) {
        val values = Properties()
        values.setProperty(VERSION, RECORD_VERSION.toString())
        values.setProperty(KEY_HOST_ID, target.host.hostId.value)
        values.setProperty(KEY_SESSION_ID, target.sessionId.value)
        write(file(instance, TARGET_PREFIX), values)
    }

    fun clearTarget(instance: InstanceId) {
        delete(file(instance, TARGET_PREFIX))
    }

    private fun file(instance: InstanceId, prefix: String): File =
        File(directory, "$prefix-${instance.value}.properties")

    private fun read(file: File): Properties? = try {
        if (!file.isFile) {
            null
        } else {
            Properties().apply { file.inputStream().use { stream -> load(stream) } }
        }
    } catch (_: Exception) {
        // A torn or unreadable record is absence, never a crash on launch.
        null
    }

    private fun write(file: File, values: Properties) {
        try {
            directory.mkdirs()
            val temporary = File(directory, "${file.name}.tmp")
            temporary.outputStream().use { values.store(it, null) }
            if (!temporary.renameTo(file)) {
                file.delete()
                if (!temporary.renameTo(file)) temporary.delete()
            }
        } catch (_: Exception) {
            // Access metadata is an optimization for recovery. A read-only
            // sandbox must not turn a valid flow into a hard failure.
        }
    }

    private fun delete(file: File) {
        try {
            file.delete()
        } catch (_: Exception) {
            // Same containment as write().
        }
    }

    companion object {
        /**
         * Pinned lifetime for one native Cloud attempt.
         *
         * Core retains the Cloud-supplied `expiresAt` (bounded by its 20-minute
         * login ceiling) inside the SecretStore, but the mobile binding exposes
         * only the URL and attempt identity, so the app cannot read the exact
         * per-attempt deadline. The audited Cloud flow issues a 600-second
         * attempt, which is also what the Core tests exercise. Using that named
         * value keeps an expired attempt from being presented as usable while
         * still letting Core reject anything the server expired earlier.
         */
        const val PENDING_ATTEMPT_LIFETIME_MILLIS: Long = 600_000L

        private const val RECORD_VERSION = 1
        private const val VERSION = "version"
        private const val PENDING_PREFIX = "pending-attempt"
        private const val TARGET_PREFIX = "last-target"
        private const val KEY_ATTEMPT_ID = "attemptId"
        private const val KEY_AUTHORIZATION_URL = "authorizationUrl"
        private const val KEY_STATE = "state"
        private const val KEY_REDIRECT_URI = "redirectUri"
        private const val KEY_ENVIRONMENT = "environment"
        private const val KEY_EXPIRES_AT = "expiresAtEpochMillis"
        private const val KEY_EXPECTED_ACCOUNT = "expectedAccountId"
        private const val KEY_EXPECTED_WORKSPACE = "expectedWorkspaceId"
        private const val KEY_HOST_ID = "hostId"
        private const val KEY_SESSION_ID = "sessionId"

        /** Store beside the authoritative app document, not inside Core data. */
        fun besideConfig(appConfigPath: String): AccessRecordStore {
            val parent = File(appConfigPath).parentFile ?: File(".")
            return AccessRecordStore(File(parent, "access"))
        }
    }
}
