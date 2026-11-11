// Command and query execution. The host is the only execution dedupe point:
// (device_id, command_id) is persisted with the payload digest, accepted is
// stored before the Source is called, terminal status is stored with the response
// payload, and crash recovery marks unfinished accepted commands unknown.

import { bytesEqual, sha256Bytes } from '../security/digest.js'
import { summaryToProto } from '../contracts/serialization.js'
import { AdapterError, DefinitiveExecutionError, PluginError, publicMessage } from '../errors.js'
import { codec as defaultCodec, resolveCodec } from '../protocol.js'

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

function approvalToProto(approval) {
  return {
    approval_id: approval.approvalId,
    session_id: approval.sessionId,
    tool_name: approval.toolName,
    call_id: approval.callId,
    reason: approval.reason,
    digest: approval.digest,
    version: approval.version,
    expires_at_unix_ms: approval.expiresAt,
    state: APPROVAL_STATE_TO_PROTO[approval.state] ?? 'APPROVAL_STATE_UNAVAILABLE',
    decision: APPROVAL_DECISION_TO_PROTO[approval.decision] ?? 'APPROVAL_DECISION_UNSPECIFIED',
  }
}

function failed(code) {
  return { status: 'failed', errorCode: code }
}

export class CommandService {
  /**
   * @param {object} options
   * @param {import('../state/store.js').StateStore} options.store
   * @param {import('../contracts/source.js').SourceAdapter} options.adapter
   * @param {import('../events/watches.js').WatchedEvents} options.events
   * @param {object} [options.logger]
   * @param {() => number} [options.now]
   * @param {(request: object) => Promise<void>} [options.onAccepted] publish host_accepted.
   * @param {import('../protocol.js').ProtoCodec} [options.codec]
   */
  constructor({ store, adapter, events, logger, now = Date.now, onAccepted = async () => {}, codec = defaultCodec }) {
    this.store = store
    this.adapter = adapter
    this.events = events
    this.logger = logger
    this.now = now
    this.onAccepted = onAccepted
    this.codec = resolveCodec(codec)
  }

  /** Crash recovery is intentionally idempotent. */
  recover() {
    const count = this.store.recoverAcceptedCommands(this.now())
    if (count > 0) this.#log('warn', 'commands_marked_unknown', { count, code: 'unavailable' })
    return count
  }

  /**
   * Handle one decrypted RequestPayload.
   * @returns {Promise<{status: string, response?: object, resultPayload?: Buffer, errorCode?: string}>}
   */
  async handle({ request, payload }) {
    if (request.kind === 'REQUEST_KIND_COMMAND') return this.#command(request, payload)
    if (request.kind === 'REQUEST_KIND_QUERY') return this.#query(request, payload)
    throw new PluginError('invalid_argument', 'request kind invalid')
  }

  // --- queries ------------------------------------------------------------

  async #query(request, payload) {
    try {
      if (payload.list_sessions) {
        const sessions = await this.adapter.listSessions()
        return { status: 'succeeded', response: { session_list: { sessions: sessions.map(summaryToProto) } } }
      }
      if (payload.get_session) {
        const session = await this.adapter.getSession(payload.get_session.session_id)
        return { status: 'succeeded', response: { session_info: summaryToProto(session) } }
      }
      if (payload.read_history) {
        return { status: 'succeeded', response: { history_page: await this.events.readHistory(payload.read_history.session_id, payload.read_history) } }
      }
      if (payload.watch_session) {
        return { status: 'succeeded', response: { watch_ready: await this.events.watch(request.device_id, request.channel_id, payload.watch_session.session_id) } }
      }
      if (payload.refresh_watches) {
        return { status: 'succeeded', response: { watch_states: await this.events.refresh(request.device_id, request.channel_id, payload.refresh_watches.watch_ids??[]) } }
      }
      if (payload.unwatch_session) {
        this.events.unwatch(request.device_id, request.channel_id, payload.unwatch_session.watch_id)
        return { status: 'succeeded', response: {} }
      }
      if (payload.list_approvals) {
        const approvals = await this.adapter.listApprovals(
          payload.list_approvals.session_id ?? '',
        )
        return {
          status: 'succeeded',
          response: { approval_list: { approvals: approvals.map(approvalToProto) } },
        }
      }
      if (payload.get_approval) {
        const approval = await this.adapter.getApproval(payload.get_approval.approval_id)
        if (!approval) return failed('not_found')
        return { status: 'succeeded', response: { approval: approvalToProto(approval) } }
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
      if (error instanceof AdapterError) return failed(error.code)
      if (error instanceof PluginError) return failed(error.code)
      return failed('internal')
    }
  }

  #commandResult(deviceId, commandId) {
    const command = this.store.getCommand(deviceId, commandId)
    if (!command) return { found: false, status: 'online_status_unspecified' }
    return {
      found: true,
      status: COMMAND_STATUS_TO_PROTO[command.status] ?? 'unknown',
      result_payload: command.resultPayload ?? new Uint8Array(0),
      error_code: command.errorCode ?? 'error_code_unspecified',
    }
  }

  // --- control commands ---------------------------------------------------

  async #command(request, payload) {
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
    if (operation.resolve) {
      const precheck = await this.#approvalPrecheck(operation.resolve)
      if (precheck) return { status: 'rejected', errorCode: precheck }
    }
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
    try {
      await this.onAccepted(request)
    } catch {
      this.#log('error', 'command_accepted_publish_failed', { command: commandId, code: 'unavailable' })
      return this.#unknown(request, 'unavailable')
    }
    let response
    try {
      response = await this.#execute(request, operation)
    } catch (error) {
      const code = error instanceof AdapterError || error instanceof PluginError ? error.code : 'internal'
      if (!(error instanceof DefinitiveExecutionError)) return this.#unknown(request, code)
      return this.#finish(request, 'failed', { error_code: code, error_detail: publicMessage(code) }, code)
    }
    return this.#finish(request, 'succeeded', response)
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
    if (!approval) return 'approval_unavailable'
    if (approval.state === 'expired' || (approval.expiresAt > 0 && approval.expiresAt <= this.now())) {
      return 'approval_expired'
    }
    if (approval.state !== 'pending') return 'approval_already_resolved'
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

  async #execute(request, operation) {
    if (operation.send) {
      if (typeof operation.send.text !== 'string' || operation.send.text.trim() === '') {
        throw new DefinitiveExecutionError('invalid_argument', 'message empty')
      }
      return this.adapter.execute(operation.send.session_id, {send_message:operation.send})
    }
    if (operation.interrupt) return this.adapter.execute(operation.interrupt.session_id, {interrupt:operation.interrupt})
    if (operation.resolve) {
      let approval
      try { approval = await this.adapter.getApproval(operation.resolve.approval_id) }
      catch (error) { throw new DefinitiveExecutionError(error instanceof AdapterError ? error.code : 'internal') }
      if (!approval) throw new DefinitiveExecutionError('approval_unavailable')
      return this.adapter.execute(approval.sessionId, {resolve_approval:operation.resolve})
    }
    throw new DefinitiveExecutionError('unsupported_capability', 'command unsupported')
  }
}

function commandOperation(payload) {
  if (payload.send_message) return { send: payload.send_message }
  if (payload.interrupt) return { interrupt: payload.interrupt }
  if (payload.resolve_approval) return { resolve: payload.resolve_approval }
  return null
}
