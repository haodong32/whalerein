// Fake DSH adapter used by component/integration tests and local runs. It
// models real session state: committed events, messages, interrupt, approvals
// with expiry and atomic one-shot consumption, credentials, and crashes.

import { createHash } from 'node:crypto'
import { AdapterError, DefinitiveExecutionError } from 'whalerein-host/errors.js'
import { approvalDigest, bytesEqual, randomId } from 'whalerein-host/security/digest.js'
import { QuestionProjection, QuestionStampTracker, questionInventoryFingerprint, validateQuestionAnswers } from './questions.js'
import {
  APPROVAL_REASON_MAX,
  DshAdapter,
  boundedApprovalText,
  normalizeApproval,
  normalizeEvent,
  normalizeSummary,
} from './base.js'
import {
  boundReviewValue,
  describeApprovalRequest,
  normalizeReviewAvailability,
  reviewAvailabilityFor,
} from './review.js'

export class FakeDshAdapter extends DshAdapter {
  #sessions = new Map()
  #eventSubscribers = new Set()
  #approvalSubscribers = new Set()
  #approvals = new Map()
  #approvalVersion = 0
  #crashed = false
  #failNext = new Map()
  #hangNext = new Set()
  #firstMessage = true
  #defaultModel = { provider: 'fixture-provider', model: 'fixture-model' }
  #questionProjection = new QuestionProjection()
  #questionStamps = new QuestionStampTracker()
  #questionOwners = new Map()
  #questions = new Map()
  #heldQuestions = new Set()
  #waitingQuestions = new Set()
  #questionInterrupts = new Map()
  #attachmentStore = new Map()

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
    this.approvalMode = behavior.approvalMode === 'delegate' ? 'delegate' : 'claim'
    this.defaultApprovalTtlMs = defaultApprovalTtlMs
    this.behavior = {
      emitOutputOnMessage: behavior.emitOutputOnMessage !== false,
      longRunning: behavior.longRunning === true,
      pendingQueueEnabled: behavior.pendingQueueEnabled === true,
      approvalOnFirstMessage: behavior.approvalOnFirstMessage === true,
      approvalEveryMessage: behavior.approvalEveryMessage === true,
      approvalTtlMs: behavior.approvalTtlMs ?? defaultApprovalTtlMs,
      modelOperations: behavior.modelOperations === true,
      modelFailures: Array.isArray(behavior.modelFailures) ? behavior.modelFailures.map(failure => ({
        id: String(failure?.id ?? ''), name: String(failure?.name ?? failure?.id ?? ''),
      })).filter(failure => failure.id !== '') : [],
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
    this.#questionStamps.reset()
    this.#questionOwners.clear()
  }

  /** Test-only: remove a session so authoritative reads return not_found. */
  deleteSession(sessionId) {
    this.#sessions.delete(sessionId)
  }

  /** Test-only: make authoritative reads fail not_found for a removed session. */
  async readState(sessionId, options) {
    if (!this.#sessions.has(sessionId)) {
      const error = new Error('session not found')
      error.code = 'not_found'
      throw error
    }
    return super.readState(sessionId, options)
  }

  describe() {
    return {
      mode: 'fake',
      available: !this.#crashed,
      capabilities: ['sessions', 'events', 'messages', 'attachments', 'history-page', 'history-exact', 'send-followup', ...(this.approvalMode === 'delegate' ? [] : ['approvals']), 'interrupt', 'sync', 'snapshot', 'session-fork', ...(this.behavior.modelOperations ? ['model-catalog', 'model-read', 'model-select'] : []), ...(this.behavior.pendingQueueEnabled ? ['pending-queue-read','pending-queue-edit','pending-queue-remove','pending-queue-steer'] : []), ...(this.behavior.questions ? ['questions-read','questions-answer-continued'] : []), ...(this.behavior.questions && this.behavior.questionsOpen ? ['questions-answer-open'] : [])],
    }
  }

  async listModels() {
    this.#check('listModels')
    if (!this.behavior.modelOperations) throw new AdapterError('unsupported_capability')
    return {
      default_selection: { ...this.#defaultModel },
      effect_scope: 'session_and_source_default_attempt',
      complete: this.behavior.modelFailures.length === 0,
      groups: [{ id: 'fixture-provider', name: 'Fixture', models: [
        { id: 'fixture-model', name: 'Fixture model', efforts: [{ id: 'high', name: 'High' }] },
        { id: 'fixture-model-2', name: 'Fixture model two', efforts: [{ id: 'high', name: 'High' }] },
      ] }],
      failures: this.behavior.modelFailures.map(failure => ({ id: failure.id, name: failure.name, code: 'unavailable' })),
    }
  }

  // `pending` is the recorded next selection; `last_used` is the model the
  // source reports for the most recent turn. They stay per-session so selecting
  // a model for one session cannot silently change another session's model.
  #modelView(session) {
    const pending = session.modelSelection ?? null
    const lastUsed = session.lastUsedModel ?? null
    const next = pending ?? lastUsed
    return {
      next,
      pending,
      pending_known: true,
      last_used: lastUsed,
      default_selection: { ...this.#defaultModel },
      default_is_fallback: next === null,
    }
  }

  async readSessionModel(sessionId) {
    this.#check('readSessionModel')
    if (!this.behavior.modelOperations) throw new AdapterError('unsupported_capability')
    return this.#modelView(this.#session(sessionId))
  }

  async selectSessionModel(sessionId, selection, context = {}) {
    this.#checkCommand('selectSessionModel')
    if (!this.behavior.modelOperations) throw new DefinitiveExecutionError('unsupported_capability')
    this.assertCommandFresh(context)
    const session = this.#session(sessionId)
    const failed = this.behavior.modelFailures.some(failure => failure.id === selection.provider)
    if (failed) throw new DefinitiveExecutionError('invalid_argument', 'provider is not advertised in the current catalog')
    if (selection.provider !== 'fixture-provider' || !['fixture-model', 'fixture-model-2'].includes(selection.model) ||
        (selection.effort && selection.effort !== 'high')) {
      throw new DefinitiveExecutionError('invalid_argument', 'model is not advertised')
    }
    const selected = { provider: selection.provider, model: selection.model, ...(selection.effort ? { effort: selection.effort } : {}) }
    // Re-selecting the effective next model is a no-op: the source receives no
    // mutation and no second indistinguishable pending command is created.
    const before = this.#modelView(session)
    if (before.next && before.next.provider === selected.provider && before.next.model === selected.model &&
        (before.next.effort ?? '') === (selected.effort ?? '')) {
      return { selected: before.next, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' }
    }
    session.modelSelection = selected
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
    this.createSession({ sessionId: childId, cwd: parent.cwd, parentSessionId: sessionId, origin: 'fork' })
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
    archived = false,
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
      archived,
      pendingDeliveries: [],
      queue: structuredClone(queue),
      modelSelection: null,
      lastUsedModel: null,
      events: [],
      nextSeq: 1,
    }
    this.#sessions.set(sessionId, session)
    return this.summaryOf(session)
  }

  #changed(sessionId) { this.notifyQuestionsChanged(sessionId) }

  // Deterministic incarnation/owner hooks for tests. A replacement changes the
  // stamp epoch without pretending the inventory content changed.
  replaceFixtureQuestionOwner(sessionId) {
    this.#session(sessionId)
    this.#questionOwners.set(sessionId, {})
  }

  resetFixtureQuestionIncarnation() {
    this.#questionStamps.reset()
    this.#questionOwners.clear()
  }

  #questionOwner(sessionId) {
    let owner = this.#questionOwners.get(sessionId)
    if (!owner) { owner = {}; this.#questionOwners.set(sessionId, owner) }
    return owner
  }

  setFixtureQuestion(sessionId, nativeQuestion) {
    this.#session(sessionId)
    if (!this.behavior.questions) throw new AdapterError('unsupported_capability')
    this.#questions.set(sessionId, [nativeQuestion])
    this.#changed(sessionId)
  }

  setFixtureQuestions(sessionId, nativeQuestions) {
    this.#session(sessionId)
    if (!this.behavior.questions || !Array.isArray(nativeQuestions)) throw new AdapterError('unsupported_capability')
    this.#questions.set(sessionId, nativeQuestions)
    this.#changed(sessionId)
  }

  appendFixtureQuestion(sessionId, nativeQuestion) {
    this.#session(sessionId)
    if (!this.behavior.questions || !nativeQuestion) throw new AdapterError('unsupported_capability')
    this.#questions.set(sessionId, [...(this.#questions.get(sessionId) ?? []), nativeQuestion])
    this.#changed(sessionId)
  }

  removeFixtureQuestion(sessionId, callId) {
    const remaining = (this.#questions.get(sessionId) ?? []).filter(question => question.callId !== callId)
    if (remaining.length) this.#questions.set(sessionId, remaining)
    else this.#questions.delete(sessionId)
    this.#changed(sessionId)
  }

  holdFixtureQuestion(sessionId) { this.#heldQuestions.add(sessionId) }
  releaseFixtureQuestionHold(sessionId) { this.#heldQuestions.delete(sessionId) }
  fixtureQuestionWaiting(sessionId) { return this.#waitingQuestions.has(sessionId) }

  async listQuestionPage(sessionId, query = {}) {
    this.#session(sessionId)
    if (!this.behavior.questions) throw new AdapterError('unsupported_capability')
    if (query.cursor) throw new AdapterError('invalid_argument')
    const native = this.#questions.get(sessionId)
    const batches = this.#questionProjection.project(sessionId, { active: native ?? [] })
      .map(batch => ({ ...batch, answerable: batch.phase === 'QUESTION_CONTINUED' || (batch.phase === 'QUESTION_OPEN' && this.behavior.questionsOpen) }))
    const stamp = this.#questionStamps.sample(sessionId, this.#questionOwner(sessionId), questionInventoryFingerprint(batches))
    return { batches, next_cursor: '', stamp }
  }

  async readQuestionStamp(sessionId, options = {}) {
    const page = await this.listQuestionPage(sessionId, { signal: options?.signal })
    if (options?.signal?.aborted) throw new AdapterError('unavailable')
    return { availability: 'available', stamp: page.stamp }
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
      const interrupt = new AbortController()
      this.#questionInterrupts.set(request.session_id, interrupt)
      this.#waitingQuestions.add(request.session_id)
      let remove = () => {}
      try {
        await new Promise((_, reject) => {
          const onAbort = () => fail('answer cancelled')
          const onInterrupt = () => fail('session interrupted')
          const fail = cause => { remove(); reject(new DefinitiveExecutionError('unavailable', cause)) }
          remove = () => {
            signal?.removeEventListener?.('abort', onAbort)
            interrupt.signal.removeEventListener('abort', onInterrupt)
          }
          if (signal?.aborted) return fail('answer cancelled')
          if (interrupt.signal.aborted) return fail('session interrupted')
          signal?.addEventListener?.('abort', onAbort, { once: true })
          interrupt.signal.addEventListener('abort', onInterrupt, { once: true })
        })
      } finally {
        remove()
        if (this.#questionInterrupts.get(request.session_id) === interrupt) this.#questionInterrupts.delete(request.session_id)
        this.#waitingQuestions.delete(request.session_id)
      }
    }
    this.#removeFixtureQuestion(request.session_id, batch.batch_id)
    return { batch_id: batch.batch_id, phase: 'QUESTION_SETTLED' }
  }

  #removeFixtureQuestion(sessionId, batchId) {
    const callId = this.#questionProjection.nativeCall(sessionId, batchId)
    const remaining = (this.#questions.get(sessionId) ?? []).filter(question => question.callId !== callId)
    if (remaining.length) this.#questions.set(sessionId, remaining)
    else this.#questions.delete(sessionId)
    this.#changed(sessionId)
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
      archived: Boolean(session.archived),
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

  attachmentsAvailable() { return true }

  /** Test-only: store deterministic attachment bytes and return the native ref. */
  saveFixtureAttachment({ kind = 'file', data = new Uint8Array(0), mediaType = '', name = '' } = {}) {
    const bytes = data instanceof Uint8Array ? data.slice() : Uint8Array.from(data ?? [])
    const digest = createHash('sha256').update(bytes).digest('hex')
    const attachmentId = kind === 'image' ? `image-${randomId()}` : `sha256:${digest}`
    const ref = kind === 'image'
      ? { attachmentId, mediaType: mediaType || 'image/png', bytes: bytes.length, ...(name ? { name } : {}) }
      : { attachmentId, bytes: bytes.length, ...(name ? { name } : {}) }
    this.#attachmentStore.set(attachmentId, { kind, data: bytes, ref })
    return ref
  }

  /** Test-only: simulate a corrupt immutable store object. */
  corruptFixtureAttachment(attachmentId) {
    const entry = this.#attachmentStore.get(attachmentId)
    if (!entry) throw new AdapterError('not_found')
    entry.data = entry.data.subarray(0, Math.max(0, entry.data.length - 1))
  }

  async nativeReadImage(ref, signal) {
    if (signal?.aborted) throw new AdapterError('unavailable')
    const entry = this.#attachmentStore.get(ref?.attachmentId)
    if (!entry || entry.kind !== 'image') throw new AdapterError('not_found')
    return entry.data.slice()
  }

  async nativeReadFileStream(ref, signal) {
    if (signal?.aborted) throw new AdapterError('unavailable')
    const entry = this.#attachmentStore.get(ref?.attachmentId)
    if (!entry || entry.kind !== 'file') throw new AdapterError('not_found')
    const data = entry.data
    return (async function* () {
      for (let offset = 0; offset < data.length; offset += 64 * 1024) {
        if (signal?.aborted) throw new AdapterError('unavailable')
        yield data.slice(offset, offset + 64 * 1024)
      }
    })()
  }

  async readWaitState(sessionId, signal) {
    if (signal?.aborted) throw new AdapterError('unavailable')
    const session = this.#session(sessionId)
    const pending = []
    const approval = [...this.#approvals.values()].some(item => item.sessionId === sessionId && item.state === 'pending')
    if (approval) pending.push('approval')
    if (this.behavior.questions && (this.#questions.get(sessionId) ?? []).length > 0) pending.push('question')
    return { active: true, running: session.running, queueAvailable: true, pendingInput: pending }
  }

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
      data: {
        turn: 0, step: 0,
        message: {
          id: `message-${randomId()}`, role: 'assistant',
          content: [{ type: 'text', text }],
          source: { kind: 'model', provider: this.#defaultModel.provider, model: this.#defaultModel.model },
        },
      },
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
      // A started turn observes (and consumes) the recorded next selection, so
      // a later read can prove which model the source actually used.
      session.lastUsedModel = session.modelSelection ?? { ...this.#defaultModel }
      session.modelSelection = null
      this.emit(sessionId, { type: 'user/message', kind: 'output', data: {
        id: messageId, role: 'user', content: [{ type: 'text', text: message }],
        source: { kind: 'user', rpcId: messageId },
      } })
      if (this.behavior.emitOutputOnMessage) {
        this.emit(sessionId, { type: 'assistant/message', kind: 'output', data: {
          turn: 0, step: 0,
          message: {
            id: `message-${randomId()}`, role: 'assistant',
            content: [{ type: 'text', text: message }],
            source: { kind: 'model', provider: this.#defaultModel.provider, model: this.#defaultModel.model },
          },
        } })
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
          // The fake models a native approval that carries call facts, so the
          // remote approve path exercises a complete review. Tests that need
          // the incomplete fixture omit arguments explicitly.
          arguments: { command: 'npm test' },
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
      // The interrupted turn owns any held question and approval claims;
      // release them even if the summary no longer reports the turn running,
      // so a later answer/wait is not permanently conflicted (STREAM-06).
      this.#questionInterrupts.get(sessionId)?.abort()
      this.#releaseApprovals(sessionId)
      // Interrupting an idle session is accepted but emits no state or attempt
      // event: there is no running attempt to end (STREAM-05).
      if (session.running !== true) return { accepted: true }
      this.notifyQuestionsChanged(sessionId)
      session.running = false
      session.updatedAt = this.now()
      this.emit(sessionId, { type: 'session/interrupted', kind: 'session_state', data: { interrupted: true } })
      this.publishInterruptedAttempt(sessionId)
      return { accepted: true }
    })
  }

  // --- approvals ----------------------------------------------------------

  #releaseApprovals(sessionId) {
    for (const approval of this.#approvals.values()) {
      if (approval.sessionId !== sessionId || approval.state !== 'pending') continue
      approval.state = 'unavailable'
      approval.decision = 'unspecified'
      this.#emitApproval('resolved', approval)
    }
  }

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
      // A TTL expiry releases the native turn. Mirror DSH: the session becomes
      // idle when no other approval still holds it, so the next turn remains
      // usable instead of staying input-required forever.
      const session = this.#sessions.get(approval.sessionId)
      const stillHeld = [...this.#approvals.values()].some((item) =>
        item.sessionId === approval.sessionId && item.state === 'pending')
      if (session && session.running && !stillHeld) {
        session.running = false
        session.updatedAt = this.now()
        this.emit(approval.sessionId, { type: 'turn/end', kind: 'session_state', data: { running: false } })
      }
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
    version = null,
    approvalId = randomId(),
    arguments: toolArguments = undefined,
    reviewAction = undefined,
    reviewTarget = undefined,
    reviewImpact = undefined,
    reviewAvailability = undefined,
  } = {}) {
    this.#check('requestApproval')
    this.#session(sessionId)
    const approvalVersion = Number.isSafeInteger(version) && version > 0 ? version : ++this.#approvalVersion
    this.#approvalVersion = Math.max(this.#approvalVersion, approvalVersion)
    const expiresAt = this.now() + ttlMs
    const toolNameValue = boundedApprovalText(toolName)
    const callIdValue = boundedApprovalText(callId)
    const reasonValue = boundedApprovalText(reason, APPROVAL_REASON_MAX)
    // Fixtures can supply either a native-shaped argument map or explicit
    // review facts. Both go through the same masking/bounding helper.
    const derived = describeApprovalRequest({
      toolName: toolNameValue,
      callId: callIdValue,
      reason: reasonValue,
      arguments: toolArguments,
    })
    const action = typeof reviewAction === 'string' ? boundReviewValue(reviewAction) : derived.reviewAction
    const target = typeof reviewTarget === 'string' ? boundReviewValue(reviewTarget) : derived.reviewTarget
    const impact = typeof reviewImpact === 'string' ? boundReviewValue(reviewImpact) : derived.reviewImpact
    const availability = reviewAvailability === undefined
      ? reviewAvailabilityFor(action, target, impact)
      : normalizeReviewAvailability(reviewAvailability)
    const approval = {
      approvalId,
      sessionId,
      toolName: toolNameValue,
      callId: callIdValue,
      reason: reasonValue,
      reviewAction: action,
      reviewTarget: target,
      reviewImpact: impact,
      reviewAvailability: availability,
      version: approvalVersion,
      expiresAt,
      state: 'pending',
      decision: 'unspecified',
      digest: approvalDigest({
        approvalId, sessionId, toolName: toolNameValue, callId: callIdValue, reason: reasonValue,
        reviewAction: action, reviewTarget: target, reviewImpact: impact, reviewAvailability: availability,
        version: approvalVersion, expiresAt,
      }),
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
    // A remote Allow is not withheld when the review is incomplete or
    // unavailable; the projected review is advisory and the v2 digest still
    // binds it. `reject` is always allowed, and a denial leaves the approval
    // pending. An accepted decision is applied atomically exactly once.
    return applyAction(() => {
      approval.state = decision === 'approve' ? 'approved' : 'rejected'
      approval.decision = decision
      this.#emitApproval('resolved', approval)
      this.logger?.info('approval_resolved', { approval: approvalId, status: approval.state })
      return { state: approval.state, decision: approval.decision }
    })
  }

  /** Test-only: model a native authority that lost a pending approval. */
  forgetApproval(approvalId) {
    this.#check('forgetApproval')
    return this.#approvals.delete(approvalId)
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
