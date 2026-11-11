export function summaryToProto(summary) {
  return {
    session_id: summary.sessionId,
    title: summary.title ?? '',
    cwd: summary.cwd ?? '',
    created_at_unix_ms: summary.createdAt ?? 0,
    updated_at_unix_ms: summary.updatedAt ?? 0,
    running: Boolean(summary.running),
    agent_available: Boolean(summary.agentAvailable),
    blank: Boolean(summary.blank),
    parent_session_id: summary.parentSessionId ?? '',
    origin: summary.origin ?? '',
    history_cursor: summary.historyCursor ?? '',
    source_capabilities: summary.sourceCapabilities,
    queued_prompts: summary.queuedPrompts ?? 0,
  }
}

