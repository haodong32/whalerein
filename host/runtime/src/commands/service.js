// Command and query execution. The host is the only execution dedupe point:
// (device_id, command_id) is persisted with the payload digest, accepted is
// stored before the Source is called, terminal status is stored with the response
// payload, and crash recovery marks unfinished accepted commands unknown.

import path from 'node:path'
import { randomBytes } from 'node:crypto'
import { bytesEqual, sha256Bytes, sha256Text, hex } from '../security/digest.js'
import { summaryToProto } from '../contracts/serialization.js'
import { validateMessageReply } from '../contracts/source.js'
import { AdapterError, DefinitiveExecutionError, PluginError, publicMessage } from '../errors.js'
import { codec as defaultCodec, resolveCodec } from '../protocol.js'
import { MESSAGE_BYTES } from '../access/transport.js'
import { QuestionPager } from './question-pager.js'
import { AttachmentService } from '../attachments/service.js'
import { transportProfileBudget, DEFAULT_TRANSPORT_PROFILE } from '../contracts/transport.js'

const COMMAND_STATUS_TO_PROTO = Object.freeze({
  accepted: 'host_accepted',
  succeeded: 'succeeded',
  failed: 'failed',
  unknown: 'unknown',
})

const APPROVAL_STATE_TO_PROTO = Object.freeze({
  pending: 'APPROVAL_STATE_PENDING',
  approved: 'APPROVAL_STATE_APPROVED',
  rejected: 'APPROVAL_STATE_REJECTED',
  expired: 'APPROVAL_STATE_EXPIRED',
  unavailable: 'APPROVAL_STATE_UNAVAILABLE',
})

const APPROVAL_DECISION_TO_PROTO = Object.freeze({
  unspecified: 'APPROVAL_DECISION_UNSPECIFIED',
  approve: 'APPROVAL_DECISION_APPROVE',
  reject: 'APPROVAL_DECISION_REJECT',
})

// Native text is bounded before it enters the wire. Tool arguments and other
// native request data are never part of an approval, so they cannot leak here.
const APPROVAL_REASON_MAX = 1024
const APPROVAL_FIELD_MAX = 256

function boundedText(value, max = APPROVAL_FIELD_MAX) {
  if (typeof value !== 'string' || value === '') return ''
  const clean = value.replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g, ' ')
  const points = Array.from(clean)
  return points.length <= max ? clean : points.slice(0, max).join('')
}

function approvalToProto(approval) {
  return {
    approval_id: approval.approvalId,
    session_id: approval.sessionId,
    tool_name: boundedText(approval.toolName),
    call_id: boundedText(approval.callId),
    reason: boundedText(approval.reason, APPROVAL_REASON_MAX),
    digest: approval.digest,
    version: approval.version,
    expires_at_unix_ms: approval.expiresAt,
    state: APPROVAL_STATE_TO_PROTO[approval.state] ?? 'APPROVAL_STATE_UNAVAILABLE',
    decision: APPROVAL_DECISION_TO_PROTO[approval.decision] ?? 'APPROVAL_DECISION_UNSPECIFIED',
  }
}

/** Terminal/durable view when the native source no longer holds the approval. */
function approvalRecordToProto(record) {
  return {
    approval_id: record.approvalId,
    session_id: record.sessionId,
    tool_name: boundedText(record.toolName),
    call_id: boundedText(record.callId),
    reason: '',
    digest: new Uint8Array(record.digest),
    version: record.version,
    expires_at_unix_ms: record.expiresAt,
    state: APPROVAL_STATE_TO_PROTO[record.state] ?? 'APPROVAL_STATE_UNAVAILABLE',
    decision: APPROVAL_DECISION_TO_PROTO[record.decision] ?? 'APPROVAL_DECISION_UNSPECIFIED',
  }
}

function failed(code) {
  return { status: 'failed', errorCode: code }
}

const LIFECYCLE_DISPOSITIONS = Object.freeze(['READY', 'READY_WITH_WARNING', 'FAILED', 'UNKNOWN'])
const LIFECYCLE_PHASES = Object.freeze(['PREFLIGHT', 'NATIVE', 'ATTACH', 'PERSIST', 'COMPLETE'])
const RECONCILE_MODES = Object.freeze(['OPERATION_ID', 'PREFERRED_SESSION_ID', 'PARENT_SESSION_ID', 'MANUAL'])
const RETRYABLE_PREFLIGHT_REASONS = Object.freeze(new Set([
  'INVALID_ARGUMENT',
  'WORKSPACE_NOT_FOUND',
  'WORKSPACE_UNAVAILABLE',
  'PRESET_NOT_FOUND',
  'PRESET_UNAVAILABLE',
]))

function safeWarningCode(value) {
  return typeof value === 'string' && /^[a-z][a-z0-9_]{0,63}$/.test(value) ? value : ''
}

// One lifecycle receipt serializer for every outcome. Optional fields are
// omitted so legacy READY receipts keep their existing wire shape.
function lifecycleReceipt({
  operation,
  operationId = '',
  disposition,
  phase = '',
  sessionId = '',
  parentSessionId = '',
  warningCode = '',
  reasonCode = '',
  retryable = false,
  reconcileMode = '',
  reconcileKey = '',
}) {
  const receipt = {
    operation,
    disposition,
    ...(operationId ? { operation_id: operationId } : {}),
    ...(LIFECYCLE_PHASES.includes(phase) ? { phase } : {}),
    ...(retryable ? { retryable: true } : {}),
  }
  if (sessionId) receipt.session_id = sessionId
  if (parentSessionId) receipt.parent_session_id = parentSessionId
  if (warningCode) receipt.warning_code = warningCode
  if (reasonCode) receipt.reason_code = reasonCode
  if (RECONCILE_MODES.includes(reconcileMode)) {
    receipt.reconcile_mode = reconcileMode
    if (reconcileKey) receipt.reconcile_key = reconcileKey
  }
  return receipt
}

function nativeReason(failureCode) {
  const code = typeof failureCode === 'string' && failureCode !== '' ? failureCode : 'internal'
  return `NATIVE_${code.toUpperCase()}`
}

function preflightReason(code) {
  return `PREFLIGHT_${typeof code === 'string' && code !== '' ? code.toUpperCase() : 'INTERNAL'}`
}

function waitStateToProto(state) {
  const kinds = { approval: 'INPUT_APPROVAL', question: 'INPUT_QUESTION', other: 'INPUT_OTHER' }
  return { active: state.active, running: state.running, queue_available: state.queueAvailable,
    pending_input: (state.pendingInput ?? []).map(kind => kinds[kind] ?? kinds.other) }
}

function scopeCode(scope) { return scope.controller.signal.reason === 'expired' ? 'expired' : 'unavailable' }

function cancelledObservationCode(entry, now, expiresAt) {
  if (now >= Number(expiresAt)) return 'expired'
  if (entry.reason === 'channel_replaced') return 'channel_replaced'
  if (entry.reason === 'revoked') return 'forbidden'
  return 'unavailable'
}

export class CommandService {
  /**
   * @param {object} options
   * @param {string} [options.hostId] configured and authenticated Host identity.
   * @param {import('../state/store.js').StateStore} options.store
   * @param {import('../contracts/source.js').SourceAdapter} options.adapter
   * @param {import('../events/watches.js').WatchedEvents} options.events
   * @param {object} [options.logger]
   * @param {() => number} [options.now]
   * @param {(request: object) => Promise<void>} [options.onAccepted] publish host_accepted.
   * @param {import('../protocol.js').ProtoCodec} [options.codec]
   * @param {object} [options.security] authenticated channel authority for attachments.
   * @param {object} [options.attachmentPolicy] Host attachment policy overrides.
   * @param {object} [options.attachments] injected transfer service (tests).
   */
  constructor({
    store, adapter, events, hostId, logger, now = Date.now, onAccepted = async () => {},
    codec = defaultCodec, security = null, attachmentPolicy = null, attachments = null,
    transportProfile = DEFAULT_TRANSPORT_PROFILE,
  }) {
    this.hostId = hostId
    this.store = store
    this.adapter = adapter
    this.events = events
    this.logger = logger
    this.now = now
    this.onAccepted = onAccepted
    this.codec = resolveCodec(codec)
    this.waits = new Map()
    this.questionAnswers = new Map()
    this.questionPager = new QuestionPager(this.codec, { key: this.#questionCursorKey() })
    this.modelQueries = new Map()
    this.sendGates = new Map()
    this.requests = new Set()
    let envelopeBytes = 0
    if (transportProfile && typeof transportProfile === 'object'
      && Number.isSafeInteger(transportProfile.envelope_bytes)) {
      envelopeBytes = transportProfile.envelope_bytes
    } else {
      try {
        envelopeBytes = transportProfileBudget(
          typeof transportProfile === 'string' ? transportProfile : DEFAULT_TRANSPORT_PROFILE,
        ).envelope_bytes
      } catch { envelopeBytes = 0 }
    }
    this.attachments = attachments ?? new AttachmentService({
      hostId,
      adapter,
      security,
      store,
      logger,
      now,
      policy: attachmentPolicy,
      transportProfile,
      envelopeBytes,
    })
  }

  // Native prompt modes are only queue/steer; there is no atomic
  // start-if-idle primitive. Serialize start-if-idle sends per session so the
  // second request observes the first turn instead of racing the idle check.
  async #withSendGate(sessionId, operation) {
    const previous = this.sendGates.get(sessionId) ?? Promise.resolve()
    let release
    const current = new Promise((resolve) => { release = resolve })
    const tail = previous.then(() => current)
    this.sendGates.set(sessionId, tail)
    await previous
    try {
      return await operation()
    } finally {
      release()
      if (this.sendGates.get(sessionId) === tail) this.sendGates.delete(sessionId)
    }
  }

  // Pagination cursors must survive a Host restart. Derive the pager key from
  // the persisted state store (created once, like the log salt); memory-only
  // stores keep an ephemeral key.
  #questionCursorKey() {
    try {
      const stored = this.store?.getMeta?.('question_cursor_key')
      if (typeof stored === 'string' && stored !== '') {
        const key = Buffer.from(stored, 'base64')
        if (key.length === 32) return key
      }
      const key = randomBytes(32)
      this.store?.setMeta?.('question_cursor_key', key.toString('base64'))
      return key
    } catch {
      return randomBytes(32)
    }
  }

  cancelChannel(deviceId, channelId, reason = 'unavailable') {
    this.#cancelScopes(entry => entry.deviceId === deviceId && entry.channelId === channelId, reason)
    this.attachments.cancelChannel(deviceId, channelId, reason)
  }

  /** Abort all executions and reads owned by a revoked Device. */
  revokeDevice(deviceId) {
    this.#cancelScopes(entry => entry.deviceId === deviceId, 'revoked')
    this.attachments.revokeDevice(deviceId)
  }

  #cancelScopes(matches, reason) {
    for (const entry of this.requests) {
      if (matches(entry)) entry.controller.abort('unavailable')
    }
    for (const entry of this.modelQueries.values()) {
      if (matches(entry)) entry.controller.abort('unavailable')
    }
    // An admitted HTTP answer is fenced by its exact lease or Device revocation.
    if (reason === 'revoked') {
      for (const entry of this.questionAnswers.values()) {
        if (matches(entry)) entry.controller.abort('unavailable')
      }
    }
    for (const wait of this.waits.values()) {
      if (matches(wait)) {
        wait.reason = reason
        wait.controller.abort()
      }
    }
  }

  /** Abort one request within the authenticated Device scope. */
  cancelRequest(deviceId, requestId, reason) {
    if (!deviceId || !requestId) return
    for (const entry of this.requests) {
      if (entry.requestId === requestId && entry.deviceId === deviceId) entry.controller.abort('unavailable')
    }
    for (const entry of this.questionAnswers.values()) {
      if (entry.requestId === requestId && entry.deviceId === deviceId) entry.controller.abort('unavailable')
    }
    for (const wait of this.waits.values()) {
      if (wait.requestId === requestId && wait.deviceId === deviceId) {
        wait.reason = reason
        wait.controller.abort()
      }
    }
  }

  /** Exact live scope metadata used to authenticate a cancellation notice. */
  requestScope(requestId) {
    if (!requestId) return null
    for (const entry of this.requests) {
      if (entry.requestId === requestId) {
        return { deviceId: entry.deviceId, channelId: entry.channelId, commandId: entry.commandId ?? '', payloadDigest: entry.payloadDigest ?? null }
      }
    }
    for (const entry of this.questionAnswers.values()) {
      if (entry.requestId === requestId) {
        return { deviceId: entry.deviceId, channelId: entry.channelId, commandId: entry.commandId, payloadDigest: null }
      }
    }
    return null
  }

  /**
   * Persist an exact pre-admission cancellation fence in the existing command
   * journal. An already journaled command keeps its proven outcome; a delayed
   * request with the same identity replays this failed terminal result instead
   * of invoking the source.
   */
  fenceCancelledCommand({ deviceId, requestId = '', commandId, digest, expiresAt = 0, errorCode = 'unavailable' }) {
    return this.store.fenceCancelledCommand({ deviceId, requestId, commandId, digest, expiresAt, errorCode, now: this.now() })
  }

  closeRequests() {
    for (const entry of this.requests) entry.controller.abort('unavailable')
  }

  closeModelQueries() {
    for (const entry of this.modelQueries.values()) entry.controller.abort('unavailable')
  }

  closeQuestionAnswers() {
    for (const entry of this.questionAnswers.values()) entry.controller.abort('unavailable')
  }

  closeWaits() {
    for (const wait of this.waits.values()) wait.controller.abort()
  }

  closeAttachments() {
    return this.attachments.disposeAll('host_stop')
  }

  /** Crash recovery is intentionally idempotent. */
  recover() {
    this.store.pruneCommandResults(this.now())
    const count = this.store.recoverAcceptedCommands(this.now())
    if (count > 0) this.#log('warn', 'commands_marked_unknown', { count, code: 'unavailable' })
    // A pending native approval cannot outlive the process that owned its
    // waterfall. Keep the identity as unavailable so a late decision fails with
    // approval_unavailable instead of silently disappearing or reading unknown.
    let invalidated = 0
    try {
      invalidated = this.store.invalidatePendingApprovals({ now: this.now() })
      this.store.pruneApprovalRecords(this.now())
    } catch {
      this.#log('error', 'approval_ledger_unavailable', { code: 'internal' })
    }
    if (invalidated > 0) this.#log('warn', 'approvals_invalidated', { count: invalidated, reason: 'process_interrupted' })
    return count
  }

  #recordApproval(approval) {
    try {
      this.store.recordApproval({
        approvalId: approval.approvalId,
        sessionId: approval.sessionId,
        toolName: approval.toolName,
        callId: approval.callId,
        digest: approval.digest,
        version: approval.version,
        expiresAt: approval.expiresAt,
        now: this.now(),
      })
    } catch {
      // The ledger is an observability/recovery aid; it must never break the
      // authoritative native approval path.
      this.#log('warn', 'approval_ledger_write_failed', { code: 'internal' })
    }
  }

  /** Retire pending ledger records whose own TTL has passed. */
  #expireLedgerApprovals(sessionId = '') {
    try {
      for (const record of this.store.listApprovalRecords({ sessionId, state: 'pending', limit: 64 })) {
        if (record.expiresAt > 0 && record.expiresAt <= this.now()) this.#settleApproval(record.approvalId, 'expired')
      }
    } catch {
      this.#log('warn', 'approval_ledger_read_failed', { code: 'internal' })
    }
  }

  #ledgerApproval(approvalId) {
    try { return this.store.getApprovalRecord(approvalId) }
    catch { return null }
  }

  #settleApproval(approvalId, state, decision = 'unspecified', invalidatedReason = null) {
    try { return this.store.settleApproval({ approvalId, state, decision, invalidatedReason, now: this.now() }) }
    catch { this.#log('warn', 'approval_ledger_write_failed', { code: 'internal' }); return null }
  }

  /** Re-read the authoritative source state and mirror one terminal outcome. */
  async #syncApprovalRecord(approvalId) {
    let approval = null
    try { approval = await this.adapter.getApproval(approvalId) } catch { approval = null }
    if (approval) {
      this.#recordApproval(approval)
      if (approval.state !== 'pending') {
        this.#settleApproval(approvalId, approval.state, approval.decision ?? 'unspecified')
      }
      return
    }
    const recorded = this.#ledgerApproval(approvalId)
    if (recorded?.state === 'pending') {
      this.#settleApproval(approvalId, 'unavailable', 'unspecified', 'native_missing')
    }
  }

  /**
   * Handle one decrypted RequestPayload.
   * @returns {Promise<{status: string, response?: object, resultPayload?: Buffer, errorCode?: string}>}
   */
  async handle({ request, payload, questionResponseBudget = MESSAGE_BYTES }) {
    if (request.kind !== 'REQUEST_KIND_COMMAND' && request.kind !== 'REQUEST_KIND_QUERY') throw new PluginError('invalid_argument', 'request kind invalid')
    const scope={requestId:request.request_id,deviceId:request.device_id,channelId:request.channel_id,controller:new AbortController()}
    // Native wait owns its own deadline. Other requests stop waiting at the
    // already-existing HostRequest expiry, even if a source ignores cancellation.
    let expiryTimer
    const expires=Number(request.expires_at_unix_ms ?? 0)
    const scheduleExpiry=remaining=>{
      expiryTimer=setTimeout(()=>{
        const left=expires-this.now()
        if(left>0) scheduleExpiry(left)
        else scope.controller.abort('expired')
      },Math.min(remaining,2147483647))
      expiryTimer.unref?.()
    }
    if(expires>0 && !(payload.wait_session || payload.read_wait_state)) {
      const remaining=expires-this.now()
      if(remaining<=0) scope.controller.abort('expired')
      else scheduleExpiry(remaining)
    }
    this.requests.add(scope)
    try {
      if(scope.controller.signal.aborted) return request.kind==='REQUEST_KIND_COMMAND' ? {status:'rejected',errorCode:'expired'} : failed('expired')
      if (request.kind === 'REQUEST_KIND_COMMAND') return await this.#command(request, payload, scope)
      try {
        // Native wait owns more specific channel/revocation outcomes.
        if (payload.wait_session || payload.read_wait_state) return await this.#query(request, payload, questionResponseBudget, scope)
        const result = await this.#untilCancelled(this.#query(request, payload, questionResponseBudget, scope), scope)
        // Never return source data if its authenticated channel was invalidated.
        return scope.controller.signal.aborted && result.status === 'succeeded' ? failed(scopeCode(scope)) : result
      } catch (error) {
        if (scope.controller.signal.aborted) return failed(scopeCode(scope))
        throw error
      }
    } finally { if(expiryTimer)clearTimeout(expiryTimer);this.requests.delete(scope) }
  }

  async #untilCancelled(promise, scope) {
    const signal=scope.controller.signal
    if (signal.aborted) {
      // The source may have synchronously invalidated its own channel while
      // returning a promise. Consume its later rejection without changing the
      // already-cancelled outcome.
      Promise.resolve(promise).catch(()=>{})
      throw new AdapterError(scopeCode(scope))
    }
    let onAbort
    const aborted=new Promise((_,reject)=>{
      onAbort=()=>reject(new AdapterError(scopeCode(scope)))
      signal.addEventListener('abort',onAbort,{once:true})
    })
    try { return await Promise.race([promise,aborted]) }
    finally { signal.removeEventListener('abort',onAbort) }
  }

  // --- queries ------------------------------------------------------------

  async #query(request, payload, questionResponseBudget, scope) {
    try {
      if (payload.get_host_capabilities) {
        return { status: 'succeeded', response: { host_capabilities: await this.adapter.describeHostCapabilities() } }
      }
      if (payload.list_models || payload.read_session_model) return await this.#modelQuery(request, payload)
      if (payload.wait_session) return await this.#waitSession(request, payload.wait_session)
      if (payload.read_wait_state) return await this.#readWaitState(request, payload.read_wait_state)
      if (payload.cancel_session_wait) {
        const { session_id, wait_id } = payload.cancel_session_wait
        const key = JSON.stringify(['wait', request.device_id, request.channel_id, session_id, wait_id])
        this.waits.get(key)?.controller.abort()
        return { status: 'succeeded', response: {} }
      }
      if (payload.cancel_question_answer) {
        const { session_id, command_id } = payload.cancel_question_answer
        if (!request.device_id || !session_id || !command_id) return failed('invalid_argument')
        let cancellationRequested = false
        for (const entry of this.questionAnswers.values()) {
          if (entry.deviceId === request.device_id && entry.sessionId === session_id && entry.commandId === command_id && entry.phase === 'QUESTION_OPEN') {
            entry.controller.abort('cancelled')
            cancellationRequested = true
          }
        }
        return { status: 'succeeded', response: { question_cancel_receipt: { cancellation_requested: cancellationRequested } } }
      }
      if (payload.list_sessions) {
        const sessions = await this.adapter.listSessions()
        return { status: 'succeeded', response: { session_list: { sessions: sessions.map(summaryToProto) } } }
      }
      if (payload.get_session) {
        const session = await this.adapter.getSession(payload.get_session.session_id)
        return { status: 'succeeded', response: { session_info: summaryToProto(session) } }
      }
      if (payload.read_pending_queue) {
        await this.#requireQueueCapability(payload.read_pending_queue.session_id, 'pending_queue_read', scope)
        this.#assertScope(scope)
        return {status:'succeeded', response:{pending_queue:await this.adapter.readPendingQueue(payload.read_pending_queue.session_id, {signal:scope.controller.signal})}}
      }
      if (payload.list_questions) {
        const query = payload.list_questions
        await this.#requireQuestionCapability(query.session_id, 'questions_read', scope)
        this.#assertScope(scope)
        const source = await this.adapter.listQuestionPage(query.session_id, { ...query, cursor: '', signal:scope.controller.signal })
        this.#assertScope(scope)
        const maxBytes = typeof questionResponseBudget === 'function' ? questionResponseBudget() : questionResponseBudget
        const page = this.questionPager.page({ deviceId: request.device_id, sessionId: query.session_id,
          cursor: query.cursor, source, maxBytes })
        return { status: 'succeeded', response: { question_page: page } }
      }
      if (payload.get_question) {
        const query = payload.get_question
        await this.#requireQuestionCapability(query.session_id, 'questions_read', scope)
        this.#assertScope(scope)
        const batch = await this.adapter.getQuestion(query.session_id, query.batch_id, {signal:scope.controller.signal})
        this.#assertScope(scope)
        if (!batch) return failed('not_found')
        return { status: 'succeeded', response: { question_batch: batch } }
      }
      if (payload.read_message_reply) {
        const read = payload.read_message_reply
        const submittedId = read.submitted_message_id
        if (!read.session_id || !submittedId || submittedId.length > 512) return failed('invalid_argument')
        if (typeof this.adapter.readMessageReply !== 'function') return failed('unsupported_capability')
        const result = await this.adapter.readMessageReply(read.session_id, submittedId, scope.controller.signal)
        this.#assertScope(scope)
        // The request scope owns the submitted identity. A Source may only
        // report the typed state/reference/evidenced input kinds.
        let reply
        try {
          reply = validateMessageReply({
            ...result,
            session_id: read.session_id,
            submitted_message_id: submittedId,
          })
        } catch (error) {
          const code = error?.code === 'payload_too_large' ? 'payload_too_large' : 'invalid_argument'
          return failed(code)
        }
        return { status: 'succeeded', response: { message_reply: reply } }
      }
      if (payload.read_history) {
        return { status: 'succeeded', response: { history_page: await this.events.readHistory(payload.read_history.session_id, payload.read_history, scope.controller.signal) } }
      }
      // Attachment transfers are ordinary encrypted queries. The transfer
      // service resolves the occurrence through the Source seam, binds the
      // handle to this device/channel/session/occurrence and enforces the part
      // and result budgets.
      if (payload.open_attachment) {
        const open = payload.open_attachment
        if (!request.device_id || !request.channel_id || !open?.session) return failed('invalid_argument')
        if (open.session.host_id && open.session.host_id !== this.hostId) return failed('forbidden')
        const metadata = await this.attachments.open({
          request,
          deviceId: request.device_id,
          channelId: request.channel_id,
          sessionId: open.session.session_id,
          attachmentRef: open.attachment_ref,
          signal: scope.controller.signal,
        })
        this.#assertScope(scope)
        return { status: 'succeeded', response: { open_attachment_result: { attachment: metadata } } }
      }
      if (payload.pull_attachment) {
        const pull = payload.pull_attachment
        if (!request.device_id || !request.channel_id || !pull?.session) return failed('invalid_argument')
        if (pull.session.host_id && pull.session.host_id !== this.hostId) return failed('forbidden')
        const part = await this.attachments.pull({
          request,
          deviceId: request.device_id,
          channelId: request.channel_id,
          sessionId: pull.session.session_id,
          attachmentRef: pull.attachment_ref,
          transferId: pull.transfer_id,
          offset: Number(pull.offset ?? 0),
          maxBytes: pull.max_bytes === undefined ? undefined : Number(pull.max_bytes),
          signal: scope.controller.signal,
        })
        this.#assertScope(scope)
        return { status: 'succeeded', response: { attachment_part: part } }
      }
      if (payload.close_attachment) {
        const close = payload.close_attachment
        if (!request.device_id || !request.channel_id || !close?.session) return failed('invalid_argument')
        if (close.session.host_id && close.session.host_id !== this.hostId) return failed('forbidden')
        const result = await this.attachments.close({
          request,
          deviceId: request.device_id,
          channelId: request.channel_id,
          sessionId: close.session.session_id,
          attachmentRef: close.attachment_ref,
          transferId: close.transfer_id,
          cancel: close.cancel === true,
        })
        this.#assertScope(scope)
        return { status: 'succeeded', response: { close_attachment_result: result } }
      }
      if (payload.watch_session) {
        const watch = payload.watch_session
        const interests = watch.interests
        if (typeof watch.session_id !== 'string' || watch.session_id === '') return failed('invalid_argument')
        if (!interests || typeof interests !== 'object') return failed('invalid_argument')
        // Proto3 omits an absent false scalar, so only an absent message is
        // missing; an explicit non-boolean value is never coerced.
        const conversation = interests.conversation === undefined ? false : interests.conversation
        const questions = interests.questions === undefined ? false : interests.questions
        if (typeof conversation !== 'boolean' || typeof questions !== 'boolean' || (!conversation && !questions)) return failed('invalid_argument')
        return {
          status: 'succeeded',
          response: {
            watch_ready: await this.events.watch(request.device_id, request.channel_id, watch.session_id, scope.controller.signal,
              { conversation, questions }),
          },
        }
      }
      if (payload.check_versions) {
        const check = payload.check_versions
        if (typeof check.session_id !== 'string' || check.session_id === '') return failed('invalid_argument')
        return {
          status: 'succeeded',
          response: {
            version_checks: await this.events.versions(
              request.device_id, request.channel_id, check.session_id, scope.controller.signal,
            ),
          },
        }
      }
      if (payload.unwatch_session) {
        this.events.unwatch(request.device_id, request.channel_id, payload.unwatch_session.watch_id)
        return { status: 'succeeded', response: {} }
      }
      if (payload.list_approvals) {
        const approvals = await this.adapter.listApprovals(
          payload.list_approvals.session_id ?? '',
        )
        for (const approval of approvals) this.#recordApproval(approval)
        this.#expireLedgerApprovals(payload.list_approvals.session_id ?? '')
        return {
          status: 'succeeded',
          response: { approval_list: { approvals: approvals.map(approvalToProto) } },
        }
      }
      if (payload.get_approval) {
        const approval = await this.adapter.getApproval(payload.get_approval.approval_id)
        if (approval) {
          this.#recordApproval(approval)
          return { status: 'succeeded', response: { approval: approvalToProto(approval) } }
        }
        // Terminal state remains observable after the native source (or the
        // process that owned it) is gone. Pending-with-no-native is a lost
        // approval, not a fresh one.
        const recorded = this.#ledgerApproval(payload.get_approval.approval_id)
        if (!recorded) return failed('not_found')
        if (recorded.state === 'pending') {
          const expired = recorded.expiresAt > 0 && recorded.expiresAt <= this.now()
          this.#settleApproval(recorded.approvalId, expired ? 'expired' : 'unavailable', 'unspecified',
            expired ? null : 'native_missing')
          return { status: 'succeeded', response: { approval: approvalRecordToProto(this.#ledgerApproval(recorded.approvalId)) } }
        }
        return { status: 'succeeded', response: { approval: approvalRecordToProto(recorded) } }
      }
      if (payload.get_command_result) {
        return { status: 'succeeded', response: { command_result: this.#commandResult(request.device_id, payload.get_command_result.command_id) } }
      }
      if (payload.ping) {
        return { status: 'succeeded', response: { pong: { host_time_unix_ms: this.now() } } }
      }
      return failed('invalid_argument')
    } catch (error) {
      if (error instanceof PluginError && error.code === 'snapshot_required') {
        return { status: 'failed', errorCode: 'snapshot_required', response: { error_code: 'snapshot_required', error_detail: publicMessage('snapshot_required') } }
      }
      // Only the Host's scoped channel lifecycle may assert replacement.
      // For wait reads, authenticate Source failures in the current channel so
      // Core can distinguish them from channel/device authorization failures.
      if (error instanceof AdapterError || error instanceof PluginError) {
        const code = error.code === 'channel_replaced' ? 'unavailable' : error.code
        if (payload.wait_session || payload.read_wait_state) {
          return { status: 'failed', errorCode: code, sourceFailure: true,
            response: { error_code: code, error_detail: publicMessage(code) } }
        }
        return failed(code)
      }
      return failed('internal')
    }
  }

  async #readWaitState(request, read) {
    if (!(this.adapter.describe?.().capabilities ?? []).includes('wait')) return failed('unsupported_capability')
    if (!request.device_id || !request.channel_id || !request.request_id || !read.session_id) return failed('invalid_argument')
    const lifetime = Number(request.expires_at_unix_ms) - this.now()
    if (!Number.isSafeInteger(lifetime) || lifetime <= 0) return failed('expired')
    const key = JSON.stringify(['state', request.device_id, request.channel_id, request.request_id])
    if (this.waits.has(key)) return failed('conflict')
    const controller = new AbortController()
    const entry = { deviceId: request.device_id, channelId: request.channel_id, controller }
    this.waits.set(key, entry)
    const timer = setTimeout(() => controller.abort(), Math.min(lifetime, 2147483647))
    timer.unref?.()
    let onAbort
    try {
      const aborted = new Promise((_, reject) => {
        onAbort = () => reject(controller.signal.reason)
        controller.signal.addEventListener('abort', onAbort, { once: true })
      })
      const state = await Promise.race([this.adapter.readWaitState(read.session_id, controller.signal), aborted])
      if (controller.signal.aborted) return failed(cancelledObservationCode(entry, this.now(), request.expires_at_unix_ms))
      if (this.now() >= Number(request.expires_at_unix_ms)) return failed('expired')
      return { status: 'succeeded', response: { session_wait_state_result: { session_id: read.session_id, state: waitStateToProto(state) } } }
    } catch (error) {
      if (controller.signal.aborted) return failed(cancelledObservationCode(entry, this.now(), request.expires_at_unix_ms))
      throw error
    } finally {
      clearTimeout(timer)
      controller.signal.removeEventListener('abort', onAbort)
      if (this.waits.get(key) === entry) this.waits.delete(key)
    }
  }

  async #waitSession(request, wait) {
    if (!(this.adapter.describe?.().capabilities ?? []).includes('wait')) return failed('unsupported_capability')
    if (!request.device_id || !request.channel_id || !wait.session_id || !wait.wait_id ||
        !Number.isSafeInteger(wait.timeout_ms) || wait.timeout_ms <= 0 || wait.timeout_ms > 2147483647) return failed('invalid_argument')
    const lifetime = Number(request.expires_at_unix_ms) - this.now()
    // Reserve room for encoding and relaying the response. Expiry always
    // remains an error even if the native operation does not observe abort.
    const timeout = Math.min(wait.timeout_ms, lifetime - 1000)
    if (!Number.isSafeInteger(lifetime) || timeout <= 0) return failed('expired')
    const key = JSON.stringify(['wait', request.device_id, request.channel_id, wait.session_id, wait.wait_id])
    if (this.waits.has(key)) return failed('conflict')
    const controller = new AbortController()
    const entry = { deviceId: request.device_id, channelId: request.channel_id, controller }
    this.waits.set(key, entry)
    const timer = setTimeout(() => controller.abort(), Math.min(lifetime, 2147483647))
    timer.unref?.()
    let onAbort
    try {
      const aborted = new Promise((_, reject) => {
        onAbort = () => reject(controller.signal.reason)
        controller.signal.addEventListener('abort', onAbort, { once: true })
      })
      const result = await Promise.race([this.adapter.waitSession(wait.session_id, timeout, controller.signal), aborted])
      if (controller.signal.aborted) return failed(cancelledObservationCode(entry, this.now(), request.expires_at_unix_ms))
      if (this.now() >= Number(request.expires_at_unix_ms)) return failed('expired')
      const reasons = { idle: 'WAIT_IDLE', 'input-required': 'WAIT_INPUT_REQUIRED', timeout: 'WAIT_TIMEOUT' }
      if (!reasons[result?.reason] || !result.state) return failed('internal')
      return { status: 'succeeded', response: { session_wait_result: {
        session_id: wait.session_id, wait_id: wait.wait_id, reason: reasons[result.reason],
        state: waitStateToProto(result.state),
      } } }
    } catch (error) {
      if (controller.signal.aborted) return failed(cancelledObservationCode(entry, this.now(), request.expires_at_unix_ms))
      throw error
    } finally {
      controller.signal.removeEventListener('abort', onAbort)
      clearTimeout(timer)
      if (this.waits.get(key) === entry) this.waits.delete(key)
    }
  }

  async #modelQuery(request, payload) {
    if (!request.device_id || !request.channel_id || !request.request_id) return failed('invalid_argument')
    const expires = Number(request.expires_at_unix_ms)
    const lifetime = expires - this.now()
    if (!Number.isSafeInteger(expires) || !Number.isSafeInteger(lifetime) || lifetime <= 0) return failed('expired')
    const key = JSON.stringify([request.device_id, request.channel_id, request.request_id])
    if (this.modelQueries.has(key)) return failed('conflict')
    const controller = new AbortController()
    const entry = { deviceId: request.device_id, channelId: request.channel_id, controller }
    this.modelQueries.set(key, entry)
    let timer
    const scheduleDeadline = remaining => {
      timer = setTimeout(() => {
        if (remaining > 2147483647) {
          const next = expires - this.now()
          if (next > 0) { scheduleDeadline(next); return }
        }
        controller.abort('expired')
      }, Math.min(remaining, 2147483647))
      timer.unref?.()
    }
    scheduleDeadline(lifetime)
    let onAbort
    try {
      const aborted = new Promise((_, reject) => {
        onAbort = () => reject(new AdapterError(controller.signal.reason === 'expired' ? 'expired' : 'unavailable'))
        controller.signal.addEventListener('abort', onAbort, { once: true })
      })
      const read = async () => {
        const capabilities = await this.adapter.describeHostCapabilities()
        controller.signal.throwIfAborted()
        if (payload.list_models) {
          if (!capabilities?.model_catalog) throw new AdapterError('unsupported_capability')
          return { model_catalog: await this.adapter.listModels({ signal: controller.signal }) }
        }
        if (!capabilities?.model_read) throw new AdapterError('unsupported_capability')
        return { session_model: await this.adapter.readSessionModel(payload.read_session_model.session_id, { signal: controller.signal }) }
      }
      const response = await Promise.race([read(), aborted])
      if (controller.signal.aborted) return failed(controller.signal.reason === 'expired' ? 'expired' : 'unavailable')
      if (this.now() >= expires) return failed('expired')
      return { status: 'succeeded', response }
    } catch (error) {
      if (controller.signal.aborted) return failed(controller.signal.reason === 'expired' ? 'expired' : 'unavailable')
      throw error
    } finally {
      clearTimeout(timer)
      controller.signal.removeEventListener('abort', onAbort)
      if (this.modelQueries.get(key) === entry) this.modelQueries.delete(key)
    }
  }

  #commandResult(deviceId, commandId) {
    const command = this.store.getCommand(deviceId, commandId)
    if (!command) return { found: false, status: 'online_status_unspecified' }
    let resultPayload = command.resultPayload ?? new Uint8Array(0)
    // A stale in-flight operation read after restart is unresolved. Surface a
    // stable UNKNOWN receipt keyed by the persisted operation identity rather
    // than an empty or success-looking payload.
    const operation = command.operation
    const proven = operation?.outcome && operation.outcome !== 'UNKNOWN'
      && (operation.outcome !== 'READY_WITH_WARNING' || safeWarningCode(operation.warningCode))
    if (command.status === 'unknown' && proven) {
      // The native action completed with a proven outcome, but its terminal
      // command write failed. Recovery returns that outcome instead of UNKNOWN.
      resultPayload = this.codec.encode('whalerein.v1.ResponsePayload', {
        lifecycle_receipt: lifecycleReceipt({
          operation: operation.operation === 'fork' ? 'fork' : 'create',
          operationId: operation.operationId,
          disposition: operation.outcome,
          phase: operation.phase,
          sessionId: operation.sessionId ?? '',
          parentSessionId: operation.parentSessionId ?? '',
          warningCode: operation.warningCode ?? '',
        }),
      })
    } else if (command.status === 'unknown' && operation) {
      const kind = operation.operation === 'fork' ? 'fork' : 'create'
      const preferred = operation.preferredSessionId ?? ''
      const parent = operation.parentSessionId ?? ''
      const reconcile = kind === 'create'
        ? preferred !== '' ? { mode: 'PREFERRED_SESSION_ID', key: preferred } : { mode: 'OPERATION_ID', key: operation.operationId }
        : parent !== '' ? { mode: 'PARENT_SESSION_ID', key: parent } : { mode: 'MANUAL', key: '' }
      try {
        resultPayload = this.codec.encode('whalerein.v1.ResponsePayload', {
          lifecycle_receipt: lifecycleReceipt({
            operation: kind, operationId: operation.operationId, disposition: 'UNKNOWN',
            phase: 'NATIVE', reasonCode: 'PROCESS_INTERRUPTED',
            reconcileMode: reconcile.mode, reconcileKey: reconcile.key,
          }),
        })
      } catch {
        resultPayload = new Uint8Array(0)
      }
    }
    return {
      found: true,
      status: COMMAND_STATUS_TO_PROTO[command.status] ?? 'unknown',
      result_payload: resultPayload,
      error_code: command.errorCode ?? 'error_code_unspecified',
    }
  }

  async #requireQuestionCapability(sessionId, capability, scope) {
    if (typeof sessionId !== 'string' || !sessionId) throw new AdapterError('invalid_argument')
    const session = await this.adapter.getSession(sessionId, {signal:scope?.controller.signal})
    this.#assertScope(scope)
    if (!session || session.sessionId !== sessionId) throw new AdapterError('not_found')
    if (session.sourceCapabilities?.[capability] !== true) throw new AdapterError('unsupported_capability')
  }

  async #requireQueueCapability(sessionId, capability, scope) {
    if (typeof sessionId !== 'string' || !sessionId) throw new AdapterError('invalid_argument')
    const session = await this.adapter.getSession(sessionId, {signal:scope?.controller.signal})
    this.#assertScope(scope)
    if (!session || session.sessionId !== sessionId) throw new AdapterError('not_found')
    if (session.sourceCapabilities?.[capability] !== true) throw new AdapterError('unsupported_capability')
  }

  // --- control commands ---------------------------------------------------

  async #command(request, payload, scope) {
    const operation = commandOperation(payload)
    if (!operation) return { status: 'rejected', errorCode: 'invalid_argument' }
    const commandId = request.command_id ?? ''
    if (commandId === '') return { status: 'rejected', errorCode: 'invalid_argument' }
    if (!(request.payload_digest instanceof Uint8Array) || request.payload_digest.length !== 32) {
      return { status: 'rejected', errorCode: 'payload_digest_mismatch' }
    }
    if (!(request.body instanceof Uint8Array) || !bytesEqual(sha256Bytes(request.body), request.payload_digest)) {
      return { status: 'rejected', errorCode: 'payload_digest_mismatch' }
    }
    if (request.expires_at_unix_ms && request.expires_at_unix_ms <= this.now()) {
      return { status: 'rejected', errorCode: 'expired' }
    }
    // Replay an existing (device_id, command_id) before re-validating an
    // approval: the same digest must always return the stored terminal result,
    // even after the approval has since been resolved or expired.
    const existing = this.store.getCommand(request.device_id, commandId)
    if (existing) {
      if (!bytesEqual(existing.digest, request.payload_digest)) {
        return { status: 'rejected', errorCode: 'payload_digest_mismatch' }
      }
      return this.#replay(existing)
    }
    scope.commandId = commandId
    scope.payloadDigest = request.payload_digest
    const { command, created } = this.store.createOrGetCommand({
      deviceId: request.device_id,
      commandId,
      requestId: request.request_id,
      digest: request.payload_digest,
      expiresAt: request.expires_at_unix_ms ?? 0,
      now: this.now(),
    })
    if (!created) {
      if (!bytesEqual(command.digest, request.payload_digest)) {
        return { status: 'rejected', errorCode: 'payload_digest_mismatch' }
      }
      return this.#replay(command)
    }
    // A definitive precheck rejection is journaled like any other terminal
    // command. Returning a bodyless negative reply without a journal row makes
    // recovery report unknown instead of the exact approval_* code, and a
    // same-command-id retry would replay the still-accepted row as
    // `host_accepted` without ever re-checking. Precheck only reads, so an
    // abort or read failure is a proven non-execution and is journaled failed.
    if (operation.resolve) {
      let precheck
      try {
        precheck = await this.#untilCancelled(this.#approvalPrecheck(operation.resolve), scope)
      } catch (error) {
        const code = scope.controller.signal.aborted
          ? scopeCode(scope)
          : error instanceof AdapterError || error instanceof PluginError ? error.code : 'internal'
        return this.#finish(request, 'failed', { error_code: code, error_detail: publicMessage(code) }, code)
      }
      if (scope.controller.signal.aborted) {
        const code = scopeCode(scope)
        return this.#finish(request, 'failed', { error_code: code, error_detail: publicMessage(code) }, code)
      }
      if (precheck) return this.#finish(request, 'failed', { error_code: precheck, error_detail: publicMessage(precheck) }, precheck)
    }
    // Register before awaiting the acceptance notification so channel loss or
    // an explicit open cancellation cannot be missed during that await.
    const questionScope = operation.answerQuestion ? this.#beginQuestionAnswer(request, operation.answerQuestion) : null
    try {
      try { await this.#untilCancelled(this.onAccepted(request), scope) }
      catch {
        if (scope.controller.signal.aborted) { const code=scopeCode(scope);return this.#finish(request, 'failed', {error_code:code,error_detail:publicMessage(code)}, code) }
        this.#log('error', 'command_accepted_publish_failed', { command: commandId, code: 'unavailable' })
        return this.#unknown(request, 'unavailable')
      }
      let response
      try {
        this.#assertFresh(request, scope)
        const executing = this.#execute(request, operation, questionScope, scope)
        response = operation.answerQuestion ? await executing : await this.#untilCancelled(executing, scope)
      }
      catch (error) {
        const code = error instanceof AdapterError || error instanceof PluginError ? error.code : 'internal'
        if (!(error instanceof DefinitiveExecutionError)) return this.#unknown(request, code)
        return this.#finish(request, 'failed', { error_code: code, error_detail: publicMessage(code) }, code)
      }
      return this.#finish(request, 'succeeded', response)
    } finally { if (questionScope) this.#endQuestionAnswer(questionScope) }
  }

  #beginQuestionAnswer(request, answer) {
    const key = JSON.stringify([request.device_id, request.channel_id, request.request_id, request.command_id])
    const controller = new AbortController()
    const expires = Number(request.expires_at_unix_ms ?? 0)
    const entry = { key, requestId: request.request_id, deviceId: request.device_id, channelId: request.channel_id,
      sessionId: answer.session_id, commandId: request.command_id, phase: answer.expected_phase, controller, expires, timer: null }
    this.questionAnswers.set(key, entry)
    const schedule = remaining => {
      entry.timer = setTimeout(() => {
        if (remaining > 2147483647 && expires - this.now() > 0) { schedule(expires - this.now()); return }
        controller.abort('expired')
      }, Math.min(Math.max(0, remaining), 2147483647))
      entry.timer.unref?.()
    }
    if (expires > 0) schedule(expires - this.now())
    return entry
  }

  #endQuestionAnswer(entry) {
    if (entry.timer) clearTimeout(entry.timer)
    if (this.questionAnswers.get(entry.key) === entry) this.questionAnswers.delete(entry.key)
  }

  #finish(request, status, response, errorCode = null) {
    let encoded
    try {
      encoded = this.codec.encode('whalerein.v1.ResponsePayload', response)
    } catch {
      return this.#unknown(request, 'internal')
    }
    // Encoding and journaling happen after native execution. Their failures
    // cannot turn a committed action into a definitive execution failure.
    try {
      const updated = this.store.setCommandStatus({
        deviceId: request.device_id,
        commandId: request.command_id,
        status,
        resultPayload: encoded,
        errorCode,
        now: this.now(),
      })
      if (!updated) return this.#unknown(request, 'internal')
    } catch {
      return this.#unknown(request, 'internal')
    }
    const fields = {
      command: request.command_id, device: request.device_id, ...(errorCode ? { code: errorCode } : {}),
    }
    if (status === 'succeeded') this.#log('info', 'command_succeeded', fields)
    else this.#log('warn', 'command_failed', fields)
    return { status, resultPayload: encoded, ...(errorCode ? { errorCode } : {}) }
  }

  #unknown(request, errorCode) {
    try {
      // A write may have committed before its caller observed an error. Keep
      // any durable terminal proof instead of downgrading it to uncertainty.
      const recorded = this.store.getCommand(request.device_id, request.command_id)
      if (recorded?.status === 'succeeded' || recorded?.status === 'failed') return this.#replay(recorded)
      if (!this.store.setCommandStatus({
        deviceId: request.device_id,
        commandId: request.command_id,
        status: 'unknown',
        errorCode,
        now: this.now(),
      })) throw new PluginError('internal', 'command journal missing')
    } catch {
      // The admission was journaled before invocation. If the store is
      // unavailable, leave it accepted for recovery; never invoke it again.
      this.#log('error', 'command_journal_unavailable', { command: request.command_id, code: 'internal' })
    }
    return { status: 'unknown', errorCode }
  }

  #log(level, event, fields) {
    try { this.logger?.[level]?.(event, fields) }
    catch { /* Diagnostics cannot change a journaled command outcome. */ }
  }

  async #approvalPrecheck(resolve) {
    if (resolve.decision !== 'APPROVAL_DECISION_APPROVE' && resolve.decision !== 'APPROVAL_DECISION_REJECT') {
      return 'invalid_argument'
    }
    const approval = await this.adapter.getApproval(resolve.approval_id)
    if (!approval) {
      // The native action may have been resolved before this process started,
      // or a pending waterfall was lost with the previous owner. The ledger
      // keeps a durable terminal answer so a late decision is deterministically
      // `already_resolved`/`expired`/`unavailable` instead of a generic unknown.
      const recorded = this.#ledgerApproval(resolve.approval_id)
      if (!recorded) return 'approval_unavailable'
      if (recorded.state === 'approved' || recorded.state === 'rejected') return 'approval_already_resolved'
      if (recorded.state === 'expired') return 'approval_expired'
      if (recorded.state === 'unavailable') return 'approval_unavailable'
      if (recorded.expiresAt > 0 && recorded.expiresAt <= this.now()) {
        this.#settleApproval(recorded.approvalId, 'expired')
        return 'approval_expired'
      }
      this.#settleApproval(recorded.approvalId, 'unavailable', 'unspecified', 'native_missing')
      return 'approval_unavailable'
    }
    this.#recordApproval(approval)
    if (approval.state === 'expired' || (approval.expiresAt > 0 && approval.expiresAt <= this.now())) {
      this.#settleApproval(approval.approvalId, 'expired')
      return 'approval_expired'
    }
    if (approval.state !== 'pending') {
      this.#settleApproval(approval.approvalId, approval.state, approval.decision ?? 'unspecified')
      return 'approval_already_resolved'
    }
    if (!(resolve.expected_digest instanceof Uint8Array) || !bytesEqual(resolve.expected_digest, approval.digest)) {
      return 'approval_version_mismatch'
    }
    if (Number(resolve.expected_version) !== approval.version) return 'approval_version_mismatch'
    return null
  }

  #replay(command) {
    if (command.status === 'accepted') return { status: 'host_accepted' }
    if (command.status === 'unknown') return { status: 'unknown', errorCode: command.errorCode ?? 'unavailable' }
    return {
      status: command.status,
      resultPayload: command.resultPayload ?? new Uint8Array(0),
      errorCode: command.errorCode ?? undefined,
    }
  }

  #assertScope(scope) {
    if (scope?.controller.signal.aborted) throw new AdapterError(scopeCode(scope))
  }

  #assertFresh(request, scope) {
    if (scope?.controller.signal.aborted) throw new DefinitiveExecutionError(scopeCode(scope))
    if (request.expires_at_unix_ms && Number(request.expires_at_unix_ms) <= this.now()) {
      throw new DefinitiveExecutionError('expired')
    }
  }

  #ready(kind, identity, context) {
    return { lifecycle_receipt: lifecycleReceipt({
      operation: kind, operationId: context.operationId, disposition: 'READY', phase: 'COMPLETE',
      sessionId: identity.session_id,
      parentSessionId: kind === 'fork' ? identity.parent_session_id : '',
    }) }
  }

  #warning(kind, identity, warningCode, context) {
    return { lifecycle_receipt: lifecycleReceipt({
      operation: kind, operationId: context.operationId, disposition: 'READY_WITH_WARNING', phase: 'ATTACH',
      sessionId: identity.session_id,
      parentSessionId: kind === 'fork' ? identity.parent_session_id : '',
      warningCode,
    }) }
  }

  #failed(kind, reason, context, retryable = RETRYABLE_PREFLIGHT_REASONS.has(reason), parentSessionId = '') {
    return { lifecycle_receipt: lifecycleReceipt({
      operation: kind, operationId: context.operationId, disposition: 'FAILED', phase: 'PREFLIGHT',
      parentSessionId: kind === 'fork' ? parentSessionId : '',
      reasonCode: reason, retryable,
    }) }
  }

  #unknownOutcome(kind, reason, context, { preferredSessionId = '', parentSessionId = '' } = {}) {
    const reconcile = kind === 'create'
      ? preferredSessionId !== ''
        ? { mode: 'PREFERRED_SESSION_ID', key: preferredSessionId }
        : { mode: 'OPERATION_ID', key: context.operationId }
      : parentSessionId !== ''
        ? { mode: 'PARENT_SESSION_ID', key: parentSessionId }
        : { mode: 'MANUAL', key: '' }
    return { lifecycle_receipt: lifecycleReceipt({
      operation: kind, operationId: context.operationId, disposition: 'UNKNOWN', phase: 'NATIVE',
      parentSessionId: kind === 'fork' ? parentSessionId : '',
      reasonCode: reason, reconcileMode: reconcile.mode, reconcileKey: reconcile.key,
    }) }
  }

  #validLifecycleReceipt(kind, parentSessionId, receipt) {
    if (!receipt || !LIFECYCLE_DISPOSITIONS.includes(receipt.disposition)) return false
    if (receipt.operation !== kind) return false
    if (kind === 'create' && receipt.parent_session_id) return false
    if (kind === 'fork') {
      if (receipt.parent_session_id && receipt.parent_session_id !== parentSessionId) return false
      if (receipt.session_id && receipt.session_id === parentSessionId) return false
    }
    if (receipt.phase && !LIFECYCLE_PHASES.includes(receipt.phase)) return false
    if (receipt.disposition === 'READY') return Boolean(receipt.session_id) && !receipt.warning_code
    if (receipt.disposition === 'READY_WITH_WARNING') return Boolean(receipt.session_id) && Boolean(receipt.warning_code)
    // Identity is never claimed by a proven failure or an unresolved outcome.
    if (receipt.disposition === 'FAILED') return !receipt.session_id
    if (receipt.disposition === 'UNKNOWN') return !receipt.session_id && RECONCILE_MODES.includes(receipt.reconcile_mode)
    return false
  }

  #mapLegacyReceipt(kind, parentSessionId, receipt, preferredSessionId, context) {
    if (!receipt || !LIFECYCLE_DISPOSITIONS.includes(receipt.disposition)) {
      return this.#unknownOutcome(kind, 'INVALID_LIFECYCLE_RECEIPT', context, { preferredSessionId, parentSessionId })
    }
    // Legacy receipts do not carry phases or operation ids. Keep their exact
    // shape for READY/READY_WITH_WARNING so existing replay payloads stay stable.
    const normalized = {
      operation: kind,
      disposition: receipt.disposition,
      ...(receipt.session_id ? { session_id: receipt.session_id } : {}),
      ...(kind === 'fork' && receipt.parent_session_id ? { parent_session_id: receipt.parent_session_id } : {}),
      ...(receipt.warning_code ? { warning_code: receipt.warning_code } : {}),
      ...(receipt.operation_id ? { operation_id: receipt.operation_id } : {}),
      ...(receipt.phase ? { phase: receipt.phase } : {}),
      ...(receipt.reason_code ? { reason_code: receipt.reason_code } : {}),
      ...(receipt.retryable ? { retryable: true } : {}),
      ...(receipt.reconcile_mode ? { reconcile_mode: receipt.reconcile_mode } : {}),
      ...(receipt.reconcile_key ? { reconcile_key: receipt.reconcile_key } : {}),
    }
    if (!this.#validLifecycleReceipt(kind, parentSessionId, normalized)) {
      return this.#unknownOutcome(kind, 'INVALID_LIFECYCLE_RECEIPT', context, { preferredSessionId, parentSessionId })
    }
    return { lifecycle_receipt: normalized }
  }

  #mapNativeFacts(kind, facts, { parentSessionId = '', preferredSessionId = '', context, setObservedIdentity = () => {} }) {
    const identity = facts?.identity && typeof facts.identity.session_id === 'string'
      ? facts.identity : null
    if (identity && kind === 'fork' && identity.session_id === parentSessionId) {
      return this.#unknownOutcome(kind, 'INVALID_NATIVE_IDENTITY', context, { preferredSessionId, parentSessionId })
    }
    if (identity && facts.attach_error) {
      const warning = safeWarningCode(facts.attach_error)
      if (!warning) return this.#unknownOutcome(kind, 'UNVERIFIED_ATTACH_WARNING', context, { preferredSessionId, parentSessionId })
      return this.#warning(kind, {
        session_id: identity.session_id,
        parent_session_id: kind === 'fork' ? parentSessionId : '',
      }, warning, context)
    }
    if (identity && !facts.failure_code) {
      setObservedIdentity(identity.session_id)
      return this.#ready(kind, {
        session_id: identity.session_id,
        parent_session_id: kind === 'fork' ? parentSessionId : '',
      }, context)
    }
    if (facts.entered === false) {
      return this.#failed(kind, nativeReason(facts.failure_code), context, true, kind === 'fork' ? parentSessionId : '')
    }
    // Any native entry without a confirmed identity is unresolved, even when
    // the native error happened to carry one.
    return this.#unknownOutcome(kind, nativeReason(facts.failure_code), context, { preferredSessionId, parentSessionId })
  }

  #mapLifecycleResult(kind, result, { parentSessionId = '', preferredSessionId = '', context }) {
    if (result && (Object.hasOwn(result, 'entered') || Object.hasOwn(result, 'identity') ||
        Object.hasOwn(result, 'attach_error') || Object.hasOwn(result, 'failure_code'))) {
      return this.#mapNativeFacts(kind, result, { parentSessionId, preferredSessionId, context })
    }
    if (result?.lifecycle_receipt) {
      return this.#mapLegacyReceipt(kind, parentSessionId, result.lifecycle_receipt, preferredSessionId, context)
    }
    return this.#unknownOutcome(kind, 'MISSING_LIFECYCLE_EVIDENCE', context, { preferredSessionId, parentSessionId })
  }

  #persistLifecycleStart(kind, request, context, { parentSessionId = '', preferredSessionId = '' }) {
    try {
      const begun = this.store.beginLifecycleOperation({
        deviceId: request.device_id,
        commandId: request.command_id,
        operationId: context.operationId,
        operation: kind,
        phase: 'NATIVE',
        parentSessionId,
        preferredSessionId,
        now: this.now(),
      })
      if (begun) return
    } catch {
      // The command row is already admitted; a failed side record must not
      // allow a native call whose outcome could not be recorded.
    }
    throw new DefinitiveExecutionError('internal', 'lifecycle operation record unavailable')
  }

  #persistLifecycleResult(kind, request, phase, identity = null, entered = true, outcome = '', warningCode = '') {
    try {
      this.store.updateLifecycleOperation({
        deviceId: request.device_id, commandId: request.command_id, phase, entered,
        sessionId: identity?.session_id ?? '', parentSessionId: identity?.parent_session_id ?? '',
        outcome, warningCode,
        now: this.now(),
      })
    } catch {
      // The terminal receipt remains the carrier; this diagnostic cannot turn
      // a committed native action into a proven failure.
      this.#log('warn', 'lifecycle_persist_failed', { phase, code: 'internal' })
    }
  }

  #noteLifecycleOutcome(kind, request, outcome) {
    const receipt = outcome?.lifecycle_receipt
    if (!receipt) return outcome
    const phase = receipt.disposition === 'READY' ? 'COMPLETE'
      : receipt.disposition === 'READY_WITH_WARNING' ? 'ATTACH'
        : receipt.disposition === 'FAILED' ? 'PREFLIGHT' : 'NATIVE'
    const identity = receipt.session_id
      ? { session_id: receipt.session_id, parent_session_id: receipt.parent_session_id ?? '' }
      : null
    this.#persistLifecycleResult(kind, request, phase, identity, receipt.disposition !== 'FAILED', receipt.disposition, receipt.warning_code ?? '')
    const fields = {
      operation: kind, disposition: receipt.disposition, phase, reason: receipt.reason_code ?? '',
      identity_present: Boolean(receipt.session_id), warning: receipt.warning_code ?? '',
      retryable: Boolean(receipt.retryable), reconcile_mode: receipt.reconcile_mode ?? '',
    }
    this.#log('info', 'lifecycle_outcome', fields)
    if (receipt.disposition === 'UNKNOWN') {
      this.#log('warn', 'lifecycle_unknown', { operation: kind, operation_id: receipt.operation_id ?? '',
        reconcile_mode: receipt.reconcile_mode ?? '', phase })
    }
    if (receipt.disposition === 'FAILED') {
      this.#log('info', 'lifecycle_preflight_failed', { operation: kind, reason: receipt.reason_code ?? '' })
    }
    return outcome
  }

  #createPreflight(create = {}) {
    for (const value of [create.cwd, create.workspace_id, create.agent_preset, create.preferred_session_id]) {
      if (value !== undefined && (typeof value !== 'string' || !value.trim())) return 'INVALID_ARGUMENT'
    }
    if ((create.cwd !== undefined && create.workspace_id !== undefined) ||
        (create.cwd !== undefined && !path.isAbsolute(create.cwd))) return 'INVALID_ARGUMENT'
    return null
  }

  async #execute(request, operation, questionScope, scope) {
    const operationId = hex(sha256Text(JSON.stringify(['whalerein/v1/operation', this.hostId ?? request.host_id, request.device_id, request.command_id])))
    const context = {operationId, expiresAt:Number(request.expires_at_unix_ms ?? 0), signal:scope.controller.signal}
    if (operation.selectModel) {
      if (!this.adapter.describe?.().capabilities?.includes('model-select')) throw new DefinitiveExecutionError('unsupported_capability')
      const selection = operation.selectModel
      if (!selection.session_id || !selection.provider || !selection.model) throw new DefinitiveExecutionError('invalid_argument')
      // Model selection is a source mutation like a queue edit or an answer. A
      // request that went stale before the native call must fail definitively
      // instead of reporting an unknown outcome for a change it never made.
      this.#assertFresh(request, scope)
      const result = await this.adapter.execute(selection.session_id, { select_session_model: selection }, context)
      const receipt = result?.model_selection_receipt
      const selected = receipt?.selected
      if (!selected || selected.provider !== selection.provider || selected.model !== selection.model ||
          selected.effort !== selection.effort ||
          receipt.effect_scope !== 'session_and_source_default_attempt' || receipt.default_persistence !== 'unconfirmed') {
        // The native action may already have committed. An invalid receipt
        // cannot prove failure, so retain the journaled unknown outcome.
        throw new Error('invalid model selection receipt')
      }
      return { model_selection_receipt: receipt }
    }
    if (operation.fork) {
      const capabilities = await this.adapter.describeHostCapabilities()
      this.#assertFresh(request, scope)
      if (!capabilities?.session_fork) throw new DefinitiveExecutionError('unsupported_capability')
      const parent = operation.fork.session_id
      if (typeof parent !== 'string' || !parent.trim()) {
        return this.#failed('fork', 'INVALID_ARGUMENT', context, undefined, operation.fork.session_id)
      }
      let source
      try {
        source = await this.adapter.getSession(parent)
      } catch (error) {
        if (error instanceof AdapterError && error.code === 'not_found') {
          return this.#failed('fork', 'SOURCE_NOT_FOUND', context, false, parent)
        }
        throw new DefinitiveExecutionError(error instanceof AdapterError ? error.code : 'internal')
      }
      if (!source) return this.#failed('fork', 'SOURCE_NOT_FOUND', context, false, parent)
      if (!source.sourceCapabilities?.session_fork) throw new DefinitiveExecutionError('unsupported_capability')
      let forkFacts = {}
      try {
        forkFacts = await this.adapter.preflightForkSession?.(parent) ?? {}
      } catch (error) {
        if (error instanceof DefinitiveExecutionError) {
          return this.#failed('fork', 'SOURCE_NO_COMPLETE_TURN', context, false, parent)
        }
        // An unavailable preflight read proves nothing; native remains authoritative.
        forkFacts = {}
      }
      if (forkFacts?.completed_turn === false) {
        return this.#failed('fork', 'SOURCE_NO_COMPLETE_TURN', context, false, parent)
      }
      this.#assertFresh(request, scope)
      this.#persistLifecycleStart('fork', request, context, { parentSessionId: parent })
      let result
      try {
        result = await this.adapter.execute(parent, { fork_session: operation.fork }, context)
        if (typeof result?.identity?.session_id === 'string' && result.identity.session_id) {
          this.#persistLifecycleResult('fork', request, 'NATIVE', result.identity, true)
        }
      } catch (error) {
        if (error instanceof DefinitiveExecutionError) {
          return this.#failed('fork', preflightReason(error.code), context, undefined, parent)
        }
        return this.#unknownOutcome('fork', nativeReason(error instanceof AdapterError ? error.code : 'internal'),
          context, { parentSessionId: parent })
      }
      const mapped = this.#mapLifecycleResult('fork', result, { parentSessionId: parent, context })
      return this.#noteLifecycleOutcome('fork', request, mapped)
    }
    if (operation.create) {
      const capabilities = await this.adapter.describeHostCapabilities()
      this.#assertFresh(request, scope)
      if (!capabilities?.session_create) throw new DefinitiveExecutionError('unsupported_capability')
      const invalid = this.#createPreflight(operation.create)
      if (invalid) return this.#failed('create', invalid, context)
      let createFacts = {}
      try {
        createFacts = await this.adapter.preflightCreateSession?.(operation.create) ?? {}
      } catch (error) {
        if (error instanceof DefinitiveExecutionError) {
          return this.#failed('create', preflightReason(error.code), context)
        }
        // A preflight read failure is not proof of a missing target.
        createFacts = {}
      }
      if (createFacts?.workspace === 'missing') return this.#failed('create', 'WORKSPACE_NOT_FOUND', context)
      if (createFacts?.workspace === 'unattachable') return this.#failed('create', 'WORKSPACE_UNAVAILABLE', context, false)
      if (createFacts?.preset === 'missing') return this.#failed('create', 'PRESET_NOT_FOUND', context)
      if (createFacts?.preset === 'unavailable') return this.#failed('create', 'PRESET_UNAVAILABLE', context, false)
      this.#assertFresh(request, scope)
      if (typeof createFacts?.reuse === 'string' && createFacts.reuse !== '') {
        // A proven reusable blank identity completes without another native create.
        return this.#ready('create', { session_id: createFacts.reuse }, context)
      }
      const preferredSessionId = operation.create.preferred_session_id ?? ''
      this.#persistLifecycleStart('create', request, context, { preferredSessionId })
      let result
      try {
        result = await this.adapter.executeHost({ create_session: operation.create }, context)
        if (typeof result?.identity?.session_id === 'string' && result.identity.session_id) {
          this.#persistLifecycleResult('create', request, 'NATIVE', result.identity, true)
        }
      } catch (error) {
        if (error instanceof DefinitiveExecutionError) {
          return this.#failed('create', preflightReason(error.code), context)
        }
        return this.#unknownOutcome('create', nativeReason(error instanceof AdapterError ? error.code : 'internal'),
          context, { preferredSessionId })
      }
      const mapped = this.#mapLifecycleResult('create', result, { preferredSessionId, context })
      return this.#noteLifecycleOutcome('create', request, mapped)
    }
    if (operation.send) {
      if (typeof operation.send.text !== 'string' || operation.send.text.trim() === '') {
        throw new DefinitiveExecutionError('invalid_argument', 'message empty')
      }
      const send = () => this.adapter.execute(operation.send.session_id, {send_message:operation.send}, context)
      // STEER remains native-authoritative: DSH reports steer-unavailable when
      // the turn it targets has ended. Only the idle precheck needs the gate.
      if (operation.send.delivery_mode === 'START_IF_IDLE') return this.#withSendGate(operation.send.session_id, send)
      return send()
    }
    if (operation.interrupt) return this.adapter.execute(operation.interrupt.session_id, {interrupt:operation.interrupt}, context)
    if (operation.queue) {
      const capability = {EDIT:'pending_queue_edit',REMOVE:'pending_queue_remove',PROMOTE:'pending_queue_steer'}[operation.queue.action]
      if (!capability) throw new DefinitiveExecutionError('invalid_argument')
      try { await this.#requireQueueCapability(operation.queue.session_id, capability, scope) }
      catch (error) { throw new DefinitiveExecutionError(error instanceof AdapterError || error instanceof PluginError ? error.code : 'internal') }
      this.#assertFresh(request, scope)
      return this.adapter.execute(operation.queue.session_id, {update_pending_queue:operation.queue}, {...context,expiresAt:Number(request.expires_at_unix_ms ?? 0)})
    }
    if (operation.answerQuestion) {
      const answer = operation.answerQuestion
      const capability = answer.expected_phase === 'QUESTION_OPEN' ? 'questions_answer_open'
        : answer.expected_phase === 'QUESTION_CONTINUED' ? 'questions_answer_continued' : null
      if (!capability) throw new DefinitiveExecutionError('invalid_argument')
      const { controller, expires } = questionScope
      if (expires > 0 && this.now() >= expires) throw new DefinitiveExecutionError('expired')
      if (controller.signal.aborted) throw new DefinitiveExecutionError(controller.signal.reason === 'expired' ? 'expired' : 'unavailable')
      let onAbort, nativeStarted = false
      try {
        const aborted = new Promise((_, reject) => {
          onAbort = () => reject(new AdapterError(controller.signal.reason === 'expired' ? 'expired' : 'unavailable'))
          controller.signal.addEventListener('abort', onAbort, { once: true })
        })
        try { await Promise.race([this.#requireQuestionCapability(answer.session_id, capability, scope), aborted]) }
        catch (error) { throw new DefinitiveExecutionError(error instanceof AdapterError ? error.code : 'internal') }
        if (expires > 0 && this.now() >= expires) throw new DefinitiveExecutionError('expired')
        if (controller.signal.aborted) throw new DefinitiveExecutionError(controller.signal.reason === 'expired' ? 'expired' : 'unavailable')
        nativeStarted = true
        const result = await Promise.race([this.adapter.execute(answer.session_id, { answer_question: answer },
          { ...context, signal: controller.signal, expiresAt: expires }), aborted])
        if (controller.signal.aborted || (expires > 0 && this.now() >= expires)) throw new AdapterError('unavailable')
        return result
      } catch (error) {
        if (!nativeStarted && controller.signal.aborted) {
          throw new DefinitiveExecutionError(controller.signal.reason === 'expired' ? 'expired' : 'unavailable')
        }
        throw error
      } finally { controller.signal.removeEventListener('abort', onAbort) }

    }
    if (operation.resolve) {
      let approval
      try { approval = await this.adapter.getApproval(operation.resolve.approval_id) }
      catch (error) { throw new DefinitiveExecutionError(error instanceof AdapterError ? error.code : 'internal') }
      if (!approval) throw new DefinitiveExecutionError('approval_unavailable')
      this.#assertFresh(request, scope)
      try {
        const result = await this.adapter.execute(approval.sessionId, {resolve_approval:operation.resolve}, context)
        this.#settleApproval(approval.approvalId,
          operation.resolve.decision === 'APPROVAL_DECISION_APPROVE' ? 'approved' : 'rejected',
          operation.resolve.decision === 'APPROVAL_DECISION_APPROVE' ? 'approve' : 'reject')
        return result
      } catch (error) {
        // The adapter is authoritative for a racing decision. Mirror its
        // terminal state so a later decision from any device is consistent.
        await this.#syncApprovalRecord(approval.approvalId)
        throw error
      }
    }
    throw new DefinitiveExecutionError('unsupported_capability', 'command unsupported')
  }
}

function commandOperation(payload) {
  if (payload.select_session_model) return { selectModel: payload.select_session_model }
  if (payload.create_session) return { create: payload.create_session }
  if (payload.update_pending_queue) return { queue: payload.update_pending_queue }
  if (payload.fork_session) return { fork: payload.fork_session }
  if (payload.send_message) return { send: payload.send_message }
  if (payload.interrupt) return { interrupt: payload.interrupt }
  if (payload.resolve_approval) return { resolve: payload.resolve_approval }
  if (payload.answer_question) return { answerQuestion: payload.answer_question }
  return null
}
