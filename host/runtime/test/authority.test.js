import assert from 'node:assert/strict'
import { randomBytes } from 'node:crypto'
import test from 'node:test'
import { ed25519 } from '@noble/curves/ed25519.js'
import {
  AUTHORITY_MAX_LIFETIME_MS,
  authorityCanonical,
  verifyScopedAuthority,
  sessionProofCanonical,
  verifySessionProof,
} from '../src/security/authority.js'

function keypair() {
  const privateKey = randomBytes(32)
  return { privateKey: new Uint8Array(privateKey), publicKey: ed25519.getPublicKey(privateKey) }
}

function permit(overrides = {}) {
  const now = 1_700_000_000_000
  return {
    authVersion: 'whalerein/v1/scoped-authority-v1',
    authorityId: 'authority-1',
    issuer: 'central',
    audience: 'whalerein-access',
    environment: 'test',
    workspaceId: 'workspace',
    hostId: 'host',
    deviceId: 'device',
    sessionId: 'session-1',
    grantGeneration: 2,
    hostGrantGeneration: 1,
    servingNodeId: 'node-a',
    routeGeneration: 4,
    sessionProofPublicKey: overrides.proofPublicKey ?? new Uint8Array(32),
    policyRevision: '7',
    issuedAtUnixMs: now,
    expiresAtUnixMs: now + AUTHORITY_MAX_LIFETIME_MS,
    keyId: 'key-1',
    ...overrides,
  }
}

function expected(value, overrides = {}) {
  return {
    hostId: value.hostId,
    deviceId: value.deviceId,
    sessionId: value.sessionId,
    servingNodeId: value.servingNodeId,
    routeGeneration: value.routeGeneration,
    proofPublicKey: value.sessionProofPublicKey,
    ...overrides,
  }
}

function signed(value, key) {
  value.signature = ed25519.sign(new TextEncoder().encode(authorityCanonical(value)), key.privateKey)
  return value
}

test('a current scoped authority verifies for the exact Host/Device/session/route', () => {
  const issuer = keypair()
  const proof = keypair()
  const value = signed(permit({ sessionProofPublicKey: proof.publicKey }), issuer)
  const result = verifyScopedAuthority(value, issuer.publicKey, expected(value), value.issuedAtUnixMs + 1)
  assert.equal(result.ok, true, result.reason)
})

test('a permit is rejected for another Host, Device, session, node, route or proof key', () => {
  const issuer = keypair()
  const proof = keypair()
  const value = signed(permit({ sessionProofPublicKey: proof.publicKey }), issuer)
  for (const [name, change] of Object.entries({
    host: { hostId: 'other' },
    device: { deviceId: 'other' },
    session: { sessionId: 'other' },
    node: { servingNodeId: 'node-b' },
    route: { routeGeneration: 5 },
    proof: { proofPublicKey: new Uint8Array(32) },
  })) {
    const result = verifyScopedAuthority(value, issuer.publicKey, expected(value, change), value.issuedAtUnixMs + 1)
    assert.equal(result.ok, false, name)
    assert.notEqual(result.reason, '', name)
  }
})

test('expired, future-issued, over-60s and tampered permits fail closed', () => {
  const issuer = keypair()
  const proof = keypair()
  const value = signed(permit({ sessionProofPublicKey: proof.publicKey }), issuer)
  assert.equal(verifyScopedAuthority(value, issuer.publicKey, expected(value), value.expiresAtUnixMs).reason, 'expired')
  assert.equal(
    verifyScopedAuthority(value, issuer.publicKey, expected(value), value.issuedAtUnixMs - 10_000).reason,
    'issued_in_future',
  )
  const long = signed(permit({
    sessionProofPublicKey: proof.publicKey,
    expiresAtUnixMs: value.issuedAtUnixMs + AUTHORITY_MAX_LIFETIME_MS + 1,
  }), issuer)
  assert.equal(verifyScopedAuthority(long, issuer.publicKey, expected(long), long.issuedAtUnixMs).reason, 'lifetime_too_long')
  const tampered = { ...value, deviceId: 'other' }
  assert.equal(verifyScopedAuthority(tampered, issuer.publicKey, expected(tampered), value.issuedAtUnixMs).reason, 'signature_invalid')
  assert.equal(verifyScopedAuthority(value, new Uint8Array(31), expected(value), value.issuedAtUnixMs).reason, 'issuer_key')
})

test('the Host canonical bytes match the Cloud issuer fixture', () => {
  const decode = (text) => Uint8Array.from(Buffer.from(text, 'base64url'))
  const authority = {
    authVersion: 'whalerein/v1/scoped-authority-v1',
    authorityId: 'auth-1',
    issuer: 'https://cloud.example',
    audience: 'https://access.example',
    environment: 'production',
    workspaceId: 'workspace',
    hostId: 'host',
    deviceId: 'device',
    sessionId: 'session',
    grantGeneration: 2,
    hostGrantGeneration: 3,
    servingNodeId: 'node-b',
    routeGeneration: 9,
    sessionProofPublicKey: decode('ZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXp7fH1-f4CBgoM'),
    policyRevision: 'r1',
    issuedAtUnixMs: 1_700_000_000_123,
    expiresAtUnixMs: 1_700_000_060_123,
    keyId: 'key-1',
    signature: decode('8Ir-VHjJT_vtyu6puNLTg3Is--1fHa0hDmcYyKDwFXmrsjwEG9zSAkCS5lDSxeq63-RF4KPR7lMq7DTgaQdcDA'),
  }
  const canonical = Buffer.from(authorityCanonical(authority)).toString('base64url')
  assert.equal(
    canonical,
    'd2hhbGVyZWluL3YxL3Njb3BlZC1hdXRob3JpdHktdjEKfGF1dGgtMQp8aHR0cHM6Ly9jbG91ZC5leGFtcGxlCnxodHRwczovL2FjY2Vzcy5leGFtcGxlCnxwcm9kdWN0aW9uCnx3b3Jrc3BhY2UKfGhvc3QKfGRldmljZQp8c2Vzc2lvbgp8Mgp8Mwp8bm9kZS1iCnw5CnxaR1ZtWjJocGFtdHNiVzV2Y0hGeWMzUjFkbmQ0ZVhwN2ZIMS1mNENCZ29NCnxyMQp8MTcwMDAwMDAwMDEyMwp8MTcwMDAwMDA2MDEyMwp8a2V5LTE',
  )
  const result = verifyScopedAuthority(
    authority,
    decode('ebVWLo_mVPlAeLES6KmLp5AfhTrmlb7X4OORC60ElmQ'),
    {
      hostId: 'host', deviceId: 'device', sessionId: 'session',
      servingNodeId: 'node-b', routeGeneration: 9,
      proofPublicKey: authority.sessionProofPublicKey,
    },
    1_700_000_000_123,
  )
  assert.equal(result.ok, true, result.reason)
})

test('the Host stages only verified permits and activates them only with the permit key', async () => {
  const { AuthorityStore } = await import('../src/security/authority-store.js')
  const issuer = keypair()
  const proof = keypair()
  const value = signed(permit({ sessionProofPublicKey: proof.publicKey }), issuer)
  const store = new AuthorityStore(issuer.publicKey)
  assert.equal(store.enabled, true)
  const expectedScope = expected(value)
  assert.equal(store.stage(value, { ...expectedScope, hostId: 'other' }, value.issuedAtUnixMs).ok, false)
  assert.equal(store.hasSession('session-1'), false)
  assert.equal(store.stage(value, expectedScope, value.issuedAtUnixMs).ok, true)
  assert.equal(store.hasSession('session-1'), true)
  assert.equal(store.validFor('session-1', 'host', 'device', value.issuedAtUnixMs), false)
  assert.equal(store.activate('session-1', new Uint8Array(32), value.issuedAtUnixMs), false)
  assert.equal(store.activate('session-1', proof.publicKey, value.issuedAtUnixMs), true)
  assert.equal(store.validFor('session-1', 'host', 'device', value.issuedAtUnixMs), true)
  assert.equal(store.validFor('session-1', 'host', 'device', value.expiresAtUnixMs), false)
  assert.equal(store.validFor('session-1', 'host', 'device', value.issuedAtUnixMs, 5), false)
  assert.equal(store.validFor('session-1', 'host', 'device', value.issuedAtUnixMs, 4), true)
  store.revokeDevice('device')
  assert.equal(store.validFor('session-1', 'host', 'device', value.issuedAtUnixMs), false)
  const disabled = new AuthorityStore(null)
  assert.equal(disabled.enabled, false)
  assert.equal(disabled.stage(value, expectedScope, value.issuedAtUnixMs).ok, false)
})

test('a transcript-bound session proof and staged permit only activate with the same key', async () => {
  const { AuthorityStore } = await import('../src/security/authority-store.js')
  const issuer = keypair()
  const proof = keypair()
  const value = signed(permit({ sessionProofPublicKey: proof.publicKey }), issuer)
  const scope = expected(value)
  const store = new AuthorityStore(issuer.publicKey)
  assert.equal(store.stage(value, scope, value.issuedAtUnixMs).ok, true)
  assert.equal(store.validFor('session-1', 'host', 'device', value.issuedAtUnixMs), false)
  const transcript = new Uint8Array(32); transcript[0] = 7
  const statement = {
    sessionId: 'session-1', hostId: 'host', deviceId: 'device', channelId: 'channel-1',
    transcriptHash: transcript, issuedAtUnixMs: value.issuedAtUnixMs,
    expiresAtUnixMs: value.issuedAtUnixMs + 30_000,
  }
  statement.signature = ed25519.sign(new TextEncoder().encode(sessionProofCanonical(statement)), proof.privateKey)
  assert.equal(verifySessionProof(proof.publicKey, statement, value.issuedAtUnixMs), true)
  assert.equal(verifySessionProof(proof.publicKey, { ...statement, sessionId: 'other' }, value.issuedAtUnixMs), false)
  assert.equal(store.activate('session-1', proof.publicKey, value.issuedAtUnixMs), true)
  assert.equal(store.validFor('session-1', 'host', 'device', value.issuedAtUnixMs), true)
  store.revokeDevice('device')
  assert.equal(store.validFor('session-1', 'host', 'device', value.issuedAtUnixMs), false)
})

test('the Host proof canonical matches the Core contract', () => {
  assert.equal(sessionProofCanonical({
    sessionId: 'session-1', hostId: 'host-1', deviceId: 'device-1', channelId: 'channel-1',
    transcriptHash: new Uint8Array(32),
    issuedAtUnixMs: 1_700_000_000_000, expiresAtUnixMs: 1_700_000_030_000,
  }), 'whalerein/v1/session-proof|session-1|host-1|device-1|channel-1|AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA|1700000000000|1700000030000')
})

test('the Host verifies a signature produced by the Core fixture', () => {
  const publicKey = Uint8Array.from(Buffer.from('XWqO2VXVBSivN9KHeV_rF-5fFt_FgKfvsPPpM7FJKcs', 'base64url'))
  const proof = {
    sessionId: 'session-1', hostId: 'host-1', deviceId: 'device-1', channelId: 'channel-1',
    transcriptHash: new Uint8Array(32),
    issuedAtUnixMs: 1_700_000_000_000, expiresAtUnixMs: 1_700_000_030_000,
    signature: Uint8Array.from(Buffer.from('3OjP2K7k06XZdOnaCjcz9aXeHNw9Dqea41uUOiei_5U5JNvDIf_Biii4leS6l8sef92yEXlam0IOuGMAFzXeBw', 'base64url')),
  }
  assert.equal(verifySessionProof(publicKey, proof, 1_700_000_000_001), true)
  assert.equal(verifySessionProof(publicKey, { ...proof, sessionId: 'session-2' }, 1_700_000_000_001), false)
})
