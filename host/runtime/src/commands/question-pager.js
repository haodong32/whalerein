// Memory-only, authenticated cursors over a complete source snapshot. Question
// contents are encoded for sizing/fingerprinting but never logged or stored.
//
// Cursor semantics (v2) are stable across mid-pagination source changes and
// Host restarts:
//  - the cursor binds the issuing device, the session and the pager key id;
//  - it stores the last delivered batch id (`anchor`), the identity of the next
//    batch (`next_id`) and that next position (`index`) as a fallback hint;
//  - a later page resumes after the anchor when it still exists, otherwise at
//    `next_id`, otherwise at the clamped index. Adding, answering or removing a
//    batch therefore never turns a valid cursor into an opaque conflict;
//  - the key is supplied by the Host (persisted in the state store), so a
//    restart keeps paging. A cursor from a different key generation is
//    reported as `snapshot_required` (restart from the first page) instead of a
//    misleading conflict.
import { createHash, createHmac, randomBytes, timingSafeEqual } from 'node:crypto'
import { AdapterError } from '../errors.js'
import { MESSAGE_BYTES } from '../access/transport.js'

const CURSOR_VERSION = 2
const MAX_INDEX = 1_000_000

function varintBytes(value) {
  let n = value, count = 1
  while (n >= 128) { n = Math.floor(n / 128); count++ }
  return count
}

// The AEAD adds a 12-byte nonce and a 16-byte authentication tag. Calculate
// the largest complete ResponsePayload that can fit the actual HostResult,
// including protobuf tags and length varints, without sealing test payloads.
export function questionResponseBudget(codec, request, hostId, completedAt, protocolVersion) {
  const hostBase = codec.encode('whalerein.v1.HostResult', {
    request_id: request.request_id ?? '', host_id: hostId, device_id: request.device_id ?? '',
    status: 'succeeded', command_id: request.command_id ?? '',
    payload_digest: request.payload_digest instanceof Uint8Array ? request.payload_digest : new Uint8Array(0),
    error_code: 'error_code_unspecified', trace_id: request.trace_id ?? '', error_message: '',
    completed_at_unix_ms: completedAt, protocol_version: protocolVersion, channel_id: request.channel_id ?? '',
  }).length
  const frameBase = codec.encode('whalerein.v1.EncryptedFrame', {
    channel_id: request.channel_id ?? '', nonce: new Uint8Array(12),
  }).length
  const fits = plain => {
    const cipher = plain + 16
    const frame = frameBase + 1 + varintBytes(cipher) + cipher
    return hostBase + 1 + varintBytes(frame) + frame <= MESSAGE_BYTES
  }
  let low = 0, high = MESSAGE_BYTES
  while (low < high) {
    const middle = Math.ceil((low + high) / 2)
    if (fits(middle)) low = middle
    else high = middle - 1
  }
  return fits(low) ? low : 0
}

export class QuestionPager {
  #key
  #kid

  /**
   * @param {object} codec protobuf codec.
   * @param {object} [options]
   * @param {Buffer|Uint8Array} [options.key] stable Host-scoped pager key.
   *   Omit only for an ephemeral pager (tests/memory-only state).
   */
  constructor(codec, { key } = {}) {
    this.codec = codec
    this.#key = key ? Buffer.from(key) : randomBytes(32)
    this.#kid = createHash('sha256').update(this.#key).digest('hex').slice(0, 16)
  }

  #auth(deviceId, sessionId, encoded) {
    return createHmac('sha256', this.#key).update(JSON.stringify([deviceId, sessionId, encoded])).digest('base64url')
  }

  #token(deviceId, sessionId, body) {
    const encoded = Buffer.from(JSON.stringify(body)).toString('base64url')
    return `${encoded}.${this.#auth(deviceId, sessionId, encoded)}`
  }

  #cursor(cursor, deviceId, sessionId) {
    if (cursor === undefined || cursor === null || cursor === '') return null
    if (typeof cursor !== 'string') throw new AdapterError('invalid_argument')
    const parts = cursor.split('.')
    if (parts.length !== 2 || !parts[0] || !parts[1]) throw new AdapterError('conflict')
    let body
    try { body = JSON.parse(Buffer.from(parts[0], 'base64url').toString('utf8')) } catch { throw new AdapterError('conflict') }
    if (!body || typeof body !== 'object' || Array.isArray(body)) throw new AdapterError('conflict')
    // A cursor issued before this Host (or before an upgrade) cannot be
    // authenticated any more. Report an explicit restart instead of pretending
    // the source snapshot changed.
    if (body.v !== CURSOR_VERSION || body.kid !== this.#kid) throw new AdapterError('snapshot_required', 'question pagination state changed; restart from the first page')
    const expected = this.#auth(deviceId, sessionId, parts[0])
    const a = Buffer.from(parts[1]), b = Buffer.from(expected)
    if (a.length !== b.length || !timingSafeEqual(a, b)) throw new AdapterError('conflict')
    if (typeof body.anchor !== 'string' || typeof body.next_id !== 'string' ||
        !Number.isSafeInteger(body.index) || body.index < 0 || body.index > MAX_INDEX) throw new AdapterError('conflict')
    return { anchor: body.anchor, nextId: body.next_id, index: body.index }
  }

  // Resolve where the next page starts in the current snapshot. The anchor wins
  // so insertions before it never shift the continuation; the recorded next
  // identity covers an answered/removed anchor; the index is only a last-resort
  // hint for a fully replaced snapshot.
  #start(cursor, ids) {
    if (!cursor) return 0
    const anchor = cursor.anchor ? ids.indexOf(cursor.anchor) : -1
    if (anchor >= 0) return anchor + 1
    const next = cursor.nextId ? ids.indexOf(cursor.nextId) : -1
    if (next >= 0) return next
    return Math.min(cursor.index, ids.length)
  }

  page({ deviceId, sessionId, cursor, source, maxBytes }) {
    if (!deviceId || !sessionId || !Number.isSafeInteger(maxBytes) || maxBytes <= 0) throw new AdapterError('invalid_argument')
    if (!source || !Array.isArray(source.batches) || source.next_cursor) throw new AdapterError('unsupported_capability')
    // Authenticate the cursor before inspecting the snapshot so a cursor from
    // another device/session is rejected as such, not as a source error.
    const resolved = this.#cursor(cursor, deviceId, sessionId)
    const ids = new Set(), sizes = [], order = []
    for (const batch of source.batches) {
      if (batch.session_id !== sessionId || !batch.batch_id || ids.has(batch.batch_id)) throw new AdapterError('unavailable')
      ids.add(batch.batch_id)
      order.push(batch.batch_id)
      sizes.push(this.codec.encode('whalerein.v1.QuestionBatch', batch).length)
    }
    const index = this.#start(resolved, order)
    const chosen = []
    let batchBytes = 0
    const build = next => {
      const anchor = chosen.at(-1)?.batch_id ?? ''
      const nextId = next < order.length ? order[next] : ''
      const body = { v: CURSOR_VERSION, kid: this.#kid, anchor, next_id: nextId, index: next }
      return { batches: chosen, next_cursor: next < order.length ? this.#token(deviceId, sessionId, body) : '' }
    }
    const fits = next => {
      const pageBase = this.codec.encode('whalerein.v1.QuestionPage', { next_cursor: build(next).next_cursor }).length
      const pageBytes = pageBase + batchBytes
      return varintBytes(260 * 8 + 2) + varintBytes(pageBytes) + pageBytes <= maxBytes
    }
    for (let i = index; i < source.batches.length; i++) {
      const batch = source.batches[i]
      chosen.push(batch)
      batchBytes += 1 + varintBytes(sizes[i]) + sizes[i]
      if (fits(i + 1)) continue
      chosen.pop()
      batchBytes -= 1 + varintBytes(sizes[i]) + sizes[i]
      if (chosen.length) break
      chosen.push({ session_id: batch.session_id, batch_id: batch.batch_id, source_epoch: batch.source_epoch,
        version: batch.version, digest: batch.digest, phase: batch.phase, answerable: false, content_unavailable: true })
      const metadataSize = this.codec.encode('whalerein.v1.QuestionBatch', chosen[0]).length
      batchBytes = 1 + varintBytes(metadataSize) + metadataSize
      if (!fits(i + 1)) throw new AdapterError('payload_too_large')
    }
    const result = build(index + chosen.length)
    if (this.codec.encode('whalerein.v1.ResponsePayload', { question_page: result }).length > maxBytes) throw new AdapterError('payload_too_large')
    return result
  }
}
