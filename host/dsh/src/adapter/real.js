import { mapAdapterError } from './errors.js'
import path from 'node:path'
// Real DSH adapter. All DSH-specific API access lives in this file and is
// written defensively: services are read dynamically, DSH packages are never
// imported at module scope, and every failure is mapped to a stable wire code.
//
// The internal DSH 0.2.2-rc.1 APIs have isolated native-runtime acceptance
// alongside unit tests. README.md separates that evidence from deployment gates.

import { assistantReplyData, assistantStreamEvent } from './assistant-stream.js'
import {
  OpenQuestionBroker, QuestionHandles, QuestionProjection, QuestionStampTracker,
  assertQuestionAdmission, questionInventoryFingerprint, validateQuestionAnswers,
} from './questions.js'
import { AdapterError, DefinitiveExecutionError } from 'whalerein-host/errors.js'
import { approvalDigest, bytesEqual, randomId } from 'whalerein-host/security/digest.js'
import {
  APPROVAL_REASON_MAX,
  DshAdapter,
  boundedApprovalText,
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

function normalizeModelSelection(selection) {
  if (!selection || typeof selection.provider !== 'string' || typeof selection.model !== 'string') return null
  return { provider: selection.provider, model: selection.model, ...(selection.reasoningEffort === undefined ? {} : { effort: selection.reasoningEffort }) }
}

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

function nonEmptyString(value) {
  return typeof value === 'string' && value.trim() !== ''
}

// Native remote failures carry a stable slash code plus optional details. Only
// these codes are known to be decided before a native mutation.
function nativeFailureCode(error) {
  const code = typeof error?.code === 'string' ? error.code : ''
  if (code.includes('workspace-attach-failed')) return 'workspace_attach_failed'
  if (code.includes('fork-unavailable') || code.includes('migration-required')) return 'conflict'
  if (code.includes('not-found')) return 'not_found'
  if (code.includes('bad-request') || code.includes('invalid') || code.includes('conflict')) return 'invalid_argument'
  return mapAdapterError(error)
}

function summaryFromControllerSession(session, sessionId, archived) {
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
    archived,
    queuedPrompts: controllerSessionQueuedPrompts(session),
  })
}

function eventKind(type) {
  if (type === 'ignore') return null
  // Publish source-state invalidation while retaining opaque history continuity.
  if (type === 'agent/inbox/spliced') return 'session_state'
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
  #questionHandles = new QuestionHandles()
  #questionProjection = new QuestionProjection()
  #questionOwnerEpochs = new WeakMap()
  #questionStamps = new QuestionStampTracker()
  #questionHandleUnsubscribe = null
  #questionContext = null
  #openQuestionBroker = null

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
    this.deliveryModes = ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER']
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

  // Optional services are also reachable through Cordis' `get`; preflight
  // reads stay read-only and never trigger native creation.
  #provided(name) {
    const direct = this.#service(name)
    if (direct !== undefined) return direct
    try {
      return typeof this.ctx?.get === 'function' ? this.ctx.get(name) : undefined
    } catch {
      return undefined
    }
  }

  #questionService() {
    return this.#questionContext?.userQuestions ?? this.#service('userQuestions')
  }

  #questionAgents() {
    return this.#questionContext?.agents ?? this.#service('agents')
  }

  /** Install the opt-out open-question broker; local answerers stay in the waterfall. */
  #installOpenQuestionBroker(scope) {
    if (this.config.enableOpenQuestionBroker === false) return undefined
    if (!scope || typeof scope.on !== 'function') return undefined
    const agents = scope.agents
    const userQuestions = scope.userQuestions
    if (!agents || !userQuestions) return undefined
    const broker = new OpenQuestionBroker({
      agents, userQuestions, handles: this.#questionHandles, now: this.now,
      onChange: sessionId => this.notifyQuestionsChanged(sessionId),
    })
    this.#openQuestionBroker = broker
    const dispose = scope.on('user-questions/request', (request, next) => broker.capture(request, next), { global: true, prepend: true })
    return typeof dispose === 'function' ? dispose : undefined
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
    if (typeof this.ctx.inject === 'function') {
      // Optional services must not prevent the main connector from starting.
      // Cordis invalidates this child scope when either service disappears.
      const fiber = this.ctx.inject(['agents', 'userQuestions'], scope => {
        this.#questionContext = scope
        const disposeProvider = this.#installOpenQuestionBroker(scope)
        return () => {
          if (this.#questionContext === scope) {
            this.#questionContext = null
            this.#openQuestionBroker?.close()
            this.#openQuestionBroker = null
          }
          disposeProvider?.()
        }
      })
      if (typeof fiber?.dispose === 'function') this.#disposers.push(() => fiber.dispose())
    } else {
      // Minimal in-memory contexts do not implement Cordis injection. When the
      // native question services are still reachable, the same opt-out broker
      // is installed directly so the open answer path is not test-only.
      this.#questionContext = this.ctx
      const disposeProvider = this.#installOpenQuestionBroker(this.ctx)
      if (disposeProvider) this.#disposers.push(disposeProvider)
    }
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
    // A broker capture/answer claim is a question inventory change even when
    // no transcript event accompanies it.
    this.#questionHandleUnsubscribe = this.#questionHandles.subscribe(event => this.notifyQuestionsChanged(event.sessionId))
    this.#closed = false
    this.opened = true
  }

  async close() {
    await super.close()
    this.#closed = true
    this.#questionHandleUnsubscribe?.()
    this.#questionHandleUnsubscribe = null
    this.#questionHandles.close()
    this.#questionProjection.clear()
    this.#questionStamps.reset()
    this.#questionOwnerEpochs = new WeakMap()
    this.#questionContext = null
    this.#openQuestionBroker?.close()
    this.#openQuestionBroker = null
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
    const controller = this.#service('sessionController')
    const sessions = this.#service('sessions')
    const subscribable = typeof this.ctx?.on === 'function'
    const capabilities = []
    if (typeof controller?.list === 'function' || typeof sessions?.list === 'function') capabilities.push('sessions')
    if (subscribable || typeof sessions?.get === 'function') capabilities.push('events')
    if (typeof controller?.prompt === 'function') capabilities.push('messages')
    // Delegate mode forwards native approvals to the local DSH UI; it has no
    // remote pending action to serve, so it must not advertise approvals.
    if (subscribable && this.approvalMode !== 'delegate') capabilities.push('approvals')
    if (typeof controller?.cancel === 'function') capabilities.push('interrupt')
    if (typeof controller?.state === 'function' || typeof controller?.events === 'function') capabilities.push('sync')
    if (typeof sessions?.get === 'function' || typeof controller?.events === 'function') capabilities.push('snapshot')
    // The pending queue is open by default and can be disabled in the profile.
    if (this.config.pendingQueueEnabled !== false && typeof controller?.state === 'function') {
      capabilities.push('pending-queue-read')
      if (typeof controller.updateQueue === 'function') {
        capabilities.push('pending-queue-edit', 'pending-queue-remove', 'pending-queue-steer')
      }
    }
    if (typeof controller?.modelCatalog === 'function') capabilities.push('model-catalog')
    if (typeof controller?.modelCatalog === 'function' && typeof controller?.projections === 'function') capabilities.push('model-read')
    if (typeof controller?.selectModel === 'function') capabilities.push('model-select')
    if (typeof controller?.state === 'function' && typeof controller?.wait === 'function') capabilities.push('wait')
    if (this.#questionService()) {
      if (typeof controller?.projections === 'function') capabilities.push('questions-read')
      if (this.#openQuestionBroker) capabilities.push('questions-answer-open')
      if (typeof this.#questionAgents()?.get === 'function' && typeof controller?.projections === 'function') {
        capabilities.push('questions-answer-continued')
      }
    }
    if (typeof controller?.fork === 'function') capabilities.push('session-fork')
    // The passive history query service is the proven fixed-cut/page path. It
    // is passive for hot and cold sessions and never activates the Session.
    const pageApi = typeof controller?.events === 'function'
    if (pageApi) capabilities.push('history-page', 'history-exact')
    const attachmentService = this.#service('attachments')
    if (pageApi && typeof attachmentService?.readImage === 'function' && typeof attachmentService?.readFileStream === 'function') {
      capabilities.push('attachments')
    }
    if (pageApi && typeof controller?.prompt === 'function') capabilities.push('send-followup')
    return {
      mode: 'real',
      available: Boolean(controller || sessions),
      capabilities,
    }
  }

  supportedDeliveryModes() {
    const modes = super.supportedDeliveryModes()
    if (typeof this.#service('sessionController')?.prompt === 'function') return modes
    return modes.filter(mode => mode === 'START_IF_IDLE')
  }

  // Model operations are advertised when the native controller exposes the
  // corresponding methods; older or limited services stay unsupported.
  async listModels({signal} = {}) {
    if (signal?.aborted) throw new AdapterError('unavailable')
    const controller = this.#service('sessionController')
    if (typeof controller?.modelCatalog !== 'function') throw new AdapterError('unsupported_capability')
    try {
      const catalog = await controller.modelCatalog()
      if (signal?.aborted) throw new AdapterError('unavailable')
      return {
        default_selection: normalizeModelSelection(catalog.default),
        effect_scope: 'session_and_source_default_attempt',
        complete: (catalog.failures ?? []).length === 0,
        groups: (catalog.groups ?? []).map(group => ({
          id: group.id, name: group.name,
          models: (group.models ?? []).map(model => ({
            id: model.id, name: model.name,
            efforts: (model.reasoning?.efforts ?? []).map(effort => ({ id: effort.id, name: effort.name })),
            default_effort: model.reasoning?.defaultEffort ?? '',
          })),
        })),
        failures: (catalog.failures ?? []).map(failure => ({ id: failure.id, name: failure.name, code: 'unavailable' })),
      }
    } catch (error) {
      throw toAdapterError(error)
    }
  }

  async readSessionModel(sessionId, { signal } = {}) {
    if (typeof sessionId !== 'string' || !sessionId) throw new AdapterError('invalid_argument')
    if (signal?.aborted) throw new AdapterError('unavailable')
    const controller = this.#service('sessionController')
    if (typeof controller?.projections !== 'function' || typeof controller?.modelCatalog !== 'function') {
      throw new AdapterError('unsupported_capability')
    }
    try {
      const projection = await controller.projections({ sessionId }, signal ?? this.#signal())
      if (signal?.aborted) throw new AdapterError('unavailable')
      if (!projection) throw new AdapterError('not_found')
      if (projection.kind !== 'sequenced') throw new AdapterError('unavailable')
      const value = projection.values?.modelSelection
      if (!value || !Object.hasOwn(value, 'next') || !Object.hasOwn(value, 'lastUsed')) throw new AdapterError('unavailable')
      const next = normalizeModelSelection(value.next)
      const lastUsed = normalizeModelSelection(value.lastUsed)
      if ((value.next != null && !next) || (value.lastUsed != null && !lastUsed) || (!next && lastUsed)) throw new AdapterError('unavailable')
      // Native wire projection merges pending and last-used into next. When
      // they are equal it cannot establish whether a pending record exists.
      const equal = next && lastUsed && next.provider === lastUsed.provider && next.model === lastUsed.model && next.effort === lastUsed.effort
      const catalog = await controller.modelCatalog()
      if (signal?.aborted) throw new AdapterError('unavailable')
      return {
        next, last_used: lastUsed,
        pending: next && !equal ? next : null,
        pending_known: !equal,
        default_selection: normalizeModelSelection(catalog.default),
        // A recorded pending/last-used choice is the session's own selection;
        // the catalog default only stands in when none was recorded.
        default_is_fallback: next === null,
      }
    } catch (error) {
      if (signal?.aborted) throw new AdapterError('unavailable')
      throw toAdapterError(error)
    }
  }

  async selectSessionModel(sessionId, selection, context = {}) {
    if (typeof sessionId !== 'string' || !sessionId || typeof selection?.provider !== 'string' || !selection.provider || typeof selection?.model !== 'string' || !selection.model || (selection.effort !== undefined && typeof selection.effort !== 'string')) {
      throw new DefinitiveExecutionError('invalid_argument')
    }
    const controller = this.#service('sessionController')
    if (typeof controller?.selectModel !== 'function') throw new DefinitiveExecutionError('unsupported_capability')
    // Model selection mutates the source just like a queue edit. Re-check the
    // command deadline immediately before the native call so expiry is
    // definitive and never reported as an unknown applied change.
    this.assertCommandFresh(context)
    try {
      const result = await controller.selectModel({
        sessionId, provider: selection.provider, model: selection.model,
        ...(selection.effort !== undefined && selection.effort !== null ? { reasoningEffort: selection.effort } : {}),
      })
      // The deadline may have passed while the native call was in flight. The
      // native action already happened, so this stays uncertain by design.
      const selected = normalizeModelSelection(result?.selected)
      if (!selected) throw new AdapterError('internal')
      return { selected, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' }
    } catch (error) {
      // Native controller can wrap failures after the change as model-unavailable.
      // Preserve uncertainty unless failure happened before the native call.
      throw new AdapterError(mapAdapterError(error))
    }
  }

  // Creation and fork advertise only when the native controller exposes the
  // corresponding entry point. Model bits follow describe().
  async describeHostCapabilities() {
    const controller = this.#service('sessionController')
    return {
      ...(await super.describeHostCapabilities()),
      session_create: typeof controller?.create === 'function',
      session_fork: typeof controller?.fork === 'function',
    }
  }

  #createInput(create) {
    if (!create) throw new DefinitiveExecutionError('unsupported_capability')
    const { cwd, workspace_id: workspaceId, agent_preset: agentPreset, preferred_session_id: sessionId } = create
    for (const value of [cwd, workspaceId, agentPreset, sessionId]) {
      if (value !== undefined && (typeof value !== 'string' || !value.trim())) throw new DefinitiveExecutionError('invalid_argument')
    }
    if ((cwd !== undefined && workspaceId !== undefined) || (cwd !== undefined && !path.isAbsolute(cwd))) throw new DefinitiveExecutionError('invalid_argument')
    return { cwd, workspaceId, agentPreset, sessionId }
  }

  // Read-only creation preflight. It reports native facts only: the Host
  // decides dispositions. Missing workspace/preset lookups are conclusive
  // only when the corresponding registry is present.
  async preflightCreateSession(command) {
    const create = command?.create_session ?? command
    const { workspaceId, agentPreset, sessionId } = this.#createInput(create)
    const facts = { workspace: 'not_configured', preset: 'not_configured', reuse: '' }
    if (workspaceId !== undefined) {
      const registry = this.#provided('workspaceRegistry')
      if (!registry || typeof registry.get !== 'function') {
        facts.workspace = 'unknown'
      } else {
        const workspace = registry.get(workspaceId)
        if (!workspace) return { ...facts, workspace: 'missing' }
        if (typeof workspace.attachSession !== 'function') return { ...facts, workspace: 'unattachable' }
        facts.workspace = 'present'
      }
    }
    if (agentPreset !== undefined) {
      const presets = this.#provided('agentPresets')
      if (!presets || typeof presets.resolve !== 'function') {
        facts.preset = 'unknown'
      } else {
        try {
          const resolved = await presets.resolve(agentPreset)
          facts.preset = resolved?.broken ? 'unavailable' : 'present'
        } catch (error) {
          if (String(error?.code ?? '').includes('agent-preset/not-found') || mapAdapterError(error) === 'not_found') {
            return { ...facts, preset: 'missing' }
          }
          facts.preset = 'unknown'
        }
      }
    }
    if (sessionId !== undefined) {
      facts.reuse = await this.#reusableIdentity(sessionId, workspaceId)
    }
    return facts
  }

  // Native creation reuses a preferred identity only when it is a live blank,
  // unarchived member of the requested workspace. Anything less is left to
  // native selection instead of claiming a reuse we cannot prove.
  async #reusableIdentity(sessionId, workspaceId) {
    let summary
    try {
      const sessions = await this.listSessions()
      summary = sessions.find((session) => session.sessionId === sessionId)
    } catch {
      return ''
    }
    if (!summary || !summary.blank || summary.running) return ''
    const registry = this.#provided('workspaceRegistry')
    if (!registry || !Array.isArray(registry.archivedSessionIds)) return ''
    if (registry.archivedSessionIds.includes(sessionId)) return ''
    if (workspaceId !== undefined) {
      const workspace = registry.get?.(workspaceId)
      if (!workspace || !Array.isArray(workspace.sessionIds) || !workspace.sessionIds.includes(sessionId)) return ''
    }
    return sessionId
  }

  // Fork preflight proves a completed turn from the native projection when it
  // can. If the read is unavailable, callers proceed to native, which remains
  // the authoritative check.
  async preflightForkSession(sessionId) {
    if (!nonEmptyString(sessionId)) throw new DefinitiveExecutionError('invalid_argument')
    try {
      const snapshot = await this.getSessionSnapshot(sessionId)
      if (!snapshot || !Array.isArray(snapshot.events)) return { completed_turn: 'unknown' }
      return { completed_turn: snapshot.events.some((event) => event?.type === 'turn/end') }
    } catch (error) {
      if (error instanceof DefinitiveExecutionError) throw error
      return { completed_turn: 'unknown' }
    }
  }

  // An attach failure names a child, but details alone are not proof. A
  // child that did not exist before this call (or the explicitly requested
  // preferred identity) is the only identity evidence this adapter trusts.
  async #verifiedAttachIdentity(error, { parentSessionId = '', preferredSessionId = '', beforeIds = null } = {}) {
    const code = typeof error?.code === 'string' ? error.code : ''
    if (!code.includes('workspace-attach-failed')) return null
    const sessionId = error?.details?.sessionId
    if (!nonEmptyString(sessionId)) return null
    if (parentSessionId && sessionId === parentSessionId) return null
    if (preferredSessionId && sessionId !== preferredSessionId) return null
    if (!beforeIds) return null
    if (beforeIds.has(sessionId) && sessionId !== preferredSessionId) return null
    let found
    try {
      found = (await this.listSessions()).find((session) => session.sessionId === sessionId)
    } catch {
      return null
    }
    if (!found) return null
    if (parentSessionId && found.parentSessionId && found.parentSessionId !== parentSessionId) return null
    return { session_id: sessionId, ...(parentSessionId ? { parent_session_id: parentSessionId } : {}) }
  }

  async #sessionIds() {
    try {
      return new Set((await this.listSessions()).map((session) => session.sessionId))
    } catch {
      return null
    }
  }

  // Native creation/fork return facts only. Product dispositions belong to the
  // Host command service and never to this adapter.
  async executeHost(command, context = {}) {
    const create = command?.create_session
    if (!create) throw new DefinitiveExecutionError('unsupported_capability')
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.create !== 'function') throw new DefinitiveExecutionError('unsupported_capability')
    const { cwd, workspaceId, agentPreset, sessionId } = this.#createInput(create)
    this.assertCommandFresh(context)
    const beforeIds = await this.#sessionIds()
    try {
      const value = await controller.create({
        ...(cwd === undefined ? {} : { cwd }),
        ...(workspaceId === undefined ? {} : { workspaceId }),
        ...(agentPreset === undefined ? {} : { agentPreset }),
        ...(sessionId === undefined ? {} : { sessionId }),
      })
      if (!nonEmptyString(value?.sessionId)) return { entered: true, failure_code: 'internal' }
      return { entered: true, identity: { session_id: value.sessionId } }
    } catch (error) {
      if (error instanceof DefinitiveExecutionError) throw error
      const identity = await this.#verifiedAttachIdentity(error, { preferredSessionId: sessionId ?? '', beforeIds })
      if (identity) return { entered: true, identity, attach_error: 'workspace_attach_failed' }
      const code = nativeFailureCode(error)
      // A bad request is rejected before creation; all other native failures
      // may follow a persisted session and stay ambiguous.
      if (code === 'invalid_argument') return { entered: false, failure_code: code }
      return { entered: true, failure_code: code }
    }
  }

  // Only handles actually claimed by this adapter are returned. Other native
  // questions remain visible through DSH's own state and provider waterfall.
  listClaimedQuestions(sessionId) {
    return this.#questionHandles.listActive(sessionId)
  }

  subscribeQuestions(callback) {
    return this.#questionHandles.subscribe(callback)
  }

  #assertQuestionOwnership(sessionId) {
    const agents = this.#questionContext?.agents ?? (typeof this.ctx?.inject === 'function' ? undefined : this.#service('agents'))
    if (typeof agents?.get !== 'function' || typeof agents?.roots !== 'function') throw new AdapterError('unsupported_capability')
    const live = agents?.get?.(sessionId)
    if (live) {
      const roots = agents.roots?.()
      if (!Array.isArray(roots) || !roots.includes(live) || agents.get(sessionId) !== live) {
        throw new AdapterError('unsupported_capability')
      }
    }
  }

  async #readQuestionProjection(sessionId, signal) {
    if (typeof sessionId !== 'string' || sessionId === '') throw new AdapterError('invalid_argument')
    if (signal?.aborted) throw new AdapterError('unavailable')
    this.#assertQuestionOwnership(sessionId)
    const controller = this.#service('sessionController')
    if (typeof controller?.projections !== 'function') throw new AdapterError('unsupported_capability')
    let observation
    try { observation = await controller.projections({ sessionId }, signal ? AbortSignal.any([this.#signal(),signal]) : this.#signal()) }
    catch (error) { if (signal?.aborted) throw new AdapterError('unavailable'); throw toAdapterError(error) }
    if (signal?.aborted) throw new AdapterError('unavailable')
    if (!observation) throw new AdapterError('not_found')
    this.#assertQuestionOwnership(sessionId)
    if (observation.kind !== 'sequenced' || !observation.values?.userQuestions) {
      throw new AdapterError('unavailable', 'question projection unavailable')
    }
    const agents = this.#questionContext?.agents ?? (typeof this.ctx?.inject === 'function' ? undefined : this.#service('agents'))
    const service = this.#questionContext?.userQuestions ?? (typeof this.ctx?.inject === 'function' ? undefined : this.#service('userQuestions'))
    const agent = agents?.get?.(sessionId)
    const session = this.#service('sessions')?.get?.(sessionId)
    const inbox = agent?.inbox
    const liveRoot = Boolean(agent && typeof agent === 'object' && agents.get(sessionId) === agent && agents.roots().includes(agent) &&
      session && typeof session === 'object' && agent.session === session && typeof service?.answer === 'function' &&
      Array.isArray(inbox?.nextTurn) && Array.isArray(inbox?.nextStep))
    const queued = new Set(liveRoot ? [...inbox.nextTurn, ...inbox.nextStep]
      .filter(message => message?.source?.kind === 'user-question-reply')
      .map(message => message.source.callId) : [])
    const source = observation.values.userQuestions
    const view = { ...source, active: Array.isArray(source.active) ? source.active.map(item =>
      item?.state === 'continued' && queued.has(item.callId) ? { ...item, state: 'reply_queued' } : item) : source.active }
    // `answerable` is an opportunity to submit, not a reservation. Native DSH
    // retains private in-flight reply state; answer() remains authoritative and
    // can reject REPLY_QUEUED even when that state is not visible in the inbox.
    let nativeOwner = 'not-live'
    if (liveRoot) {
      let sessions = this.#questionOwnerEpochs.get(agent)
      if (!sessions) { sessions = new WeakMap(); this.#questionOwnerEpochs.set(agent, sessions) }
      if (!sessions.has(session)) sessions.set(session, randomId())
      nativeOwner = sessions.get(session)
    }
    const batches = this.#questionProjection.project(sessionId, view, nativeOwner).map(batch =>
      batch.phase === 'QUESTION_CONTINUED' && liveRoot ? { ...batch, answerable: true } : batch)
    return { batches, asOfSeq: observation.asOfSeq, ownerAgent: liveRoot ? agent : null, ownerSession: liveRoot ? session : null }
  }

  async listProjectedQuestions(sessionId, signal) {
    return (await this.#readQuestionProjection(sessionId, signal)).batches
  }

  // One authoritative inventory read produces both the wire page collection
  // and the semantic stamp that certifies it. Broker-only questions without a
  // committed Message participate in the same ordered collection.
  async #questionInventory(sessionId, query = {}) {
    if (query.cursor) throw new AdapterError('invalid_argument')
    const { batches: projected, asOfSeq, ownerAgent, ownerSession } = await this.#readQuestionProjection(sessionId, query.signal)
    if (query.signal?.aborted) throw new AdapterError('unavailable')
    this.#assertQuestionOwnership(sessionId)
    const live = this.#openQuestionBroker?.list(sessionId) ?? []
    const liveByCall = new Map(live.map(batch => [this.#openQuestionBroker.nativeCall(sessionId, batch.batch_id), batch]))
    const batches = projected.map(batch => {
      if (batch.phase !== 'QUESTION_OPEN') return batch
      const callId = this.#questionProjection.nativeCall(sessionId, batch.batch_id)
      const replacement = liveByCall.get(callId)
      if (!replacement) return batch
      liveByCall.delete(callId)
      return replacement
    })
    if (batches.length && liveByCall.size) {
      const session = this.#service('sessions')?.get?.(sessionId)
      const events = session?.snapshotEvents?.()
      if (!Array.isArray(events) || !Number.isSafeInteger(asOfSeq)) throw new AdapterError('unavailable')
      const positions = new Map()
      for (const event of events) {
        if (!Number.isSafeInteger(event.seq) || event.seq > asOfSeq) continue
        const callId = event.type === 'tool/call' ? event.data?.callId
          : event.type === 'tool/ptc-dispatch' ? event.data?.subCallId : undefined
        if (typeof callId === 'string') positions.set(callId, event.seq)
      }
      const ordered = batches.map((batch, ordinal) => {
        const callId = this.#questionProjection.nativeCall(sessionId, batch.batch_id) ??
          this.#openQuestionBroker?.nativeCall(sessionId, batch.batch_id)
        const seq = positions.get(callId)
        if (!Number.isSafeInteger(seq)) throw new AdapterError('unavailable')
        return { batch, position: seq, tie: 1, ordinal }
      })
      for (const batch of liveByCall.values()) {
        const marker = this.#openQuestionBroker.position(sessionId, batch.batch_id)
        if (!Number.isSafeInteger(marker?.position)) throw new AdapterError('unavailable')
        ordered.push({ batch, position: marker.position, tie: 0, ordinal: marker.ordinal })
      }
      ordered.sort((a, b) => a.position - b.position || a.tie - b.tie || a.ordinal - b.ordinal)
      const orderedBatches = ordered.map(item => item.batch)
      return { batches: orderedBatches, stamp: this.#sampleStamp(sessionId, ownerAgent, ownerSession, orderedBatches) }
    }
    batches.push(...liveByCall.values())
    return { batches, stamp: this.#sampleStamp(sessionId, ownerAgent, ownerSession, batches) }
  }

  #sampleStamp(sessionId, ownerAgent, ownerSession, batches) {
    const owner = ownerAgent && ownerSession ? [ownerAgent, ownerSession] : null
    return this.#questionStamps.sample(sessionId, owner, questionInventoryFingerprint(batches))
  }

  async listQuestionPage(sessionId, query = {}) {
    return this.#questionInventory(sessionId, query)
  }

  async readQuestionStamp(sessionId, options = {}) {
    if (typeof sessionId !== 'string' || sessionId === '') throw new AdapterError('invalid_argument')
    const page = await this.#questionInventory(sessionId, { signal: options?.signal })
    if (options?.signal?.aborted) throw new AdapterError('unavailable')
    return { availability: 'available', stamp: page.stamp }
  }

  async getQuestion(sessionId, batchId, options = {}) {
    const page = await this.listQuestionPage(sessionId, options)
    this.#assertQuestionOwnership(sessionId)
    return page.batches.find(batch => batch.batch_id === batchId) ?? null
  }

  async answerOpenQuestion(request, signal, context = {}) {
    if (!this.#openQuestionBroker) throw new DefinitiveExecutionError('unsupported_capability')
    return this.#openQuestionBroker.answer(request, signal, context)
  }

  // The continued-answer path is advertised when the native agents and
  // question projection are present; ownership is rechecked at answer time.
  async answerContinuedQuestion(request, context = {}) {
    const sessionId = request?.session_id
    let snapshot
    try { snapshot = await this.#readQuestionProjection(sessionId, context.signal) }
    catch (error) { throw new DefinitiveExecutionError(mapAdapterError(error)) }
    const batch = snapshot.batches.find(item => item.batch_id === request.batch_id)
    if (!batch) throw new DefinitiveExecutionError('not_found')
    if (!batch.answerable || batch.phase !== 'QUESTION_CONTINUED' || request.expected_phase !== batch.phase ||
        request.source_epoch !== batch.source_epoch || Number(request.expected_version) !== batch.version ||
        !bytesEqual(request.expected_digest, batch.digest)) throw new DefinitiveExecutionError('conflict')
    let answers
    try { answers = validateQuestionAnswers(batch.questions, request.answers) }
    catch { throw new DefinitiveExecutionError('invalid_argument') }
    const agents = this.#questionContext?.agents ?? (typeof this.ctx?.inject === 'function' ? undefined : this.#service('agents'))
    const service = this.#questionContext?.userQuestions ?? (typeof this.ctx?.inject === 'function' ? undefined : this.#service('userQuestions'))
    const agent = agents?.get?.(sessionId)
    if (!agent || !snapshot.ownerAgent || typeof service?.answer !== 'function') throw new DefinitiveExecutionError('unsupported_capability')
    const callId = this.#questionProjection.nativeCall(sessionId, batch.batch_id)
    const stillOwned = () => agent === snapshot.ownerAgent && agent.session === snapshot.ownerSession &&
      this.#service('sessions')?.get?.(sessionId) === snapshot.ownerSession && agents.get(sessionId) === agent &&
      Array.isArray(agents.roots?.()) && agents.roots().includes(agent)
    if (!callId || !stillOwned()) throw new DefinitiveExecutionError('conflict')
    assertQuestionAdmission(context, this.now)
    if (!stillOwned()) throw new DefinitiveExecutionError('conflict')
    // Once called, an exception is not evidence that the source made no change.
    let accepted
    try { accepted = await service.answer(agent, callId, { answers }) }
    catch (error) {
      if (error?.code === 'REPLY_QUEUED') throw new DefinitiveExecutionError('conflict')
      if (error?.code === 'BAD_ANSWER') throw new DefinitiveExecutionError('invalid_argument')
      if (error?.code === 'CALLER_NOT_LIVE' || error?.code === 'DELEGATED_CALLER') throw new DefinitiveExecutionError('unsupported_capability')
      throw error
    }
    if (!accepted) throw new DefinitiveExecutionError('conflict')
    return { batch_id: batch.batch_id, phase: 'QUESTION_REPLY_QUEUED' }
  }

  async forkSession(sessionId) {
    if (typeof sessionId !== 'string' || !sessionId.trim()) throw new DefinitiveExecutionError('invalid_argument')
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.fork !== 'function') throw new DefinitiveExecutionError('unsupported_capability')
    const beforeIds = await this.#sessionIds()
    try {
      // Omission of atSeq delegates completed-prefix selection to DSH.
      const result = await controller.fork({ sessionId })
      if (!nonEmptyString(result?.sessionId) || result.sessionId === sessionId) return { entered: true, failure_code: 'internal' }
      return { entered: true, identity: { session_id: result.sessionId, parent_session_id: sessionId } }
    } catch (error) {
      if (error instanceof DefinitiveExecutionError) throw error
      const identity = await this.#verifiedAttachIdentity(error, { parentSessionId: sessionId, beforeIds })
      if (identity) return { entered: true, identity, attach_error: 'workspace_attach_failed' }
      const code = nativeFailureCode(error)
      // These native rejections are decided before a child is created.
      if (['not_found', 'conflict', 'invalid_argument'].includes(code)) return { entered: false, failure_code: code }
      return { entered: true, failure_code: code }
    }
  }

  // DSH keeps archive state in the workspace registry, not on the Session
  // record. A missing or unreadable registry reports no archives instead of
  // failing the listing.
  #archivedSessionIds() {
    try {
      const registry = this.#provided('workspaceRegistry')
      const ids = registry?.archivedSessionIds
      return Array.isArray(ids) ? new Set(ids.map(String)) : new Set()
    } catch {
      return new Set()
    }
  }

  // --- sessions -----------------------------------------------------------

  async listNativeSessions({signal} = {}) {
    try {
      if(signal?.aborted)throw new AdapterError('unavailable')
      const archived = this.#archivedSessionIds()
      const controller = this.#service('sessionController')
      if (controller && typeof controller.list === 'function') {
        const value = await controller.list({}, signal ? AbortSignal.any([this.#signal(),signal]) : this.#signal())
        if(signal?.aborted)throw new AdapterError('unavailable')
        return controllerSessionRows(value).map((session) => {
          const sessionId = String(session?.sessionId ?? '')
          return summaryFromControllerSession(session, sessionId, archived.has(sessionId))
        })
      }
      const store = this.#service('sessions')
      if (store && typeof store.list === 'function') {
        return store.list().map((session) => this.#summaryFromLive(session, archived))
      }
      throw new AdapterError('unsupported_capability', 'session listing unavailable')
    } catch (error) {
      if(signal?.aborted)throw new AdapterError('unavailable')
      throw toAdapterError(error)
    }
  }

  #summaryFromLive(session, archived) {
    const header = session?.header ?? {}
    const events = typeof session?.snapshotEvents === 'function' ? session.snapshotEvents() : []
    const last = events[events.length - 1]
    const sessionId = String(session?.id ?? header.id ?? '')
    return normalizeSummary({
      sessionId,
      cwd: header.cwd ?? '',
      createdAt: Number(header.createdAt ?? 0),
      updatedAt: Number(last?.time ?? header.createdAt ?? 0),
      running: false,
      agentAvailable: true,
      blank: events.length === 0,
      parentSessionId: header.parentSession ?? '',
      origin: header.origin ?? '',
      archived: archived.has(sessionId),
    })
  }

  async #stateOf(sessionId, signal) {
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.state !== 'function') return null
    if (signal?.aborted) throw new AdapterError('unavailable')
    try {
      const state=await controller.state({ sessionId }, signal ? AbortSignal.any([this.#signal(),signal]) : this.#signal())
      if (signal?.aborted) throw new AdapterError('unavailable')
      return state
    } catch {
      if (signal?.aborted) throw new AdapterError('unavailable')
      return null
    }
  }

  #hasPendingApproval(sessionId) {
    return [...this.#approvals.values()].some(record =>
      record.sessionId === sessionId && record.state === 'pending' && record.expiresAt > this.now())
  }

  // The optional questions capability owns its handles; this adapter consumes
  // only their scoped, content-free lifecycle. Native pending input remains
  // authoritative for providers that were not claimed by WhaleRein.
  #hasClaimedQuestion(sessionId) {
    return typeof this.listClaimedQuestions === 'function' && this.listClaimedQuestions(sessionId).length > 0
  }

  #normalizeWaitState(sessionId, state) {
    const pending = new Set(Array.isArray(state.pendingInput) ? state.pendingInput : [])
    if (this.#hasPendingApproval(sessionId)) pending.add('approval')
    if (this.#hasClaimedQuestion(sessionId)) pending.add('question')
    return { active: Boolean(state.active), running: Boolean(state.running),
      queueAvailable: Array.isArray(state.queue), pendingInput: [...new Set([...pending].map(kind =>
        kind === 'approval' || kind === 'question' ? kind : 'other'))] }
  }

  async readWaitState(sessionId, signal) {
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.state !== 'function') throw new AdapterError('unsupported_capability')
    try {
      const state = await controller.state({ sessionId }, signal)
      signal.throwIfAborted()
      return this.#normalizeWaitState(sessionId, state)
    } catch (error) {
      if (signal.aborted) throw signal.reason
      throw toAdapterError(error)
    }
  }

  // Native wait performs its own whenIdle and identity checks. Claimed
  // approvals are not necessarily present in DSH's pending Remote events.
  async waitSession(sessionId, timeoutMs, signal) {
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.wait !== 'function' || typeof controller.state !== 'function') {
      throw new AdapterError('unsupported_capability')
    }
    if ((typeof this.listClaimedQuestions === 'function') !== (typeof this.subscribeQuestions === 'function')) {
      throw new AdapterError('unsupported_capability')
    }
    if (typeof sessionId !== 'string' || !sessionId || !Number.isSafeInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > 2147483647) {
      throw new AdapterError('invalid_argument')
    }
    const expires = performance.now() + timeoutMs
    const hasClaimedInput = () => this.#hasPendingApproval(sessionId) || this.#hasClaimedQuestion(sessionId)
    try {
      for (;;) {
        signal.throwIfAborted()
        if (hasClaimedInput()) {
          const state = await this.readWaitState(sessionId, signal)
          signal.throwIfAborted()
          if (state.pendingInput.length) return { reason: 'input-required', state }
        }
        const remaining = Math.max(1, Math.ceil(expires - performance.now()))
        const inner = new AbortController()
        const inputChanged = new Error('claimed input changed')
        const combined = AbortSignal.any([signal, inner.signal])
        const unsubscribe = this.subscribeApprovals(event => {
          if (event.type === 'requested' && event.approval?.sessionId === sessionId) inner.abort(inputChanged)
        })
        const unsubscribeQuestions = typeof this.subscribeQuestions === 'function'
          ? this.subscribeQuestions(event => {
            if (event.type === 'requested' && event.sessionId === sessionId) inner.abort(inputChanged)
          })
          : () => {}
        try {
          // Check after subscribing, so a request cannot be lost between the
          // initial check and registration.
          if (hasClaimedInput()) continue
          const result = await controller.wait({ sessionId, timeoutMs: remaining }, combined)
          signal.throwIfAborted()
          const state = this.#normalizeWaitState(sessionId, result.state)
          return { reason: state.pendingInput.length ? 'input-required' : result.reason, state }
        } catch (error) {
          if (signal.aborted) throw signal.reason
          let causedByInput = false
          for (let cause = error, depth = 0; cause && depth < 4; cause = cause.cause, depth++) {
            if (cause === inputChanged) { causedByInput = true; break }
          }
          if (!inner.signal.aborted || !causedByInput) throw error
          if (performance.now() >= expires && !hasClaimedInput()) throw error
        } finally { unsubscribe(); unsubscribeQuestions() }
      }
    } catch (error) {
      if (signal.aborted) throw signal.reason
      throw toAdapterError(error)
    }
  }

  async #summaryFor(sessionId, signal) {
    if (signal?.aborted) throw new AdapterError('unavailable')
    const controller = this.#service('sessionController')
    const archived = this.#archivedSessionIds()
    const key = String(sessionId)
    if (controller && typeof controller.list === 'function') {
      try {
        const value = await controller.list({}, signal ? AbortSignal.any([this.#signal(),signal]) : this.#signal())
        if (signal?.aborted) throw new AdapterError('unavailable')
        const found = controllerSessionRows(value).find((session) => String(session.sessionId) === key)
        if (found) return summaryFromControllerSession(found, key, archived.has(key))
      } catch {
        if (signal?.aborted) throw new AdapterError('unavailable')
        // fall through to the live session view
      }
    }
    if (signal?.aborted) throw new AdapterError('unavailable')
    const live = this.#service('sessions')?.get?.(sessionId)
    if (live) {
      const summary = this.#summaryFromLive(live, archived)
      const state = await this.#stateOf(sessionId, signal)
      if (state) {
        summary.running = Boolean(state.running)
        summary.queuedPrompts = Array.isArray(state.queue) ? state.queue.length : 0
      }
      return summary
    }
    return normalizeSummary({ sessionId, archived: archived.has(key) })
  }

  // The source state endpoint is passive: null means there is no live inbox.
  async readPendingQueue(sessionId, {signal} = {}) {
    if (signal?.aborted) throw new AdapterError('unavailable')
    if (this.config.pendingQueueEnabled === false) throw new AdapterError('unsupported_capability')
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.state !== 'function') throw new AdapterError('unsupported_capability')
    if (typeof sessionId !== 'string' || !sessionId) throw new AdapterError('invalid_argument')
    let state
    try { state = await controller.state({ sessionId }, signal ? AbortSignal.any([this.#signal(),signal]) : this.#signal()) }
    catch (error) { if (signal?.aborted) throw new AdapterError('unavailable'); throw toAdapterError(error) }
    if (signal?.aborted) throw new AdapterError('unavailable')
    if (!state || state.queue === null) return { session_id: sessionId, available: false, running: Boolean(state?.running), items: [] }
    if (!Array.isArray(state.queue)) throw new AdapterError('unavailable')
    return { session_id: sessionId, available: true, running: Boolean(state.running), items: state.queue.map((item) => {
      const placement = {queued:'QUEUED', steering:'STEERING', context:'CONTEXT'}[item.placement] ?? 'QUEUE_PLACEMENT_UNSPECIFIED'
      // rpcId is the only visible positive indication that DSH identifies the
      // occurrence as user work. Context and unidentifiable work stay read-only.
      const editable = typeof item.rpcId === 'string' && item.rpcId !== '' && ['queued','steering'].includes(item.placement)
      const blocks = !editable ? [{block_id:'redacted',notice:{category:'unsupported_content',text:'Content is unavailable in this projection'}}] : (Array.isArray(item.content) ? item.content : []).map((block, index) => {
        if (block?.type === 'text' && typeof block.text === 'string') return {block_id:`text-${index}`,text:{text:block.text}}
        const attachment = block?.attachment
        if (['image','file'].includes(block?.type) && attachment && typeof attachment === 'object' && Number.isSafeInteger(attachment.bytes) && attachment.bytes >= 0) {
          const name = typeof attachment.name === 'string' && !/[\\/]/.test(attachment.name) ? attachment.name : ''
          const media = block.type === 'image' && ['image/png','image/jpeg','image/webp','image/gif'].includes(attachment.mediaType) ? attachment.mediaType : ''
          return {block_id:`attachment-${index}`,attachment:{media_type:media,name,size:attachment.bytes,available:false}}
        }
        return {block_id:`unsupported-${index}`,notice:{category:'unsupported_content',text:'Content is unavailable in this projection'}}
      })
      return {item_id:String(item.id ?? ''),placement,delivery_id:editable ? item.rpcId : '',blocks,actions:editable ? ['EDIT','REMOVE',...(item.placement === 'queued' && state.running ? ['PROMOTE'] : [])] : []}
    }) }
  }

  async updatePendingQueue(sessionId, command, context = {}) {
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.updateQueue !== 'function') throw new DefinitiveExecutionError('unsupported_capability')
    if (!sessionId || !command.item_id || !['EDIT','REMOVE','PROMOTE'].includes(command.action)) throw new DefinitiveExecutionError('invalid_argument')
    if (command.action === 'EDIT' && (typeof command.text !== 'string' || !command.text.trim())) throw new DefinitiveExecutionError('invalid_argument')
    let queue
    try { queue = await this.readPendingQueue(sessionId, {signal:context.signal}) }
    catch (error) { throw new DefinitiveExecutionError(error instanceof AdapterError ? error.code : 'unavailable') }
    if (!queue.available) throw new DefinitiveExecutionError('unavailable')
    let item = queue.items.find((entry) => entry.item_id === command.item_id)
    if (!item) throw new DefinitiveExecutionError('not_found')
    if (!item.actions.includes(command.action)) {
      // An idle/running transition between the observation and this action can
      // add PROMOTE. Re-read once so a benign state flip is not reported as a
      // stale conflict; the native update remains authoritative.
      try { queue = await this.readPendingQueue(sessionId, {signal:context.signal}) }
      catch (error) { throw new DefinitiveExecutionError(error instanceof AdapterError ? error.code : 'unavailable') }
      if (!queue.available) throw new DefinitiveExecutionError('unavailable')
      item = queue.items.find((entry) => entry.item_id === command.item_id)
      if (!item) throw new DefinitiveExecutionError('not_found')
      if (!item.actions.includes(command.action)) throw new DefinitiveExecutionError('conflict')
    }
    this.assertCommandFresh(context)
    const action = command.action === 'EDIT' ? {kind:'edit',content:[{type:'text',text:command.text}]} : {kind:command.action === 'REMOVE' ? 'remove' : 'steer'}
    try {
      await controller.updateQueue({sessionId,itemId:command.item_id,action},this.#signal())
      return {}
    } catch {
      // Native callbacks can throw even after a mutation; a matching error
      // code alone cannot establish that this call made no change.
      throw new AdapterError('unavailable')
    }
  }

  async nativeHead(sessionId, {signal} = {}) {
    if(signal?.aborted)throw new AdapterError('unavailable')
    const live=this.#service('sessions')?.get?.(sessionId)
    if(Number.isSafeInteger(live?.seq))return live.seq-1
    // Cold/passive: one bounded native page is enough to establish the cut.
    // Never materialize the whole log just to learn the head position.
    const page=await this.nativeEventsPage(sessionId,{maxMessages:1,signal})
    return page.events.at(-1)?.dshSeq ?? -1
  }

  attachmentsAvailable() {
    return this.describe().capabilities.includes('attachments')
  }

  // One passive, message-aligned backwards page from the proven native history
  // query service. `throughSeq` pins the fixed cut; `beforeSeq` continues
  // toward older history. It never activates a cold Session.
  async nativeEventsPage(sessionId, { throughSeq, beforeSeq, maxMessages = 200, signal } = {}) {
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.events !== 'function') {
      throw new AdapterError('unsupported_capability', 'native history pages unavailable')
    }
    if (signal?.aborted) throw new AdapterError('unavailable')
    try {
      const value = await controller.events({
        address: { kind: 'session', sessionId },
        ...(Number.isSafeInteger(throughSeq) ? { throughSeq } : {}),
        ...(Number.isSafeInteger(beforeSeq) ? { beforeSeq } : {}),
        maxMessages: Math.max(1, Math.min(500, maxMessages)),
      }, signal ? AbortSignal.any([this.#signal(), signal]) : this.#signal())
      if (signal?.aborted) throw new AdapterError('unavailable')
      const events = (Array.isArray(value?.records) ? value.records : [])
        .map((record) => this.#normalizeEvent(sessionId, record?.event))
        .filter(Boolean)
        .sort((left, right) => left.dshSeq - right.dshSeq)
      return { events, hasMore: value?.hasMore === true, cursor: value?.cursor ?? null }
    } catch (error) {
      if (signal?.aborted) throw new AdapterError('unavailable')
      throw toAdapterError(error)
    }
  }

  // Exact hot ranges are cheap and bounded; cold windows reuse passive pages.
  async nativeEventWindow(sessionId, seq, { signal, before = 256 } = {}) {
    if (signal?.aborted) throw new AdapterError('unavailable')
    if (!Number.isSafeInteger(seq) || seq < 0) throw new AdapterError('invalid_argument')
    try {
      const live = this.#service('sessions')?.get?.(sessionId)
      if (live && typeof live.snapshotEvents === 'function') {
        const events = live.snapshotEvents(Math.max(0, seq - before + 1), seq + 1)
        return events
          .map((event) => this.#normalizeEvent(sessionId, event))
          .filter((event) => Boolean(event) && event.dshSeq <= seq)
      }
      const page = await this.nativeEventsPage(sessionId, { throughSeq: seq, maxMessages: before, signal })
      return page.events.filter((event) => event.dshSeq <= seq).slice(-before)
    } catch (error) {
      if (signal?.aborted) throw new AdapterError('unavailable')
      throw toAdapterError(error)
    }
  }

  async nativeEventsRange(sessionId, fromSeq, toSeqExclusive, { signal } = {}) {
    if (signal?.aborted) throw new AdapterError('unavailable')
    try {
      const live = this.#service('sessions')?.get?.(sessionId)
      if (live && typeof live.snapshotEvents === 'function') {
        return live.snapshotEvents(fromSeq, toSeqExclusive)
          .map((event) => this.#normalizeEvent(sessionId, event))
          .filter((event) => Boolean(event) && event.dshSeq >= fromSeq && event.dshSeq < toSeqExclusive)
          .sort((left, right) => left.dshSeq - right.dshSeq)
      }
    } catch (error) {
      if (signal?.aborted) throw new AdapterError('unavailable')
      throw toAdapterError(error)
    }
    const events = []
    let beforeSeq = toSeqExclusive
    for (let page = 0; page < 8; page++) {
      const value = await this.nativeEventsPage(sessionId, {
        throughSeq: toSeqExclusive - 1,
        beforeSeq: page === 0 ? undefined : beforeSeq,
        maxMessages: 500,
        signal,
      })
      events.unshift(...value.events)
      if (!value.hasMore) break
      const first = value.events[0]?.dshSeq
      if (!Number.isSafeInteger(first) || first <= fromSeq) break
      beforeSeq = first
    }
    return events.filter((event) => event.dshSeq >= fromSeq && event.dshSeq < toSeqExclusive)
      .sort((left, right) => left.dshSeq - right.dshSeq)
  }

  // Image reads buffer the normalized representation; the reader defers this
  // call until its first read so metadata is returned without touching bytes.
  async nativeReadImage(ref, signal) {
    const service = this.#service('attachments')
    if (!service || typeof service.readImage !== 'function') throw new AdapterError('unsupported_capability')
    try {
      const stored = await service.readImage(ref, signal)
      if (!(stored?.data instanceof Uint8Array)) throw new AdapterError('unavailable')
      return stored.data
    } catch (error) {
      throw toAdapterError(error)
    }
  }

  async nativeReadFileStream(ref, signal) {
    const service = this.#service('attachments')
    if (!service || typeof service.readFileStream !== 'function') throw new AdapterError('unsupported_capability')
    try {
      return await service.readFileStream(ref, signal)
    } catch (error) {
      throw toAdapterError(error)
    }
  }

  async getSessionSnapshot(sessionId, {signal} = {}) {
    try {
      if (typeof sessionId !== 'string' || sessionId === '') {
        throw new AdapterError('invalid_argument', 'session id missing')
      }
      if (signal?.aborted) throw new AdapterError('unavailable')
      const controller = this.#service('sessionController')
      // The passive page API is the fixed-cut snapshot path. It is complete,
      // bounded per call and does not truncate a long log to a tail window.
      if (controller && typeof controller.events === 'function') {
        return await this.#passiveSnapshot(sessionId, signal)
      }
      const live = this.#service('sessions')?.get?.(sessionId)
      if (live && typeof live.snapshotEvents === 'function') {
        const all = live.snapshotEvents()
        const events = all.map((event) => this.#normalizeEvent(sessionId, event)).filter(Boolean)
        const summary = await this.#summaryFor(sessionId, signal)
        return {
          summary,
          events,
          cursor: all.at(-1)?.seq ?? -1,
          retained_from: all[0]?.seq ?? 0,
          complete: true,
          source: 'dsh',
        }
      }
      throw new AdapterError('unsupported_capability', 'session snapshot unavailable')
    } catch (error) {
      throw toAdapterError(error)
    }
  }

  async #passiveSnapshot(sessionId, signal) {
    const controller = this.#service('sessionController')
    const records = []
    const seen = new Set()
    let beforeSeq
    let complete = false
    let cursor = null
    for (let page = 0; page < 1000; page++) {
      if (signal?.aborted) throw new AdapterError('unavailable')
      const value = await this.nativeEventsPage(sessionId, { beforeSeq, maxMessages: 500, signal })
      for (const event of value.events) {
        if (!Number.isSafeInteger(event.dshSeq) || seen.has(event.dshSeq)) continue
        seen.add(event.dshSeq)
        records.push(event)
      }
      if (Number.isSafeInteger(value.cursor)) cursor = value.cursor
      if (!value.hasMore) { complete = true; break }
      const first = value.events[0]?.dshSeq
      if (!Number.isSafeInteger(first) || first === beforeSeq) break
      beforeSeq = first
    }
    records.sort((left, right) => left.dshSeq - right.dshSeq)
    const summary = await this.#summaryFor(sessionId, signal)
    return {
      summary,
      events: records,
      cursor: Number.isSafeInteger(cursor) ? cursor : records.at(-1)?.dshSeq ?? -1,
      retained_from: records[0]?.dshSeq ?? 0,
      complete,
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
      data: event.type === 'agent/inbox/spliced' ? null : assistantReplyData(event.type, event.data ?? null),
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

  async sendUserMessage(sessionId, message, mode = 'queue', operationId) {
    if (typeof message !== 'string' || message.trim() === '') {
      throw new DefinitiveExecutionError('invalid_argument', 'message empty')
    }
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.prompt !== 'function') {
      throw new DefinitiveExecutionError('unsupported_capability', 'message prompt unavailable')
    }
    const requestId = operationId || randomId()
    try {
      const receipt = await controller.prompt({
        requestId,
        sessionId,
        mode,
        content: [{ type: 'text', text: message }],
      }, this.#signal())
      if (receipt?.accepted !== true) throw new AdapterError('unavailable')
      return { messageId: requestId, deliveryId: requestId }
    } catch (error) {
      throw new AdapterError(mapAdapterError(error))
    }
  }

  async interrupt(sessionId) {
    const controller = this.#service('sessionController')
    if (!controller || typeof controller.cancel !== 'function') {
      throw new DefinitiveExecutionError('unsupported_capability', 'interrupt unavailable')
    }
    let running = false
    try {
      const state = typeof controller.state === 'function' ? await controller.state({ sessionId }) : null
      running = state?.running === true
    } catch {
      // A state read failure does not block cancellation; it only means no
      // synthetic attempt-end is emitted when the turn state is unknown.
    }
    try {
      const value = controller.cancel({ sessionId })
      if (value && typeof value.then === 'function') await value
      // The interrupted turn owns any in-flight question and approval claims.
      // Release both so a later wait does not keep reporting input-required.
      this.#openQuestionBroker?.interruptSession?.(sessionId)
      this.#releaseApprovals(sessionId)
      if (running) this.publishInterruptedAttempt(sessionId)
      return { accepted: true }
    } catch (error) {
      throw new AdapterError(mapAdapterError(error))
    }
  }

  // Settle every pending approval claim owned by this session. A decision that
  // is already resolving is left to win its own race.
  #releaseApprovals(sessionId) {
    for (const record of this.#approvals.values()) {
      if (record.sessionId !== sessionId || record.state !== 'pending' || record.resolving) continue
      record.state = 'unavailable'
      record.decision = 'unspecified'
      try { record.settle?.('cancelled') } catch { /* Native cancellation remains authoritative. */ }
      this.#emitApproval('resolved', record)
      this.logger?.info('approval_released', { approval: record.approvalId, status: 'interrupted' })
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
      toolName: boundedApprovalText(request?.toolName),
      callId: boundedApprovalText(request?.callId),
      reason: boundedApprovalText(request?.reason, APPROVAL_REASON_MAX),
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
      // A timeout is a cancelled decision, not a missing approval channel.
      // Reporting 'unavailable' makes DSH tell the model no channel exists,
      // which then suppresses later approval attempts in the same session.
      settle('cancelled')
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
      // Same decision as the TTL timer: expiry cancels the approval surface
      // instead of reporting that no approval channel exists.
      record.settle?.('cancelled')
      this.#emitApproval('resolved', record)
      return true
    }
    return false
  }

  async resolveApproval(approvalId, decision, expectedDigest, expectedVersion, context = {}) {
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
      this.assertCommandFresh(context)
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
