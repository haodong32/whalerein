// Verified App-session permits installed on a Host. The store never accepts a
// permit that fails the canonical signature/scope/proof checks.

import { verifyScopedAuthority } from './authority.js'

export class AuthorityStore {
  #issuerKey
  #entries = new Map()
  #pending = new Map()

  constructor(issuerKey) {
    this.#issuerKey = issuerKey instanceof Uint8Array && issuerKey.length === 32 ? issuerKey : null
  }

  get enabled() {
    return this.#issuerKey !== null
  }

  /**
   * Verify the Cloud permit and park it until the Device proves possession.
   * A staged permit is never used for dispatch.
   */
  stage(authority, expected, now = Date.now()) {
    if (!this.#issuerKey) return { ok: false, reason: 'issuer_key' }
    const permitKey = authority.session_proof_public_key ?? authority.sessionProofPublicKey
    const result = verifyScopedAuthority(authority, this.#issuerKey,
      { ...expected, proofPublicKey: permitKey }, now)
    if (!result.ok) return result
    this.#pending.set(expected.sessionId, {
      authority, expected, proofPublicKey: permitKey, expiresAt: result.expiresAt,
    })
    return result
  }

  hasSession(sessionId) {
    return this.#entries.has(sessionId) || this.#pending.has(sessionId)
  }

  pendingProofKey(sessionId) {
    return this.#pending.get(sessionId)?.proofPublicKey ?? null
  }

  /** Activate a staged permit after the transcript-bound proof verifies. */
  activate(sessionId, proofKey, now = Date.now()) {
    const pending = this.#pending.get(sessionId)
    if (!pending || pending.expiresAt <= now) return false
    if (!(proofKey instanceof Uint8Array) || proofKey.length !== 32 ||
        proofKey.length !== pending.proofPublicKey.length) return false
    for (let i = 0; i < proofKey.length; i += 1) {
      if (proofKey[i] !== pending.proofPublicKey[i]) return false
    }
    this.#entries.set(sessionId, {
      authority: pending.authority,
      expiresAt: pending.expiresAt,
      hostId: pending.expected.hostId,
      deviceId: pending.expected.deviceId,
      servingNodeId: pending.expected.servingNodeId,
      routeGeneration: pending.expected.routeGeneration,
      proofPublicKey: pending.proofPublicKey,
    })
    this.#pending.delete(sessionId)
    return true
  }

  /** True when an installed, unexpired permit owns this exact session scope. */
  validFor(sessionId, hostId, deviceId, now = Date.now(), routeGeneration) {
    const entry = this.#entries.get(sessionId)
    if (!entry || entry.expiresAt <= now) return false
    if (entry.hostId !== hostId || entry.deviceId !== deviceId) return false
    if (routeGeneration !== undefined && routeGeneration !== null &&
        entry.routeGeneration !== routeGeneration) return false
    return true
  }

  /** A Device revocation kills every staged or active session for it. */
  revokeDevice(deviceId) {
    if (typeof deviceId !== 'string' || deviceId === '') return
    for (const [sessionId, entry] of this.#entries) {
      if (entry.deviceId === deviceId) this.#entries.delete(sessionId)
    }
    for (const [sessionId, pending] of this.#pending) {
      if (pending.expected.deviceId === deviceId) this.#pending.delete(sessionId)
    }
  }

}
