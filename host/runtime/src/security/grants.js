// Host-owned device trust.
//
// The Host is the durable authority for device grants. This module owns the
// grant rows, the pending enrollment decisions and the recovery verifier /
// challenge registrations. It never speaks a wire protocol and never decides
// a pairing transcript; callers must authenticate the candidate first and then
// commit exactly that candidate here.
//
// Invariants enforced inside the store's IMMEDIATE SQLite transaction:
// - one ACTIVE grant per (Host key fingerprint, device public key);
// - grantable rights are a subset of use rights on creation and every edit;
// - only an owner approver sets the grantable ceiling or add-device;
// - a sponsor can issue only within its CURRENT grantable ceiling and needs
//   add-device authority; the sponsored child starts empty with no add-device;
// - reducing use rights intersects the ceiling in the same transaction and
//   increasing them never regrows it;
// - an exact retry returns the original decision, a competing or changed
//   attempt conflicts without touching the active grant;
// - revocation fences outstanding pre-revocation candidates and never cascades
//   to independent child grants;
// - recovery commits only while the proven verifier revision is still active,
//   the selected rights fit its current scope and the one-time challenge is
//   live, and consumes the challenge in the same transaction as the grant.

import { randomBytes } from 'node:crypto'
import { PluginError } from '../errors.js'
import { bytesEqual, sha256Bytes } from './digest.js'

/** The three use rights a Host grant can carry. */
export const USE_RIGHTS = Object.freeze(['read', 'write', 'approve'])

const RIGHT_SET = new Set(USE_RIGHTS)
const MAX_PENDING_CANDIDATES = 256
const MAX_ID_LENGTH = 256
const MAX_LABEL_LENGTH = 128

/**
 * Wire operation (RequestPayload field) to the current Host right it needs.
 * `null` means the operation only requires an active grant: capability
 * discovery never authorizes an execution. Any operation absent from this map
 * is unclassified and denied by default.
 */
const OPERATION_RIGHTS = Object.freeze({
  // Read scope.
  list_sessions: 'read',
  get_session: 'read',
  get_command_result: 'read',
  list_approvals: 'read',
  read_history: 'read',
  watch_session: 'read',
  unwatch_session: 'read',
  read_message_reply: 'read',
  list_models: 'read',
  read_session_model: 'read',
  wait_session: 'read',
  cancel_session_wait: 'read',
  read_wait_state: 'read',
  read_pending_queue: 'read',
  list_questions: 'read',
  get_question: 'read',
  open_attachment: 'read',
  pull_attachment: 'read',
  close_attachment: 'read',
  // Mutating scope.
  send_message: 'write',
  interrupt: 'write',
  select_session_model: 'write',
  create_session: 'write',
  update_pending_queue: 'write',
  answer_question: 'write',
  cancel_question_answer: 'write',
  fork_session: 'write',
  // Approval scope: review detail and resolution require the approval right.
  get_approval: 'approve',
  resolve_approval: 'approve',
  // Discovery only needs a live grant.
  ping: null,
  check_versions: null,
  get_host_capabilities: null,
})

const OPERATION_CLASSIFICATION = new Map(
  Object.entries(OPERATION_RIGHTS).map(([operation, right]) => [operation, Object.freeze({ operation, right, classified: true })]),
)

/**
 * Classify one wire operation against the Host operation map.
 * @returns {{operation: string, right: string|null, classified: boolean}}
 */
export function classifyOperation(operation) {
  const entry = typeof operation === 'string' ? OPERATION_CLASSIFICATION.get(operation) : undefined
  if (entry) return entry
  return Object.freeze({
    operation: typeof operation === 'string' ? operation : '',
    right: null,
    classified: false,
  })
}

/**
 * Normalize a rights selection into the canonical order. Unknown, duplicate
 * and non-string entries are invalid arguments; the Host never guesses a
 * missing right into existence.
 */
export function normalizeRights(value, { name = 'rights' } = {}) {
  if (!Array.isArray(value)) throw new PluginError('invalid_argument', `${name} must be an array`)
  const selected = new Set()
  for (const right of value) {
    if (typeof right !== 'string' || !RIGHT_SET.has(right)) throw new PluginError('invalid_argument', `${name} contains an unknown right`)
    if (selected.has(right)) throw new PluginError('invalid_argument', `${name} contains a duplicate right`)
    selected.add(right)
  }
  return USE_RIGHTS.filter((right) => selected.has(right))
}

/** True when every right of `subset` is present in `superset`. */
export function rightsSubset(subset, superset) {
  return subset.every((right) => superset.includes(right))
}

function intersectRights(left, right) {
  return left.filter((value) => right.includes(value))
}

function sameRights(left, right) {
  return left.length === right.length && left.every((value, index) => value === right[index])
}

function encodeRights(rights) {
  return JSON.stringify(normalizeRights(rights))
}

function decodeRights(text) {
  let parsed
  try {
    parsed = JSON.parse(text)
  } catch {
    throw new PluginError('internal', 'stored rights are unreadable')
  }
  return normalizeRights(parsed)
}

function boundedString(value, name, maxLength = MAX_ID_LENGTH) {
  if (typeof value !== 'string' || value === '' || value.length > maxLength) {
    throw new PluginError('invalid_argument', `${name} invalid`)
  }
  return value
}

function keyBytes(value, name = 'device public key') {
  if (!(value instanceof Uint8Array) || value.length !== 32) {
    throw new PluginError('invalid_argument', `${name} must be 32 bytes`)
  }
  return Buffer.from(value)
}

function safeTimestamp(value, name) {
  if (!Number.isSafeInteger(value) || value < 0) throw new PluginError('invalid_argument', `${name} invalid`)
  return value
}

function safeRevision(value, name) {
  if (!Number.isSafeInteger(value) || value < 1) throw new PluginError('invalid_argument', `${name} invalid`)
  return value
}

function randomToken() {
  return randomBytes(32).toString('base64url')
}

/** A high-entropy grant identity, independent of any label or wire route. */
export function generateGrantId() {
  return `grant_${randomToken()}`
}

/** A high-entropy invitation/challenge identity. */
export function generateCandidateId() {
  return `candidate_${randomToken()}`
}

function requestDigest({ method, hostKeyFingerprint, candidatePublicKey, useRights, grantableRights, addDevice, sponsorGrantId, recoveryVerifierRevision }) {
  const canonical = [
    'whalerein/host-grant-request-v1',
    method,
    hostKeyFingerprint,
    Buffer.from(candidatePublicKey).toString('hex'),
    encodeRights(useRights),
    encodeRights(grantableRights),
    addDevice ? '1' : '0',
    sponsorGrantId ?? '',
    recoveryVerifierRevision === null || recoveryVerifierRevision === undefined ? '' : String(recoveryVerifierRevision),
  ].join('\n')
  return Buffer.from(sha256Bytes(Buffer.from(canonical, 'utf8')))
}

function rowGrant(row) {
  if (!row) return null
  return {
    grantId: row.grant_id,
    hostKeyFingerprint: row.host_key_fingerprint,
    devicePublicKey: Buffer.from(row.device_public_key),
    deviceId: row.device_id,
    deviceLabel: row.device_label ?? '',
    useRights: decodeRights(row.use_rights),
    grantableRights: decodeRights(row.grantable_rights),
    addDevice: row.add_device === 1,
    approverKind: row.approver_kind,
    sponsorGrantId: row.sponsor_grant_id ?? null,
    recoveryVerifierRevision: row.recovery_verifier_revision ?? null,
    revision: row.revision,
    state: row.state,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
    revokedAt: row.revoked_at ?? null,
  }
}

function rowCandidate(row) {
  if (!row) return null
  return {
    candidateId: row.candidate_id,
    method: row.method,
    hostKeyFingerprint: row.host_key_fingerprint,
    candidatePublicKey: Buffer.from(row.candidate_public_key),
    deviceId: row.device_id,
    deviceLabel: row.device_label ?? '',
    useRights: decodeRights(row.use_rights),
    grantableRights: decodeRights(row.grantable_rights),
    addDevice: row.add_device === 1,
    sponsorGrantId: row.sponsor_grant_id ?? null,
    sponsorRevision: row.sponsor_revision ?? null,
    recoveryVerifierRevision: row.recovery_verifier_revision ?? null,
    requestDigest: Buffer.from(row.request_digest),
    state: row.state,
    grantId: row.grant_id ?? null,
    decidedAt: row.decided_at ?? null,
    expiresAt: row.expires_at,
    createdAt: row.created_at,
  }
}

function rowVerifier(row) {
  if (!row) return null
  return {
    revision: row.revision,
    hostKeyFingerprint: row.host_key_fingerprint,
    publicKey: Buffer.from(row.public_key),
    allowedRights: decodeRights(row.allowed_rights),
    active: row.active === 1,
    createdAt: row.created_at,
    retiredAt: row.retired_at ?? null,
  }
}

function rowChallenge(row) {
  if (!row) return null
  return {
    challengeId: row.challenge_id,
    hostKeyFingerprint: row.host_key_fingerprint,
    verifierRevision: row.verifier_revision,
    nonce: row.nonce,
    candidatePublicKey: Buffer.from(row.candidate_public_key),
    useRights: decodeRights(row.use_rights),
    expiresAt: row.expires_at,
    consumed: row.consumed === 1,
    consumedAt: row.consumed_at ?? null,
    grantId: row.grant_id ?? null,
    createdAt: row.created_at,
  }
}

function deny(reason, { right = null, grantId = null, revision = null } = {}) {
  return { allowed: false, reason, right, grantId, revision }
}

export class DeviceTrustStore {
  #db
  #transaction
  #now

  /**
   * @param {object} options
   * @param {import('../state/store.js').StateStore} options.store open Host store.
   * @param {() => number} [options.now] clock injection (wall clock ms).
   */
  constructor({ store, now = Date.now } = {}) {
    if (!store || typeof store.trustStore !== 'function') throw new PluginError('internal', 'state store unavailable')
    const handle = store.trustStore()
    this.#db = handle.db
    this.#transaction = handle.transaction
    this.#now = now
  }

  // --- grant reads --------------------------------------------------------

  getGrant(grantId) {
    if (typeof grantId !== 'string' || grantId === '') return null
    return rowGrant(this.#db.prepare('SELECT * FROM device_grants WHERE grant_id = ?').get(grantId))
  }

  isGrantActive(grantId) {
    return this.getGrant(grantId)?.state === 'active'
  }

  findActiveGrant({ hostKeyFingerprint, devicePublicKey }) {
    const fingerprint = boundedString(hostKeyFingerprint, 'host key fingerprint')
    const key = keyBytes(devicePublicKey)
    return rowGrant(this.#db.prepare(
      `SELECT * FROM device_grants
        WHERE host_key_fingerprint = ? AND device_public_key = ? AND state = 'active'`,
    ).get(fingerprint, key))
  }

  listGrants({ hostKeyFingerprint = null, state = null, limit = 256 } = {}) {
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 1024) throw new PluginError('invalid_argument', 'limit invalid')
    const clauses = []
    const params = []
    if (hostKeyFingerprint !== null) {
      clauses.push('host_key_fingerprint = ?')
      params.push(boundedString(hostKeyFingerprint, 'host key fingerprint'))
    }
    if (state !== null) {
      if (state !== 'active' && state !== 'revoked') throw new PluginError('invalid_argument', 'state invalid')
      clauses.push('state = ?')
      params.push(state)
    }
    const where = clauses.length > 0 ? `WHERE ${clauses.join(' AND ')}` : ''
    params.push(limit)
    return this.#db.prepare(`SELECT * FROM device_grants ${where} ORDER BY created_at ASC, rowid ASC LIMIT ?`)
      .all(...params).map(rowGrant)
  }

  /** Direct approvals by a sponsor, all states, for the loss-review surface. */
  listChildren(sponsorGrantId) {
    const id = boundedString(sponsorGrantId, 'sponsor grant id')
    return this.#db.prepare('SELECT * FROM device_grants WHERE sponsor_grant_id = ? ORDER BY created_at ASC, rowid ASC')
      .all(id).map(rowGrant)
  }

  // --- live operation authorization --------------------------------------

  /**
   * Answer whether the current grant allows one wire operation. Unknown
   * grants, revoked grants, unclassified operations and missing rights are
   * denied. Discovery operations require only an active grant.
   */
  authorizeOperation({ grantId, operation } = {}) {
    const classified = classifyOperation(operation)
    if (!classified.classified) return deny('unknown_operation', { grantId: typeof grantId === 'string' ? grantId : null })
    return this.#authorize(grantId, classified.right)
  }

  /** Approval right helper for detail release and resolution. */
  authorizeApproval(grantId) {
    return this.#authorize(grantId, 'approve')
  }

  /** Explicit right helper; an unknown right is denied rather than ignored. */
  authorizeRight({ grantId, right } = {}) {
    if (typeof right !== 'string' || !RIGHT_SET.has(right)) return deny('unknown_right', { grantId: typeof grantId === 'string' ? grantId : null })
    return this.#authorize(grantId, right)
  }

  #authorize(grantId, right) {
    const grant = this.getGrant(grantId)
    if (!grant) return deny('unknown_grant', { grantId: typeof grantId === 'string' ? grantId : null })
    if (grant.state !== 'active') return deny('grant_revoked', { grantId: grant.grantId, revision: grant.revision })
    if (right === null) return { allowed: true, reason: 'ok', right: null, grantId: grant.grantId, revision: grant.revision }
    if (!grant.useRights.includes(right)) return deny('missing_right', { right, grantId: grant.grantId, revision: grant.revision })
    return { allowed: true, reason: 'ok', right, grantId: grant.grantId, revision: grant.revision }
  }

  // --- pending candidates -------------------------------------------------

  /**
   * Stage a local or sponsor invitation candidate. The request digest binds
   * the exact applicant key, requested rights and approver provenance so a
   * later attempt cannot merge or replace them. Recovery candidates are
   * created by {@link issueRecoveryChallenge} because they also need the
   * one-time challenge row.
   */
  stageCandidate({
    candidateId,
    method,
    hostKeyFingerprint,
    candidatePublicKey,
    deviceId,
    deviceLabel = '',
    useRights,
    grantableRights = [],
    addDevice = false,
    sponsorGrantId = null,
    sponsorRevision = null,
    expiresAt,
    now = this.#now(),
  }) {
    const id = boundedString(candidateId, 'candidate id')
    if (method !== 'local' && method !== 'sponsor') throw new PluginError('invalid_argument', 'candidate method invalid')
    const fingerprint = boundedString(hostKeyFingerprint, 'host key fingerprint')
    const key = keyBytes(candidatePublicKey, 'candidate public key')
    const device = boundedString(deviceId, 'device id', MAX_LABEL_LENGTH)
    if (typeof deviceLabel !== 'string' || deviceLabel.length > MAX_LABEL_LENGTH) {
      throw new PluginError('invalid_argument', 'device label invalid')
    }
    const label = deviceLabel
    const use = normalizeRights(useRights, { name: 'use rights' })
    const ceiling = normalizeRights(grantableRights, { name: 'grantable rights' })
    if (typeof addDevice !== 'boolean') throw new PluginError('invalid_argument', 'add-device invalid')
    if (method === 'sponsor') {
      boundedString(sponsorGrantId, 'sponsor grant id')
      if (sponsorRevision !== null) safeRevision(sponsorRevision, 'sponsor revision')
    } else if (sponsorGrantId !== null || sponsorRevision !== null) {
      throw new PluginError('invalid_argument', 'local candidate cannot carry sponsor provenance')
    }
    // The ceiling is always bounded by use rights, before persistence.
    if (!rightsSubset(ceiling, use)) throw new PluginError('forbidden', 'grantable rights exceed use rights')
    const expiry = safeTimestamp(expiresAt, 'candidate expiry')
    const at = safeTimestamp(now, 'clock')
    if (expiry <= at) throw new PluginError('expired', 'candidate already expired')
    const digest = requestDigest({
      method,
      hostKeyFingerprint: fingerprint,
      candidatePublicKey: key,
      useRights: use,
      grantableRights: ceiling,
      addDevice,
      sponsorGrantId: method === 'sponsor' ? sponsorGrantId : null,
      recoveryVerifierRevision: null,
    })
    return this.#transaction(() => {
      const existing = this.#candidateRow(id)
      if (existing) {
        if (!bytesEqual(existing.candidatePublicKey, key) || !bytesEqual(existing.requestDigest, digest)) {
          throw new PluginError('conflict', 'candidate already staged for another request')
        }
        return { candidate: existing, created: false }
      }
      const pending = this.#db.prepare("SELECT COUNT(*) AS n FROM enrollment_candidates WHERE state = 'pending'").get().n
      if (pending >= MAX_PENDING_CANDIDATES) throw new PluginError('rate_limited', 'pending candidate budget exhausted')
      this.#db.prepare(`
        INSERT INTO enrollment_candidates (
          candidate_id, method, host_key_fingerprint, candidate_public_key, device_id, device_label,
          use_rights, grantable_rights, add_device, sponsor_grant_id, sponsor_revision,
          recovery_verifier_revision, request_digest, state, grant_id, decided_at, expires_at, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, 'pending', NULL, NULL, ?, ?)
      `).run(
        id, method, fingerprint, key, device, label, encodeRights(use), encodeRights(ceiling),
        addDevice ? 1 : 0, method === 'sponsor' ? sponsorGrantId : null,
        method === 'sponsor' ? sponsorRevision : null,
        digest, expiry, at,
      )
      return { candidate: this.#candidateRow(id), created: true }
    })
  }

  getCandidate(candidateId) {
    if (typeof candidateId !== 'string' || candidateId === '') return null
    return this.#candidateRow(candidateId)
  }

  /** Record an explicit Host-local or sponsor rejection. */
  rejectCandidate({ candidateId, now = this.#now() }) {
    const at = safeTimestamp(now, 'clock')
    return this.#transaction(() => {
      const candidate = this.#candidateRow(boundedString(candidateId, 'candidate id'))
      if (!candidate) throw new PluginError('not_found', 'candidate unknown')
      if (candidate.state === 'rejected') return { changed: false, candidate }
      if (candidate.state !== 'pending') throw new PluginError('conflict', 'candidate already decided')
      this.#db.prepare("UPDATE enrollment_candidates SET state = 'rejected', decided_at = ? WHERE candidate_id = ?")
        .run(at, candidate.candidateId)
      return { changed: true, candidate: this.#candidateRow(candidate.candidateId) }
    })
  }

  // --- the single grant writer -------------------------------------------

  /**
   * Commit one authenticated local or sponsored enrollment decision together
   * with its independent grant. The transaction rechecks the exact candidate,
   * sponsor authority/revision, expiry, active-grant uniqueness and rights
   * bounds. The first winner commits; every other attempt conflicts without
   * changing the active grant.
   */
  commitEnrollment({
    candidateId,
    candidatePublicKey,
    useRights,
    grantableRights = [],
    addDevice = false,
    approver,
    now = this.#now(),
  }) {
    const at = safeTimestamp(now, 'clock')
    const key = keyBytes(candidatePublicKey, 'candidate public key')
    const use = normalizeRights(useRights, { name: 'use rights' })
    const ceiling = normalizeRights(grantableRights, { name: 'grantable rights' })
    if (typeof addDevice !== 'boolean') throw new PluginError('invalid_argument', 'add-device invalid')
    if (!approver || typeof approver !== 'object') throw new PluginError('invalid_argument', 'approver missing')
    const approverKind = approver.kind
    if (approverKind !== 'owner' && approverKind !== 'sponsor') throw new PluginError('invalid_argument', 'approver kind invalid')
    if (approverKind === 'sponsor') boundedString(approver.grantId, 'approver grant id')
    if (approver.revision !== undefined && approver.revision !== null) safeRevision(approver.revision, 'approver revision')

    return this.#transaction(() => {
      const candidate = this.#candidateRow(boundedString(candidateId, 'candidate id'))
      if (!candidate) throw new PluginError('not_found', 'candidate unknown')
      if (candidate.method === 'recovery') throw new PluginError('invalid_argument', 'recovery candidates need commitRecovery')
      if (!bytesEqual(candidate.candidatePublicKey, key)) throw new PluginError('conflict', 'candidate key changed')
      if (!rightsSubset(ceiling, use)) throw new PluginError('forbidden', 'grantable rights exceed use rights')

      const digest = requestDigest({
        method: candidate.method,
        hostKeyFingerprint: candidate.hostKeyFingerprint,
        candidatePublicKey: candidate.candidatePublicKey,
        useRights: use,
        grantableRights: ceiling,
        addDevice,
        sponsorGrantId: candidate.sponsorGrantId,
        recoveryVerifierRevision: null,
      })

      if (candidate.state !== 'pending') {
        if (!bytesEqual(candidate.requestDigest, digest)) throw new PluginError('conflict', 'candidate already decided for another request')
        return this.#decisionResult(candidate)
      }
      if (!bytesEqual(candidate.requestDigest, digest)) throw new PluginError('conflict', 'candidate request changed')
      if (candidate.expiresAt <= at) throw new PluginError('expired', 'enrollment candidate expired')

      // Sponsored grants are bounded by the sponsor's CURRENT authority. The
      // approval is also bound to the revision it observed.
      if (candidate.method === 'sponsor') {
        if (approverKind !== 'sponsor' || approver.grantId !== candidate.sponsorGrantId) {
          throw new PluginError('forbidden', 'sponsor provenance mismatch')
        }
        const sponsor = this.getGrant(approver.grantId)
        if (!sponsor || sponsor.state !== 'active') throw new PluginError('forbidden', 'sponsor grant is not active')
        // The approval is bound to the revision it observed. A sponsor commit with
  // no observed revision is refused rather than silently trusting a moving
  // authority.
  const observedRevision = approver.revision ?? candidate.sponsorRevision
  if (observedRevision === null || observedRevision === undefined) {
    throw new PluginError('invalid_argument', 'sponsor revision missing')
  }
  if (observedRevision !== sponsor.revision) {
    throw new PluginError('conflict', 'sponsor authority changed')
  }
        if (!sponsor.addDevice) throw new PluginError('forbidden', 'sponsor lacks add-device authority')
        if (!rightsSubset(use, sponsor.grantableRights)) throw new PluginError('forbidden', 'requested rights exceed sponsor ceiling')
        // A sponsored child starts with an empty ceiling and no add-device.
        if (ceiling.length !== 0 || addDevice) throw new PluginError('forbidden', 'sponsor cannot delegate a ceiling or add-device')
      } else if (approverKind !== 'owner') {
        throw new PluginError('forbidden', 'local enrollment requires the owner')
      }

      const active = this.findActiveGrant({ hostKeyFingerprint: candidate.hostKeyFingerprint, devicePublicKey: candidate.candidatePublicKey })
      if (active) throw new PluginError('conflict', 'device key already has an active grant')

      const grantId = generateGrantId()
      this.#insertGrant({
        grantId,
        candidate,
        useRights: use,
        grantableRights: ceiling,
        addDevice,
        approverKind,
        sponsorGrantId: approverKind === 'sponsor' ? approver.grantId : null,
        recoveryVerifierRevision: null,
        now: at,
      })
      this.#db.prepare("UPDATE enrollment_candidates SET state = 'approved', grant_id = ?, decided_at = ? WHERE candidate_id = ?")
        .run(grantId, at, candidate.candidateId)
      return this.#decisionResult(this.#candidateRow(candidate.candidateId), { retried: false })
    })
  }

  // --- edits and revocation ----------------------------------------------

  /**
   * Edit one active grant. Only the owner may set the ceiling or add-device.
   * A ceiling can never grow past use rights, use-right reduction intersects
   * the ceiling in the same transaction and a later increase never regrows it.
   */
  editGrant({
    grantId,
    actor,
    useRights,
    grantableRights,
    addDevice,
    expectedRevision,
    now = this.#now(),
  }) {
    const at = safeTimestamp(now, 'clock')
    if (!actor || typeof actor !== 'object') throw new PluginError('invalid_argument', 'actor missing')
    if (actor.kind !== 'owner' && actor.kind !== 'sponsor') throw new PluginError('invalid_argument', 'actor kind invalid')
    if (actor.kind === 'sponsor') boundedString(actor.grantId, 'actor grant id')
    if (expectedRevision !== undefined) safeRevision(expectedRevision, 'expected revision')
    if (addDevice !== undefined && typeof addDevice !== 'boolean') throw new PluginError('invalid_argument', 'add-device invalid')

    return this.#transaction(() => {
      const grant = this.getGrant(boundedString(grantId, 'grant id'))
      if (!grant) throw new PluginError('not_found', 'grant unknown')
      if (grant.state !== 'active') throw new PluginError('forbidden', 'grant revoked')
      if (expectedRevision !== undefined && expectedRevision !== grant.revision) throw new PluginError('conflict', 'grant revision changed')

      let nextUse = grant.useRights
      let nextCeiling = grant.grantableRights
      let nextAddDevice = grant.addDevice

      if (actor.kind === 'owner') {
        if (useRights !== undefined) nextUse = normalizeRights(useRights, { name: 'use rights' })
        if (grantableRights !== undefined) {
          nextCeiling = normalizeRights(grantableRights, { name: 'grantable rights' })
          if (!rightsSubset(nextCeiling, nextUse)) throw new PluginError('forbidden', 'grantable rights exceed use rights')
        } else if (useRights !== undefined) {
          // Reducing use rights intersects the ceiling immediately. Increasing
          // them leaves the old (smaller) ceiling untouched.
          nextCeiling = intersectRights(grant.grantableRights, nextUse)
        }
        if (addDevice !== undefined) nextAddDevice = addDevice
      } else {
        if (grantableRights !== undefined || addDevice !== undefined) {
          throw new PluginError('forbidden', 'only the owner can set the ceiling or add-device')
        }
        const sponsor = this.getGrant(actor.grantId)
        if (!sponsor || sponsor.state !== 'active') throw new PluginError('forbidden', 'sponsor grant is not active')
        if (grant.sponsorGrantId !== sponsor.grantId) throw new PluginError('forbidden', 'grant is not sponsored by the actor')
        if (useRights === undefined) return { changed: false, grant }
        nextUse = normalizeRights(useRights, { name: 'use rights' })
        if (!rightsSubset(nextUse, sponsor.grantableRights)) throw new PluginError('forbidden', 'rights exceed sponsor ceiling')
      }

      // Final defense: the persisted ceiling is always inside use rights.
      if (!rightsSubset(nextCeiling, nextUse)) throw new PluginError('forbidden', 'grantable rights exceed use rights')

      const changed = !sameRights(nextUse, grant.useRights) ||
        !sameRights(nextCeiling, grant.grantableRights) ||
        nextAddDevice !== grant.addDevice
      if (!changed) return { changed: false, grant }

      this.#db.prepare(`
        UPDATE device_grants
        SET use_rights = ?, grantable_rights = ?, add_device = ?, revision = revision + 1, updated_at = ?
        WHERE grant_id = ? AND state = 'active'
      `).run(encodeRights(nextUse), encodeRights(nextCeiling), nextAddDevice ? 1 : 0, at, grant.grantId)
      return { changed: true, grant: this.getGrant(grant.grantId) }
    })
  }

  /**
   * Revoke exactly the selected active grants. Their pending pre-revocation
   * candidates (same key or sponsored by a selected grant) are fenced in the
   * same transaction. Independently committed child grants are untouched.
   */
  revokeGrants({ grantIds, now = this.#now() }) {
    if (!Array.isArray(grantIds)) throw new PluginError('invalid_argument', 'grant ids must be an array')
    const unique = []
    for (const id of grantIds) {
      const value = boundedString(id, 'grant id')
      if (!unique.includes(value)) unique.push(value)
    }
    const at = safeTimestamp(now, 'clock')
    return this.#transaction(() => {
      const revoked = []
      const alreadyRevoked = []
      const unknown = []
      for (const grantId of unique) {
        const grant = this.getGrant(grantId)
        if (!grant) {
          unknown.push(grantId)
          continue
        }
        if (grant.state !== 'active') {
          alreadyRevoked.push(grantId)
          continue
        }
        this.#db.prepare(`
          UPDATE device_grants SET state = 'revoked', revoked_at = ?, updated_at = ?, revision = revision + 1
          WHERE grant_id = ? AND state = 'active'
        `).run(at, at, grantId)
        revoked.push(grantId)
      }

      let fencedCandidates = []
      if (revoked.length > 0) {
        const placeholders = revoked.map(() => '?').join(', ')
        const pending = this.#db.prepare(`
          SELECT candidate_id FROM enrollment_candidates
          WHERE state = 'pending' AND (
            candidate_public_key IN (SELECT device_public_key FROM device_grants WHERE grant_id IN (${placeholders}))
            OR sponsor_grant_id IN (${placeholders})
          )
        `).all(...revoked, ...revoked)
        for (const row of pending) {
          const info = this.#db.prepare("UPDATE enrollment_candidates SET state = 'fenced', decided_at = ? WHERE candidate_id = ? AND state = 'pending'")
            .run(at, row.candidate_id)
          if (Number(info.changes) === 1) fencedCandidates.push(row.candidate_id)
        }
      }
      return { revoked, alreadyRevoked, unknown, fencedCandidates }
    })
  }

  // --- recovery authority -------------------------------------------------

  /**
   * Register a recovery verifier. Registering rotates any active verifier for
   * the same Host: the previous revision is retired, its pending recovery
   * candidates are fenced and a fresh, never-reused revision is issued.
   */
  registerRecoveryVerifier({ hostKeyFingerprint, publicKey, allowedRights, now = this.#now() }) {
    const fingerprint = boundedString(hostKeyFingerprint, 'host key fingerprint')
    const key = keyBytes(publicKey, 'verifier public key')
    const rights = normalizeRights(allowedRights, { name: 'allowed rights' })
    const at = safeTimestamp(now, 'clock')
    return this.#transaction(() => {
      this.#retireActiveVerifiers(fingerprint, at)
      const info = this.#db.prepare(`
        INSERT INTO recovery_verifiers (host_key_fingerprint, public_key, allowed_rights, active, created_at, retired_at)
        VALUES (?, ?, ?, 1, ?, NULL)
      `).run(fingerprint, key, encodeRights(rights), at)
      return rowVerifier(this.#db.prepare('SELECT * FROM recovery_verifiers WHERE revision = ?').get(Number(info.lastInsertRowid)))
    })
  }

  getRecoveryVerifier(revision) {
    if (!Number.isSafeInteger(revision) || revision < 1) return null
    return rowVerifier(this.#db.prepare('SELECT * FROM recovery_verifiers WHERE revision = ?').get(revision))
  }

  getActiveRecoveryVerifier(hostKeyFingerprint) {
    const fingerprint = boundedString(hostKeyFingerprint, 'host key fingerprint')
    return rowVerifier(this.#db.prepare('SELECT * FROM recovery_verifiers WHERE host_key_fingerprint = ? AND active = 1').get(fingerprint))
  }

  listRecoveryVerifiers({ hostKeyFingerprint, limit = 64 } = {}) {
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 256) throw new PluginError('invalid_argument', 'limit invalid')
    const fingerprint = boundedString(hostKeyFingerprint, 'host key fingerprint')
    return this.#db.prepare('SELECT * FROM recovery_verifiers WHERE host_key_fingerprint = ? ORDER BY revision DESC LIMIT ?')
      .all(fingerprint, limit).map(rowVerifier)
  }

  /** Retire one revision. Pending challenges bound to it are fenced. */
  retireRecoveryVerifier({ revision, now = this.#now() }) {
    const target = safeRevision(revision, 'verifier revision')
    const at = safeTimestamp(now, 'clock')
    return this.#transaction(() => {
      const verifier = this.getRecoveryVerifier(target)
      if (!verifier) throw new PluginError('not_found', 'verifier revision unknown')
      if (!verifier.active) return { changed: false, verifier }
      this.#db.prepare('UPDATE recovery_verifiers SET active = 0, retired_at = ? WHERE revision = ? AND active = 1').run(at, target)
      this.#fenceRecoveryCandidates([target], at)
      return { changed: true, verifier: this.getRecoveryVerifier(target) }
    })
  }

  /**
   * Change the allowed-rights scope by retiring the revision and issuing a new
   * one. Outstanding challenges for the old revision therefore die, including
   * after a restart.
   */
  updateRecoveryPolicy({ revision, allowedRights, now = this.#now() }) {
    const target = safeRevision(revision, 'verifier revision')
    const rights = normalizeRights(allowedRights, { name: 'allowed rights' })
    const at = safeTimestamp(now, 'clock')
    return this.#transaction(() => {
      const verifier = this.getRecoveryVerifier(target)
      if (!verifier) throw new PluginError('not_found', 'verifier revision unknown')
      if (!verifier.active) throw new PluginError('conflict', 'verifier revision retired')
      this.#db.prepare('UPDATE recovery_verifiers SET active = 0, retired_at = ? WHERE revision = ? AND active = 1').run(at, target)
      this.#fenceRecoveryCandidates([target], at)
      const info = this.#db.prepare(`
        INSERT INTO recovery_verifiers (host_key_fingerprint, public_key, allowed_rights, active, created_at, retired_at)
        VALUES (?, ?, ?, 1, ?, NULL)
      `).run(verifier.hostKeyFingerprint, verifier.publicKey, encodeRights(rights), at)
      return rowVerifier(this.#db.prepare('SELECT * FROM recovery_verifiers WHERE revision = ?').get(Number(info.lastInsertRowid)))
    })
  }

  /**
   * Issue one single-use recovery challenge for a live verifier revision and
   * stage its applicant candidate in the same transaction. The persisted
   * nonce, revision, key, exact rights and expiry are what `commitRecovery`
   * rechecks.
   */
  issueRecoveryChallenge({
    hostKeyFingerprint,
    verifierRevision,
    candidatePublicKey,
    deviceId,
    deviceLabel = '',
    useRights,
    expiresAt,
    challengeId = generateCandidateId(),
    now = this.#now(),
  }) {
    const fingerprint = boundedString(hostKeyFingerprint, 'host key fingerprint')
    const target = safeRevision(verifierRevision, 'verifier revision')
    const key = keyBytes(candidatePublicKey, 'candidate public key')
    const device = boundedString(deviceId, 'device id', MAX_LABEL_LENGTH)
    if (typeof deviceLabel !== 'string' || deviceLabel.length > MAX_LABEL_LENGTH) throw new PluginError('invalid_argument', 'device label invalid')
    const use = normalizeRights(useRights, { name: 'use rights' })
    const expiry = safeTimestamp(expiresAt, 'challenge expiry')
    const at = safeTimestamp(now, 'clock')
    const id = boundedString(challengeId, 'challenge id')
    if (expiry <= at) throw new PluginError('expired', 'challenge already expired')

    return this.#transaction(() => {
      const verifier = this.getRecoveryVerifier(target)
      if (!verifier || !verifier.active || verifier.hostKeyFingerprint !== fingerprint) {
        throw new PluginError('conflict', 'recovery verifier revision is not active')
      }
      if (!rightsSubset(use, verifier.allowedRights)) throw new PluginError('forbidden', 'selected rights exceed verifier scope')
      if (this.#candidateRow(id) || this.#challengeRow(id)) throw new PluginError('conflict', 'challenge id already used')
      const digest = requestDigest({
        method: 'recovery',
        hostKeyFingerprint: fingerprint,
        candidatePublicKey: key,
        useRights: use,
        grantableRights: [],
        addDevice: false,
        sponsorGrantId: null,
        recoveryVerifierRevision: target,
      })
      this.#db.prepare(`
        INSERT INTO enrollment_candidates (
          candidate_id, method, host_key_fingerprint, candidate_public_key, device_id, device_label,
          use_rights, grantable_rights, add_device, sponsor_grant_id, sponsor_revision,
          recovery_verifier_revision, request_digest, state, grant_id, decided_at, expires_at, created_at
        ) VALUES (?, 'recovery', ?, ?, ?, ?, ?, '[]', 0, NULL, NULL, ?, ?, 'pending', NULL, NULL, ?, ?)
      `).run(id, fingerprint, key, device, deviceLabel, encodeRights(use), target, digest, expiry, at)
      const nonce = randomToken()
      this.#db.prepare(`
        INSERT INTO recovery_challenges (
          challenge_id, host_key_fingerprint, verifier_revision, nonce, candidate_public_key,
          use_rights, expires_at, consumed, consumed_at, grant_id, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, 0, NULL, NULL, ?)
      `).run(id, fingerprint, target, nonce, key, encodeRights(use), expiry, at)
      return {
        challengeId: id,
        nonce,
        verifierRevision: target,
        candidatePublicKey: Buffer.from(key),
        useRights: use,
        expiresAt: expiry,
      }
    })
  }

  getRecoveryChallenge(challengeId) {
    if (typeof challengeId !== 'string' || challengeId === '') return null
    return this.#challengeRow(challengeId)
  }

  /**
   * Commit one recovery enrollment. Checks the proven verifier revision is
   * still active, the selected rights fit its CURRENT scope, the challenge is
   * live and unused, active-grant uniqueness holds, then consumes the
   * challenge and writes grant + idempotent decision in one transaction.
   */
  commitRecovery({
    challengeId,
    nonce,
    verifierRevision,
    candidatePublicKey,
    useRights,
    now = this.#now(),
  }) {
    const at = safeTimestamp(now, 'clock')
    const target = safeRevision(verifierRevision, 'verifier revision')
    const key = keyBytes(candidatePublicKey, 'candidate public key')
    const use = normalizeRights(useRights, { name: 'use rights' })
    if (typeof nonce !== 'string' || nonce === '') throw new PluginError('unauthorized', 'recovery nonce missing')

    return this.#transaction(() => {
      const candidate = this.#candidateRow(boundedString(challengeId, 'challenge id'))
      if (!candidate || candidate.method !== 'recovery') throw new PluginError('not_found', 'recovery challenge unknown')
      const challenge = this.#challengeRow(candidate.candidateId)
      if (!challenge) throw new PluginError('not_found', 'recovery challenge unknown')
      if (!bytesEqual(candidate.candidatePublicKey, key) || !bytesEqual(challenge.candidatePublicKey, key)) {
        throw new PluginError('conflict', 'recovery candidate key changed')
      }
      // The proof is bound to the exact challenge nonce even on a retry.
      if (!bytesEqual(Buffer.from(nonce, 'utf8'), Buffer.from(challenge.nonce, 'utf8'))) {
        throw new PluginError('unauthorized', 'recovery nonce mismatch')
      }
      if (challenge.verifierRevision !== target || candidate.recoveryVerifierRevision !== target) {
        throw new PluginError('conflict', 'recovery verifier revision changed')
      }
      const digest = requestDigest({
        method: 'recovery',
        hostKeyFingerprint: candidate.hostKeyFingerprint,
        candidatePublicKey: candidate.candidatePublicKey,
        useRights: use,
        grantableRights: [],
        addDevice: false,
        sponsorGrantId: null,
        recoveryVerifierRevision: candidate.recoveryVerifierRevision,
      })
      if (candidate.state !== 'pending') {
        if (!bytesEqual(candidate.requestDigest, digest)) throw new PluginError('conflict', 'recovery request changed')
        return this.#decisionResult(candidate)
      }
      if (!bytesEqual(candidate.requestDigest, digest)) throw new PluginError('conflict', 'recovery request changed')
      if (candidate.expiresAt <= at || challenge.expiresAt <= at) throw new PluginError('expired', 'recovery challenge expired')
      if (challenge.consumed) throw new PluginError('conflict', 'recovery challenge already consumed')
      const verifier = this.getRecoveryVerifier(target)
      if (!verifier || !verifier.active) throw new PluginError('conflict', 'recovery verifier revision is not active')
      if (verifier.hostKeyFingerprint !== candidate.hostKeyFingerprint) throw new PluginError('conflict', 'recovery verifier host changed')
      if (!rightsSubset(use, verifier.allowedRights)) throw new PluginError('forbidden', 'selected rights exceed verifier scope')
      const active = this.findActiveGrant({ hostKeyFingerprint: candidate.hostKeyFingerprint, devicePublicKey: candidate.candidatePublicKey })
      if (active) throw new PluginError('conflict', 'device key already has an active grant')

      const grantId = generateGrantId()
      this.#insertGrant({
        grantId,
        candidate,
        useRights: use,
        grantableRights: [],
        addDevice: false,
        approverKind: 'recovery',
        sponsorGrantId: null,
        recoveryVerifierRevision: target,
        now: at,
      })
      this.#db.prepare(`
        UPDATE recovery_challenges SET consumed = 1, consumed_at = ?, grant_id = ?
        WHERE challenge_id = ? AND consumed = 0
      `).run(at, grantId, candidate.candidateId)
      this.#db.prepare("UPDATE enrollment_candidates SET state = 'approved', grant_id = ?, decided_at = ? WHERE candidate_id = ?")
        .run(grantId, at, candidate.candidateId)
      return this.#decisionResult(this.#candidateRow(candidate.candidateId), { retried: false })
    })
  }

  // --- internals ----------------------------------------------------------

  #candidateRow(candidateId) {
    return rowCandidate(this.#db.prepare('SELECT * FROM enrollment_candidates WHERE candidate_id = ?').get(candidateId))
  }

  #challengeRow(challengeId) {
    return rowChallenge(this.#db.prepare('SELECT * FROM recovery_challenges WHERE challenge_id = ?').get(challengeId))
  }

  #insertGrant({ grantId, candidate, useRights, grantableRights, addDevice, approverKind, sponsorGrantId, recoveryVerifierRevision, now }) {
    this.#db.prepare(`
      INSERT INTO device_grants (
        grant_id, host_key_fingerprint, device_public_key, device_id, device_label,
        use_rights, grantable_rights, add_device, approver_kind, sponsor_grant_id,
        recovery_verifier_revision, revision, state, created_at, updated_at, revoked_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 'active', ?, ?, NULL)
    `).run(
      grantId, candidate.hostKeyFingerprint, candidate.candidatePublicKey, candidate.deviceId,
      candidate.deviceLabel, encodeRights(useRights), encodeRights(grantableRights), addDevice ? 1 : 0,
      approverKind, sponsorGrantId, recoveryVerifierRevision, now, now,
    )
  }

  #decisionResult(candidate, { retried = true } = {}) {
    if (candidate.state === 'fenced') throw new PluginError('pairing_fenced', 'enrollment candidate was fenced')
    if (candidate.state === 'rejected') {
      return {
        ok: true,
        retried,
        decision: { candidateId: candidate.candidateId, outcome: 'rejected', grantId: null, decidedAt: candidate.decidedAt },
        grant: null,
      }
    }
    if (candidate.state !== 'approved') throw new PluginError('internal', 'candidate is not decided')
    const grant = this.getGrant(candidate.grantId)
    if (!grant) throw new PluginError('pairing_committed_unavailable', 'committed grant is unavailable')
    return {
      ok: true,
      retried,
      decision: {
        candidateId: candidate.candidateId,
        outcome: 'approved',
        grantId: candidate.grantId,
        decidedAt: candidate.decidedAt,
      },
      grant,
    }
  }

  #retireActiveVerifiers(hostKeyFingerprint, now) {
    const active = this.#db.prepare('SELECT revision FROM recovery_verifiers WHERE host_key_fingerprint = ? AND active = 1')
      .all(hostKeyFingerprint).map((row) => row.revision)
    if (active.length === 0) return
    for (const revision of active) {
      this.#db.prepare('UPDATE recovery_verifiers SET active = 0, retired_at = ? WHERE revision = ? AND active = 1').run(now, revision)
    }
    this.#fenceRecoveryCandidates(active, now)
  }

  #fenceRecoveryCandidates(revisions, now) {
    if (revisions.length === 0) return
    const placeholders = revisions.map(() => '?').join(', ')
    const pending = this.#db.prepare(`
      SELECT candidate_id FROM enrollment_candidates
      WHERE state = 'pending' AND method = 'recovery' AND recovery_verifier_revision IN (${placeholders})
    `).all(...revisions)
    for (const row of pending) {
      this.#db.prepare("UPDATE enrollment_candidates SET state = 'fenced', decided_at = ? WHERE candidate_id = ? AND state = 'pending'")
        .run(now, row.candidate_id)
    }
  }
}

export { MAX_PENDING_CANDIDATES }