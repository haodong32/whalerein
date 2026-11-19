// Independently shaped text-only SourceAdapter fixture. It deliberately shares
// no implementation with FakeDshAdapter or the DSH history/cursor machinery.
// Optional expanded-conversation features stay unsupported instead of being
// downgraded or simulated.
import { SourceAdapter } from 'whalerein-host/contracts/source.js'
import { AdapterError } from 'whalerein-host/errors.js'

const CURSOR = (sessionId, seq) => Buffer.from(JSON.stringify(['text', sessionId, seq])).toString('base64url')

function cursorPosition(token, sessionId) {
  if (typeof token !== 'string' || token === '' || token.length > 4096) throw new AdapterError('snapshot_required')
  let value
  try { value = JSON.parse(Buffer.from(token, 'base64url').toString('utf8')) } catch { throw new AdapterError('snapshot_required') }
  if (!Array.isArray(value) || value.length !== 3 || value[0] !== 'text' || value[1] !== sessionId || !Number.isSafeInteger(value[2]) || value[2] < -1) {
    throw new AdapterError('snapshot_required')
  }
  return value[2]
}

export class TextOnlySource extends SourceAdapter {
  #sessions = new Map()
  #watchers = new Map()
  #revision = 0

  constructor({ sessions = [] } = {}) {
    super()
    for (const session of sessions) this.addSession(session)
  }

  addSession({ sessionId, title = '', messages = [] }) {
    this.#sessions.set(sessionId, {
      sessionId, title, running: false, nextSeq: 1,
      events: messages.map(message => ({
        seq: 0,
        messageId: `message-${message.role}-${messages.indexOf(message)}`,
        role: message.role ?? 'USER',
        text: message.text ?? '',
      })),
    })
    const session = this.#sessions.get(sessionId)
    session.events = session.events.map((event, index) => ({ ...event, seq: index + 1 }))
    session.nextSeq = session.events.length + 1
  }

  async open({ metadata } = {}) { this.metadata = metadata }
  async close() { this.#watchers.clear() }

  capabilities() {
    return {
      history: true,
      resumable_history: true,
      live_text: true,
      live_reasoning: false,
      tool_activity: false,
      conversation_notices: false,
      attachments: false,
      history_page: false,
      history_exact: false,
      send_followup: false,
      interrupt: false,
      approvals: false,
      delivery_modes: ['START_IF_IDLE'],
      pending_queue_read: false,
      pending_queue_edit: false,
      pending_queue_remove: false,
      pending_queue_steer: false,
      questions_read: false,
      questions_answer_open: false,
      questions_answer_continued: false,
      session_fork: false,
    }
  }

  async listSessions({ signal } = {}) {
    if (signal?.aborted) throw new AdapterError('unavailable')
    return [...this.#sessions.values()].map(session => this.#summary(session))
  }

  async getSession(sessionId, options) {
    const session = this.#sessions.get(sessionId)
    if (!session) throw new AdapterError('not_found')
    return this.#summary(session)
  }

  #summary(session) {
    return {
      sessionId: session.sessionId,
      title: session.title,
      cwd: '',
      createdAt: 0,
      updatedAt: 0,
      running: session.running,
      agentAvailable: true,
      blank: session.events.length === 0,
      parentSessionId: '',
      origin: '',
      queuedPrompts: 0,
      historyCursor: '',
      sourceCapabilities: this.capabilities(),
    }
  }

  #message(session, event) {
    return {
      message_id: event.messageId,
      role: event.role,
      blocks: [{ block_id: 'text-0', text: { text: event.text } }],
      authored_at_unix_ms: event.seq,
    }
  }

  watch(sessionId, sink) {
    const sinks = this.#watchers.get(sessionId) ?? new Set()
    sinks.add(sink)
    this.#watchers.set(sessionId, sinks)
    return { close: () => { sinks.delete(sink); if (sinks.size === 0) this.#watchers.delete(sessionId) } }
  }

  append(sessionId, { role = 'ASSISTANT', text = '' } = {}) {
    const session = this.#sessions.get(sessionId)
    if (!session) throw new AdapterError('not_found')
    const event = {
      seq: session.nextSeq++,
      messageId: `message-${role.toLowerCase()}-${session.nextSeq}`,
      role,
      text,
    }
    session.events.push(event)
    const from = CURSOR(sessionId, event.seq - 1)
    const to = CURSOR(sessionId, event.seq)
    const update = {
      session: { session_id: sessionId },
      history: {
        from_cursor: from,
        to_cursor: to,
        batch_id: `text-batch-${event.seq}`,
        projection_id: 'text-projection-1',
        changes: [{ message: this.#message(session, event) }],
      },
    }
    for (const sink of this.#watchers.get(sessionId) ?? []) sink(update)
    return to
  }

  async readHistory(sessionId, read = {}, { signal } = {}) {
    if (signal?.aborted) throw new AdapterError('unavailable')
    const session = this.#sessions.get(sessionId)
    if (!session) throw new AdapterError('not_found')
    if (read.page != null || read.exact != null || (read.continuation !== undefined && read.continuation !== '')) {
      throw new AdapterError('unsupported_capability', 'independent history selection unavailable')
    }
    const head = session.events.at(-1)?.seq ?? 0
    const after = read.after_cursor ? cursorPosition(read.after_cursor, sessionId) : 0
    const cut = read.cut_cursor ? cursorPosition(read.cut_cursor, sessionId) : head
    if (after > cut || cut > head) throw new AdapterError('snapshot_required')
    const selected = session.events.filter(event => event.seq > after && event.seq <= cut)
    const limit = Math.max(1, Math.min(read.max_messages || 100, 1000))
    const changes = selected.slice(0, limit).map(event => ({ message: this.#message(session, event) }))
    const inspected = changes.length === selected.length ? cut : changes.at(-1)?.message ? after + changes.length : after
    return {
      session: { session_id: sessionId },
      batch: {
        from_cursor: CURSOR(sessionId, after),
        to_cursor: CURSOR(sessionId, inspected),
        batch_id: `text-batch-${after}-${inspected}`,
        projection_id: 'text-projection-1',
        changes,
      },
      cut_cursor: CURSOR(sessionId, cut),
      projection_id: 'text-projection-1',
      source: 'HISTORY_SOURCE_ONLINE',
      complete: inspected === cut,
      snapshot: read.snapshot === true,
    }
  }

  async readState(sessionId) {
    const session = this.#sessions.get(sessionId)
    if (!session) throw new AdapterError('not_found')
    return {
      session: { session_id: sessionId },
      live_epoch: 'text-epoch-1',
      revision: ++this.#revision,
      history_head: CURSOR(sessionId, session.events.at(-1)?.seq ?? 0),
      run_phase: 'IDLE',
      approvals: [],
    }
  }
}
