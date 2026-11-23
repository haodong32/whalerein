// The normalized session summary every source adapter must produce. `archived`
// is a source fact: the source's own default listing hides the Session. It is
// never a WhaleRein preference, and it must not stand in for deletion or
// inaccessibility (those Sessions are not listed at all).
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
    archived: Boolean(summary.archived),
  }
}

