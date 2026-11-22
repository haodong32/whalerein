package com.whalerein.android.state.conversation

import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticLevel
import com.whalerein.android.state.core.DiagnosticSink

/**
 * Conversation diagnostic vocabulary.
 *
 * Only the five owned stages are emitted. `INFO` is reserved for a small set of
 * allowlisted milestones (observation start/stop, send/queue acceptance,
 * recovery, pending-input transition); every WARN/ERROR records a typed code and
 * counts. Message text, reasoning, drafts, tool arguments, question text and
 * paths must never be passed here.
 */
object ConversationDiagnosticStage {
    const val OBSERVATION = "observation"
    const val REPLY_READ = "reply_read"
    const val COMMAND = "command"
    const val QUEUE = "queue"
    const val PENDING_INPUT = "pending_input"
}

object ConversationDiagnosticEventName {
    const val WATCH_STARTED = "conversation.watch.started"
    const val WATCH_STOPPED = "conversation.watch.stopped"
    const val WATCH_FAILED = "conversation.watch.failed"
    const val EVENTS_DROPPED = "conversation.events.dropped"
    const val RESYNC_STARTED = "conversation.resync.started"
    const val SEND_ACCEPTED = "conversation.send.accepted"
    const val SEND_FAILED = "conversation.send.failed"
    const val SEND_UNCONFIRMED = "conversation.send.unconfirmed"
    const val REPLY_UNREADABLE = "conversation.reply.unreadable"
    const val RECOVERY_FINISHED = "conversation.recovery.finished"
    const val STOP_FAILED = "conversation.stop.failed"
    const val QUEUE_OPERATION = "conversation.queue.operation"
    const val PENDING_INPUT_OPERATION = "conversation.pending_input.operation"
    const val PENDING_INPUT_STALE = "conversation.pending_input.stale"
    const val SESSION_LIST_LOADED = "conversation.session_list.loaded"
    const val SESSION_LIST_FAILED = "conversation.session_list.failed"
    const val SESSION_OPERATION = "conversation.session.operation"
}

class ConversationDiagnostics(private val sink: DiagnosticSink) {
    fun info(
        stage: String,
        event: String,
        actionId: String? = null,
        instanceId: String? = null,
        code: String? = null,
        fields: Map<String, String> = emptyMap(),
    ) = emit(DiagnosticLevel.Info, stage, event, actionId, instanceId, code, fields)

    fun warn(
        stage: String,
        event: String,
        actionId: String? = null,
        instanceId: String? = null,
        code: String? = null,
        fields: Map<String, String> = emptyMap(),
    ) = emit(DiagnosticLevel.Warn, stage, event, actionId, instanceId, code, fields)

    fun error(
        stage: String,
        event: String,
        actionId: String? = null,
        instanceId: String? = null,
        code: String? = null,
        fields: Map<String, String> = emptyMap(),
    ) = emit(DiagnosticLevel.Error, stage, event, actionId, instanceId, code, fields)

    private fun emit(
        level: DiagnosticLevel,
        stage: String,
        event: String,
        actionId: String?,
        instanceId: String?,
        code: String?,
        fields: Map<String, String>,
    ) {
        // A diagnostic sink must never change an operation outcome.
        runCatching {
            sink.emit(
                DiagnosticEvent(
                    level = level,
                    event = event,
                    stage = stage,
                    code = code,
                    actionId = actionId,
                    instanceId = instanceId,
                    fields = fields,
                ),
            )
        }
    }
}
