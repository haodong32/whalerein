// Fake DSH adapter used by component/integration tests and local runs. It
// models real session state: committed events, messages, interrupt, approvals
// with expiry and atomic one-shot consumption, credentials, and crashes.

import { AdapterError, DefinitiveExecutionError } from 'whalerein-host/errors.js'
import { approvalDigest, randomId } from 'whalerein-host/security/digest.js'
import {
  DshAdapter,
  normalizeApproval,
  normalizeEvent,
  normalizeSummary,
} from './base.js'

export class FakeDshAdapter extends DshAdapter {
  #sessions = new Map()
  #eventSubscribers = new Set()
  #approvalSubscribers = new Set()
  #approvals = new Map()
  #crashed = false
  #failNext = new Map()
  #hangNext = new Set()
  #firstMessage = true

  /**
   * @param {object} [options]
   * @param {() => number} [options.now]
   * @param {Array<object>} [options.sessions] initial sessions.
   * @param {Map<string,string>} [options.credentials] initial credential values.
   * @param {string} [options.credentialFile] test-only credential file backing.
   * @param {number} [options.defaultApprovalTtlMs]
   * @param {object} [options.behavior] deterministic FakeDSH behavior:
   *   emitOutputOnMessage (default true), longRunning (false),
   *   approvalOnFirstMessage (false), approvalTtlMs.
   */
  constructor({
    now = Date.now,
    sessions = [],
    defaultApprovalTtlMs = 300_000,
    behavior = {},
    ...rest
  } = {}) {
    super({ now, ...rest })
    this.defaultApprovalTtlMs = defaultApprovalTtlMs
    this.behavior = {
      emitOutputOnMessage: behavior.emitOutputOnMessage !== false,
      longRunning: behavior.longRunning === true,
      approvalOnFirstMessage: behavior.approvalOnFirstMessage === true,
      approvalEveryMessage: behavior.approvalEveryMessage === true,
      approvalTtlMs: behavior.approvalTtlMs ?? defaultApprovalTtlMs,
    }
    for (const session of sessions) this.createSession(session)
  }

  async open(options) {
    await super.open(options)
    this.opened = true
  }

  async close() {
    await super.close()
    this.opened = false
    this.#eventSubscribers.clear()
  }

  describe() {
    return {
      mode: 'fake',
      available: !this.#crashed,
      capabilities: ['sessions', 'events', 'messages', 'approvals', 'interrupt', 'sync', 'snapshot'],
    }
  }

  // --- crash / failure injection -----------------------------------------

  crash() {
    this.#crashed = true
  }

  recover() {
    this.#crashed = false
  }

  failNext(method, code = 'unavailable') {
    this.#failNext.set(method, code)
  }

  hangNext(method) {
    this.#hangNext.add(method)
  }

  #check(method) {
    if (this.#crashed) throw new AdapterError('unavailable', 'fake adapter crashed')
    const failCode = this.#failNext.get(method)
    if (failCode) {
      this.#failNext.delete(method)
      throw new AdapterError(failCode, 'injected failure')
    }
    if (this.#hangNext.has(method)) {
      this.#hangNext.delete(method)
      return new Promise(() => {})
    }
    return null
  }

  #checkCommand(method) {
    try { return this.#check(method) }
    catch (error) { throw new DefinitiveExecutionError(error.code) }
  }

  // --- sessions -----------------------------------------------------------

  createSession({
    sessionId = `session-${this.#sessions.size + 1}`,
    title = '',
    cwd = '',
    createdAt = this.now(),
    updatedAt = this.now(),
    running = false,
    agentAvailable = true,
    blank = false,
    parentSessionId = '',
    origin = '',
    queuedPrompts = 0,
  } = {}) {
    if (this.#sessions.has(sessionId)) throw new AdapterError('conflict', 'session exists')
    const session = {
      sessionId,
      title,
      cwd,
      createdAt,
      updatedAt,
      running,
      agentAvailable,
      blank,
      parentSessionId,
      origin,
      queuedPrompts,
      events: [],
      nextSeq: 1,
    }
    this.#sessions.set(sessionId, session)
    return this.summaryOf(session)
  }

  hasSession(sessionId) {
    return this.#sessions.has(sessionId)
  }

  #session(sessionId) {
    const session = this.#sessions.get(sessionId)
    if (!session) throw new DefinitiveExecutionError('not_found', 'session not found')
    return session
  }

  summaryOf(session) {
    return normalizeSummary({
      sessionId: session.sessionId,
      title: session.title,
      cwd: session.cwd,
      createdAt: session.createdAt,
      updatedAt: session.updatedAt,
      running: session.running,
      agentAvailable: session.agentAvailable,
      blank: session.blank,
      parentSessionId: session.parentSessionId,
      origin: session.origin,
      queuedPrompts: session.queuedPrompts,
    })
  }

  async listNativeSessions() {
    this.#check('listSessions')
    return [...this.#sessions.values()]
      .sort((a, b) => b.updatedAt - a.updatedAt)
      .map((session) => this.summaryOf(session))
  }

  async nativeHead(sessionId) { return this.#session(sessionId).nextSeq - 1 }

  async getSessionSnapshot(sessionId) {
    this.#check('getSessionSnapshot')
    const session = this.#session(sessionId)
    const events = session.events.map((event) => this.#normalize(session, event))
    return {
      summary: this.summaryOf(session),
      events,
      cursor: events.length > 0 ? events[events.length - 1].dshSeq ?? 0 : 0,
      complete: true,
      source: 'dsh',
    }
  }

  setRunning(sessionId, running) {
    const session = this.#session(sessionId)
    session.running = running
    session.updatedAt = this.now()
  }

  // --- committed events ---------------------------------------------------

  /**
   * Append one committed DSH event. Delivery is synchronous, like the real
   * `session/event` feed.
   */
  emit(sessionId, { type = 'assistant/message', kind = 'output', data = null, createdAt = this.now(), ephemeral = false, suppressNotification = false } = {}) {
    this.#check('emit')
    const session = this.#session(sessionId)
    const event = {
      seq: ephemeral ? null : session.nextSeq++,
      type,
      kind,
      data,
      createdAt,
      ephemeral,
      time: createdAt,
    }
    session.updatedAt = createdAt
    if (!ephemeral) session.events.push(event)
    if(!suppressNotification)this.observeNative(this.#normalize(session,event))
    for (const subscriber of suppressNotification ? [] : this.#eventSubscribers) {
      try {
        subscriber(this.#normalize(session, event))
      } catch {
        // adapter feeds contain observer failures
      }
    }
    return event
  }

  /** Append ordinary output and return its DSH sequence. */
  output(sessionId, text, options = {}) {
    return this.emit(sessionId, {
      type: 'assistant/message',
      kind: 'output',
      data: { text },
      ...options,
    })
  }

  subscribeEvents(callback) {
    this.#eventSubscribers.add(callback)
    return () => this.#eventSubscribers.delete(callback)
  }

  #normalize(session, event) {
    return normalizeEvent({
      sessionId: session.sessionId,
      type: event.type,
      kind: event.kind,
      data: event.data,
      createdAt: event.createdAt,
      dshSeq: event.seq,
      ephemeral: event.ephemeral,
    })
  }

  // --- messages / interrupt ----------------------------------------------

  async sendUserMessage(sessionId, message) {
    const hang = this.#checkCommand('sendUserMessage')
    if (hang) return hang
    const session = this.#session(sessionId)
    if (typeof message !== 'string' || message.trim() === '') {
      throw new DefinitiveExecutionError('invalid_argument', 'message empty')
    }
    const messageId = randomId()
    return applyAction(() => {
      session.running = true
      session.updatedAt = this.now()
      this.emit(sessionId, { type: 'user/message', kind: 'output', data: { text: message } })
      if (this.behavior.emitOutputOnMessage) {
        this.emit(sessionId, { type: 'assistant/message', kind: 'output', data: { text: message } })
      }
      if (this.behavior.longRunning) {
        this.emit(sessionId, { type: 'turn/start', kind: 'session_state', data: { running: true } })
      } else {
        session.running = false
        this.emit(sessionId, { type: 'turn/end', kind: 'session_state', data: { running: false } })
      }
      if (this.behavior.approvalEveryMessage || (this.behavior.approvalOnFirstMessage && this.#firstMessage)) {
        this.#firstMessage = false
        session.running = true
        this.requestApproval({
          sessionId,
          toolName: 'bash',
          reason: 'deterministic FakeDSH approval',
          ttlMs: this.behavior.approvalTtlMs,
        })
      }
      this.logger?.info('dsh_message_sent', { session: sessionId })
      return { messageId }
    })
  }

  async interrupt(sessionId) {
    const hang = this.#checkCommand('interrupt')
    if (hang) return hang
    const session = this.#session(sessionId)
    return applyAction(() => {
      session.running = false
      session.updatedAt = this.now()
      this.emit(sessionId, { type: 'session/interrupted', kind: 'session_state', data: { interrupted: true } })
      return { accepted: true }
    })
  }

  // --- approvals ----------------------------------------------------------

  subscribeApprovals(callback) {
    this.#approvalSubscribers.add(callback)
    return () => this.#approvalSubscribers.delete(callback)
  }

  #emitApproval(type, approval) {
    this.observeApproval({type,approval:normalizeApproval(approval)})
    for (const subscriber of this.#approvalSubscribers) {
      try {
        subscriber({ type, approval: normalizeApproval(approval) })
      } catch {
        // observer failures are contained
      }
    }
  }

  #expire(approval) {
    if (approval.state === 'pending' && approval.expiresAt <= this.now()) {
      approval.state = 'expired'
      this.#emitApproval('resolved', approval)
      return true
    }
    return false
  }

  /**
   * Ask for a fake approval and notify subscribers. The returned value is a
   * snapshot; use getApproval for current state.
   */
  requestApproval({
    sessionId,
    toolName = 'bash',
    callId = randomId(8),
    reason = '',
    ttlMs = this.defaultApprovalTtlMs,
    version = 1,
    approvalId = randomId(),
  } = {}) {
    this.#check('requestApproval')
    this.#session(sessionId)
    const expiresAt = this.now() + ttlMs
    const approval = {
      approvalId,
      sessionId,
      toolName,
      callId,
      reason,
      version,
      expiresAt,
      state: 'pending',
      decision: 'unspecified',
      digest: approvalDigest({ approvalId, sessionId, toolName, callId, reason, version, expiresAt }),
      createdAt: this.now(),
    }
    this.#approvals.set(approvalId, approval)
    this.#emitApproval('requested', approval)
    return normalizeApproval(approval)
  }

  async getApproval(approvalId) {
    this.#check('getApproval')
    const approval = this.#approvals.get(approvalId)
    if (!approval) return null
    this.#expire(approval)
    return normalizeApproval(approval)
  }

  async listApprovals(sessionId = '') {
    const out = []
    for (const approval of this.#approvals.values()) {
      if (sessionId && approval.sessionId !== sessionId) continue
      if (this.#expire(approval)) continue
      if (approval.state !== 'pending') continue
      out.push(normalizeApproval(approval))
    }
    return out
  }

  /**
   * Validate and consume one approval. All validation and the state mutation
   * happen synchronously before any await, so two callers cannot both win.
   */
  async resolveApproval(approvalId, decision, expectedDigest, expectedVersion) {
    const hang = this.#checkCommand('resolveApproval')
    if (hang) return hang
    const approval = this.#approvals.get(approvalId)
    if (!approval) throw new DefinitiveExecutionError('approval_unavailable', 'approval unknown')
    this.#expire(approval)
    if (approval.state === 'expired') throw new DefinitiveExecutionError('approval_expired', 'approval expired')
    if (approval.state !== 'pending') throw new DefinitiveExecutionError('approval_already_resolved', 'approval resolved')
    if (!(expectedDigest instanceof Uint8Array) || !sameBytes(expectedDigest, approval.digest)) {
      throw new DefinitiveExecutionError('approval_version_mismatch', 'approval digest mismatch')
    }
    if (Number(expectedVersion) !== approval.version) {
      throw new DefinitiveExecutionError('approval_version_mismatch', 'approval version mismatch')
    }
    const session = this.#sessions.get(approval.sessionId)
    if (!session || !session.running || !session.agentAvailable) {
      throw new DefinitiveExecutionError('approval_unavailable', 'session not runnable')
    }
    if (decision !== 'approve' && decision !== 'reject') {
      throw new DefinitiveExecutionError('invalid_argument', 'approval decision invalid')
    }
    return applyAction(() => {
      approval.state = decision === 'approve' ? 'approved' : 'rejected'
      approval.decision = decision
      this.#emitApproval('resolved', approval)
      this.logger?.info('approval_resolved', { approval: approvalId, status: approval.state })
      return { state: approval.state, decision: approval.decision }
    })
  }

  /** Simulate another answerer consuming the approval first. */
  async simulateResolution(approvalId, decision = 'reject') {
    const approval = this.#approvals.get(approvalId)
    if (!approval) throw new AdapterError('approval_unavailable', 'approval unknown')
    return this.resolveApproval(approvalId, decision, approval.digest, approval.version)
  }


}

function sameBytes(a, b) {
  if (!(a instanceof Uint8Array) || !(b instanceof Uint8Array) || a.length !== b.length) return false
  let diff = 0
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i]
  return diff === 0
}

// Once a mutation starts, nested observer or logging failures cannot prove that
// the enclosing action failed, even if a nested operation carried that proof.
function applyAction(action) {
  try { return action() }
  catch (error) { throw new AdapterError(error instanceof AdapterError ? error.code : 'internal') }
}
