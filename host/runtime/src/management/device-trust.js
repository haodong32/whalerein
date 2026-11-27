// Host-local device-trust management surface.
//
// This is the backend used by the local owner settings service, the loss
// review and the recovery ceremony. It is deliberately not a wire surface:
// nothing here is reachable through a device channel. Every write goes through
// the single durable grant writer in `security/grants.js`, so local owner
// enrollment, sponsored enrollment, recovery and pairing cannot diverge into a
// second store or a label-derived grant.

import { PluginError } from '../errors.js'
import { fingerprint } from '../security/noise.js'
import { generateCandidateId, normalizeRights } from '../security/grants.js'

const CANDIDATE_TTL_MS = 120_000
const MAX_LABEL_LENGTH = 128

function boundedString(value, name, maxLength = 256) {
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

function grantSummary(grant) {
  if (!grant) return null
  return {
    grantId: grant.grantId,
    deviceId: grant.deviceId,
    deviceLabel: grant.deviceLabel,
    devicePublicKey: Buffer.from(grant.devicePublicKey),
    useRights: [...grant.useRights],
    grantableRights: [...grant.grantableRights],
    addDevice: grant.addDevice,
    approverKind: grant.approverKind,
    sponsorGrantId: grant.sponsorGrantId,
    recoveryVerifierRevision: grant.recoveryVerifierRevision,
    revision: grant.revision,
    state: grant.state,
    createdAt: grant.createdAt,
    updatedAt: grant.updatedAt,
    revokedAt: grant.revokedAt,
  }
}

export class HostDeviceTrustService {
  #trust
  #security
  #store
  #hostKeyFingerprint
  #now
  #logger

  /**
   * @param {object} options
   * @param {import('../security/grants.js').DeviceTrustStore} options.trust
   * @param {import('../security/manager.js').SecurityManager|null} [options.security]
   * @param {import('../state/store.js').StateStore} options.store
   * @param {string} options.hostKeyFingerprint Host Noise key fingerprint.
   * @param {() => number} [options.now]
   */
  constructor({ trust, security = null, store, hostKeyFingerprint, now = Date.now, logger = null }) {
    if (!trust) throw new PluginError('internal', 'device trust store unavailable')
    if (!store) throw new PluginError('internal', 'state store unavailable')
    this.#trust = trust
    this.#security = security
    this.#store = store
    this.#hostKeyFingerprint = boundedString(hostKeyFingerprint, 'host key fingerprint')
    this.#now = now
    this.#logger = logger
  }

  // --- grants -------------------------------------------------------------

  /** Every grant with provenance, rights and timestamps. */
  listGrants({ state = null, limit = 256 } = {}) {
    return this.#trust.listGrants({
      hostKeyFingerprint: this.#hostKeyFingerprint,
      state,
      limit,
    }).map(grantSummary)
  }

  getGrant(grantId) {
    const grant = this.#trust.getGrant(grantId)
    if (!grant || grant.hostKeyFingerprint !== this.#hostKeyFingerprint) return null
    return grantSummary(grant)
  }

  /**
   * Create one owner-approved grant through the shared writer. `replace`
   * retires the existing active grant for the same device key first; without
   * it a live pair conflicts and is never merged or raised.
   */
  createGrant({
    deviceId,
    devicePublicKey,
    deviceLabel = '',
    useRights,
    grantableRights = [],
    addDevice = false,
    replace = false,
  }) {
    const id = boundedString(deviceId, 'device id', MAX_LABEL_LENGTH)
    const key = keyBytes(devicePublicKey)
    if (typeof deviceLabel !== 'string' || deviceLabel.length > MAX_LABEL_LENGTH) {
      throw new PluginError('invalid_argument', 'device label invalid')
    }
    if (typeof addDevice !== 'boolean') throw new PluginError('invalid_argument', 'add-device invalid')
    const use = normalizeRights(useRights, { name: 'use rights' })
    const ceiling = normalizeRights(grantableRights, { name: 'grantable rights' })
    if (!ceiling.every((right) => use.includes(right))) {
      throw new PluginError('forbidden', 'grantable rights exceed use rights')
    }
    const peer = this.#store.getPeerKey(id)
    if (peer && !peer.publicKey.equals(key)) {
      throw new PluginError('conflict', 'device identity cannot be rebound')
    }
    const active = this.#trust.findActiveGrant({
      hostKeyFingerprint: this.#hostKeyFingerprint,
      devicePublicKey: key,
    })
    if (active) {
      if (!replace) throw new PluginError('conflict', 'device key already has an active grant')
      this.revokeGrants({ grantIds: [active.grantId], reason: 'replaced' })
    }
    const candidateId = generateCandidateId()
    const now = this.#now()
    this.#trust.stageCandidate({
      candidateId,
      method: 'local',
      hostKeyFingerprint: this.#hostKeyFingerprint,
      candidatePublicKey: key,
      deviceId: id,
      deviceLabel,
      useRights: use,
      grantableRights: ceiling,
      addDevice,
      expiresAt: now + CANDIDATE_TTL_MS,
      now,
    })
    const outcome = this.#trust.commitEnrollment({
      candidateId,
      candidatePublicKey: key,
      useRights: use,
      grantableRights: ceiling,
      addDevice,
      approver: { kind: 'owner' },
      now,
    })
    if (!outcome.grant) throw new PluginError('internal', 'grant not committed')
    this.#bindPeer(outcome.grant, { claimId: null, grantGeneration: peer?.grantGeneration ?? 0, os: peer?.os ?? '' })
    this.#logger?.info('device_grant_created', { device: id, grant: outcome.grant.grantId })
    return { decision: outcome.decision, grant: grantSummary(outcome.grant), retried: outcome.retried }
  }

  /** Replace one grant through the same writer (retire, then enroll). */
  replaceGrant(options = {}) {
    return this.createGrant({ ...options, replace: true })
  }

  /**
   * Owner edit. A removed read right closes the grant's channels so no further
   * protected output is released; other rights remain usable on a fresh
   * channel. The ceiling can never grow past use rights and never regrows by
   * itself.
   */
  editGrant({ grantId, useRights, grantableRights, addDevice, expectedRevision }) {
    const before = this.#trust.getGrant(boundedString(grantId, 'grant id'))
    if (!before) throw new PluginError('not_found', 'grant unknown')
    const outcome = this.#trust.editGrant({
      grantId,
      actor: { kind: 'owner' },
      useRights,
      grantableRights,
      addDevice,
      expectedRevision,
      now: this.#now(),
    })
    if (!outcome.changed) return { changed: false, grant: grantSummary(outcome.grant) }
    this.#updatePeerGrant(outcome.grant)
    // Losing read stops content output; losing approve stops protected approval
    // detail. Either closes the grant's channels so queued event output cannot
    // outlive the right. A removed write is enforced per operation, keeping the
    // remaining read stream usable on the same channel.
    const outputRemoved = ['read', 'approve'].some(
      (right) => before.useRights.includes(right) && !outcome.grant.useRights.includes(right),
    )
    if (outputRemoved) this.#security?.closeGrantChannels?.(grantId, 'rights_changed')
    this.#logger?.info('device_grant_edited', {
      grant: grantId, revision: outcome.grant.revision, output_removed: outputRemoved,
    })
    return { changed: true, grant: grantSummary(outcome.grant) }
  }

  /**
   * Revoke exactly the selected grant IDs. Ordinary retirement keeps already
   * committed child grants; the loss response revokes only what the owner
   * selected. Channels, pending handshakes and queued ciphertext for those
   * grants are fenced.
   */
  revokeGrants({ grantIds, reason = 'revoked' } = {}) {
    if (!Array.isArray(grantIds)) throw new PluginError('invalid_argument', 'grant ids must be an array')
    const selected = []
    for (const value of grantIds) {
      const id = boundedString(value, 'grant id')
      if (!selected.includes(id)) selected.push(id)
    }
    // The owner may only select grants of this Host.
    for (const id of selected) {
      const grant = this.#trust.getGrant(id)
      if (!grant) throw new PluginError('not_found', 'grant unknown')
      if (grant.hostKeyFingerprint !== this.#hostKeyFingerprint) throw new PluginError('forbidden', 'grant belongs to another Host')
    }
    const result = this.#security
      ? this.#security.revokeGrants({ grantIds: selected, reason })
      : this.#trust.revokeGrants({ grantIds: selected, now: this.#now() })
    return {
      revoked: result.revoked,
      alreadyRevoked: result.alreadyRevoked,
      unknown: result.unknown,
      fencedCandidates: result.fencedCandidates ?? [],
      closedChannels: result.closed ?? [],
    }
  }

  /** Directly approved children (all states) with their approval timestamps. */
  listChildren(sponsorGrantId) {
    const id = boundedString(sponsorGrantId, 'sponsor grant id')
    const sponsor = this.#trust.getGrant(id)
    if (!sponsor || sponsor.hostKeyFingerprint !== this.#hostKeyFingerprint) {
      throw new PluginError('not_found', 'grant unknown')
    }
    return this.#trust.listChildren(id).map(grantSummary)
  }

  /** Loss review: the selected grant plus every directly approved child. */
  lossReview(grantId) {
    const grant = this.getGrant(grantId)
    if (!grant) throw new PluginError('not_found', 'grant unknown')
    return {
      grant,
      children: this.listChildren(grantId),
    }
  }

  // --- recovery authority -------------------------------------------------

  registerRecoveryVerifier({ publicKey, allowedRights }) {
    const verifier = this.#trust.registerRecoveryVerifier({
      hostKeyFingerprint: this.#hostKeyFingerprint,
      publicKey: keyBytes(publicKey, 'verifier public key'),
      allowedRights,
      now: this.#now(),
    })
    return this.#verifierSummary(verifier)
  }

  retireRecoveryVerifier({ revision }) {
    const result = this.#trust.retireRecoveryVerifier({ revision, now: this.#now() })
    return { changed: result.changed, verifier: this.#verifierSummary(result.verifier) }
  }

  updateRecoveryPolicy({ revision, allowedRights }) {
    const verifier = this.#trust.updateRecoveryPolicy({ revision, allowedRights, now: this.#now() })
    return this.#verifierSummary(verifier)
  }

  listRecoveryVerifiers() {
    return this.#trust.listRecoveryVerifiers({
      hostKeyFingerprint: this.#hostKeyFingerprint,
    }).map((verifier) => this.#verifierSummary(verifier))
  }

  issueRecoveryChallenge({
    verifierRevision,
    deviceId,
    devicePublicKey,
    deviceLabel = '',
    useRights,
    expiresAt,
  }) {
    const id = boundedString(deviceId, 'device id', MAX_LABEL_LENGTH)
    if (typeof deviceLabel !== 'string' || deviceLabel.length > MAX_LABEL_LENGTH) {
      throw new PluginError('invalid_argument', 'device label invalid')
    }
    return this.#trust.issueRecoveryChallenge({
      hostKeyFingerprint: this.#hostKeyFingerprint,
      verifierRevision,
      candidatePublicKey: keyBytes(devicePublicKey),
      deviceId: id,
      deviceLabel,
      useRights,
      expiresAt,
      now: this.#now(),
    })
  }

  /**
   * Commit one recovery enrollment. The writer rechecks the verifier revision,
   * its current scope, the live single-use challenge and grant uniqueness in
   * one transaction; a retirement or policy change that won the race yields
   * zero grants.
   */
  commitRecovery({ challengeId, nonce, verifierRevision, devicePublicKey, useRights }) {
    const key = keyBytes(devicePublicKey)
    // Bind the durable decision only to a free or same-key device identity.
    const candidate = this.#trust.getCandidate(challengeId)
    if (candidate) {
      const peer = this.#store.getPeerKey(candidate.deviceId)
      if (peer && !peer.publicKey.equals(key)) {
        throw new PluginError('conflict', 'device identity cannot be rebound')
      }
    }
    const outcome = this.#trust.commitRecovery({
      challengeId,
      nonce,
      verifierRevision,
      candidatePublicKey: key,
      useRights,
      now: this.#now(),
    })
    if (outcome.grant) this.#bindPeer(outcome.grant, { claimId: null, os: '' })
    return { decision: outcome.decision, grant: grantSummary(outcome.grant), retried: outcome.retried }
  }

  // --- internals ----------------------------------------------------------

  #verifierSummary(verifier) {
    if (!verifier) return null
    return {
      revision: verifier.revision,
      publicKey: Buffer.from(verifier.publicKey),
      allowedRights: [...verifier.allowedRights],
      active: verifier.active,
      createdAt: verifier.createdAt,
      retiredAt: verifier.retiredAt,
    }
  }

  #bindPeer(grant, { claimId = null, grantGeneration = 0, os = '' } = {}) {
    const peer = this.#store.getPeerKey(grant.deviceId)
    if (peer && !peer.publicKey.equals(grant.devicePublicKey)) {
      throw new PluginError('conflict', 'device identity cannot be rebound')
    }
    this.#store.upsertPeerKey({
      deviceId: grant.deviceId,
      publicKey: grant.devicePublicKey,
      fingerprint: peer?.fingerprint ?? fingerprint(grant.devicePublicKey),
      claimId: peer?.claimId ?? claimId,
      grantGeneration: peer?.grantGeneration ?? grantGeneration,
      grantId: grant.grantId,
      grantRevision: grant.revision,
      pairedAt: peer?.pairedAt ?? this.#now(),
      displayName: grant.deviceLabel || peer?.displayName || '',
      os: peer?.os ?? os,
    })
  }

  #updatePeerGrant(grant) {
    const peer = this.#store.getPeerKey(grant.deviceId)
    if (!peer || !peer.publicKey.equals(grant.devicePublicKey)) return
    this.#store.upsertPeerKey({
      deviceId: peer.deviceId,
      publicKey: peer.publicKey,
      fingerprint: peer.fingerprint,
      claimId: peer.claimId,
      grantGeneration: peer.grantGeneration,
      grantId: grant.grantId,
      grantRevision: grant.revision,
      pairedAt: peer.pairedAt,
      displayName: peer.displayName,
      os: peer.os,
    })
  }
}