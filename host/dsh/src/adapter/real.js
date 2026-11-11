import { mapAdapterError } from './errors.js'
// Real DSH adapter. All DSH-specific API access lives in this file and is
// written defensively: services are read dynamically, DSH packages are never
// imported at module scope, and every failure is mapped to a stable wire code.
//
// The internal DSH 0.2.2-rc.1 APIs have isolated native-runtime acceptance
// alongside unit tests. README.md separates that evidence from deployment gates.

import { assistantReplyData, assistantStreamEvent } from './assistant-stream.js'
import { AdapterError, DefinitiveExecutionError } from 'whalerein-host/errors.js'
import { approvalDigest, bytesEqual, randomId, sha256Text } from 'whalerein-host/security/digest.js'
import {
  DshAdapter,
  normalizeApproval,
  normalizeEvent,
  normalizeSummary,
} from './base.js'

const OUTPUT_TYPES = new Set([
  'assistant/message',
  'tool/result',
  'user/message',
  'system/message',
  'developer/message',
])
const CONTROL_TYPES = new Set([
  'turn/start',
  'turn/end',
  'step/start',
  'step/end',
  'session/start',
  'session/end',
  'session/not-found',
  'session/interrupted',
  'approval/policy',
  'permission/preset',
  'sandbox/mode',
  'request/header',
  'request/context',
])

// DSH SessionController.list returns `{ items }`, not `{ sessions }`.
function controllerSessionRows(value) {
  return Array.isArray(value?.items) ? value.items : []
}

// DSH keeps the durable title on the list row's title projection, not on SessionSummary.
function controllerSessionTitle(session) {
  const title = session?.projections?.values?.title
  return typeof title === 'string' ? title : ''
}

// Pending prompts live on the inbox projection. The list row has no queuedPrompts field.
function controllerSessionQueuedPrompts(session) {
  const inbox = session?.projections?.values?.inbox
  if (!inbox || typeof inbox !== 'object') return 0
  const nextTurn = Array.isArray(inbox['next-turn']) ? inbox['next-turn'].length : 0
  const nextStep = Array.isArray(inbox['next-step']) ? inbox['next-step'].length : 0
  return nextTurn + nextStep
}

function summaryFromControllerSession(session, sessionId = session?.sessionId) {
  return normalizeSummary({
    sessionId: String(sessionId ?? ''),
    title: controllerSessionTitle(session),
    cwd: session?.cwd ?? '',
    createdAt: Number(session?.createdAt ?? 0),
    updatedAt: Number(session?.updatedAt ?? 0),
    running: Boolean(session?.running),
    agentAvailable: Boolean(session?.agentAvailable),
    blank: Boolean(session?.blank),
    parentSessionId: session?.parentSessionId ?? '',
    origin: session?.origin ?? '',
    queuedPrompts: controllerSessionQueuedPrompts(session),
  })
}

function eventKind(type) {
  if (type === 'ignore') return null
  if (OUTPUT_TYPES.has(type)) return 'output'
  if (CONTROL_TYPES.has(type) || type.startsWith('session/')) return 'session_state'
  if (type.includes('error') || type === 'notice' || type.startsWith('hook/') || type.startsWith('plugin:')) {
    return 'notice'
  }
  return 'output'
}

export class RealDshAdapter extends DshAdapter {
  #disposers = []
  #streamEpochs = new WeakMap()
  #eventSubscribers = new Set()
  #approvalSubscribers = new Set()
  #approvals = new Map()
  #approvalTimers = new Set()
  #approvalVersion = 0
  #usedAuditIds = new Set()
  #closed = true

  /**
   * @param {object} options
   * @param {object} options.ctx Cordis host context.
   * @param {object} [options.config] resolved plugin config.
   * @param {() => number} [options.now]
   * @param {object} [options.store]
   * @param {object} [options.logger]
   */
  constructor({ ctx, config = {}, now = Date.now, store = null, logger = null } = {}) {
    super({ now, store, logger })
    this.ctx = ctx
    this.config = config
    this.approvalMode = config.approvalMode ?? 'claim'
    this.approvalTtlMs = config.approvalTtlMs ?? 300_000
    this.requestTimeoutMs = config.requestTimeoutMs ?? 30_000
    this.hostId = config.hostId ?? ''
  }

  #service(name) {
    try {
      return this.ctx?.[name] ?? undefined
    } catch {
      return undefined
    }
  }

  #signal(timeoutMs = this.requestTimeoutMs) {
    if (typeof AbortSignal?.timeout === 'function') return AbortSignal.timeout(timeoutMs)
    return new AbortController().signal
  }

  async open(options) {
    await super.open(options)
    if (!this.ctx || typeof this.ctx !== 'object') {
      throw new AdapterError('unavailable', 'DSH context unavailable')
    }
    const controller = this.#service('sessionController')
    const sessions = this.#service('sessions')
    if (!controller && !sessions) {
      throw new AdapterError('unavailable', 'DSH session services unavailable')
    }
    const on = typeof this.ctx.on === 'function' ? this.ctx.on.bind(this.ctx) : null
    if (on) {
      const eventDisposer = on('session/event', (session, event) => this.#onSessionEvent(session, event), { global: true })
      const streamDisposer = on('agent/assistant-stream', (notification) => {
        const event = assistantStreamEvent(notification, this.now)
        if (event) {
          // DSH attempt IDs are only unique within one Agent lifecycle.
          const agent = notification.agent
          if (!this.#streamEpochs.has(agent)) this.#streamEpochs.set(agent, randomId())
          event.data.stream_id = `${this.#streamEpochs.get(agent)}:${event.data.stream_id}`
          this.#notifyEvent(event)
        }
      }, { global: true })
      if (typeof streamDisposer === 'function') this.#disposers.push(streamDisposer)
      if (typeof eventDisposer === 'function') this.#disposers.push(eventDisposer)
      // Claim owns native Agent approvals before the UI bridge can hold them.
      // Delegate immediately forwards to the next answerer.
      const approvalDisposer = on('approval/request', (request, next) => this.#onApprovalRequest(request, next), { global: true, prepend: true })
      if (typeof approvalDisposer === 'function') this.#disposers.push(approvalDisposer)
    }
    this.#closed = false
    this.opened = true
  }

  async close() {
    await super.close()
    this.#closed = true
    for (const dispose of this.#disposers.splice(0)) {
      try {
        await dispose()
      } catch {
        // subscription teardown failures are contained
      }
    }
    for (const record of this.#approvals.values()) {
      if (record.state !== 'pending') continue
      record.state = 'unavailable'
      record.settle?.('unavailable')
      this.#emitApproval('resolved', record)
    }
    this.#approvals.clear()
    for (const timer of this.#approvalTimers) clearTimeout(timer)
    this.#approvalTimers.clear()
    this.#streamEpochs = new WeakMap()
    this.#eventSubscribers.clear()
    this.#approvalSubscribers.clear()
    this.opened = false
  }

  describe() {
    return {
      mode: 'real',
      available: Boolean(this.#service('sessionController') || this.#service('sessions')),
      capabilities: ['sessions', 'events', 'messages', 'approvals', 'interrupt', 'sync', 'snapshot'],
    }
  }

  // --- sessions -----------------------------------------------------------

  async listNativeSessions() {
    try {
      const controller = this.#service('sessionController')
      if (controller && typeof controller.list === 'function') {
        const value = await controller.list({}, this.#signal())
        return controllerSessionRows(value).map((session) => summaryFromControllerSession(session))
      }
      const store = this.#service('sessions')
      if (store && typeof store.list === 'function') {
        return store.list().map((session) => this.#summaryFromLive(session))
      }
      throw new AdapterError('unsupported_capability', 'session listing unavailable')
    } catch (error) {
      throw toAdapterError(error)
    }
  }

  #summaryFromLive(session) {
    const header = session?.header ?? {}
    const events = typeof session?.snapshotEvents === 'function' ? session.snapshotEvents() : []
    const last = events[events.length - 1]
    return normalizeSummary({
      sessionId: String(session?.id ?? header.id ?? ''),
      cwd: header.cwd ?? '',
      createdAt: Number(header.createdAt ?? 0),
      updatedAt: Number(last?.time ?? header.createdAt ?? 0),
      running: false,
      agentAvailable: true,
      blank: events.length === 0,
      parentSessionId: header.parentSession ?? '',
      origin: header.origin ?? '',
    })
  }

  async #stateOf(sessionId) {
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.state !== 'function') return null
    try {
      return await controller.state({ sessionId }, this.#signal())
    } catch {
      return null
    }
  }

  async #summaryFor(sessionId) {
    const controller = this.#service('sessionController')
    if (controller && typeof controller.list === 'function') {
      try {
        const value = await controller.list({}, this.#signal())
        const found = controllerSessionRows(value).find((session) => String(session.sessionId) === sessionId)
        if (found) return summaryFromControllerSession(found, sessionId)
      } catch {
        // fall through to the live session view
      }
    }
    const live = this.#service('sessions')?.get?.(sessionId)
    if (live) {
      const summary = this.#summaryFromLive(live)
      const state = await this.#stateOf(sessionId)
      if (state) {
        summary.running = Boolean(state.running)
        summary.queuedPrompts = Array.isArray(state.queue) ? state.queue.length : 0
      }
      return summary
    }
    return normalizeSummary({ sessionId })
  }

  async nativeHead(sessionId) {
    const live=this.#service('sessions')?.get?.(sessionId)
    if(Number.isSafeInteger(live?.seq))return live.seq-1
    return (await this.getSessionSnapshot(sessionId)).cursor
  }

  async getSessionSnapshot(sessionId) {
    try {
      if (typeof sessionId !== 'string' || sessionId === '') {
        throw new AdapterError('invalid_argument', 'session id missing')
      }
      const live = this.#service('sessions')?.get?.(sessionId)
      if (live && typeof live.snapshotEvents === 'function') {
        const max = this.config.snapshotMaxEvents ?? 5000
        const all = live.snapshotEvents()
        const retained = all.slice(-max)
        const events = retained.map((event) => this.#normalizeEvent(sessionId, event))
        const normalized = events.filter(Boolean)
        const summary = await this.#summaryFor(sessionId)
        return {
          summary,
          events: normalized,
          cursor: all.at(-1)?.seq ?? -1,
          retained_from: retained[0]?.seq ?? 0,
          complete: all.length <= max,
          source: 'dsh',
        }
      }
      return await this.#coldSnapshot(sessionId)
    } catch (error) {
      throw toAdapterError(error)
    }
  }

  async #coldSnapshot(sessionId) {
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.events !== 'function') {
      throw new AdapterError('unsupported_capability', 'session snapshot unavailable')
    }
    const max = this.config.snapshotMaxEvents ?? 5000
    const records = []
    let beforeSeq
    let complete = false
    for (let page = 0; page < 100; page++) {
      const value = await controller.events({
        address: { kind: 'session', sessionId },
        ...(beforeSeq === undefined ? {} : { beforeSeq }),
        maxMessages: Math.min(500, Math.max(1, max - records.length)),
      }, this.#signal())
      const pageRecords = Array.isArray(value?.records) ? value.records : []
      records.unshift(...pageRecords)
      if (!value?.hasMore) {
        complete = true
        break
      }
      const first = pageRecords[0]?.event?.seq
      if (first === undefined || first === beforeSeq || records.length >= max) break
      beforeSeq = first
    }
    const events = records.slice(-max)
    const normalized = events.map((record) => this.#normalizeEvent(sessionId, record.event)).filter(Boolean)
    const summary = await this.#summaryFor(sessionId)
    return {
      summary,
      events: normalized,
      cursor: events.at(-1)?.event?.seq ?? -1,
      retained_from: events[0]?.event?.seq ?? 0,
      complete: complete && records.length <= max,
      source: 'dsh',
    }
  }

  // --- committed events ---------------------------------------------------

  #normalizeEvent(sessionId, event) {
    if (!event || typeof event.type !== 'string') return null
    if (event.type === 'approval/asked' || event.type === 'approval/decided') return null
    const kind = eventKind(event.type)
    if (kind === null) return null
    return normalizeEvent({
      sessionId,
      type: event.type,
      kind,
      data: assistantReplyData(event.type, event.data ?? null),
      createdAt: Number(event.time ?? this.now()),
      dshSeq: Number.isSafeInteger(event.seq) ? event.seq : null,
      ephemeral: event.ephemeral === true,
    })
  }

  #onSessionEvent(session, event) {
    const sessionId = String(session?.id ?? session?.sessionId ?? '')
    if (sessionId === '') return
    let normalized
    try {
      normalized = this.#normalizeEvent(sessionId, event)
    } catch {
      return
    }
    if (!normalized) return
    this.#notifyEvent(normalized)
  }

  #notifyEvent(normalized) {
    this.observeNative(normalized)
    for (const subscriber of this.#eventSubscribers) {
      try {
        subscriber(normalized)
      } catch {
        // observer failures never affect the DSH append
      }
    }
  }

  subscribeEvents(callback) {
    this.#eventSubscribers.add(callback)
    return () => this.#eventSubscribers.delete(callback)
  }

  // --- messages / interrupt ----------------------------------------------

  async sendUserMessage(sessionId, message) {
    if (typeof message !== 'string' || message.trim() === '') {
      throw new DefinitiveExecutionError('invalid_argument', 'message empty')
    }
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.prompt !== 'function') {
      throw new DefinitiveExecutionError('unsupported_capability', 'message prompt unavailable')
    }
    const requestId = randomId()
    try {
      await controller.prompt({
        requestId,
        sessionId,
        mode: 'queue',
        content: [{ type: 'text', text: message }],
      }, this.#signal())
      return { messageId: requestId }
    } catch (error) {
      throw new AdapterError(mapAdapterError(error))
    }
  }

  async interrupt(sessionId) {
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.cancel !== 'function') {
      throw new DefinitiveExecutionError('unsupported_capability', 'interrupt unavailable')
    }
    try {
      const value = controller.cancel({ sessionId })
      if (value && typeof value.then === 'function') await value
      return { accepted: true }
    } catch (error) {
      throw new AdapterError(mapAdapterError(error))
    }
  }

  // --- approvals ----------------------------------------------------------

  subscribeApprovals(callback) {
    this.#approvalSubscribers.add(callback)
    return () => this.#approvalSubscribers.delete(callback)
  }

  async listApprovals(sessionId = '') {
    const out = []
    for (const record of this.#approvals.values()) {
      if (sessionId && record.sessionId !== sessionId) continue
      if (this.#expire(record)) continue
      if (record.state !== 'pending') continue
      out.push(normalizeApproval(record))
    }
    return out
  }

  #emitApproval(type, record) {
    this.observeApproval({type,approval:normalizeApproval(record)})
    const approval = normalizeApproval(record)
    for (const subscriber of this.#approvalSubscribers) {
      try {
        subscriber({ type, approval })
      } catch {
        // observer failures contained
      }
    }
  }

  /**
   * Waterfall listener: claim the approval and hold DSH until the remote
   * command resolves it, the lifetime aborts, or the TTL expires. Timeout and
   * cancellation fail closed with `unavailable`/`cancelled`.
   */
  #onApprovalRequest(request, next) {
    if (this.approvalMode !== 'claim') return next()
    if (this.#closed) return Promise.resolve('unavailable')
    if (request?.signal?.aborted) return Promise.resolve('cancelled')
    const session = request?.agent?.session
    const sessionId = String(session?.id ?? request?.agent?.sessionId ?? '')
    if (sessionId === '') return Promise.resolve('unavailable')
    const auditId = this.#findAuditId(session, request)
    const approvalId = auditId ?? randomId()
    this.#usedAuditIds.add(approvalId)
    this.#approvalVersion += 1
    const version = this.#approvalVersion
    const expiresAt = this.now() + this.approvalTtlMs
    const record = {
      approvalId,
      sessionId,
      toolName: typeof request?.toolName === 'string' ? request.toolName : '',
      callId: typeof request?.callId === 'string' ? request.callId : '',
      reason: typeof request?.reason === 'string' ? request.reason : '',
      version,
      expiresAt,
      state: 'pending',
      decision: 'unspecified',
      digest: new Uint8Array(0),
      settle: null,
    }
    record.digest = approvalDigest(record)
    let settle
    const outcome = new Promise((resolve) => {
      let settled = false
      settle = (value) => {
        if (settled) return
        settled = true
        if (timer) clearTimeout(timer)
        this.#approvalTimers.delete(timer)
        request?.signal?.removeEventListener?.('abort', onAbort)
        resolve(value)
      }
    })
    record.settle = settle
    const timer = setTimeout(() => {
      if (record.state !== 'pending') return
      record.state = 'expired'
      this.#emitApproval('resolved', record)
      settle('unavailable')
    }, Math.max(1, expiresAt - this.now()))
    this.#approvalTimers.add(timer)
    timer.unref?.()
    const onAbort = () => {
      if (record.state !== 'pending') return
      record.state = 'unavailable'
      this.#emitApproval('resolved', record)
      settle('cancelled')
    }
    request?.signal?.addEventListener?.('abort', onAbort, { once: true })
    if (request?.signal?.aborted) onAbort()
    this.#approvals.set(approvalId, record)
    this.#emitApproval('requested', record)
    this.logger?.info('approval_pending', { approval: approvalId, session: sessionId })
    return outcome
  }

  #findAuditId(session, request) {
    if (!session || typeof session.snapshotEvents !== 'function') return null
    let events
    try {
      const seq = typeof session.seq === 'number' ? session.seq : 0
      events = session.snapshotEvents(Math.max(0, seq - 8))
    } catch {
      return null
    }
    for (let i = events.length - 1; i >= 0; i--) {
      const event = events[i]
      if (event?.type !== 'approval/asked') continue
      const data = event.data ?? {}
      if (data.toolName !== request.toolName) continue
      if ((data.callId ?? '') !== (request.callId ?? '')) continue
      if ((data.reason ?? '') !== (request.reason ?? '')) continue
      const id = String(data.id ?? '')
      if (id === '' || this.#usedAuditIds.has(id)) continue
      return id
    }
    return null
  }

  async #sessionRunnable(sessionId) {
    const store = this.#service('sessions')
    const live = store?.get?.(sessionId)
    if (store && !live) return false
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.state !== 'function') return Boolean(live)
    try {
      const state = await controller.state({ sessionId }, this.#signal())
      if (state?.active === false) return false
      if (state && state.running === false) return false
      return true
    } catch {
      return false
    }
  }

  async getApproval(approvalId) {
    try {
      const record = this.#approvals.get(approvalId)
      if (!record) return null
      this.#expire(record)
      return normalizeApproval(record)
    } catch (error) {
      throw toAdapterError(error)
    }
  }

  #expire(record) {
    if (record.state === 'pending' && record.expiresAt <= this.now()) {
      record.state = 'expired'
      record.settle?.('unavailable')
      this.#emitApproval('resolved', record)
      return true
    }
    return false
  }

  async resolveApproval(approvalId, decision, expectedDigest, expectedVersion) {
    const record = this.#approvals.get(approvalId)
    if (!record) throw new DefinitiveExecutionError('approval_unavailable', 'approval unknown')
    this.#expire(record)
    if (record.state === 'expired') throw new DefinitiveExecutionError('approval_expired', 'approval expired')
    if (record.state !== 'pending' || record.resolving) {
      throw new DefinitiveExecutionError('approval_already_resolved', 'approval resolved')
    }
    if (!(expectedDigest instanceof Uint8Array) || !bytesEqual(expectedDigest, record.digest)) {
      throw new DefinitiveExecutionError('approval_version_mismatch', 'approval digest mismatch')
    }
    if (Number(expectedVersion) !== record.version) {
      throw new DefinitiveExecutionError('approval_version_mismatch', 'approval version mismatch')
    }
    if (decision !== 'approve' && decision !== 'reject') {
      throw new DefinitiveExecutionError('invalid_argument', 'approval decision invalid')
    }
    record.resolving = true
    try {
      if (!(await this.#sessionRunnable(record.sessionId))) {
        throw new DefinitiveExecutionError('approval_unavailable', 'session not runnable')
      }
      if (this.#closed) throw new DefinitiveExecutionError('approval_unavailable', 'plugin closing')
      if (record.state !== 'pending') {
        throw new DefinitiveExecutionError('approval_already_resolved', 'approval resolved')
      }
      try {
        record.state = decision === 'approve' ? 'approved' : 'rejected'
        record.decision = decision
        record.settle?.(decision === 'approve' ? 'allowed-once' : 'rejected')
        this.#emitApproval('resolved', record)
        this.logger?.info('approval_resolved', { approval: approvalId, status: record.state })
        return { state: record.state, decision: record.decision }
      } catch (error) {
        // A nested failure cannot prove failure of an already settled action.
        throw new AdapterError(mapAdapterError(error))
      }
    } finally {
      record.resolving = false
    }
  }
}

function toAdapterError(error) {
  if (error instanceof AdapterError) return error
  return new AdapterError(mapAdapterError(error))
}
