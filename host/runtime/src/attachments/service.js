// Memory-only Host attachment transfer ownership.
//
// One Source iterator and at most one cached pending part exist per active
// transfer. A handle is bound to the authenticated Host, device, channel,
// session and occurrence; every part rechecks that binding and authorization.
// Nothing here is persisted: no transfer id, byte or plaintext metadata enters
// StateStore, an outbox or a command journal.
import { createHash, randomBytes } from 'node:crypto'
import { AdapterError, isErrorCode } from '../errors.js'
import { validateAttachmentMetadata, validateAttachmentPolicy } from '../contracts/source.js'
import { validateId } from '../contracts/ids.js'
import { transportProfileBudget, DEFAULT_TRANSPORT_PROFILE } from '../contracts/transport.js'
import { MESSAGE_BYTES } from '../access/transport.js'
import {
  AttachmentReader,
  RESULT_ENVELOPE_OVERHEAD_BYTES,
  RESULT_FRAMING_OVERHEAD_BYTES,
  defaultAttachmentPolicy,
  effectiveChunkBytes,
  minPositive,
  normalizeRead,
  safeSuggestion,
} from './reader.js'

const CLOSED_LIMIT = 256
const CLOSED_TTL_MS = 30_000
const ACTIVE_MULTIPLIER = 4

function isPositive(value) {
  return Number.isSafeInteger(value) && value > 0
}

function isCount(value) {
  return Number.isSafeInteger(value) && value >= 0
}

function validId(kind, value) {
  return typeof value === 'string' && validateId(kind, value).ok
}

function wireCode(error) {
  return isErrorCode(error?.code) ? error.code : 'internal'
}

function abortCode(signal, fallback = 'unavailable') {
  const reason = signal?.reason
  if (reason === 'expired') return 'expired'
  if (reason === 'revoked') return 'forbidden'
  if (reason === 'channel_closed' || reason === 'channel_replaced') return 'unavailable'
  if (reason === 'host_stop') return 'unavailable'
  return fallback
}

function joinSignals(left, right) {
  if (!left) return right
  if (!right) return left
  if (typeof AbortSignal.any === 'function') return AbortSignal.any([left, right])
  const controller = new AbortController()
  const abort = (signal) => controller.abort(signal?.reason)
  for (const signal of [left, right]) {
    if (signal.aborted) { abort(signal); break }
    signal.addEventListener('abort', () => abort(signal), { once: true })
  }
  return controller.signal
}

function boundedBytes(value, limit) {
  let result = ''
  for (const character of value) {
    if (Buffer.byteLength(result + character, 'utf8') > limit) break
    result += character
  }
  return result
}

function copyPart(part) {
  return {
    offset: part.offset,
    data: part.data,
    final: part.final,
    ...(part.final ? { total_size: part.total_size, sha256: part.sha256 } : {}),
  }
}

export class AttachmentService {
  #transfers = new Map()
  #deviceTransfers = new Map()
  #channelTransfers = new Map()
  #deviceReserved = new Map()
  #retired = new Map()
  #maxActive

  /**
   * @param {object} options
   * @param {string} options.hostId
   * @param {import('../contracts/source.js').SourceAdapter} options.adapter
   * @param {object} [options.security] SecurityManager-like channel authority.
   * @param {object} [options.store] state store for revocation evidence.
   * @param {object} [options.logger] redacted diagnostics sink.
   * @param {() => number} [options.now]
   * @param {object} [options.policy] Host attachment policy overrides.
   * @param {string|object} [options.transportProfile] packaged profile or budget.
   * @param {number} [options.envelopeBytes] local logical envelope ceiling.
   * @param {number} [options.maxActive] global active-transfer ceiling.
   * @param {boolean} [options.timers] arm idle/deadline timers (default true).
   */
  constructor({
    hostId,
    adapter,
    security = null,
    store = null,
    logger = null,
    now = Date.now,
    policy = null,
    transportProfile = DEFAULT_TRANSPORT_PROFILE,
    envelopeBytes = 0,
    maxActive = 0,
    timers = true,
  }) {
    this.hostId = hostId
    this.adapter = adapter
    this.security = security
    this.store = store
    this.logger = logger
    this.now = now
    this.timers = timers
    let profileBudget = null
    if (transportProfile && typeof transportProfile === 'object'
      && Number.isSafeInteger(transportProfile.envelope_bytes)) {
      profileBudget = transportProfile
    } else {
      try {
        const profile = typeof transportProfile === 'string' ? transportProfile : DEFAULT_TRANSPORT_PROFILE
        profileBudget = transportProfileBudget(profile)
      } catch {
        profileBudget = null
      }
    }
    const base = profileBudget
      ? defaultAttachmentPolicy({
        fragmentPayloadBytes: profileBudget.fragment_payload_bytes ?? 61440,
        bulkConcurrency: profileBudget.bulk_concurrency ?? 4,
        fragmentIdleTimeoutMs: profileBudget.fragment_idle_timeout_ms ?? 30000,
        transferTimeoutMs: profileBudget.transfer_timeout_ms ?? 120000,
      })
      : {
        max_total_bytes: 0,
        max_chunk_bytes: 61440,
        max_concurrent: 4,
        idle_timeout_ms: 30000,
        overall_timeout_ms: 120000,
      }
    const overrides = {}
    for (const [key, value] of Object.entries(policy ?? {})) {
      if (value !== undefined && value !== null) overrides[key] = value
    }
    this.policy = validateAttachmentPolicy({ ...base, ...overrides })
    const configuredEnvelope = isPositive(envelopeBytes)
      ? envelopeBytes
      : (profileBudget?.envelope_bytes ?? MESSAGE_BYTES)
    this.envelopeBytes = Math.min(configuredEnvelope, MESSAGE_BYTES)
    this.#maxActive = isPositive(maxActive)
      ? maxActive
      : this.policy.max_concurrent * ACTIVE_MULTIPLIER
  }

  /** Redacted operational summary; safe for `diagnostics()`. */
  describe() {
    let reservedBytes = 0
    for (const value of this.#deviceReserved.values()) reservedBytes += value
    let active = 0
    // Retained part bytes are the measurable in-flight buffer: at most one
    // cached part per active transfer, bounded by the effective chunk budget.
    let bufferedBytes = 0
    for (const transfer of this.#transfers.values()) {
      if (transfer.finished) continue
      active++
      bufferedBytes += transfer.lastPart?.data?.length ?? 0
    }
    return {
      active,
      handles: this.#transfers.size,
      devices: this.#deviceTransfers.size,
      reservedBytes,
      bufferedBytes,
      maxActive: this.#maxActive,
    }
  }

  /** The one public transfer inventory; tests and diagnostics only. */
  get activeTransfers() {
    let active = 0
    for (const transfer of this.#transfers.values()) if (!transfer.finished) active++
    return active
  }

  #log(level, event, fields) {
    try { this.logger?.[level]?.(event, fields) } catch { /* diagnostics never fail a transfer */ }
  }

  #authorize({ deviceId, channelId, hostId }) {
    if (hostId !== undefined && hostId !== '' && hostId !== this.hostId) {
      throw new AdapterError('forbidden')
    }
    if (!validId('device_id', deviceId) || !validId('channel_id', channelId)) {
      throw new AdapterError('invalid_argument')
    }
    if (this.security) {
      const channel = this.security.getChannel(channelId)
      if (!channel) throw new AdapterError('unauthorized')
      if (channel.deviceId !== deviceId) throw new AdapterError('forbidden')
    }
    let revoked = false
    try { revoked = Boolean(this.store?.isRevoked?.(deviceId)) }
    catch { revoked = true }
    if (revoked) throw new AdapterError('forbidden')
  }

  #receiverEnvelope(channelId) {
    const receive = this.security?.getChannelReceive?.(channelId)
    const value = receive?.budget?.envelope_bytes
    return isPositive(value) ? value : 0
  }

  #requestResultBound(request) {
    const declared = request?.transport?.result_max_bytes
    if (declared === undefined || declared === null || declared === 0 || declared === '0') return 0
    const value = Number(declared)
    if (!isPositive(value)) throw new AdapterError('invalid_argument')
    return value
  }

  #chunkCeiling(maxChunkBytes, request, channelId) {
    const envelope = minPositive(this.envelopeBytes, this.#receiverEnvelope(channelId))
    const resultMaxBytes = this.#requestResultBound(request)
    return effectiveChunkBytes({ maxChunkBytes, resultMaxBytes, envelopeBytes: envelope })
  }

  #effectivePolicy(adapterPolicy, request, channelId) {
    const host = this.policy
    const maxTotal = minPositive(adapterPolicy.max_total_bytes, host.max_total_bytes)
    let maxChunk = minPositive(adapterPolicy.max_chunk_bytes, host.max_chunk_bytes)
      || host.max_chunk_bytes
    if (maxTotal > 0) maxChunk = Math.min(maxChunk, maxTotal)
    maxChunk = Math.min(0xffffffff, this.#chunkCeiling(maxChunk, request, channelId))
    if (maxChunk < 1) throw new AdapterError('payload_too_large')
    return validateAttachmentPolicy({
      max_total_bytes: maxTotal,
      max_chunk_bytes: maxChunk,
      max_concurrent: Math.min(0xffffffff, minPositive(adapterPolicy.max_concurrent, host.max_concurrent) || 1),
      idle_timeout_ms: Math.min(0xffffffff, minPositive(adapterPolicy.idle_timeout_ms, host.idle_timeout_ms) || host.idle_timeout_ms),
      overall_timeout_ms: Math.min(0xffffffff, minPositive(adapterPolicy.overall_timeout_ms, host.overall_timeout_ms) || host.overall_timeout_ms),
    })
  }

  #readerFor(opened, sessionId, attachmentRef) {
    const reader = opened?.reader
    if (reader && typeof reader.read === 'function') {
      return new AttachmentReader({
        read: (maxBytes, options) => reader.read(maxBytes, options),
        close: (options) => (typeof reader.close === 'function' ? reader.close(options) : undefined),
      })
    }
    if (typeof this.adapter?.pullAttachment === 'function') {
      return new AttachmentReader({
        read: (maxBytes, { signal, offset }) => this.adapter.pullAttachment(sessionId, attachmentRef, {
          offset, maxBytes, signal,
        }),
        close: ({ cancel }) => (typeof this.adapter.closeAttachment === 'function'
          ? this.adapter.closeAttachment(sessionId, attachmentRef, { cancel })
          : undefined),
      })
    }
    return null
  }

  #count(map, key) {
    return map.get(key)?.size ?? 0
  }

  #add(map, key, transferId) {
    let set = map.get(key)
    if (!set) { set = new Set(); map.set(key, set) }
    set.add(transferId)
  }

  #remove(map, key, transferId) {
    const set = map.get(key)
    if (!set) return
    set.delete(transferId)
    if (set.size === 0) map.delete(key)
  }

  #admit({ deviceId, channelId, size, policy }) {
    if (this.#count(this.#deviceTransfers, deviceId) >= policy.max_concurrent) {
      throw new AdapterError('rate_limited')
    }
    if (this.#count(this.#channelTransfers, channelId) >= policy.max_concurrent) {
      throw new AdapterError('rate_limited')
    }
    if (this.activeTransfers >= this.#maxActive) throw new AdapterError('rate_limited')
    const reserved = this.#deviceReserved.get(deviceId) ?? 0
    const limit = policy.max_total_bytes > 0 ? policy.max_total_bytes : Infinity
    if (reserved + size > limit) throw new AdapterError('rate_limited')
  }

  #register(transfer) {
    this.#transfers.set(transfer.transferId, transfer)
    this.#add(this.#deviceTransfers, transfer.deviceId, transfer.transferId)
    this.#add(this.#channelTransfers, transfer.channelId, transfer.transferId)
    if (transfer.size > 0) {
      this.#deviceReserved.set(transfer.deviceId, (this.#deviceReserved.get(transfer.deviceId) ?? 0) + transfer.size)
    }
  }

  // Release the concurrency slot, channel slot and declaration reservation
  // while optionally keeping the handle for the one idempotent final retry.
  #releaseResources(transfer) {
    if (transfer.resourcesReleased) return
    transfer.resourcesReleased = true
    this.#remove(this.#deviceTransfers, transfer.deviceId, transfer.transferId)
    this.#remove(this.#channelTransfers, transfer.channelId, transfer.transferId)
    if (transfer.size > 0) {
      const remaining = (this.#deviceReserved.get(transfer.deviceId) ?? 0) - transfer.size
      if (remaining > 0) this.#deviceReserved.set(transfer.deviceId, remaining)
      else this.#deviceReserved.delete(transfer.deviceId)
    }
  }

  #detach(transfer) {
    this.#transfers.delete(transfer.transferId)
  }

  #retire(transfer, { reason, code = 'not_found', cancel = true }) {
    if (transfer.disposed) return
    transfer.disposed = true
    this.#clearTimers(transfer)
    this.#releaseResources(transfer)
    this.#detach(transfer)
    const closing = this.#closeReader(transfer, { cancel })
    transfer.controller.abort(reason)
    this.#rememberClosed(transfer, { reason, code, cancelled: cancel })
    this.#log('info', 'attachment_cleanup', {
      stage: 'cleanup', transfer: transfer.transferId, reason,
      bytes: transfer.nextOffset, ...(code !== 'not_found' ? { code } : {}),
    })
    return closing
  }

  #rememberClosed(transfer, { reason, code, cancelled }) {
    const now = this.now()
    for (const [id, entry] of this.#retired) {
      if (entry.until <= now) this.#retired.delete(id)
    }
    this.#retired.set(transfer.transferId, {
      deviceId: transfer.deviceId,
      channelId: transfer.channelId,
      sessionId: transfer.sessionId,
      attachmentRef: transfer.attachmentRef,
      code,
      reason,
      cancelled,
      until: now + CLOSED_TTL_MS,
    })
    while (this.#retired.size > CLOSED_LIMIT) this.#retired.delete(this.#retired.keys().next().value)
  }

  #finish(transfer) {
    this.#releaseResources(transfer)
    void this.#closeReader(transfer, { cancel: false })
  }

  async #closeReader(transfer, { cancel }) {
    const reader = transfer.reader
    transfer.reader = null
    if (!reader) return
    try { await reader.close({ cancel }) } catch { /* contained */ }
  }

  #clearTimers(transfer) {
    if (transfer.idleTimer) { clearTimeout(transfer.idleTimer); transfer.idleTimer = null }
    if (transfer.deadlineTimer) { clearTimeout(transfer.deadlineTimer); transfer.deadlineTimer = null }
  }

  #armIdle(transfer) {
    if (!this.timers || transfer.disposed) return
    if (transfer.idleTimer) clearTimeout(transfer.idleTimer)
    const delay = Math.max(1, transfer.idleAt - this.now())
    transfer.idleTimer = setTimeout(() => {
      transfer.idleTimer = null
      if (transfer.disposed) return
      if (this.now() < transfer.idleAt) { this.#armIdle(transfer); return }
      this.#retire(transfer, { reason: 'idle', code: 'expired' })
    }, Math.min(delay, 2147483647))
    transfer.idleTimer.unref?.()
  }

  #armDeadline(transfer) {
    if (!this.timers || transfer.disposed) return
    if (transfer.deadlineTimer) clearTimeout(transfer.deadlineTimer)
    const delay = Math.max(1, transfer.deadline - this.now())
    transfer.deadlineTimer = setTimeout(() => {
      transfer.deadlineTimer = null
      if (transfer.disposed) return
      if (this.now() < transfer.deadline) { this.#armDeadline(transfer); return }
      this.#retire(transfer, { reason: 'deadline', code: 'expired' })
    }, Math.min(delay, 2147483647))
    transfer.deadlineTimer.unref?.()
  }

  #touch(transfer) {
    transfer.idleAt = this.now() + transfer.policy.idle_timeout_ms
    this.#armIdle(transfer)
  }

  #checkLiveness(transfer) {
    const now = this.now()
    if (now >= transfer.deadline) {
      this.#retire(transfer, { reason: 'deadline', code: 'expired' })
      throw new AdapterError('expired')
    }
    if (now >= transfer.idleAt) {
      this.#retire(transfer, { reason: 'idle', code: 'expired' })
      throw new AdapterError('expired')
    }
  }

  /** Expire idle/deadline handles. Safe to call from a timer or a test. */
  sweep() {
    const now = this.now()
    let expired = 0
    for (const transfer of [...this.#transfers.values()]) {
      if (transfer.disposed) continue
      if (now >= transfer.deadline) { this.#retire(transfer, { reason: 'deadline', code: 'expired' }); expired++ }
      else if (now >= transfer.idleAt) { this.#retire(transfer, { reason: 'idle', code: 'expired' }); expired++ }
    }
    for (const [id, entry] of this.#retired) if (entry.until <= now) this.#retired.delete(id)
    if (expired > 0) this.#log('info', 'attachment_sweep', { stage: 'cleanup', count: expired })
    return expired
  }

  #lookup(transferId, { deviceId, channelId }) {
    const transfer = this.#transfers.get(transferId)
    if (transfer) {
      if (transfer.deviceId !== deviceId || transfer.channelId !== channelId) throw new AdapterError('forbidden')
      return transfer
    }
    const retired = this.#retired.get(transferId)
    if (retired) {
      if (retired.until <= this.now()) { this.#retired.delete(transferId); throw new AdapterError('not_found') }
      if (retired.deviceId !== deviceId || retired.channelId !== channelId) throw new AdapterError('forbidden')
      throw new AdapterError(retired.code)
    }
    throw new AdapterError('not_found')
  }

  #bind(transfer, { sessionId, attachmentRef }) {
    if (sessionId !== undefined && sessionId !== transfer.sessionId) throw new AdapterError('forbidden')
    if (attachmentRef !== undefined && attachmentRef !== transfer.attachmentRef) throw new AdapterError('forbidden')
  }

  #descriptor(transfer) {
    return {
      session: { host_id: this.hostId, session_id: transfer.sessionId },
      attachment_ref: transfer.attachmentRef,
      transfer_id: transfer.transferId,
    }
  }

  /**
   * Resolve an authorized occurrence through the Source seam, then reserve a
   * short-lived transfer. No native byte is read here.
   */
  async open({ request = {}, deviceId, channelId, sessionId, attachmentRef, signal } = {}) {
    const device = deviceId ?? request.device_id
    const channel = channelId ?? request.channel_id
    const trace = request.trace_id
    let opened = null
    let reader = null
    try {
      this.#authorize({ deviceId: device, channelId: channel, hostId: request.host_id })
      if (!validId('session_id', sessionId)) throw new AdapterError('invalid_argument')
      if (!validId('attachment_ref', attachmentRef)) throw new AdapterError('invalid_argument')
      if (!this.adapter || typeof this.adapter.openAttachment !== 'function') {
        throw new AdapterError('unsupported_capability')
      }
      if (signal?.aborted) throw new AdapterError(abortCode(signal))
      if (typeof this.adapter.getSession === 'function') {
        const target = await this.adapter.getSession(sessionId, { signal })
        if (signal?.aborted) throw new AdapterError(abortCode(signal))
        if (!target || target.sessionId !== sessionId) throw new AdapterError('not_found')
        this.#authorize({ deviceId: device, channelId: channel })
      }
      opened = await this.adapter.openAttachment(sessionId, attachmentRef, { signal })
      if (signal?.aborted) throw new AdapterError(abortCode(signal))
      this.#authorize({ deviceId: device, channelId: channel })
      const attachment = opened?.attachment
      if (!attachment || typeof attachment !== 'object') throw new AdapterError('not_found')
      if (attachment.available === false) throw new AdapterError('not_found')
      if (attachment.attachment_ref !== undefined && attachment.attachment_ref !== attachmentRef) {
        throw new AdapterError('forbidden')
      }
      const scope = attachment.session ?? {}
      if (scope.host_id !== undefined && scope.host_id !== this.hostId) throw new AdapterError('forbidden')
      if (scope.session_id !== undefined && scope.session_id !== sessionId) throw new AdapterError('forbidden')
      if (!isCount(attachment.size)) throw new AdapterError('invalid_argument')
      const rawPolicy = opened.policy ?? attachment.policy
      const adapterPolicy = validateAttachmentPolicy({ ...this.policy, ...(rawPolicy ?? {}) })
      const policy = this.#effectivePolicy(adapterPolicy, request, channel)
      if (policy.max_total_bytes > 0 && attachment.size > policy.max_total_bytes) {
        throw new AdapterError('payload_too_large')
      }
      reader = this.#readerFor(opened, sessionId, attachmentRef)
      if (!reader) throw new AdapterError('unsupported_capability')
      this.#admit({ deviceId: device, channelId: channel, size: attachment.size, policy })
      const now = this.now()
      const transfer = {
        transferId: randomBytes(16).toString('hex'),
        hostId: this.hostId,
        deviceId: device,
        channelId: channel,
        sessionId,
        attachmentRef,
        requestId: request.request_id ?? '',
        traceId: trace,
        mediaType: boundedBytes(
          typeof attachment.media_type === 'string' && attachment.media_type !== ''
            ? attachment.media_type
            : 'application/octet-stream',
          256,
        ),
        name: safeSuggestion(attachment.name),
        size: attachment.size,
        policy,
        reader,
        hash: createHash('sha256'),
        expectedSha256: attachment.sha256 instanceof Uint8Array && attachment.sha256.length === 32
          ? Buffer.from(attachment.sha256)
          : null,
        nextOffset: 0,
        lastPart: null,
        finished: false,
        disposed: false,
        resourcesReleased: false,
        controller: new AbortController(),
        queue: Promise.resolve(),
        idleAt: now + policy.idle_timeout_ms,
        deadline: now + policy.overall_timeout_ms,
        openedAt: now,
        idleTimer: null,
        deadlineTimer: null,
      }
      const metadata = validateAttachmentMetadata({
        session: { host_id: this.hostId, session_id: sessionId },
        attachment_ref: attachmentRef,
        media_type: transfer.mediaType,
        ...(transfer.name ? { name: transfer.name } : {}),
        size: transfer.size,
        policy,
        transfer_id: transfer.transferId,
        expires_at_unix_ms: transfer.deadline,
      })
      this.#register(transfer)
      this.#armIdle(transfer)
      this.#armDeadline(transfer)
      this.#log('info', 'attachment_open', {
        stage: 'admit', transfer: transfer.transferId, request: transfer.requestId, trace,
        device, channel, bytes: transfer.size, limit: policy.max_total_bytes,
      })
      return metadata
    } catch (error) {
      if (reader) await reader.close({ cancel: true })
      else if (opened?.reader && typeof opened.reader.close === 'function') {
        try { await opened.reader.close({ cancel: true }) } catch { /* contained */ }
      }
      const code = wireCode(error)
      this.#log('warn', 'attachment_open_rejected', {
        stage: 'open', code, request: request.request_id, trace, device, channel,
      })
      throw error instanceof AdapterError ? error : new AdapterError(code)
    }
  }

  /** Serialize one transfer's reads so only one native iterator advances. */
  #serialize(transfer, operation) {
    const run = transfer.queue.then(operation, operation)
    transfer.queue = run.then(() => undefined, () => undefined)
    return run
  }

  /**
   * Read one contiguous part. The last completed offset is the only idempotent
   * retry; every other replay is a scoped conflict.
   */
  async pull({
    request = {}, deviceId, channelId, sessionId, attachmentRef, transferId,
    offset, maxBytes, signal,
  } = {}) {
    const device = deviceId ?? request.device_id
    const channel = channelId ?? request.channel_id
    try {
      this.#authorize({ deviceId: device, channelId: channel, hostId: request.host_id })
      if (!validId('transfer_id', transferId)) throw new AdapterError('invalid_argument')
      if (!Number.isSafeInteger(offset) || offset < 0) throw new AdapterError('invalid_argument')
      if (maxBytes !== undefined && maxBytes !== null
        && (!Number.isSafeInteger(maxBytes) || maxBytes < 0 || maxBytes > 0xffffffff)) {
        throw new AdapterError('invalid_argument')
      }
      const transfer = this.#lookup(transferId, { deviceId: device, channelId: channel })
      this.#bind(transfer, { sessionId, attachmentRef })
      this.#checkLiveness(transfer)
      // A client bound is a ceiling, never a demand: clamp it to the effective
      // chunk. Each pull also re-derives the encoded bound from its own request
      // metadata so a smaller per-request HTTPS budget yields a smaller part
      // instead of an oversize result.
      const ceiling = this.#chunkCeiling(transfer.policy.max_chunk_bytes, request, channel)
      if (ceiling < 1) throw new AdapterError('payload_too_large')
      const requested = Math.min(maxBytes || transfer.policy.max_chunk_bytes, transfer.policy.max_chunk_bytes, ceiling)
      const result = await this.#serialize(transfer, () => this.#pullPart(transfer, offset, requested, signal))
      this.#log('info', 'attachment_part', {
        stage: 'read', transfer: transfer.transferId, request: request.request_id, trace: request.trace_id,
        device, channel, bytes: result.length, limit: transfer.policy.max_chunk_bytes,
        offset: result.offset, final: result.final,
      })
      return {
        ...this.#descriptor(transfer),
        offset: result.offset,
        length: result.data.length,
        data: result.data,
        final: result.final,
        ...(result.final ? { total_size: result.total_size, sha256: result.sha256 } : {}),
      }
    } catch (error) {
      const code = wireCode(error)
      this.#log('warn', 'attachment_pull_rejected', {
        stage: 'read', code, request: request.request_id, trace: request.trace_id,
        device, channel, offset: Number.isSafeInteger(offset) ? offset : undefined,
      })
      throw error instanceof AdapterError ? error : new AdapterError(code)
    }
  }

  async #pullPart(transfer, offset, requested, requestSignal) {
    if (transfer.disposed) throw new AdapterError('not_found')
    this.#checkLiveness(transfer)
    if (requestSignal?.aborted) {
      const code = abortCode(requestSignal)
      this.#retire(transfer, { reason: 'request_aborted', code })
      throw new AdapterError(code)
    }
    const last = transfer.lastPart
    if (last && offset === last.offset) {
      if (last.retried) throw new AdapterError('conflict')
      last.retried = true
      this.#touch(transfer)
      return copyPart(last)
    }
    if (offset !== transfer.nextOffset) throw new AdapterError('conflict')
    if (transfer.finished) throw new AdapterError('conflict')
    if (!transfer.reader) throw new AdapterError('unavailable')
    const signal = joinSignals(requestSignal, transfer.controller.signal)
    if (signal.aborted) {
      this.#retire(transfer, { reason: 'request_aborted', code: abortCode(transfer.controller.signal, abortCode(signal)) })
      throw new AdapterError(abortCode(transfer.controller.signal, abortCode(signal)))
    }
    let read
    let onAbort
    const aborted = new Promise((_, reject) => {
      onAbort = () => reject(new AdapterError(abortCode(transfer.controller.signal, abortCode(requestSignal, abortCode(signal)))))
      signal.addEventListener('abort', onAbort, { once: true })
    })
    const pending = transfer.reader.read(requested, { signal, offset: transfer.nextOffset })
    pending.catch(() => {})
    try {
      read = await Promise.race([pending, aborted])
    } catch (error) {
      const code = abortCode(transfer.controller.signal, requestSignal?.aborted ? abortCode(requestSignal) : wireCode(error))
      this.#retire(transfer, { reason: requestSignal?.aborted ? 'request_aborted' : 'source_error', code })
      throw new AdapterError(code)
    } finally {
      signal.removeEventListener('abort', onAbort)
    }
    if (signal.aborted) {
      const code = abortCode(transfer.controller.signal, abortCode(requestSignal))
      this.#retire(transfer, { reason: 'request_aborted', code })
      throw new AdapterError(code)
    }
    this.#checkLiveness(transfer)
    let bytes, eof
    try { ({ bytes, eof } = normalizeRead(read)) }
    catch {
      this.#retire(transfer, { reason: 'source_error', code: 'invalid_argument' })
      throw new AdapterError('invalid_argument')
    }
    if (bytes.length > requested) {
      this.#retire(transfer, { reason: 'oversize_part', code: 'payload_too_large' })
      throw new AdapterError('payload_too_large')
    }
    if (transfer.nextOffset + bytes.length > transfer.size) {
      this.#retire(transfer, { reason: 'size_mismatch', code: 'payload_digest_mismatch' })
      throw new AdapterError('payload_digest_mismatch')
    }
    if (bytes.length === 0 && !eof && transfer.nextOffset < transfer.size) {
      this.#retire(transfer, { reason: 'stalled_read', code: 'invalid_argument' })
      throw new AdapterError('invalid_argument')
    }
    transfer.hash.update(bytes)
    const nextOffset = transfer.nextOffset + bytes.length
    const final = eof || nextOffset >= transfer.size
    if (eof && nextOffset !== transfer.size) {
      this.#retire(transfer, { reason: 'size_mismatch', code: 'payload_digest_mismatch' })
      throw new AdapterError('payload_digest_mismatch')
    }
    let totalSize
    let sha256
    if (final) {
      if (nextOffset !== transfer.size) {
        this.#retire(transfer, { reason: 'size_mismatch', code: 'payload_digest_mismatch' })
        throw new AdapterError('payload_digest_mismatch')
      }
      sha256 = new Uint8Array(transfer.hash.digest())
      if (transfer.expectedSha256 && !transfer.expectedSha256.equals(Buffer.from(sha256))) {
        this.#retire(transfer, { reason: 'digest_mismatch', code: 'payload_digest_mismatch' })
        this.#log('warn', 'attachment_verify_failed', {
          stage: 'verify', transfer: transfer.transferId, code: 'payload_digest_mismatch', bytes: nextOffset,
        })
        throw new AdapterError('payload_digest_mismatch')
      }
      totalSize = transfer.size
      transfer.finished = true
      this.#finish(transfer)
      this.#log('info', 'attachment_verified', {
        stage: 'verify', transfer: transfer.transferId, bytes: totalSize,
      })
    }
    transfer.nextOffset = nextOffset
    transfer.lastPart = {
      offset, data: bytes, final, retried: false,
      ...(final ? { total_size: totalSize, sha256 } : {}),
    }
    this.#touch(transfer)
    return copyPart(transfer.lastPart)
  }

  /** Dispose a transfer. Close is idempotent for its exact device/channel scope. */
  async close({
    request = {}, deviceId, channelId, sessionId, attachmentRef, transferId, cancel = false,
  } = {}) {
    const device = deviceId ?? request.device_id
    const channel = channelId ?? request.channel_id
    try {
      this.#authorize({ deviceId: device, channelId: channel, hostId: request.host_id })
      if (!validId('transfer_id', transferId)) throw new AdapterError('invalid_argument')
      if (cancel !== undefined && typeof cancel !== 'boolean') throw new AdapterError('invalid_argument')
      const retired = this.#retired.get(transferId)
      if (!this.#transfers.has(transferId) && retired) {
        if (retired.until <= this.now()) { this.#retired.delete(transferId); throw new AdapterError('not_found') }
        if (retired.deviceId !== device || retired.channelId !== channel) throw new AdapterError('forbidden')
        if (retired.code === 'expired') throw new AdapterError('expired')
        return { closed: true, cancelled: Boolean(retired.cancelled) }
      }
      const transfer = this.#lookup(transferId, { deviceId: device, channelId: channel })
      this.#bind(transfer, { sessionId, attachmentRef })
      await this.#retire(transfer, {
        reason: cancel ? 'client_cancel' : 'client_close',
        code: 'not_found',
        cancel: Boolean(cancel),
      })
      return { closed: true, cancelled: Boolean(cancel) }
    } catch (error) {
      const code = wireCode(error)
      this.#log('warn', 'attachment_close_rejected', {
        stage: 'close', code, request: request.request_id, trace: request.trace_id, device, channel,
      })
      throw error instanceof AdapterError ? error : new AdapterError(code)
    }
  }

  /** Channel loss disposes every transfer bound to that exact channel. */
  cancelChannel(deviceId, channelId, reason = 'channel_closed') {
    let count = 0
    for (const transfer of [...this.#transfers.values()]) {
      if (transfer.deviceId !== deviceId || transfer.channelId !== channelId) continue
      this.#retire(transfer, { reason, code: reason === 'revoked' ? 'forbidden' : 'unavailable' })
      count++
    }
    if (count > 0) this.#log('info', 'attachment_channel_closed', { stage: 'cleanup', device: deviceId, channel: channelId, count })
    return count
  }

  /** A revoked Device loses every transfer immediately. */
  revokeDevice(deviceId) {
    let count = 0
    for (const transfer of [...this.#transfers.values()]) {
      if (transfer.deviceId !== deviceId) continue
      this.#retire(transfer, { reason: 'revoked', code: 'forbidden' })
      count++
    }
    if (count > 0) this.#log('info', 'attachment_device_revoked', { stage: 'cleanup', device: deviceId, count })
    return count
  }

  /** Host stop / reload disposes every active iterator. */
  disposeAll(reason = 'host_stop') {
    const count = this.#transfers.size
    for (const transfer of [...this.#transfers.values()]) {
      this.#retire(transfer, { reason, code: 'unavailable' })
    }
    return count
  }
}

export { RESULT_ENVELOPE_OVERHEAD_BYTES, RESULT_FRAMING_OVERHEAD_BYTES }
