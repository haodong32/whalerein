// Central-signed scoped authority and App-session proof checks.
//
// The Host must not trust a relay's claim that a permit is valid. This module
// verifies the exact canonical bytes, the immutable session proof key, the
// absolute 60-second ceiling, and the Host/Device/session/route scope.

import { ed25519 } from '@noble/curves/ed25519.js'

export const AUTHORITY_VERSION = 'whalerein/v1/scoped-authority-v1'
export const AUTHORITY_MAX_LIFETIME_MS = 60_000
const CLOCK_SKEW_MS = 5_000
const SESSION_TRANSCRIPT_PROOF_PREFIX = 'whalerein/v1/session-proof'

function value(source, ...names) {
  for (const name of names) {
    if (source?.[name] !== undefined && source?.[name] !== null) return source[name]
  }
  return undefined
}

function string(source, ...names) {
  const found = value(source, ...names)
  return typeof found === 'string' ? found : ''
}

function uint64(source, ...names) {
  const found = value(source, ...names)
  if (typeof found === 'number' && Number.isSafeInteger(found) && found >= 0) return found
  if (typeof found === 'bigint' && found >= 0n) return Number(found)
  if (typeof found === 'string' && /^\d+$/.test(found)) return Number(found)
  return 0
}

function bytes(source, ...names) {
  const found = value(source, ...names)
  if (found instanceof Uint8Array) return found
  if (Array.isArray(found)) return Uint8Array.from(found)
  if (typeof found === 'string' && found !== '') {
    try { return Uint8Array.from(Buffer.from(found, 'base64url')) } catch { return new Uint8Array() }
  }
  return new Uint8Array()
}

function rawBase64URL(input) {
  return Buffer.from(input).toString('base64url')
}

/** The exact bytes Central signs for a ScopedAuthority. */
export function authorityCanonical(authority) {
  return [
    string(authority, 'authVersion', 'auth_version') || AUTHORITY_VERSION,
    '|' + string(authority, 'authorityId', 'authority_id'),
    '|' + string(authority, 'issuer'),
    '|' + string(authority, 'audience'),
    '|' + string(authority, 'environment'),
    '|' + string(authority, 'workspaceId', 'workspace_id'),
    '|' + string(authority, 'hostId', 'host_id'),
    '|' + string(authority, 'deviceId', 'device_id'),
    '|' + string(authority, 'sessionId', 'session_id'),
    '|' + uint64(authority, 'grantGeneration', 'grant_generation'),
    '|' + uint64(authority, 'hostGrantGeneration', 'host_grant_generation'),
    '|' + string(authority, 'servingNodeId', 'serving_node_id'),
    '|' + uint64(authority, 'routeGeneration', 'route_generation'),
    '|' + rawBase64URL(bytes(authority, 'sessionProofPublicKey', 'session_proof_public_key')),
    '|' + string(authority, 'policyRevision', 'policy_revision'),
    '|' + uint64(authority, 'issuedAtUnixMs', 'issued_at_unix_ms'),
    '|' + uint64(authority, 'expiresAtUnixMs', 'expires_at_unix_ms'),
    '|' + string(authority, 'keyId', 'key_id'),
  ].join('\n')
}

/** Canonical Device -> Host proof bound to one Noise transcript. */
export function sessionProofCanonical(proof) {
  const hash = bytes(proof, 'transcriptHash', 'transcript_hash')
  return `${SESSION_TRANSCRIPT_PROOF_PREFIX}|${string(proof, 'sessionId', 'session_id')}|${string(proof, 'hostId', 'host_id')}|${string(proof, 'deviceId', 'device_id')}|${string(proof, 'channelId', 'channel_id')}|${rawBase64URL(hash)}|${uint64(proof, 'issuedAtUnixMs', 'issued_at_unix_ms')}|${uint64(proof, 'expiresAtUnixMs', 'expires_at_unix_ms')}`
}

/** Verify one transcript-bound possession proof with the bound session key. */
export function verifySessionProof(publicKey, proof, now = Date.now()) {
  if (!(publicKey instanceof Uint8Array) || publicKey.length !== 32) return false
  const signature = bytes(proof, 'signature')
  if (signature.length !== 64) return false
  const issued = uint64(proof, 'issuedAtUnixMs', 'issued_at_unix_ms')
  const expires = uint64(proof, 'expiresAtUnixMs', 'expires_at_unix_ms')
  if (issued === 0 || expires <= issued || expires <= now || issued > now + CLOCK_SKEW_MS) return false
  if (expires - issued > AUTHORITY_MAX_LIFETIME_MS) return false
  try {
    return ed25519.verify(signature, new TextEncoder().encode(sessionProofCanonical(proof)), publicKey)
  } catch {
    return false
  }
}

/**
 * Verify one permit against the local Host's view.
 *
 * `expected` must contain hostId, deviceId, sessionId, servingNodeId,
 * routeGeneration and proofPublicKey. Any missing/mismatched value fails
 * closed. `now` is injectable for clock/suspend tests.
 */
export function verifyScopedAuthority(authority, publicKey, expected, now = Date.now()) {
  const fail = (reason) => ({ ok: false, reason })
  if (!(publicKey instanceof Uint8Array) || publicKey.length !== 32) return fail('issuer_key')
  const signature = bytes(authority, 'signature')
  if (signature.length !== 64) return fail('signature_missing')
  let verified = false
  try {
    verified = ed25519.verify(signature, new TextEncoder().encode(authorityCanonical(authority)), publicKey)
  } catch {
    return fail('signature_invalid')
  }
  if (!verified) return fail('signature_invalid')

  const issuedAt = uint64(authority, 'issuedAtUnixMs', 'issued_at_unix_ms')
  const expiresAt = uint64(authority, 'expiresAtUnixMs', 'expires_at_unix_ms')
  if (issuedAt === 0 || expiresAt === 0 || expiresAt <= issuedAt) return fail('lifetime')
  if (expiresAt - issuedAt > AUTHORITY_MAX_LIFETIME_MS) return fail('lifetime_too_long')
  if (issuedAt > now + CLOCK_SKEW_MS) return fail('issued_in_future')
  if (expiresAt <= now) return fail('expired')

  if (string(authority, 'hostId', 'host_id') !== expected?.hostId) return fail('host')
  if (string(authority, 'deviceId', 'device_id') !== expected?.deviceId) return fail('device')
  if (string(authority, 'sessionId', 'session_id') !== expected?.sessionId) return fail('session')
  if (string(authority, 'servingNodeId', 'serving_node_id') !== expected?.servingNodeId) return fail('node')
  if (uint64(authority, 'routeGeneration', 'route_generation') !== expected?.routeGeneration) return fail('route_generation')
  const proof = bytes(authority, 'sessionProofPublicKey', 'session_proof_public_key')
  const want = expected?.proofPublicKey instanceof Uint8Array ? expected.proofPublicKey : new Uint8Array()
  if (proof.length !== 32 || want.length !== 32) return fail('proof_key')
  for (let index = 0; index < proof.length; index += 1) {
    if (proof[index] !== want[index]) return fail('proof_key')
  }
  return { ok: true, reason: '', issuedAt, expiresAt }
}
