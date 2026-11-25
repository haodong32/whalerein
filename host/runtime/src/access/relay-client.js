// Host-authority HTTPS client for the Access temporary ciphertext directory.
//
// The Host signs reserve/upload/finish/get/delete with the Ed25519 key that its
// authenticated HostReady already registered. There is no second identity.
// Ciphertext, object filenames and signing keys never enter logs.

import { readFileSync } from 'node:fs'
import { ed25519 } from '@noble/curves/ed25519.js'
import { Agent } from 'undici'
import { bytesEqual, hex, sha256Bytes } from '../security/digest.js'
import { AdapterError, PluginError } from '../errors.js'
import { codec as defaultCodec, resolveCodec } from '../protocol.js'
import { RELAY_VERSION, validateTransportBudget } from '../contracts/transport.js'

export const RELAY_SIGNATURE_PREFIX = 'whalerein/v1/relay'

const EPOCH_HEADER = 'WhaleRein-Relay-Epoch'
const SIGNED_AT_HEADER = 'WhaleRein-Host-Signed-At-Unix-Ms'
const SIGNATURE_HEADER = 'WhaleRein-Host-Signature'
const OBJECT_ID = /^[a-f0-9]{32}$/

function b64url(bytes) {
  return Buffer.from(bytes).toString('base64url')
}

function toSnake(key) {
  return key.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`)
}

function normalizeJsonKeys(value) {
  if (Array.isArray(value)) return value.map(normalizeJsonKeys)
  if (!value || typeof value !== 'object') return value
  const out = {}
  for (const [key, item] of Object.entries(value)) {
    out[toSnake(key)] = normalizeJsonKeys(item)
  }
  return out
}

/** Fixed signed text forms from proto/v1/WIRE.md. */
export function relaySignedText(purpose, fields) {
  if (purpose === 'reserve') {
    return `${RELAY_SIGNATURE_PREFIX}|reserve|${fields.relayVersion}|${fields.hostId}|${fields.deviceId}|${fields.sessionId}|${fields.channelId}|${fields.purpose}|${fields.partId}|${fields.ciphertextBytes}|${fields.sha256Hex}|${fields.expiresAt}|${fields.signedAt}`
  }
  if (purpose === 'upload' || purpose === 'finish') {
    return `${RELAY_SIGNATURE_PREFIX}|${purpose}|${fields.relayVersion}|${fields.objectId}|${fields.ciphertextBytes}|${fields.sha256Hex}|${fields.signedAt}`
  }
  if (purpose === 'object') {
    return `${RELAY_SIGNATURE_PREFIX}|object|${fields.relayVersion}|${fields.objectId}|${fields.signedAt}`
  }
  throw new PluginError('invalid_argument', 'unknown relay signature purpose')
}

function sign(privateKey, text) {
  return ed25519.sign(new TextEncoder().encode(text), privateKey)
}

/** Validate one opaque reference before it is fetched or published. */
export function validatePayloadReference(reference, { envelopeBytes, now = Date.now, purpose = null } = {}) {
  if (!reference || typeof reference !== 'object' || Array.isArray(reference)) {
    throw new PluginError('invalid_argument', 'payload reference missing')
  }
  if (typeof reference.object_id !== 'string' || !OBJECT_ID.test(reference.object_id)) {
    throw new PluginError('invalid_argument', 'payload reference object id invalid')
  }
  if (typeof reference.owner_node_id !== 'string' || reference.owner_node_id.length === 0
    || reference.owner_node_id.length > 256) {
    throw new PluginError('invalid_argument', 'payload reference owner invalid')
  }
  const size = Number(reference.ciphertext_bytes)
  if (!Number.isSafeInteger(size) || size <= 0) {
    throw new PluginError('invalid_argument', 'payload reference length invalid')
  }
  if (Number.isSafeInteger(envelopeBytes) && size > envelopeBytes) {
    throw new PluginError('payload_too_large', 'payload reference exceeds the envelope budget')
  }
  if (!(reference.sha256 instanceof Uint8Array) || reference.sha256.length !== 32) {
    throw new PluginError('invalid_argument', 'payload reference digest invalid')
  }
  const expiresAt = Number(reference.expires_at_unix_ms)
  if (!Number.isSafeInteger(expiresAt) || expiresAt <= 0) {
    throw new PluginError('invalid_argument', 'payload reference expiry invalid')
  }
  if (expiresAt <= now()) throw new PluginError('expired', 'payload reference lifetime ended')
  if (purpose !== null && reference.purpose !== purpose) {
    throw new PluginError('forbidden', 'payload reference purpose mismatch')
  }
  return reference
}

/**
 * Host-authority relay object client.
 *
 * `origin` is the configured Access HTTPS origin. `privateKey` is the same
 * Ed25519 seed announced in HostReady.authorization_public_key.
 */
export class RelayObjectClient {
  constructor({
    origin,
    hostId,
    privateKey,
    codec = defaultCodec,
    fetch: fetchImpl = globalThis.fetch,
    now = Date.now,
    timeoutMs = 120_000,
    caFile = '',
    logger,
  }) {
    let parsed
    try {
      parsed = new URL(origin)
    } catch {
      throw new AdapterError('invalid_argument', 'Access relay origin invalid')
    }
    if (!['https:', 'http:'].includes(parsed.protocol)) {
      throw new AdapterError('invalid_argument', 'Access relay origin must be HTTP(S)')
    }
    if (parsed.username || parsed.password || parsed.search || parsed.hash) {
      throw new AdapterError('invalid_argument', 'Access relay origin must not carry credentials or a query')
    }
    if (typeof hostId !== 'string' || hostId === '') {
      throw new AdapterError('invalid_argument', 'Host identity is required for relay objects')
    }
    if (!(privateKey instanceof Uint8Array) || privateKey.length !== 32) {
      throw new AdapterError('invalid_argument', 'Host relay signing key is unavailable')
    }
    if (typeof fetchImpl !== 'function') {
      throw new AdapterError('unavailable', 'HTTP client is unavailable')
    }
    this.origin = `${parsed.origin}${parsed.pathname.replace(/\/+$/, '')}`
    this.originRoot = parsed.origin
    this.hostId = hostId
    this.privateKey = privateKey
    this.codec = resolveCodec(codec)
    this.fetchImpl = fetchImpl
    this.now = now
    this.timeoutMs = Number.isSafeInteger(timeoutMs) && timeoutMs > 0 ? timeoutMs : 120_000
    this.logger = logger
    this.dispatcher = caFile ? new Agent({ connect: { ca: readFileSync(caFile) } }) : null
  }

  #url(path) {
    if (typeof path === 'string' && /^https?:\/\//.test(path)) {
      let target
      try {
        target = new URL(path)
      } catch {
        throw new PluginError('invalid_argument', 'relay upload path is invalid')
      }
      if (target.origin !== this.originRoot) {
        throw new PluginError('forbidden', 'relay upload path is outside the configured origin')
      }
      return path
    }
    return `${this.origin}${path.startsWith('/') ? '' : '/'}${path}`
  }

  #json(typeName, value) {
    const bytes = this.codec.encode(typeName, value)
    return JSON.stringify(this.codec.decodeJson(typeName, bytes))
  }

  #parseJson(typeName, text, label) {
    let value
    try {
      value = JSON.parse(text)
    } catch {
      throw new PluginError('protocol_version_incompatible', `${label} response is not JSON`)
    }
    try {
      return this.codec.decode(typeName, this.codec.encodeJsonValue(typeName, normalizeJsonKeys(value)))
    } catch {
      throw new PluginError('protocol_version_incompatible', `${label} response is invalid`)
    }
  }

  async #request(path, { method, headers = {}, body, signal = null, timeoutMs = this.timeoutMs, expected = [200] } = {}) {
    const target = this.#url(path)
    const controller = new AbortController()
    const timer = setTimeout(() => controller.abort(), timeoutMs)
    timer.unref?.()
    const abort = () => controller.abort()
    signal?.addEventListener?.('abort', abort, { once: true })
    let response
    try {
      response = await this.fetchImpl(target, {
        method,
        headers,
        ...(body === undefined ? {} : { body }),
        redirect: 'error',
        signal: controller.signal,
        ...(this.dispatcher ? { dispatcher: this.dispatcher } : {}),
      })
    } catch (error) {
      if (error instanceof PluginError) throw error
      throw new PluginError('unavailable', 'relay directory request failed')
    } finally {
      clearTimeout(timer)
      signal?.removeEventListener?.('abort', abort)
    }
    if (response.redirected) throw new PluginError('forbidden', 'relay directory response redirected')
    if (!Number.isSafeInteger(response.status)) throw new PluginError('unavailable', 'relay directory response invalid')
    if (expected.includes(response.status)) return response
    await this.#raise(response)
  }

  async #raise(response) {
    let code = null
    let reason = ''
    try {
      const payload = await response.json()
      code = typeof payload?.code === 'string' ? payload.code : null
      reason = typeof payload?.reason === 'string' ? payload.reason : ''
    } catch {
      // Error bodies are optional metadata; the status is authoritative.
    }
    const mapped = code && typeof code === 'string' ? code : null
    const byStatus = {
      400: 'invalid_argument',
      401: 'unauthorized',
      403: 'forbidden',
      404: 'not_found',
      409: 'conflict',
      410: 'expired',
      413: 'payload_too_large',
      429: 'rate_limited',
      503: 'unavailable',
    }
    const wire = mapped ?? byStatus[response.status] ?? 'unavailable'
    const error = new PluginError(wire, `relay directory rejected the request (${response.status})`)
    if (reason === 'wrong_owner_path' || reason === 'quota_exceeded' || reason === 'storage_pressure') {
      error.reason = reason
    }
    throw error
  }

  #hostHeaders(objectId, signedAt) {
    return {
      [EPOCH_HEADER]: String(RELAY_VERSION),
      [SIGNED_AT_HEADER]: String(signedAt),
      [SIGNATURE_HEADER]: b64url(sign(this.privateKey, relaySignedText('object', {
        relayVersion: RELAY_VERSION, objectId, signedAt,
      }))),
    }
  }

  async reserve({
    deviceId, sessionId = '', channelId = '', purpose, partId = '',
    ciphertext, expiresAt, requestId = '', connectionRef = '', enrollmentGeneration = 0,
  }) {
    if (!(ciphertext instanceof Uint8Array) || ciphertext.length === 0) {
      throw new PluginError('invalid_argument', 'relay ciphertext missing')
    }
    const signedAt = this.now()
    const digest = sha256Bytes(ciphertext)
    const text = relaySignedText('reserve', {
      relayVersion: RELAY_VERSION,
      hostId: this.hostId,
      deviceId,
      sessionId,
      channelId,
      purpose,
      partId,
      ciphertextBytes: ciphertext.length,
      sha256Hex: hex(digest),
      expiresAt,
      signedAt,
    })
    const body = this.#json('whalerein.v1.RelayObjectReserveRequest', {
      relay_version: RELAY_VERSION,
      host_id: this.hostId,
      device_id: deviceId,
      session_id: sessionId,
      channel_id: channelId,
      purpose,
      part_id: partId,
      ciphertext_bytes: ciphertext.length,
      sha256: digest,
      expires_at_unix_ms: expiresAt,
      request_id: requestId,
      enrollment_generation: enrollmentGeneration,
      connection_ref: connectionRef,
      signed_at_unix_ms: signedAt,
      signature: sign(this.privateKey, text),
    })
    const response = await this.#request('/v1/relay/objects', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body,
      expected: [200],
    })
    let reservation
    try {
      reservation = this.#parseJson('whalerein.v1.RelayObjectReservation', await response.text(), 'reserve')
    } catch (error) {
      throw error instanceof PluginError ? error : new PluginError('protocol_version_incompatible', 'reserve response invalid')
    }
    if (typeof reservation.object_id !== 'string' || !OBJECT_ID.test(reservation.object_id)
      || typeof reservation.upload_path !== 'string' || reservation.upload_path === '') {
      throw new PluginError('protocol_version_incompatible', 'reserve response is missing object routing')
    }
    this.logger?.debug('relay_object_reserved', { purpose, bytes: ciphertext.length })
    return { ...reservation, sha256: digest, expires_at_unix_ms: expiresAt }
  }

  async upload({ reservation, ciphertext, timeoutMs = reservation.upload_timeout_ms }) {
    if (!(ciphertext instanceof Uint8Array) || ciphertext.length !== Number(reservation.max_bytes)) {
      throw new PluginError('invalid_argument', 'relay upload length does not match the reservation')
    }
    const signedAt = this.now()
    const digest = sha256Bytes(ciphertext)
    const signature = sign(this.privateKey, relaySignedText('upload', {
      relayVersion: RELAY_VERSION,
      objectId: reservation.object_id,
      ciphertextBytes: ciphertext.length,
      sha256Hex: hex(digest),
      signedAt,
    }))
    await this.#request(reservation.upload_path, {
      method: 'PUT',
      headers: {
        'content-type': 'application/octet-stream',
        'content-length': String(ciphertext.length),
        [EPOCH_HEADER]: String(RELAY_VERSION),
        [SIGNED_AT_HEADER]: String(signedAt),
        [SIGNATURE_HEADER]: b64url(signature),
      },
      body: ciphertext,
      timeoutMs: Number.isSafeInteger(timeoutMs) && timeoutMs > 0 ? timeoutMs : this.timeoutMs,
      expected: [204],
    })
  }

  async finish({ objectId, ciphertext }) {
    const signedAt = this.now()
    const digest = sha256Bytes(ciphertext)
    const body = this.#json('whalerein.v1.RelayObjectFinishRequest', {
      relay_version: RELAY_VERSION,
      ciphertext_bytes: ciphertext.length,
      sha256: digest,
      signed_at_unix_ms: signedAt,
      signature: sign(this.privateKey, relaySignedText('finish', {
        relayVersion: RELAY_VERSION,
        objectId,
        ciphertextBytes: ciphertext.length,
        sha256Hex: hex(digest),
        signedAt,
      })),
    })
    const response = await this.#request(`/v1/relay/objects/${objectId}/finish`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body,
      expected: [200],
    })
    let object
    try {
      object = this.#parseJson('whalerein.v1.RelayObject', await response.text(), 'finish')
    } catch (error) {
      throw error instanceof PluginError ? error : new PluginError('protocol_version_incompatible', 'finish response invalid')
    }
    if (object.object_id !== objectId) {
      throw new PluginError('protocol_version_incompatible', 'finish response object mismatch')
    }
    this.logger?.debug('relay_object_finished', { purpose: object.purpose, bytes: ciphertext.length })
    return object
  }

  /** Fetch one reference and verify its exact length and SHA-256. */
  async fetch({ reference, envelopeBytes, purpose, signal = null }) {
    const size = Number(reference.ciphertext_bytes)
    validatePayloadReference(reference, { envelopeBytes, now: this.now })
    if (purpose !== undefined && reference.purpose !== purpose) {
      throw new PluginError('forbidden', 'payload reference purpose mismatch')
    }
    const headers = this.#hostHeaders(reference.object_id, this.now())
    const response = await this.#request(`/v1/relay/objects/${reference.object_id}`, {
      method: 'GET',
      headers,
      signal,
      expected: [200],
    })
    const declared = response.headers?.get?.('content-length')
    if (declared !== null && declared !== undefined) {
      const length = Number(declared)
      if (!Number.isSafeInteger(length) || length !== size) {
        throw new PluginError('invalid_argument', 'relay object length does not match the reference')
      }
    }
    let bytes
    try {
      bytes = new Uint8Array(await response.arrayBuffer())
    } catch {
      throw new PluginError('unavailable', 'relay object stream failed')
    }
    if (bytes.length !== size) {
      throw new PluginError('invalid_argument', 'relay object is truncated or oversized')
    }
    if (!bytesEqual(sha256Bytes(bytes), reference.sha256)) {
      throw new PluginError('payload_digest_mismatch', 'relay object digest does not match the reference')
    }
    return bytes
  }

  async delete({ objectId }) {
    const signedAt = this.now()
    const response = await this.#request(`/v1/relay/objects/${objectId}`, {
      method: 'DELETE',
      headers: this.#hostHeaders(objectId, signedAt),
      expected: [200],
    })
    try {
      return this.#parseJson('whalerein.v1.RelayObjectDeleteResult', await response.text(), 'delete')
    } catch {
      // A bounded cancel is best effort; the caller already failed.
      return { cancelled: false }
    }
  }
}

export { validateTransportBudget }
