// Fake DSH adapter used by component/integration tests and local runs. It
// models real session state: committed events, messages, interrupt, approvals
// with expiry and atomic one-shot consumption, credentials, and crashes.

import { AdapterError, DefinitiveExecutionError } from 'whalerein-host/errors.js'
import { approvalDigest, bytesEqual, randomId } from 'whalerein-host/security/digest.js'
import { QuestionProjection, validateQuestionAnswers } from './questions.js'
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
  #defaultModel = { provider: 'fixture-provider', model: 'fixture-model' }
  #questionProjection = new QuestionProjection()
  #questions = new Map()
  #heldQuestions = new Set()
  #waitingQuestions = new Set()

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
    this.deliveryModes = ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER']
    this.defaultApprovalTtlMs = defaultApprovalTtlMs
    this.behavior = {
      emitOutputOnMessage: behavior.emitOutputOnMessage !== false,
      longRunning: behavior.longRunning === true,
      pendingQueueEnabled: behavior.pendingQueueEnabled === true,
      approvalOnFirstMessage: behavior.approvalOnFirstMessage === true,
      approvalEveryMessage: behavior.approvalEveryMessage === true,
      approvalTtlMs: behavior.approvalTtlMs ?? defaultApprovalTtlMs,
      modelOperations: behavior.modelOperations === true,
      questions: behavior.questions === true,
      questionsOpen: behavior.questionsOpen === true,
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
      capabilities: ['sessions', 'events', 'messages', 'approvals', 'interrupt', 'sync', 'snapshot', 'session-fork', ...(this.behavior.modelOperations ? ['model-catalog', 'model-read', 'model-select'] : []), ...(this.behavior.pendingQueueEnabled ? ['pending-queue-read','pending-queue-edit','pending-queue-remove','pending-queue-steer'] : []), ...(this.behavior.questions ? ['questions-read','questions-answer-continued'] : []), ...(this.behavior.questions && this.behavior.questionsOpen ? ['questions-answer-open'] : [])],
    }
  }

  async listModels() {
    this.#check('listModels')
    if (!this.behavior.modelOperations) throw new AdapterError('unsupported_capability')
    return { default_selection: { ...this.#defaultModel }, effect_scope: 'session_and_source_default_attempt', complete: true, groups: [{ id: 'fixture-provider', name: 'Fixture', models: [{ id: 'fixture-model', name: 'Fixture model', efforts: [{ id: 'high', name: 'High' }] }] }], failures: [] }
  }

  async readSessionModel(sessionId) {
    this.#check('readSessionModel')
    if (!this.behavior.modelOperations) throw new AdapterError('unsupported_capability')
    const session = this.#session(sessionId)
    return { pending: session.modelSelection ?? null, next: session.modelSelection ?? null, pending_known: true, last_used: null, default_selection: { ...this.#defaultModel }, default_is_fallback: true }
  }

  async selectSessionModel(sessionId, selection) {
    this.#checkCommand('selectSessionModel')
    if (!this.behavior.modelOperations) throw new DefinitiveExecutionError('unsupported_capability')
    const session = this.#session(sessionId)
    if (selection.provider !== 'fixture-provider' || selection.model !== 'fixture-model' || (selection.effort && selection.effort !== 'high')) throw new DefinitiveExecutionError('invalid_argument')
    const selected = { provider: selection.provider, model: selection.model, ...(selection.effort ? { effort: selection.effort } : {}) }
    session.modelSelection = selected
    this.#defaultModel = selected
    return { selected, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' }
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

  async describeHostCapabilities() { return {...await super.describeHostCapabilities(), session_create: true, session_fork: true} }

  async executeHost(command) {
    const input = command?.create_session
    if (!input) throw new DefinitiveExecutionError('unsupported_capability')
    const {cwd, workspace_id: workspace, agent_preset: preset, preferred_session_id: preferred} = input
    for (const value of [cwd, workspace, preset, preferred]) {
      if (value !== undefined && (typeof value !== 'string' || !value.trim())) throw new DefinitiveExecutionError('invalid_argument')
    }
    if (cwd !== undefined && (workspace !== undefined || !cwd.startsWith('/'))) throw new DefinitiveExecutionError('invalid_argument')
    const existing = preferred && this.#sessions.get(preferred)
    let id = preferred && (!existing || existing.blank) ? preferred : `session-${randomId()}`
    if (!existing || id !== preferred) this.createSession({sessionId:id,cwd:cwd ?? '/host-default',blank:true})
    return {lifecycle_receipt:{session_id:id,operation:'create',disposition:'READY'}}
  }

  async forkSession(sessionId) {
    const hang = this.#checkCommand('forkSession')
    if (hang) return hang
    const parent = this.#session(sessionId)
    const lastEnd = parent.events.findLastIndex(event => event.type === 'turn/end')
    if (lastEnd < 0) throw new DefinitiveExecutionError('conflict')
    let end = lastEnd + 1
    while (end < parent.events.length && !['turn/start', 'user/message', 'agent/inbox/spliced'].includes(parent.events[end].type)) end++
    const childId = `session-${randomId()}`
    this.createSession({ sessionId: childId, cwd: parent.cwd, parentSessionId: sessionId })
    const child = this.#sessions.get(childId)
    child.events = structuredClone(parent.events.slice(0, end))
    child.nextSeq = child.events.reduce((max, event) => Math.max(max, event.seq ?? 0), 0) + 1
    return { lifecycle_receipt: { session_id: childId, parent_session_id: sessionId, operation: 'fork', disposition: 'READY' } }
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
    queue = [],
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
      pendingDeliveries: [],
      queue: structuredClone(queue),
      events: [],
      nextSeq: 1,
    }
    this.#sessions.set(sessionId, session)
    return this.summaryOf(session)
  }

  hasSession(sessionId) {
    return this.#sessions.has(sessionId)
  }

  setFixtureQuestion(sessionId, nativeQuestion) {
    this.#session(sessionId)
    if (!this.behavior.questions) throw new AdapterError('unsupported_capability')
    this.#questions.set(sessionId, [nativeQuestion])
  }

  setFixtureQuestions(sessionId, nativeQuestions) {
    this.#session(sessionId)
    if (!this.behavior.questions || !Array.isArray(nativeQuestions)) throw new AdapterError('unsupported_capability')
    this.#questions.set(sessionId, nativeQuestions)
  }

  holdFixtureQuestion(sessionId) { this.#heldQuestions.add(sessionId) }
  fixtureQuestionWaiting(sessionId) { return this.#waitingQuestions.has(sessionId) }

  async listQuestionPage(sessionId, query = {}) {
    this.#session(sessionId)
    if (!this.behavior.questions) throw new AdapterError('unsupported_capability')
    if (query.cursor) throw new AdapterError('invalid_argument')
    const native = this.#questions.get(sessionId)
    const batches = this.#questionProjection.project(sessionId, { active: native ?? [] })
    return { batches: batches.map(batch => ({ ...batch, answerable: batch.phase === 'QUESTION_CONTINUED' || this.behavior.questionsOpen })), next_cursor: '' }
  }

  async getQuestion(sessionId, batchId) {
    return (await this.listQuestionPage(sessionId)).batches.find(batch => batch.batch_id === batchId) ?? null
  }

  async answerContinuedQuestion(request) {
    const batch = await this.getQuestion(request.session_id, request.batch_id)
    if (!batch || !batch.answerable || request.expected_phase !== batch.phase || request.source_epoch !== batch.source_epoch ||
      Number(request.expected_version) !== batch.version || !bytesEqual(request.expected_digest, batch.digest)) {
      throw new DefinitiveExecutionError('conflict')
    }
    try { validateQuestionAnswers(batch.questions, request.answers) }
    catch { throw new DefinitiveExecutionError('invalid_argument') }
    this.#removeFixtureQuestion(request.session_id, batch.batch_id)
    return { batch_id: batch.batch_id, phase: 'QUESTION_REPLY_QUEUED' }
  }

  async answerOpenQuestion(request, signal) {
    if (signal?.aborted) throw new DefinitiveExecutionError('unavailable')
    const batch = await this.getQuestion(request.session_id, request.batch_id)
    if (!this.behavior.questionsOpen || !batch || batch.phase !== 'QUESTION_OPEN' || request.expected_phase !== batch.phase ||
      request.source_epoch !== batch.source_epoch || Number(request.expected_version) !== batch.version ||
      !bytesEqual(request.expected_digest, batch.digest)) throw new DefinitiveExecutionError('conflict')
    try { validateQuestionAnswers(batch.questions, request.answers) }
    catch { throw new DefinitiveExecutionError('invalid_argument') }
    if (signal?.aborted) throw new DefinitiveExecutionError('unavailable')
    if (this.#heldQuestions.has(request.session_id)) {
      this.#waitingQuestions.add(request.session_id)
      try {
        await new Promise((_, reject) => {
          if (!signal || signal.aborted) return reject(new DefinitiveExecutionError('unavailable'))
          signal.addEventListener('abort', () => reject(new DefinitiveExecutionError('unavailable')), { once: true })
        })
      } finally { this.#waitingQuestions.delete(request.session_id) }
    }
    this.#removeFixtureQuestion(request.session_id, batch.batch_id)
    return { batch_id: batch.batch_id, phase: 'QUESTION_SETTLED' }
  }

  #removeFixtureQuestion(sessionId, batchId) {
    const callId = this.#questionProjection.nativeCall(sessionId, batchId)
    const remaining = (this.#questions.get(sessionId) ?? []).filter(question => question.callId !== callId)
    if (remaining.length) this.#questions.set(sessionId, remaining)
    else this.#questions.delete(sessionId)
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

  async readPendingQueue(sessionId) {
    if (!this.behavior.pendingQueueEnabled) throw new AdapterError('unsupported_capability')
    this.#check('readPendingQueue')
    const session = this.#session(sessionId)
    if (!session.agentAvailable) return {session_id:sessionId,available:false,running:false,items:[]}
    return {session_id:sessionId,available:true,running:session.running,items:session.queue.map(item => ({
      item_id:item.id,placement:item.placement ?? 'QUEUED',delivery_id:item.deliveryId ?? '',
      blocks:[{block_id:'text-0',text:{text:item.text ?? ''}}],
      actions:item.readonly ? [] : ['EDIT','REMOVE',...(session.running && (!item.placement || item.placement === 'QUEUED') ? ['PROMOTE'] : [])],
    }))}
  }

  async updatePendingQueue(sessionId, command) {
    if (!this.behavior.pendingQueueEnabled) throw new DefinitiveExecutionError('unsupported_capability')
    this.#checkCommand('updatePendingQueue')
    if (!command.item_id || !['EDIT','REMOVE','PROMOTE'].includes(command.action) || (command.action === 'EDIT' && (typeof command.text !== 'string' || !command.text.trim()))) throw new DefinitiveExecutionError('invalid_argument')
    const session = this.#session(sessionId)
    if (!session.agentAvailable) throw new DefinitiveExecutionError('unavailable')
    const index = session.queue.findIndex(item => item.id === command.item_id)
    if (index < 0) throw new DefinitiveExecutionError('not_found')
    const item = session.queue[index]
    if (item.readonly || (command.action === 'PROMOTE' && (!session.running || (item.placement && item.placement !== 'QUEUED')))) throw new DefinitiveExecutionError('conflict')
    if (command.action === 'EDIT') { item.text = command.text; delete item.attachments }
    else if (command.action === 'REMOVE') session.queue.splice(index, 1)
    else item.placement = 'STEERING'
    return {}
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
    if (!running && session.pendingDeliveries.length) {
      const next = session.pendingDeliveries.shift()
      session.queuedPrompts--
      queueMicrotask(() => { void this.sendUserMessage(sessionId, next.message, next.mode, next.messageId) })
    }
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

  async sendUserMessage(sessionId, message, mode = 'queue', operationId) {
    const hang = this.#checkCommand('sendUserMessage')
    if (hang) return hang
    const session = this.#session(sessionId)
    if (typeof message !== 'string' || message.trim() === '') {
      throw new DefinitiveExecutionError('invalid_argument', 'message empty')
    }
    const messageId = operationId || randomId()
    return applyAction(() => {
      if (session.running) {
        const delivery = {message, mode, messageId}
        const nextTurn = session.pendingDeliveries.findIndex(item => item.mode !== 'steer')
        if (mode === 'steer' && nextTurn >= 0) session.pendingDeliveries.splice(nextTurn, 0, delivery)
        else session.pendingDeliveries.push(delivery)
        session.queuedPrompts++
        return {messageId, deliveryId: messageId}
      }
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
      if (!session.running && session.pendingDeliveries.length) this.setRunning(sessionId, false)
      this.logger?.info('dsh_message_sent', { session: sessionId })
      return { messageId, deliveryId: messageId }
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
