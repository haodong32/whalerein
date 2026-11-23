package com.whalerein.android.state.support

/**
 * Stable diagnostic stage names shared by Android state, `:app` and
 * `:core-bridge`.
 *
 * The names match the design's stage vocabulary. Both the local Android journal
 * and Core's redacted `DiagnosticSink` events use these values, so a reader can
 * filter a single timeline without a translation table.
 */
object DiagnosticStages {
    const val STARTUP = "startup"
    const val CONFIGURATION = "configuration"
    const val LOGIN = "login"
    const val TRANSPORT = "transport"
    const val OBSERVATION = "observation"
    const val REPLY_READ = "reply_read"
    const val COMMAND = "command"
    const val QUEUE = "queue"
    const val PENDING_INPUT = "pending_input"
    const val TRANSFER = "transfer"
    const val SAVE = "save"
    const val EXPORT = "export"
    const val PROFILE = "profile"

    val ALL: Set<String> = setOf(
        STARTUP,
        CONFIGURATION,
        LOGIN,
        TRANSPORT,
        OBSERVATION,
        REPLY_READ,
        COMMAND,
        QUEUE,
        PENDING_INPUT,
        TRANSFER,
        SAVE,
        EXPORT,
        PROFILE,
    )
}

/**
 * Stable local diagnostic event names.
 *
 * Only allowlisted [INFO_MILESTONES] are retained at `INFO`; everything else at
 * `INFO` and every `DEBUG`/delta event is excluded from the default journal.
 * `WARN` and `ERROR` are always retained after redaction.
 */
object DiagnosticEvents {
    const val STARTUP_CORE_OPENED = "startup.core_opened"
    const val STARTUP_CORE_FAILED = "startup.core_failed"
    const val CONFIGURATION_LOADED = "configuration.loaded"
    const val LOGIN_SUCCEEDED = "login.succeeded"
    const val LOGIN_FAILED = "login.failed"
    const val TRANSPORT_CONNECTED = "transport.connected"
    const val TRANSPORT_FAILED = "transport.failed"
    const val OBSERVATION_STARTED = "observation.started"
    const val OBSERVATION_STOPPED = "observation.stopped"
    const val OBSERVATION_FAILED = "observation.failed"
    const val REPLY_READ_FAILED = "reply_read.failed"
    const val COMMAND_SUBMITTED = "command.submitted"
    const val COMMAND_RECEIPT_CONFIRMED = "command.receipt_confirmed"
    const val COMMAND_UNCONFIRMED = "command.unconfirmed"
    const val COMMAND_FAILED = "command.failed"
    const val COMMAND_RECOVERED = "command.recovered"
    const val COMMAND_RECOVERY_REFUSED = "command.recovery_refused"
    const val OPERATIONS_INVENTORY_LOADED = "command.inventory_loaded"
    const val QUEUE_CHANGED = "queue.changed"
    const val PENDING_INPUT_RESOLVED = "pending_input.resolved"
    const val PROFILE_SWITCHED = "profile.switched"
    const val PROFILE_FORGOTTEN = "profile.forgotten"
    const val ACCOUNT_SIGNED_OUT = "login.signed_out"
    const val ACCOUNT_SIGN_OUT_PENDING = "login.sign_out_pending"
    const val ACCOUNT_SESSION_REVOKED = "login.app_session_revoked"
    const val ACCOUNT_SESSIONS_REVOKED_ALL = "login.app_sessions_revoked_all"
    const val ACCOUNT_GRANT_REVOKED = "login.cloud_grant_revoked"
    const val ACCOUNT_DEVICE_REVOKED = "login.standalone_device_revoked"
    const val ACCOUNT_CURRENT_DEVICE_REVOKED = "login.current_device_revoked"
    const val TRANSFER_STARTED = "transfer.started"
    const val TRANSFER_COMPLETED = "transfer.completed"
    const val TRANSFER_FAILED = "transfer.failed"
    const val TRANSFER_CANCELLED = "transfer.cancelled"
    const val SAVE_STARTED = "save.started"
    const val SAVE_COMPLETED = "save.completed"
    const val SAVE_FAILED = "save.failed"
    const val SAVE_UNCONFIRMED = "save.unconfirmed"
    const val SAVE_CANCELLED = "save.cancelled"
    const val ATTACHMENT_UNAVAILABLE = "transfer.unavailable"
    const val STAGING_DISCARDED = "transfer.staging_discarded"
    const val EXPORT_REQUESTED = "export.requested"
    const val EXPORT_COMPLETED = "export.completed"
    const val EXPORT_FAILED = "export.failed"
    const val EXPORT_UNCONFIRMED = "export.unconfirmed"
    const val DIAGNOSTICS_DEGRADED = "startup.diagnostics_degraded"
    const val DIAGNOSTICS_CLEARED = "configuration.diagnostics_cleared"

    /** `INFO` events that the default journal keeps. */
    val INFO_MILESTONES: Set<String> = setOf(
        STARTUP_CORE_OPENED,
        LOGIN_SUCCEEDED,
        PROFILE_SWITCHED,
        PROFILE_FORGOTTEN,
        OBSERVATION_STARTED,
        COMMAND_RECEIPT_CONFIRMED,
        COMMAND_RECOVERED,
        ACCOUNT_SIGNED_OUT,
        TRANSFER_COMPLETED,
        SAVE_COMPLETED,
        EXPORT_COMPLETED,
        DIAGNOSTICS_CLEARED,
    )

    /**
     * Noisy per-token names that must never reach the default journal even if a
     * caller labels them `WARN`.
     */
    val EXCLUDED_EVENTS: Set<String> = setOf(
        "content.delta",
        "content_delta",
        "reasoning.delta",
        "reasoning_delta",
        "preview.delta",
        "preview_delta",
        "progress.tick",
    )
}
