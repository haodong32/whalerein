// Memory-only, authenticated cursors over a complete source snapshot. Question
// contents are encoded for sizing/fingerprinting but never logged or stored.
import { createHash, createHmac, randomBytes, timingSafeEqual } from 'node:crypto'
import { AdapterError } from '../errors.js'
import { MESSAGE_BYTES } from '../access/transport.js'

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
  #key = randomBytes(32)
  constructor(codec) { this.codec = codec }

  #token(deviceId, sessionId, fingerprint, index) {
    const body = Buffer.from(JSON.stringify({ v: 1, index })).toString('base64url')
    const auth = createHmac('sha256', this.#key).update(JSON.stringify([deviceId, sessionId, fingerprint, body])).digest('base64url')
    return `${body}.${auth}`
  }

  #offset(cursor, deviceId, sessionId, fingerprint, length) {
    if (!cursor) return 0
    if (typeof cursor !== 'string') throw new AdapterError('invalid_argument')
    const parts = cursor.split('.')
    if (parts.length !== 2 || !parts[0] || !parts[1]) throw new AdapterError('conflict')
    const expected = this.#token(deviceId, sessionId, fingerprint, (() => {
      try { return JSON.parse(Buffer.from(parts[0], 'base64url').toString()).index } catch { return NaN }
    })())
    const a = Buffer.from(cursor), b = Buffer.from(expected)
    if (a.length !== b.length || !timingSafeEqual(a, b)) throw new AdapterError('conflict')
    let data
    try { data = JSON.parse(Buffer.from(parts[0], 'base64url').toString()) } catch { throw new AdapterError('conflict') }
    if (data.v !== 1 || !Number.isSafeInteger(data.index) || data.index < 1 || data.index >= length) throw new AdapterError('conflict')
    return data.index
  }

  page({ deviceId, sessionId, cursor, source, maxBytes }) {
    if (!deviceId || !sessionId || !Number.isSafeInteger(maxBytes) || maxBytes <= 0) throw new AdapterError('invalid_argument')
    if (!source || !Array.isArray(source.batches) || source.next_cursor) throw new AdapterError('unsupported_capability')
    const ids = new Set(), digest = createHash('sha256'), sizes = []
    for (const batch of source.batches) {
      if (batch.session_id !== sessionId || !batch.batch_id || ids.has(batch.batch_id)) throw new AdapterError('unavailable')
      ids.add(batch.batch_id)
      const encoded = this.codec.encode('whalerein.v1.QuestionBatch', batch)
      sizes.push(encoded.length)
      const length = Buffer.alloc(8); length.writeBigUInt64BE(BigInt(encoded.length))
      digest.update(length).update(encoded)
    }
    const fingerprint = digest.digest('hex')
    const index = this.#offset(cursor, deviceId, sessionId, fingerprint, source.batches.length)
    const chosen = []
    let batchBytes = 0
    const build = next => ({ batches: chosen, next_cursor: next < source.batches.length ? this.#token(deviceId, sessionId, fingerprint, next) : '' })
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
