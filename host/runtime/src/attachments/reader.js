// Bounded attachment reader helpers. The Host owns the iterator and the one
// pending part; this module only normalizes adapter return values and derives
// the raw-byte allowance from the encoded result geometry.
import { AdapterError } from '../errors.js'

// Access reserves a fixed protojson envelope around the encrypted result. The
// Host sees the encoded HTTPS bound, so it derives the raw ciphertext it can
// carry (base64 costs four bytes per three) and then the per-part framing.
export const RESULT_ENVELOPE_OVERHEAD_BYTES = 2048
export const RESULT_FRAMING_OVERHEAD_BYTES = 1024

/**
 * Default policy from one packaged transport profile. A part travels as one
 * complete HTTPS read; its raw chunk is bounded by the complete MQTT packet
 * budget and the transfer by the single relay transfer timeout. There is no
 * fragment width or idle-reassembly timeout in the complete-message contract.
 */
export function defaultAttachmentPolicy(profile) {
  const overall = profile.transferTimeoutMs
  // The receiver requires a strictly positive idle window below the overall
  // transfer deadline. Keep the historical 30 s idle ceiling unless a shorter
  // transfer timeout demands a smaller one.
  const idle = Math.max(1, Math.min(30_000, Math.floor(overall / 2)))
  // One part travels as a complete result envelope inside one MQTT packet.
  // Reserve the profile's framing overhead so a 64 KiB hop carries about
  // 60 KiB of part ciphertext instead of turning every part into an HTTPS
  // reference and consuming the temporary-object budget.
  const packet = Number.isSafeInteger(profile.packetBytes) && profile.packetBytes > 0
    ? profile.packetBytes
    : 65536
  return {
    max_total_bytes: 0,
    max_chunk_bytes: Math.max(1024, packet - 4096),
    max_concurrent: profile.bulkConcurrency,
    idle_timeout_ms: idle,
    overall_timeout_ms: overall,
  }
}

/** One policy value wins only when strictly lower; zero means "no total cap". */
export function minPositive(...values) {
  let result = 0
  for (const value of values) {
    if (!Number.isSafeInteger(value) || value <= 0) continue
    result = result === 0 ? value : Math.min(result, value)
  }
  return result
}

/**
 * Raw payload allowance for one attachment part. `resultMaxBytes` is the
 * encoded HTTPS result bound; `envelopeBytes` is the local/receiver logical
 * envelope. Both are hard ceilings, never targets.
 */
export function effectiveChunkBytes({ maxChunkBytes, resultMaxBytes = 0, envelopeBytes = 0 }) {
  let allowance = Number.isSafeInteger(maxChunkBytes) && maxChunkBytes > 0 ? maxChunkBytes : 0
  if (Number.isSafeInteger(envelopeBytes) && envelopeBytes > 0) {
    const envelopeAllowance = envelopeBytes - RESULT_ENVELOPE_OVERHEAD_BYTES - RESULT_FRAMING_OVERHEAD_BYTES
    allowance = allowance === 0 ? envelopeAllowance : Math.min(allowance, envelopeAllowance)
  }
  if (Number.isSafeInteger(resultMaxBytes) && resultMaxBytes > 0) {
    const raw = resultMaxBytes - RESULT_ENVELOPE_OVERHEAD_BYTES
    const encoded = raw <= 0 ? 0 : Math.floor((raw * 3) / 4) - RESULT_FRAMING_OVERHEAD_BYTES
    allowance = allowance === 0 ? encoded : Math.min(allowance, encoded)
  }
  return allowance > 0 ? allowance : 0
}

/** A Source filename is only a display suggestion; never a local path. */
export function safeSuggestion(value) {
  if (typeof value !== 'string') return ''
  const cleaned = value.replace(/[\u0000-\u001f\u007f]/g, '').trim()
  if (cleaned === '') return ''
  const base = cleaned.split(/[\\/]/).pop() ?? ''
  if (base === '' || base === '.' || base === '..') return ''
  let suggestion = ''
  for (const character of base) {
    if (Buffer.byteLength(suggestion + character, 'utf8') > 512) break
    suggestion += character
  }
  return suggestion
}

/** Normalize one adapter part read. */
export function normalizeRead(result) {
  if (!result || typeof result !== 'object' || Array.isArray(result)) {
    throw new AdapterError('invalid_argument')
  }
  const value = result.bytes ?? result.data
  let bytes
  if (value instanceof Uint8Array) bytes = value
  else if (Array.isArray(value)) bytes = Uint8Array.from(value)
  else if (value instanceof ArrayBuffer) bytes = new Uint8Array(value)
  else throw new AdapterError('invalid_argument')
  const eof = result.eof === true || result.final === true
  return { bytes, eof }
}

/** Wrap one Source reader with a stable, idempotent close. */
export class AttachmentReader {
  #read
  #close
  #closed = false
  #cancel = false

  constructor({ read, close }) {
    if (typeof read !== 'function') throw new AdapterError('unsupported_capability')
    this.#read = read
    this.#close = typeof close === 'function' ? close : null
  }

  get closed() {
    return this.#closed
  }

  async read(maxBytes, { signal, offset } = {}) {
    if (this.#closed) throw new AdapterError('not_found')
    if (signal?.aborted) throw new AdapterError('unavailable')
    return this.#read(maxBytes, { signal, offset })
  }

  async close({ cancel = false } = {}) {
    if (this.#closed) return
    this.#closed = true
    this.#cancel = Boolean(cancel)
    try {
      await this.#close?.({ cancel: Boolean(cancel) })
    } catch {
      // Cleanup is best effort and never changes the caller's outcome.
    }
  }
}
