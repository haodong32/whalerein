// Managed Host enrollment client.
//
// This module owns the frozen Cloud create/poll contract, the Access Host
// enrollment proof, the new Host ownership proof, and the one-time credential
// persistence boundary. It never logs or returns secret material except the
// transport credential handed to the runtime after it has been persisted.

import { createHash, randomBytes } from 'node:crypto'
import { readFileSync, unlinkSync } from 'node:fs'
import { join } from 'node:path'
import { ed25519 } from '@noble/curves/ed25519.js'
import { PluginError } from './errors.js'
import { CREDENTIAL_PURPOSES } from './credentials.js'
import { atomicWriteJson } from './registry.js'
import {
  OFFICIAL_SERVICES,
  SERVICE_PROTOCOL_VERSION,
  validateServiceMetadata,
  validateServiceOrigin,
} from './services.js'

export const INTENT_PROOF_PREFIX = 'whalerein/host-intent-v1'
export const HOST_PROOF_PREFIX = 'whalerein/v1/host-proof-v1'
export const ENROLLMENT_PROOF_PREFIX = 'whalerein/v1/enrollment'
export const AUTHORIZATION_PREFIX = 'whalerein/v1/ext-auth-v1'
export const INTENT_SCHEMA = 'whalerein-host/v1/enrollment-intent'
export const POLL_DEFAULT_MS = 2000
export const MAX_BACKOFF_MS = 30_000
export const DEFAULT_DEADLINE_MS = 10 * 60 * 1000
export const LOCALES = Object.freeze(['en', 'zh-CN'])

const REQUIRED_CAPABILITIES = Object.freeze(['host-enrollment-v1'])
const AUTHORIZATION_PURPOSE = 'host_enrollment'
const RECOVERY_PURPOSE = 'host_credential_recovery'

export class EnrollmentError extends Error {
  constructor(code, detail = '', extra = {}) {
    super(detail || code)
    this.name = 'EnrollmentError'
    this.code = code
    this.detail = detail
    Object.assign(this, extra)
  }
}

function fail(code, detail, extra) {
  throw new EnrollmentError(code, detail, extra)
}

function b64(value) {
  return Buffer.from(value).toString('base64url')
}

function decodeFlexibleBase64(value) {
  if (typeof value !== 'string' || value === '') return null
  try {
    return new Uint8Array(Buffer.from(value, 'base64url'))
  } catch {
    try {
      return new Uint8Array(Buffer.from(value, 'base64'))
    } catch {
      return null
    }
  }
}

function normalizeFieldNames(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return value
  const out = {}
  for (const [key, item] of Object.entries(value)) {
    out[key] = item
    const snake = key.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`)
    if (!(snake in out)) out[snake] = item
  }
  return out
}

function isoString(milliseconds) {
  return new Date(milliseconds).toISOString()
}

function parseTime(value) {
  if (typeof value === 'number' && Number.isSafeInteger(value) && value > 0) return value
  if (typeof value === 'string' && value !== '') {
    const numeric = Number(value)
    if (Number.isSafeInteger(numeric) && numeric > 0) return numeric
    const parsed = Date.parse(value)
    if (Number.isFinite(parsed)) return parsed
  }
  return null
}

function originOf(value) {
  try {
    return new URL(value).origin
  } catch {
    return null
  }
}

function assertLocale(locale) {
  if (!LOCALES.includes(locale)) fail('invalid_argument', 'unsupported locale')
}

/** Exact canonical create-intent proof text. */
export function canonicalIntentProof({ intentId, hostId, signingKey, noiseKey, service, environment, displayName = '', locale = '', issuedAt, nonce }) {
  return [
    INTENT_PROOF_PREFIX,
    intentId,
    hostId,
    signingKey,
    noiseKey,
    service ?? '',
    environment ?? '',
    displayName ?? '',
    locale ?? '',
    issuedAt,
    nonce,
  ].join('\n')
}

/** Exact newline-joined Host ownership proof text from the frozen contract. */
export function hostProofText({ purpose, hostId, subjectId = '', issuedAt, nonce }) {
  return [
    HOST_PROOF_PREFIX,
    `|${purpose}`,
    `|${hostId}`,
    `|${subjectId}`,
    `|${issuedAt}`,
    `|${nonce}`,
  ].join('\n')
}

/** Exact Access enrollment proof input (already implemented by stock Access). */
export function enrollmentProofInput({ authorization, operationId, hostSigningPublicKey }) {
  return [
    ENROLLMENT_PROOF_PREFIX,
    `|${authorization.authorizationId ?? authorization.authorization_id ?? ''}`,
    `|${canonicalNonce(authorization.nonce)}`,
    `|${authorization.hostId ?? authorization.host_id ?? ''}`,
    `|${b64(hostSigningPublicKey)}`,
    `|${operationId}`,
  ].join('')
}

function wireAuthorization(authorization) {
  // Emit exactly the canonical protojson field names. Carrying both the
  // camelCase and snake_case spelling makes protojson treat one of them as a
  // duplicate field and reject the whole request.
  const source = {}
  for (const [canonical, aliases] of Object.entries(AUTHORIZATION_FIELDS)) {
    for (const name of [canonical, ...aliases]) {
      if (authorization[name] !== undefined) {
        source[canonical] = authorization[name]
        break
      }
    }
  }
  const next = {}
  for (const [key, value] of Object.entries(source)) {
    if (key === 'hostPublicKeys' || key === 'devicePublicKeys') {
      if (Array.isArray(value)) next[key] = value.map((item) => Buffer.from(rawNonce(item)).toString('base64'))
      continue
    }
    if (key === 'nonce' || key === 'signature') {
      if (typeof value === 'string' && value !== '') next[key] = Buffer.from(rawNonce(value)).toString('base64')
      continue
    }
    next[key] = value
  }
  return next
}

const AUTHORIZATION_FIELDS = Object.freeze({
  authVersion: ['auth_version'],
  authorizationId: ['authorization_id'],
  issuer: [],
  audience: [],
  purpose: [],
  workspaceId: ['workspace_id'],
  regionId: ['region_id'],
  operationId: ['operation_id'],
  enrollmentRevision: ['enrollment_revision'],
  precondition: [],
  expectedRevision: ['expected_revision'],
  hostId: ['host_id'],
  hostPublicKeys: ['host_public_keys'],
  deviceId: ['device_id'],
  devicePublicKeys: ['device_public_keys'],
  issuedAtUnixMs: ['issued_at_unix_ms'],
  expiresAtUnixMs: ['expires_at_unix_ms'],
  nonce: [],
  keyId: ['key_id'],
  signature: [],
})

function rawNonce(value) {
  if (value instanceof Uint8Array) return value
  return decodeFlexibleBase64(value) ?? new Uint8Array(0)
}

function canonicalNonce(value) {
  const bytes = rawNonce(value)
  return bytes.length > 0 ? b64(bytes) : ''
}

function hashKeys(keys) {
  const normalized = Array.isArray(keys)
    ? keys.map((key) => (key instanceof Uint8Array ? key : decodeFlexibleBase64(key))).filter(Boolean)
    : []
  normalized.sort((left, right) => Buffer.compare(Buffer.from(left), Buffer.from(right)))
  const hasher = createHash('sha256')
  for (const key of normalized) {
    const length = Buffer.alloc(4)
    length.writeUInt32BE(key.length, 0)
    hasher.update(length)
    hasher.update(Buffer.from(key))
  }
  return hasher.digest('base64url')
}

/** Canonical portable-authorization bytes shared with Access. */
export function canonicalAuthorizationInput(value) {
  const authorization = normalizeFieldNames(value)
  const version = authorization.authVersion || authorization.auth_version || AUTHORIZATION_PREFIX
  return [
    version,
    `|${authorization.authorizationId ?? authorization.authorization_id ?? ''}`,
    `|${authorization.issuer ?? ''}`,
    `|${authorization.audience ?? ''}`,
    `|${authorization.purpose ?? ''}`,
    `|${authorization.workspaceId ?? authorization.workspace_id ?? ''}`,
    `|${authorization.regionId ?? authorization.region_id ?? ''}`,
    `|${authorization.operationId ?? authorization.operation_id ?? ''}`,
    `|${authorization.enrollmentRevision ?? authorization.enrollment_revision ?? ''}`,
    `|${authorization.precondition ?? ''}`,
    `|${authorization.expectedRevision ?? authorization.expected_revision ?? ''}`,
    `|${authorization.hostId ?? authorization.host_id ?? ''}`,
    `|${hashKeys(authorization.hostPublicKeys ?? authorization.host_public_keys ?? [])}`,
    `|${authorization.deviceId ?? authorization.device_id ?? ''}`,
    `|${hashKeys(authorization.devicePublicKeys ?? authorization.device_public_keys ?? [])}`,
    `|${authorization.issuedAtUnixMs ?? authorization.issued_at_unix_ms ?? 0}`,
    `|${authorization.expiresAtUnixMs ?? authorization.expires_at_unix_ms ?? 0}`,
    `|${canonicalNonce(authorization.nonce)}`,
  ].join('\n')
}

/** Reusable Host ownership proof header (JSON is base64url encoded). */
export function createHostProof({ signingPrivateKey, hostId, purpose, subjectId = '', issuedAt = Date.now(), nonce }) {
  if (!(signingPrivateKey instanceof Uint8Array) || signingPrivateKey.length !== 32) {
    fail('internal', 'Host signing key is unavailable')
  }
  if (typeof hostId !== 'string' || hostId === '') fail('internal', 'Host id is unavailable')
  if (!['enrollment_status', 'credential_replacement'].includes(purpose)) fail('internal', 'Host proof purpose is invalid')
  const nonceValue = nonce ?? b64(randomBytes(16))
  const text = hostProofText({ purpose, hostId, subjectId, issuedAt, nonce: nonceValue })
  const signature = b64(ed25519.sign(new TextEncoder().encode(text), signingPrivateKey))
  return Buffer.from(JSON.stringify({ hostId, issuedAt, nonce: nonceValue, signature }), 'utf8').toString('base64url')
}

/** Non-secret intent journal in the profile state directory. */
export class IntentJournal {
  #stateDir
  #write

  constructor({ stateDir, write = atomicWriteJson } = {}) {
    if (typeof stateDir !== 'string' || stateDir === '') fail('invalid_argument', 'intent state directory is required')
    this.#stateDir = stateDir
    this.#write = write
  }

  get path() {
    return join(this.#stateDir, 'enrollment-intent.json')
  }

  read() {
    let bytes
    try {
      bytes = readFileSync(this.path)
    } catch (error) {
      if (error?.code === 'ENOENT') return null
      fail('corrupt_intent', 'enrollment intent could not be read')
    }
    let intent
    try {
      intent = JSON.parse(bytes.toString('utf8'))
    } catch {
      fail('corrupt_intent', 'enrollment intent is not valid JSON')
    }
    if (!intent || intent.schema !== INTENT_SCHEMA || typeof intent.intentId !== 'string') {
      fail('corrupt_intent', 'enrollment intent is invalid')
    }
    return intent
  }

  write(intent) {
    if (!intent || intent.schema !== INTENT_SCHEMA || typeof intent.intentId !== 'string') fail('invalid_argument', 'intent is invalid')
    try {
      this.#write(this.path, intent)
    } catch {
      fail('unavailable', 'enrollment intent could not be persisted')
    }
    return intent
  }

  clear() {
    try {
      unlinkSync(this.path)
    } catch (error) {
      if (error?.code !== 'ENOENT') fail('unavailable', 'enrollment intent could not be cleared')
    }
  }
}

/** Load `/api/host-service` and validate it before any credential is sent. */
export async function fetchServiceMetadata({ apiOrigin, fetch, timeoutMs = 10_000, allowLoopback = false, expectedService = '', expectedEnvironment = '' } = {}) {
  if (typeof fetch !== 'function') fail('unavailable', 'HTTP client is unavailable')
  const origin = validateServiceOrigin(apiOrigin, { allowLoopback })
  let response
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), timeoutMs)
  timer.unref?.()
  try {
    response = await fetch(`${origin}/api/host-service`, {
      method: 'GET',
      headers: { accept: 'application/json' },
      redirect: 'error',
      signal: controller.signal,
    })
  } catch {
    fail('unavailable', 'service metadata is unavailable')
  } finally {
    clearTimeout(timer)
  }
  if (!response || !Number.isSafeInteger(response.status)) fail('unavailable', 'service metadata response is invalid')
  if (response.redirected) fail('forbidden', 'service metadata redirected to another origin')
  if (response.url && originOf(response.url) !== origin) fail('forbidden', 'service metadata response origin mismatch')
  if (response.status !== 200) {
    fail(response.status === 503 ? 'unavailable' : 'protocol_version_incompatible', 'service metadata was refused')
  }
  let metadata
  try {
    metadata = await response.json()
  } catch {
    fail('protocol_version_incompatible', 'service metadata is not JSON')
  }
  return validateServiceMetadata(metadata, { expectedService, expectedEnvironment, allowLoopback })
}

/**
 * Managed enrollment state machine. One instance belongs to one active
 * generation; its caller fences callbacks with `isActive` checks.
 */
export class ManagedEnrollmentClient {
  #options
  #fetch
  #now
  #sleep
  #journal
  #store
  #signingPrivateKey
  #signingPublicKey
  #noisePublicKey
  #hostId
  #apiOrigin
  #metadata

  constructor(options) {
    const {
      apiOrigin,
      metadata,
      hostId,
      signingPrivateKey,
      signingPublicKey,
      noisePublicKey,
      credentialStore,
      journal,
      fetch: fetchImpl = globalThis.fetch,
      now = Date.now,
      sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
      log = null,
      allowLoopback = false,
      timeoutMs = 15_000,
      verifyAuthorization = null,
      expectedIssuer = '',
      expectedAudience = '',
      displayName = 'WhaleRein Host',
      locale = 'en',
    } = options ?? {}
    if (!metadata || typeof metadata !== 'object') fail('invalid_argument', 'validated service metadata is required')
    if (typeof hostId !== 'string' || !/^[A-Za-z0-9_.:-]{1,128}$/.test(hostId)) fail('invalid_argument', 'Host id is invalid')
    if (!(signingPrivateKey instanceof Uint8Array) || signingPrivateKey.length !== 32) fail('invalid_argument', 'Host signing key is required')
    if (!(signingPublicKey instanceof Uint8Array) || signingPublicKey.length !== 32) fail('invalid_argument', 'Host signing public key is required')
    if (!(noisePublicKey instanceof Uint8Array) || noisePublicKey.length !== 32) fail('invalid_argument', 'Host Noise public key is required')
    if (!credentialStore || typeof credentialStore.get !== 'function' || typeof credentialStore.set !== 'function') {
      fail('invalid_argument', 'profile credential store is required')
    }
    if (typeof fetchImpl !== 'function') fail('invalid_argument', 'fetch is required')
    assertLocale(locale)
    this.#options = { allowLoopback, timeoutMs, verifyAuthorization, expectedIssuer, expectedAudience, displayName, locale }
    this.#fetch = fetchImpl
    this.#now = now
    this.#sleep = sleep
    this.#journal = journal
    this.#store = credentialStore
    this.#signingPrivateKey = signingPrivateKey
    this.#signingPublicKey = signingPublicKey
    this.#noisePublicKey = noisePublicKey
    this.#hostId = hostId
    this.#apiOrigin = validateServiceOrigin(apiOrigin, { allowLoopback })
    this.#metadata = metadata
    this.#options.log = log
  }

  get journal() {
    return this.#journal
  }

  get metadata() {
    return this.#metadata
  }

  /** Start or replay one immutable approval intent. */
  async createIntent({ retry = false } = {}) {
    let intent = this.#journal.read()
    if (intent && !retry && !['create_uncertain', 'create_failed'].includes(intent.state)) {
      return intent
    }
    if (intent && retry) {
      this.#journal.clear()
      intent = null
    }
    const intentId = intent?.intentId ?? b64(randomBytes(16))
    const bound = intent ?? {
      schema: INTENT_SCHEMA,
      intentId,
      hostId: this.#hostId,
      signingKey: b64(this.#signingPublicKey),
      noiseKey: b64(this.#noisePublicKey),
      service: this.#metadata.service,
      environment: this.#metadata.environment,
      displayName: this.#options.displayName,
      locale: this.#options.locale,
      createdAt: this.#now(),
      state: 'creating',
    }
    // The intent and its bound proof inputs are durable before the request.
    this.#journal.write({ ...bound, state: bound.state === 'create_uncertain' ? 'create_uncertain' : 'creating' })
    const issuedAt = isoString(this.#now())
    const nonce = b64(randomBytes(16))
    const body = {
      intentId: bound.intentId,
      hostId: bound.hostId,
      signingKey: bound.signingKey,
      noiseKey: bound.noiseKey,
      displayName: bound.displayName,
      locale: bound.locale,
      service: bound.service,
      environment: bound.environment,
      proof: {
        nonce,
        issuedAt,
        signature: b64(ed25519.sign(new TextEncoder().encode(canonicalIntentProof({ ...bound, issuedAt, nonce })), this.#signingPrivateKey)),
      },
    }
    let response
    try {
      response = await this.#request(`${this.#apiOrigin}/api/enrollment-requests`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', accept: 'application/json' },
        body: JSON.stringify(body),
        expectedOrigin: this.#apiOrigin,
      })
    } catch (error) {
      this.#journal.write({ ...bound, state: 'create_uncertain', attemptedAt: this.#now() })
      if (error instanceof EnrollmentError) throw error
      fail('unavailable', 'approval request reply was lost; replay is safe', { recoverable: true })
    }
    const payload = await this.#json(response, 'approval request')
    if (response.status === 409) {
      const code = payload?.code === 'approval_expired' ? 'expired' : 'conflict'
      this.#journal.write({ ...bound, state: code === 'expired' ? 'expired' : 'create_failed' })
      fail(code, 'approval request conflict')
    }
    if (response.status === 429) {
      this.#journal.write({ ...bound, state: 'create_uncertain', attemptedAt: this.#now() })
      fail('rate_limited', 'approval request was rate limited', { recoverable: true, retryAfterMs: retryAfterMs(response) })
    }
    if (response.status === 400) {
      this.#journal.write({ ...bound, state: 'create_failed' })
      fail('forbidden', 'approval proof was refused')
    }
    if (response.status === 503) {
      this.#journal.write({ ...bound, state: 'create_uncertain', attemptedAt: this.#now() })
      fail('unavailable', 'enrollment is unavailable', { recoverable: true })
    }
    if (response.status !== 200 && response.status !== 201) {
      this.#journal.write({ ...bound, state: 'create_failed' })
      fail('unavailable', 'approval request failed')
    }
    const pollToken = typeof payload?.pollToken === 'string' ? payload.pollToken : ''
    if (typeof payload?.id !== 'string' || payload.id === '' || pollToken === '') {
      this.#journal.write({ ...bound, state: 'create_failed' })
      fail('protocol_version_incompatible', 'approval response is incomplete')
    }
    const service = payload.service ?? this.#metadata.service
    const environment = payload.environment ?? this.#metadata.environment
    if (service !== this.#metadata.service || environment !== this.#metadata.environment) {
      this.#journal.write({ ...bound, state: 'create_failed' })
      fail('forbidden', 'approval response environment mismatch')
    }
    const approveUrl = this.#approvalUrl(payload.approveUrl, payload.id)
    const expiresAt = parseTime(payload.expiresAt)
    if (expiresAt === null) fail('protocol_version_incompatible', 'approval expiry is invalid')
    const pollAfterMs = Number.isSafeInteger(payload.pollAfterMs) && payload.pollAfterMs >= 100 ? payload.pollAfterMs : this.#metadata.pollAfterMs
    const next = {
      ...bound,
      id: payload.id,
      approveUrl,
      expiresAt,
      pollAfterMs,
      fingerprint: typeof payload.fingerprint === 'string' ? payload.fingerprint : '',
      displayFingerprint: typeof payload.displayFingerprint === 'string' ? payload.displayFingerprint : '',
      state: payload.state === 'approved' ? 'approved' : 'awaiting_approval',
      requestedAt: this.#now(),
    }
    // Persist the rotated poll token and intent atomically-enough ordering:
    // the credential is the only way to recover the request, so write it first.
    await this.#store.set(CREDENTIAL_PURPOSES.pollCredential, pollToken)
    this.#journal.write(next)
    return next
  }

  /** Replay a persisted uncertain create without creating a new request. */
  async resume() {
    const intent = this.#journal.read()
    if (!intent) fail('not_found', 'no persisted approval intent')
    if (intent.state === 'create_uncertain' || intent.state === 'create_failed') {
      return this.createIntent()
    }
    return intent
  }

  /**
   * Poll serially until a terminal response. The caller's `isActive` fence
   * prevents a late response from enrolling a profile after cancel/switch.
   */
  async poll({ isActive = () => true, signal = null, deadlineMs = DEFAULT_DEADLINE_MS } = {}) {
    let intent = this.#journal.read()
    if (!intent) fail('not_found', 'no persisted approval intent')
    const deadline = Math.min(this.#now() + deadlineMs, intent.expiresAt ?? Infinity)
    let token = await this.#store.get(CREDENTIAL_PURPOSES.pollCredential)
    if (!token) {
      // A lost create reply is recoverable by replaying the same intent.
      if (intent.state === 'create_uncertain') {
        intent = await this.resume()
        token = await this.#store.get(CREDENTIAL_PURPOSES.pollCredential)
      }
      if (!token) fail('unauthorized', 'approval poll credential is missing', { recovery: 'create' })
    }
    let interval = Math.max(100, intent.pollAfterMs ?? POLL_DEFAULT_MS)
    let backoff = 0
    for (;;) {
      if (signal?.aborted || !isActive()) fail('cancelled', 'approval polling stopped')
      if (this.#now() >= deadline) {
        this.#journal.write({ ...intent, state: 'expired' })
        fail('expired', 'approval request expired')
      }
      let response
      try {
        response = await this.#request(`${this.#apiOrigin}/api/enrollment-requests/${encodeURIComponent(intent.id)}`, {
          method: 'GET',
          headers: { authorization: `Bearer ${token}`, accept: 'application/json' },
          expectedOrigin: this.#apiOrigin,
          signal,
        })
      } catch (error) {
        if (error instanceof EnrollmentError) throw error
        if (signal?.aborted || !isActive()) fail('cancelled', 'approval polling stopped')
        backoff = Math.min(Math.max(interval * 2, interval + 1000), MAX_BACKOFF_MS)
        await this.#sleep(backoff)
        interval = Math.min(interval * 2, MAX_BACKOFF_MS)
        continue
      }
      if (response.status === 200) {
        const payload = await this.#json(response, 'approval poll')
        // A response that arrives after cancel/switch/dispose must not write a
        // terminal intent that a later restart could resume.
        if (signal?.aborted || !isActive()) fail('cancelled', 'approval polling stopped')
        if (payload?.id !== undefined && payload.id !== intent.id) fail('forbidden', 'approval poll identity mismatch')
        if (payload?.hostId !== undefined && payload.hostId !== this.#hostId) fail('forbidden', 'approval poll Host mismatch')
        const state = payload?.state
        if (state === 'pending') {
          const next = retryAfterMs(response) ?? interval
          await this.#sleep(next)
          continue
        }
        if (state === 'denied') {
          this.#journal.write({ ...intent, state: 'denied' })
          await this.#clearPollCredential()
          fail('denied', 'approval was denied')
        }
        if (state === 'expired') {
          this.#journal.write({ ...intent, state: 'expired' })
          await this.#clearPollCredential()
          fail('expired', 'approval request expired')
        }
        if (state === 'approved') {
          const approved = this.validateApproval(payload, { intent })
          this.#journal.write({ ...intent, state: 'approved', approvedAt: this.#now(), operationId: approved.operationId })
          return approved
        }
        fail('protocol_version_incompatible', 'approval state is unknown')
      }
      if (response.status === 401 || response.status === 403) {
        fail('unauthorized', 'approval poll credential was refused')
      }
      if (response.status === 404) fail('not_found', 'approval request does not exist')
      if (response.status === 429) {
        const wait = retryAfterMs(response) ?? Math.min(interval * 2, MAX_BACKOFF_MS)
        interval = Math.min(interval * 2, MAX_BACKOFF_MS)
        await this.#sleep(wait)
        continue
      }
      backoff = Math.min(Math.max(interval * 2, 1000), MAX_BACKOFF_MS)
      interval = Math.min(interval * 2, MAX_BACKOFF_MS)
      await this.#sleep(backoff)
    }
  }

  /** Validate the signed authorization and assigned route before secrets move. */
  validateApproval(payload, { intent = null } = {}) {
    if (!payload || typeof payload !== 'object') fail('protocol_version_incompatible', 'approval payload is missing')
    const authorization = normalizeFieldNames(payload.authorization)
    const route = normalizeFieldNames(payload.route)
    if (!authorization || typeof authorization !== 'object') fail('forbidden', 'approval authorization is missing')
    if (!route || typeof route !== 'object') fail('forbidden', 'approval route is missing')
    if (authorization.authVersion && authorization.authVersion !== AUTHORIZATION_PREFIX) {
      fail('protocol_version_incompatible', 'authorization version is unsupported')
    }
    if (authorization.purpose !== AUTHORIZATION_PURPOSE) fail('forbidden', 'authorization purpose is not Host enrollment')
    if (this.#options.expectedIssuer && authorization.issuer !== this.#options.expectedIssuer) {
      fail('forbidden', 'authorization issuer is not trusted')
    }
    // The authorization is addressed to the regional Access service, not to
    // the account service: the approved route names that exact authority, and
    // the Host must see both agree before it sends any secret.
    const audience = route.accessOrigin
    if (authorization.audience !== audience) fail('forbidden', 'authorization audience is not the assigned service')
    if (authorization.hostId !== this.#hostId) fail('forbidden', 'authorization Host mismatch')
    const operationId = authorization.operationId
    if (typeof operationId !== 'string' || operationId === '') fail('forbidden', 'authorization operation id is missing')
    const enrollmentRevision = authorization.enrollmentRevision
    if (typeof enrollmentRevision !== 'string' || enrollmentRevision === '') {
      fail('forbidden', 'authorization enrollment revision is missing')
    }
    if (intent?.operationId && intent.operationId !== operationId) fail('conflict', 'approval operation changed')
    const issuedAt = Number(authorization.issuedAtUnixMs)
    const expiresAt = Number(authorization.expiresAtUnixMs)
    if (!Number.isSafeInteger(issuedAt) || !Number.isSafeInteger(expiresAt)) fail('forbidden', 'authorization time bounds are invalid')
    if (issuedAt > this.#now() + 5 * 60 * 1000) fail('forbidden', 'authorization was issued in the future')
    if (expiresAt <= this.#now()) fail('expired', 'authorization expired')
    const approvedKeys = Array.isArray(authorization.hostPublicKeys) ? authorization.hostPublicKeys : []
    const bindsKey = (target) => approvedKeys.some((candidate) => equalKey(candidate, target))
    if (!bindsKey(this.#signingPublicKey) || !bindsKey(this.#noisePublicKey)) {
      fail('forbidden', 'authorization does not bind this profile keys')
    }
    const nonce = canonicalNonce(authorization.nonce)
    const signature = decodeFlexibleBase64(authorization.signature)
    if (nonce === '' || !signature || signature.length !== 64) fail('forbidden', 'authorization proof is malformed')
    if (typeof this.#options.verifyAuthorization === 'function') {
      let valid = false
      try {
        valid = Boolean(this.#options.verifyAuthorization(authorization, canonicalAuthorizationInput(authorization)))
      } catch {
        valid = false
      }
      if (!valid) fail('forbidden', 'authorization signature is invalid')
    }
    const region = route.region
    if (typeof region !== 'string' || region === '') fail('forbidden', 'route region is missing')
    if (authorization.regionId !== undefined && authorization.regionId !== region) {
      fail('forbidden', 'authorization region does not match the assigned route')
    }
    const routeExpiry = parseTime(route.expiresAt)
    if (routeExpiry === null || routeExpiry <= this.#now()) fail('expired', 'assigned route expired')
    let parsedRoute
    try {
      parsedRoute = new URL(route.accessOrigin)
    } catch {
      fail('forbidden', 'assigned route origin is invalid')
    }
    const loopbackRoute = parsedRoute.hostname === '127.0.0.1' || parsedRoute.hostname === 'localhost' || parsedRoute.hostname === '[::1]'
    if (parsedRoute.protocol !== 'https:' && !(this.#options.allowLoopback && parsedRoute.protocol === 'http:' && loopbackRoute)) {
      fail('forbidden', 'assigned route is not HTTPS')
    }
    let accessOrigin
    try {
      accessOrigin = validateServiceOrigin(route.accessOrigin, { allowLoopback: this.#options.allowLoopback })
    } catch {
      fail('forbidden', 'assigned route origin is not canonical')
    }
    return {
      authorization,
      route: { region, accessOrigin, expiresAt: routeExpiry },
      operationId,
    }
  }

  /** Submit the proof-bound enrollment at Access, then persist the credential. */
  async enroll({ authorization, route, operationId }) {
    if (!authorization || !route || typeof operationId !== 'string' || operationId === '') {
      fail('invalid_argument', 'approved authorization and route are required')
    }
    const proof = enrollmentProofInput({ authorization, operationId, hostSigningPublicKey: this.#signingPublicKey })
    const body = {
      authorization: wireAuthorization(authorization),
      operationId,
      hostId: this.#hostId,
      hostSigningPublicKey: Buffer.from(this.#signingPublicKey).toString('base64'),
      hostNoisePublicKey: Buffer.from(this.#noisePublicKey).toString('base64'),
      proofSignature: Buffer.from(ed25519.sign(new TextEncoder().encode(proof), this.#signingPrivateKey)).toString('base64'),
    }
    const endpoint = `${route.accessOrigin}/v1/enrollments/hosts`
    let response
    try {
      response = await this.#request(endpoint, {
        method: 'POST',
        headers: { 'content-type': 'application/json', accept: 'application/json' },
        body: JSON.stringify(body),
        expectedOrigin: route.accessOrigin,
      })
    } catch (error) {
      if (error instanceof EnrollmentError) throw error
      const committed = await this.#readStatus({ accessOrigin: route.accessOrigin, operationId }).catch(() => null)
      this.#markUncertain({ operationId, authorizationId: authorization.authorizationId, phase: 'enrollment', committed: committed?.state ? true : null })
      fail('recovery_required', 'enrollment reply was lost; reconcile with Host proof', {
        committed: committed?.state ? true : null,
        recoverable: true,
      })
    }
    const payload = await this.#json(response, 'Host enrollment')
    if (response.status === 409) {
      const committed = await this.#readStatus({ accessOrigin: route.accessOrigin, operationId }).catch(() => null)
      this.#markUncertain({ operationId, authorizationId: authorization.authorizationId, phase: 'enrollment', committed: true })
      fail('conflict', 'enrollment operation conflicts with a recorded payload', { committed: committed?.state ? true : true })
    }
    if (response.status === 401 || response.status === 403) fail('forbidden', 'enrollment authorization was refused')
    if (response.status === 404) fail('not_found', 'enrollment endpoint was not found')
    if (response.status !== 200) fail('unavailable', 'enrollment failed')
    const status = normalizeFieldNames(payload?.status)
    if (!status || status.hostId !== this.#hostId) fail('forbidden', 'enrollment response Host mismatch')
    if (!equalKey(status.hostSigningPublicKey, this.#signingPublicKey) || !equalKey(status.hostNoisePublicKey, this.#noisePublicKey)) {
      fail('forbidden', 'enrollment response key mismatch')
    }
    const credential = normalizeFieldNames(payload?.credential)
    if (!credential) {
      this.#markUncertain({ operationId, authorizationId: authorization.authorizationId, phase: 'credential', committed: true })
      fail('recovery_required', 'enrollment committed but its credential was not delivered', { committed: true })
    }
    this.#validateCredential(credential)
    const record = {
      hostId: this.#hostId,
      mqttUrl: credential.mqttUrl,
      username: credential.username,
      password: credential.password,
      accessOrigin: route.accessOrigin,
      region: route.region,
      routeExpiresAt: route.expiresAt,
      enrollmentRevision: status.enrollmentRevision ?? authorization.enrollmentRevision,
      operationId,
      persistedAt: this.#now(),
    }
    try {
      await this.#store.setJson(CREDENTIAL_PURPOSES.transportCredential, record)
    } catch {
      this.#markUncertain({ operationId, authorizationId: authorization.authorizationId, phase: 'credential', committed: true })
      throw new EnrollmentError('recovery_required', 'credential could not be persisted; reconcile before reconnecting', { committed: true })
    }
    this.#journal.write({
      ...(this.#journal.read() ?? { schema: INTENT_SCHEMA, intentId: '' }),
      state: 'enrolled',
      operationId,
      enrollmentRevision: record.enrollmentRevision,
      enrolledAt: this.#now(),
    })
    await this.#clearPollCredential()
    return { status, credential: record }
  }

  /** Proof-scoped status read. Never returns a credential. */
  async readStatus({ accessOrigin, operationId = '', nonce = undefined } = {}) {
    return this.#readStatus({ accessOrigin, operationId, nonce })
  }

  async #readStatus({ accessOrigin, operationId = '', nonce }) {
    if (typeof accessOrigin !== 'string' || accessOrigin === '') fail('invalid_argument', 'Access origin is required')
    const origin = validateServiceOrigin(accessOrigin, { allowLoopback: this.#options.allowLoopback })
    const issuedAt = this.#now()
    const proof = createHostProof({
      signingPrivateKey: this.#signingPrivateKey,
      hostId: this.#hostId,
      purpose: 'enrollment_status',
      subjectId: operationId,
      issuedAt,
      nonce,
    })
    let response
    try {
      response = await this.#request(`${origin}/v1/hosts/${encodeURIComponent(this.#hostId)}/enrollment`, {
        method: 'GET',
        headers: { accept: 'application/json', 'X-Whalerein-Host-Proof': proof },
        expectedOrigin: origin,
      })
    } catch (error) {
      if (error instanceof EnrollmentError) throw error
      fail('unavailable', 'enrollment status is unavailable')
    }
    const payload = await this.#json(response, 'enrollment status')
    if (response.status === 401 || response.status === 403) fail('unauthorized', 'Host status proof was refused')
    if (response.status === 404) fail('not_found', 'enrollment was not found')
    if (response.status !== 200) fail('unavailable', 'enrollment status is unavailable')
    const status = normalizeFieldNames(payload?.status ?? payload)
    if (!status || status.hostId !== this.#hostId) fail('forbidden', 'enrollment status Host mismatch')
    if (status.hostSigningPublicKey !== undefined && !equalKey(status.hostSigningPublicKey, this.#signingPublicKey)) {
      fail('forbidden', 'enrollment status key mismatch')
    }
    return {
      state: status.state ?? '',
      revision: status.enrollmentRevision ?? '',
      operationId: status.operationId ?? operationId,
      route: payload?.route ?? null,
    }
  }

  /**
   * One-time credential replacement after a lost reply/credential write.
   * Requires a fresh Cloud authorization with the recovery purpose.
   */
  async replaceCredential({ accessOrigin, authorization, nonce = undefined }) {
    if (!authorization || typeof authorization !== 'object') fail('invalid_argument', 'recovery authorization is required')
    const expectedAudience = this.#options.expectedAudience || ''
    if (authorization.purpose !== RECOVERY_PURPOSE) fail('forbidden', 'authorization purpose is not credential recovery')
    if (authorization.hostId !== this.#hostId) fail('forbidden', 'recovery authorization Host mismatch')
    if (this.#options.expectedIssuer && authorization.issuer !== this.#options.expectedIssuer) fail('forbidden', 'recovery authorization issuer is not trusted')
    const audience = expectedAudience || accessOrigin
    if (authorization.audience !== audience) fail('forbidden', 'recovery authorization audience is not this service')
    if (Number(authorization.expiresAtUnixMs) <= this.#now()) fail('expired', 'recovery authorization expired')
    const recoveryKeys = Array.isArray(authorization.hostPublicKeys) ? authorization.hostPublicKeys : []
    if (recoveryKeys.length > 0) {
      const bindsKey = (target) => recoveryKeys.some((candidate) => equalKey(candidate, target))
      if (!bindsKey(this.#signingPublicKey) || !bindsKey(this.#noisePublicKey)) {
        fail('forbidden', 'recovery authorization does not bind this profile keys')
      }
    }
    const origin = validateServiceOrigin(accessOrigin, { allowLoopback: this.#options.allowLoopback })
    const authorizationId = authorization.authorizationId
    if (typeof authorizationId !== 'string' || authorizationId === '') fail('forbidden', 'recovery authorization id is missing')
    const proof = createHostProof({
      signingPrivateKey: this.#signingPrivateKey,
      hostId: this.#hostId,
      purpose: 'credential_replacement',
      subjectId: authorizationId,
      issuedAt: this.#now(),
      nonce,
    })
    let response
    try {
      response = await this.#request(`${origin}/v1/enrollments/hosts/${encodeURIComponent(this.#hostId)}/credential`, {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          accept: 'application/json',
          'X-Whalerein-Host-Proof': proof,
        },
        body: JSON.stringify({ authorization: wireAuthorization(authorization) }),
        expectedOrigin: origin,
      })
    } catch {
      fail('unavailable', 'credential replacement reply was lost')
    }
    const payload = await this.#json(response, 'credential replacement')
    if (response.status === 401 || response.status === 403) fail('forbidden', 'credential replacement was refused')
    if (response.status === 409) fail('conflict', 'credential replacement conflicts with a previous operation')
    if (response.status === 410 || response.status === 404) fail('not_found', 'credential replacement is unavailable')
    if (response.status !== 200) fail('unavailable', 'credential replacement failed')
    const credential = normalizeFieldNames(payload?.credential)
    if (!credential) fail('forbidden', 'credential replacement returned no credential')
    this.#validateCredential(credential)
    const record = {
      hostId: this.#hostId,
      mqttUrl: credential.mqttUrl,
      username: credential.username,
      password: credential.password,
      accessOrigin: origin,
      region: payload.region ?? '',
      routeExpiresAt: null,
      enrollmentRevision: payload.revision ?? '',
      operationId: payload.operationId ?? '',
      persistedAt: this.#now(),
    }
    await this.#store.setJson(CREDENTIAL_PURPOSES.transportCredential, record)
    return record
  }

  #validateCredential(credential) {
    if (credential.hostId !== undefined && credential.hostId !== this.#hostId) fail('forbidden', 'credential Host mismatch')
    if (credential.username !== `host:${this.#hostId}`) fail('forbidden', 'credential username is not the Host principal')
    if (typeof credential.password !== 'string' || credential.password === '') fail('forbidden', 'credential is missing')
    const url = credential.mqttUrl
    if (typeof url !== 'string') fail('forbidden', 'credential MQTT URL is missing')
    let parsed
    try {
      parsed = new URL(url)
    } catch {
      fail('forbidden', 'credential MQTT URL is invalid')
    }
    const secure = parsed.protocol === 'mqtts:'
    const loopback = parsed.hostname === '127.0.0.1' || parsed.hostname === 'localhost' || parsed.hostname === '[::1]'
    if (!secure && !(this.#options.allowLoopback && parsed.protocol === 'mqtt:' && loopback)) {
      fail('forbidden', 'credential MQTT URL must use TLS')
    }
    if (parsed.username || parsed.password || parsed.search || parsed.hash) fail('forbidden', 'credential MQTT URL is not canonical')
  }

  #approvalUrl(value, id) {
    const expected = `${this.#metadata.approvalOrigin}${this.#metadata.approvalPath.replace('{id}', encodeURIComponent(id))}`
    let url
    try {
      url = new URL(typeof value === 'string' && value !== '' ? value : expected)
    } catch {
      fail('protocol_version_incompatible', 'approval URL is invalid')
    }
    if (url.origin !== this.#metadata.approvalOrigin) fail('forbidden', 'approval URL origin is not approved')
    if (url.protocol !== 'https:' && !(this.#options.allowLoopback && originOf(url.origin)?.startsWith('http://'))) {
      fail('forbidden', 'approval URL must use HTTPS')
    }
    url.searchParams.set('locale', this.#options.locale)
    return url.toString()
  }

  #markUncertain({ operationId, authorizationId, phase, committed }) {
    let intent = null
    try {
      intent = this.#journal.read()
    } catch {
      intent = null
    }
    if (!intent) return
    try {
      this.#journal.write({
        ...intent,
        state: 'uncertain',
        operationId,
        authorizationId,
        uncertainPhase: phase,
        committed: committed ?? null,
        uncertainAt: this.#now(),
      })
    } catch {
      // The operation remains visibly unresolved through the original error.
    }
  }

  async #clearPollCredential() {
    try {
      if (typeof this.#store.remove === 'function') await this.#store.remove(CREDENTIAL_PURPOSES.pollCredential)
      else await this.#store.set(CREDENTIAL_PURPOSES.pollCredential, 'removed')
    } catch {
      // A terminal intent no longer polls; leaving the token is local-only.
    }
  }

  async #request(url, { method, headers, body, expectedOrigin, signal = null }) {
    let target
    try {
      target = new URL(url)
    } catch {
      fail('invalid_argument', 'request URL is invalid')
    }
    if (expectedOrigin && target.origin !== expectedOrigin) fail('forbidden', 'request target origin mismatch')
    const controller = new AbortController()
    const timer = setTimeout(() => controller.abort(), this.#options.timeoutMs)
    timer.unref?.()
    const abort = () => controller.abort()
    signal?.addEventListener?.('abort', abort, { once: true })
    let response
    try {
      response = await this.#fetch(url, {
        method,
        headers,
        ...(body === undefined ? {} : { body }),
        redirect: 'error',
        signal: controller.signal,
      })
    } finally {
      clearTimeout(timer)
      signal?.removeEventListener?.('abort', abort)
    }
    if (!response || !Number.isSafeInteger(response.status)) fail('unavailable', 'HTTP response is invalid')
    if (response.redirected) fail('forbidden', 'HTTP response redirected')
    if (response.url && originOf(response.url) !== expectedOrigin) fail('forbidden', 'HTTP response origin mismatch')
    return response
  }

  async #json(response, label) {
    try {
      const value = await response.json()
      if (!value || typeof value !== 'object') throw new Error('not an object')
      return value
    } catch {
      fail('protocol_version_incompatible', `${label} response is not JSON`)
    }
  }
}

function retryAfterMs(response) {
  const raw = response?.headers?.get?.('retry-after')
  if (typeof raw !== 'string' || raw === '') return null
  const seconds = Number(raw)
  if (Number.isFinite(seconds) && seconds >= 0) return Math.min(Math.round(seconds * 1000), MAX_BACKOFF_MS)
  const date = Date.parse(raw)
  if (!Number.isFinite(date)) return null
  return Math.min(Math.max(0, date - Date.now()), MAX_BACKOFF_MS)
}

function equalKey(value, expected) {
  if (typeof value !== 'string' || value === '') return false
  const decoded = decodeFlexibleBase64(value)
  return decoded !== null && Buffer.from(decoded).equals(Buffer.from(expected))
}

export { OFFICIAL_SERVICES, SERVICE_PROTOCOL_VERSION, REQUIRED_CAPABILITIES }
