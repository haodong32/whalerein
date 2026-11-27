import { MemoryHostCredentials } from '../src/security/credentials.js'
import test from 'node:test'
import assert from 'node:assert/strict'
import {
  NoiseXX,
  channelPrologue,
  eventAad,
  fingerprint,
  generateKeypair,
  pairingPrologue,
  requestAad,
} from '../src/security/noise.js'
import { SecurityManager, HOST_CAPABILITIES, defaultReceiveAdvertisement } from '../src/security/manager.js'
import { transportProfileBudget } from '../src/contracts/transport.js'
import { loadOrCreateHostIdentity, HOST_KEY_CREDENTIAL } from '../src/security/identity.js'
import { StateStore } from '../src/state/store.js'
import { codec as defaultCodec } from '../src/protocol.js'
import { ensureOwnerGrant, establishChannel, openFrame } from './support/channel.js'

const hostId = 'host-1'
const codec = defaultCodec()

function makeSecurity(overrides = {}) {
  const store = new StateStore({ path: ':memory:', now: () => overrides.now?.() ?? 1000 })
  const identity = generateKeypair()
  const security = new SecurityManager({
    hostId,
    identity,
    store,
    codec,
    now: overrides.now ?? (() => 1000),
    pendingTtlMs: overrides.pendingTtlMs ?? 60_000,
    maxPending: overrides.maxPending ?? 32,
    channelIdleMs: overrides.channelIdleMs,
    maxDeviceChannels: overrides.maxDeviceChannels,
    maxHostChannels: overrides.maxHostChannels,
    onClosedChannels: overrides.onClosedChannels,
    onRevokedChannels: overrides.onRevokedChannels,
  })
  return { store, identity, security }
}

/** Persist the pinned peer row with the Host grant the channel now requires. */
function persistPeerWithGrant(security, store, { deviceId, identity, claimId = `claim-${deviceId}` }) {
  const grant = ensureOwnerGrant({ security, deviceId, identity })
  store.upsertPeerKey({
    deviceId,
    publicKey: identity.publicKey,
    fingerprint: fingerprint(identity.publicKey),
    claimId,
    pairedAt: 1,
    grantId: grant?.grantId ?? null,
    grantRevision: grant?.revision ?? null,
  })
  return grant
}

function pairingStart({ deviceId = 'device-1', tokenHash = 'ab'.repeat(32), claimId = 'claim-1', identity = generateKeypair() } = {}) {
  const noise = new NoiseXX({
    initiator: true,
    staticKeypair: identity,
    prologue: pairingPrologue({ hostId, tokenHash, claimId }),
  })
  const hello = codec.encode('whalerein.v1.PairingHello', {
    device_id: deviceId,
    host_id: hostId,
    protocol_version: 1,
    capabilities: { capabilities: [...HOST_CAPABILITIES], protocol_version: 1 },
    device_nonce: 'nonce',
    display_name: 'device',
    os: 'Linux',
  })
  return { noise, identity, deviceId, tokenHash, claimId, msg1: noise.writeMessage(hello) }
}

function helloClaim(start, overrides = {}) {
  return {
    claim_id: start.claimId,
    token_hash: start.tokenHash,
    phase: 'PAIRING_PHASE_HELLO',
    noise_message: start.msg1,
    device_id: start.deviceId,
    expires_at_unix_ms: 0,
    ...overrides,
  }
}

function finishPairing({ security, start, request = {}, fingerprintOverride = null, approvedKey }) {
  const finish = codec.encode('whalerein.v1.PairingFinish', {
    device_id: start.deviceId,
    host_id: hostId,
    host_fingerprint: fingerprintOverride ?? security.hostFingerprint,
    device_auth_hash: new Uint8Array(32).fill(7),
  })
  const msg3 = start.noise.writeMessage(finish)
  return security.handlePairingClaim({
    request: { device_id: start.deviceId, protocol_version: 1, ...request },
    claim: {
      claim_id: start.claimId,
      token_hash: start.tokenHash,
      phase: 'PAIRING_PHASE_FINISH',
      device_public_key: approvedKey,
      device_auth_hash: new Uint8Array(32).fill(7),
      noise_message: msg3,
      device_id: start.deviceId,
      expires_at_unix_ms: 0,
    },
  })
}

test('when HELLO/FINISH learns a device key, it remains transient until AUTHORIZE persists it', () => {
  const { security, store, identity } = makeSecurity()
  const start = pairingStart()
  const hello = security.handlePairingClaim({
    request: { device_id: start.deviceId, protocol_version: 1 },
    claim: helloClaim(start),
  })
  assert.equal(hello.device_id, start.deviceId)
  assert.deepEqual([...hello.host_public_key], [...identity.publicKey])
  assert.equal(hello.host_fingerprint, fingerprint(identity.publicKey))
  start.noise.readMessage(hello.noise_message)
  assert.deepEqual([...start.noise.remoteStaticKey], [...identity.publicKey])
  assert.equal(store.getPeerKey(start.deviceId), null)

  const finished = finishPairing({ security, start })
  assert.equal(finished.device_id, start.deviceId)
  assert.deepEqual([...finished.device_public_key], [...start.identity.publicKey])
  assert.equal(store.getPeerKey(start.deviceId), null)

  const authorized = security.handlePairingClaim({
    request: { device_id: start.deviceId, protocol_version: 1 },
    claim: {
      claim_id: start.claimId,
      phase: 'PAIRING_PHASE_AUTHORIZE',
      device_id: start.deviceId,
      device_public_key: start.identity.publicKey,
      expires_at_unix_ms: 0,
    },
  })
  assert.equal(authorized.authorized, true)
  assert.equal(store.getPeerKey(start.deviceId), null)
  security.handlePairingClaim({ request: { device_id: start.deviceId, protocol_version: 1 }, claim: { claim_id: start.claimId, phase: 'PAIRING_PHASE_COMMIT', device_id: start.deviceId, device_public_key: start.identity.publicKey } })
  const stored = store.getPeerKey(start.deviceId)
  assert.deepEqual([...stored.publicKey], [...start.identity.publicKey])
  assert.equal(stored.fingerprint, fingerprint(start.identity.publicKey))
  assert.equal(stored.displayName, 'device')
  assert.equal(stored.os, 'linux')
})

test('when Cloud approves a Device key, the Host repeats the exact HELLO and verifies the Noise peer', () => {
  const { security, store } = makeSecurity()
  const start = pairingStart()
  const request = { device_id: start.deviceId, protocol_version: 1 }
  const approvedKey = start.identity.publicKey
  const claim = helloClaim(start, { device_public_key: approvedKey })
  const reply = security.handlePairingClaim({ request, claim })
  assert.deepEqual(security.handlePairingClaim({ request, claim }), reply)
  assert.throws(() => security.handlePairingClaim({
    request,
    claim: { ...claim, device_public_key: generateKeypair().publicKey },
  }), { code: 'conflict' })
  start.noise.readMessage(reply.noise_message)
  finishPairing({ security, start, approvedKey })
  assert.deepEqual(store.getPendingPeer(start.claimId).publicKey, Buffer.from(approvedKey))

  const mismatched = pairingStart({ claimId: 'wrong-key' })
  const mismatchedKey = generateKeypair().publicKey
  const otherRequest = { device_id: mismatched.deviceId, protocol_version: 1 }
  const otherReply = security.handlePairingClaim({
    request: otherRequest,
    claim: helloClaim(mismatched, { device_public_key: mismatchedKey }),
  })
  mismatched.noise.readMessage(otherReply.noise_message)
  assert.throws(() => finishPairing({
    security,
    start: mismatched,
    approvedKey: mismatchedKey,
  }), { code: 'unauthorized' })
  assert.equal(store.getPendingPeer(mismatched.claimId), null)
})

test('when a pairing token, claim, version, fingerprint, or step order is invalid, pairing is rejected', () => {
  const { security } = makeSecurity()
  const start = pairingStart()
  const request = { device_id: start.deviceId, protocol_version: 1 }
  assert.throws(() => security.handlePairingClaim({ request, claim: helloClaim(start, { token_hash: 'zz' }) }), (error) => error.code === 'invalid_argument')
  assert.throws(() => security.handlePairingClaim({ request, claim: helloClaim(start, { claim_id: '' }) }), (error) => error.code === 'invalid_argument')
  assert.throws(() => security.handlePairingClaim({ request, claim: helloClaim(start, { expires_at_unix_ms: 1 }) }), (error) => error.code === 'expired')
  assert.throws(() => security.handlePairingClaim({ request, claim: helloClaim(start, { noise_message: new Uint8Array(0) }) }), (error) => error.code === 'invalid_argument')
  assert.throws(() => security.handlePairingClaim({ request, claim: helloClaim(start, { phase: 'PAIRING_PHASE_UNSPECIFIED' }) }), (error) => error.code === 'invalid_argument')
  assert.throws(() => security.handlePairingClaim({ request, claim: helloClaim(start, { device_id: 'other' }) }), (error) => error.code === 'forbidden')
  assert.throws(() => security.handlePairingClaim({ request: { device_id: 'other', protocol_version: 1 }, claim: helloClaim(start) }), (error) => error.code === 'forbidden')

  const acceptedHello = security.handlePairingClaim({ request, claim: helloClaim(start) })
  start.noise.readMessage(acceptedHello.noise_message)
  assert.throws(() => security.handlePairingClaim({ request, claim: helloClaim(start) }), (error) => error.code === 'conflict')
  assert.throws(() => finishPairing({ security, start, fingerprintOverride: '00' }), (error) => error.code === 'unauthorized')
  assert.throws(() => security.handlePairingClaim({
    request,
    claim: { claim_id: start.claimId, phase: 'PAIRING_PHASE_FINISH', noise_message: new Uint8Array(1), device_id: start.deviceId },
  }), (error) => error.code === 'invalid_argument')
  assert.throws(() => security.handlePairingClaim({
    request,
    claim: { claim_id: 'missing', phase: 'PAIRING_PHASE_AUTHORIZE', device_id: start.deviceId, device_public_key: start.identity.publicKey },
  }), (error) => error.code === 'unauthorized')
})

test('when pairing identity mismatches or revocation is pending, the manager blocks authorization', () => {
  const { security } = makeSecurity()
  const start = pairingStart()
  const wrongHostNoise = new NoiseXX({
    initiator: true,
    staticKeypair: start.identity,
    prologue: pairingPrologue({ hostId, tokenHash: start.tokenHash, claimId: start.claimId }),
  })
  const wrongHostHello = codec.encode('whalerein.v1.PairingHello', {
    device_id: start.deviceId,
    host_id: 'other-host',
    protocol_version: 1,
  })
  assert.throws(() => security.handlePairingClaim({
    request: { device_id: start.deviceId, protocol_version: 1 },
    claim: helloClaim(start, { noise_message: wrongHostNoise.writeMessage(wrongHostHello) }),
  }), (error) => error.code === 'forbidden')

  const wrongVersionNoise = new NoiseXX({
    initiator: true,
    staticKeypair: start.identity,
    prologue: pairingPrologue({ hostId, tokenHash: start.tokenHash, claimId: start.claimId }),
  })
  const wrongVersionHello = codec.encode('whalerein.v1.PairingHello', {
    device_id: start.deviceId,
    host_id: hostId,
    protocol_version: 2,
  })
  assert.throws(() => security.handlePairingClaim({
    request: { device_id: start.deviceId, protocol_version: 1 },
    claim: helloClaim(start, { noise_message: wrongVersionNoise.writeMessage(wrongVersionHello) }),
  }), (error) => error.code === 'protocol_version_incompatible')

  security.setRevocationsPending(true)
  assert.throws(() => security.handlePairingClaim({ request: { device_id: start.deviceId, protocol_version: 1 }, claim: helloClaim(start) }), (error) => error.code === 'revocations_pending')
  security.setRevocationsPending(false)

  assert.throws(() => security.handlePairingClaim({
    request: { device_id: start.deviceId, protocol_version: 1 },
    claim: helloClaim(start, { noise_message: new Uint8Array(32) }),
  }), (error) => error.code === 'invalid_argument')
})

test('when AUTHORIZE has a mismatched device key or expired claim, the manager rejects it', () => {
  let now = 1000
  const { security } = makeSecurity({ now: () => now, pendingTtlMs: 10 })
  const start = pairingStart()
  const request = { device_id: start.deviceId, protocol_version: 1 }
  security.handlePairingClaim({ request, claim: helloClaim(start) })
  now += 20
  assert.throws(() => security.handlePairingClaim({
    request,
    claim: { claim_id: start.claimId, phase: 'PAIRING_PHASE_FINISH', noise_message: new Uint8Array(1), device_id: start.deviceId },
  }), (error) => error.code === 'invalid_argument')
  const start2 = pairingStart({ claimId: 'claim-2' })
  const hello2 = security.handlePairingClaim({ request, claim: helloClaim(start2) })
  start2.noise.readMessage(hello2.noise_message)
  finishPairing({ security, start: start2 })
  assert.throws(() => security.handlePairingClaim({
    request,
    claim: { claim_id: start2.claimId, phase: 'PAIRING_PHASE_AUTHORIZE', device_id: start2.deviceId, device_public_key: generateKeypair().publicKey },
  }), (error) => error.code === 'unauthorized')
})

test('when pending pairing claims exceed the limit, the manager rejects additional claims', () => {
  const { security } = makeSecurity({ maxPending: 1 })
  const first = pairingStart()
  const second = pairingStart({ claimId: 'claim-2' })
  const request = { device_id: first.deviceId, protocol_version: 1 }
  security.handlePairingClaim({ request, claim: helloClaim(first) })
  assert.throws(() => security.handlePairingClaim({ request, claim: helloClaim(second) }), (error) => error.code === 'rate_limited')
})

test('when a channel handshake succeeds, it installs the envelope ID and rejects invalid peers or versions', () => {
  const { security, store } = makeSecurity()
  const channel = establishChannel({ security, store, hostId, deviceId: 'device-1', codec })
  assert.equal(channel.ready.device_id, 'device-1')
  assert.equal(security.listChannels().length, 1)
  assert.equal(security.listChannels()[0].channelId, channel.channelId)
  assert.equal(security.getChannel(channel.channelId).channelId, channel.channelId)
  assert.equal(security.getChannel('missing'), null)

  assert.throws(() => establishChannel({
    security,
    store,
    hostId,
    deviceId: 'device-2',
    codec,
    persistPeer: false,
  }), (error) => error.code === 'unauthorized')

  // Peer exists but is marked revoked: forbidden.
  store.upsertPeerKey({ deviceId: 'device-3', publicKey: generateKeypair().publicKey, fingerprint: 'f' })
  const original = store.isRevoked
  store.isRevoked = () => true
  assert.throws(() => establishChannel({ security, store, hostId, deviceId: 'device-3', codec, persistPeer: false }), (error) => error.code === 'forbidden')
  store.isRevoked = original

  const identity = generateKeypair()
  store.upsertPeerKey({ deviceId: 'device-4', publicKey: identity.publicKey, fingerprint: 'f' })
  const channelId = 'channel-v2'
  const noise = new NoiseXX({ initiator: true, staticKeypair: identity, prologue: channelPrologue({ hostId, deviceId: 'device-4', channelId }) })
  const hello = codec.encode('whalerein.v1.ChannelHello', {
    device_id: 'device-4',
    host_id: hostId,
    protocol_version: 2,
  })
  assert.throws(() => security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test',
    channel_id: channelId, device_id: 'device-4', protocol_version: 1,
    body: noise.writeMessage(hello),
  }), (error) => error.code === 'protocol_version_incompatible')
})

test('when revocation replay, message shape, or TTL checks fail, the channel handshake is rejected', () => {
  let now = 1000
  const { security, store } = makeSecurity({ now: () => now, pendingTtlMs: 10 })
  const identity = generateKeypair()
  persistPeerWithGrant(security, store, { deviceId: 'device-1', identity })
  const channelId = 'channel-1'
  const noise = new NoiseXX({ initiator: true, staticKeypair: identity, prologue: channelPrologue({ hostId, deviceId: 'device-1', channelId }) })
  const hello = codec.encode('whalerein.v1.ChannelHello', { device_id: 'device-1', host_id: hostId, protocol_version: 1 })

  security.setRevocationsPending(true)
  assert.throws(() => security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test',
    channel_id: channelId, device_id: 'device-1',
    body: noise.writeMessage(hello),
  }), (error) => error.code === 'revocations_pending')
  security.setRevocationsPending(false)

  assert.throws(() => security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test', channel_id: '', device_id: 'device-1', body: hello }), (error) => error.code === 'invalid_argument')
  assert.throws(() => security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test', channel_id: channelId, device_id: 'device-1', body: new Uint8Array(0) }), (error) => error.code === 'invalid_argument')

  const noise2 = new NoiseXX({ initiator: true, staticKeypair: identity, prologue: channelPrologue({ hostId, deviceId: 'device-1', channelId }) })
  security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test',
    channel_id: channelId, device_id: 'device-1',
    body: noise2.writeMessage(hello),
  })
  now += 20
  assert.throws(() => security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test',
    channel_id: channelId, device_id: 'device-1',
    body: new Uint8Array(1),
  }), (error) => error.code === 'unauthorized')
})

test('when request frame channel/MAC/AAD checks pass, results are sealed to that same channel', async () => {
  const { security, store } = makeSecurity()
  const device = establishChannel({ security, store, hostId, deviceId: 'device-1', codec })
  const request = {
    request_id: 'request-1',
    host_id: hostId,
    device_id: 'device-1',
    kind: 'REQUEST_KIND_QUERY',
    command_id: '',
    expires_at_unix_ms: 5000,
    channel_id: device.channelId,
    protocol_version: 1,
    relay_version: 5,
    connection_ref: device.connectionRef,
    grant_generation: device.grantGeneration,
    enrollment_generation: device.enrollmentGeneration,
  }
  const aad = requestAad({
    requestId: request.request_id,
    hostId,
    deviceId: 'device-1',
    kind: request.kind,
    commandId: '',
    expiresAt: request.expires_at_unix_ms,
  })
  const plaintext = new TextEncoder().encode('payload')
  const sealed = device.cipher.seal('c2h', aad, plaintext)
  const body = codec.encode('whalerein.v1.EncryptedFrame', {
    channel_id: device.channelId,
    nonce: sealed.nonce,
    ciphertext: sealed.ciphertext,
  })
  const authenticate = (request, body) => security.withRequest({ ...request, body }, result => result)
  const opened = await authenticate(request, body)
  assert.equal(opened.ok, true)
  // A durable command has no Access-filled query scope; absent generations
  // must not be treated as a zero generation mismatch.
  const command = { ...request, kind: 'REQUEST_KIND_COMMAND', command_id: 'c-1' }
  const commandAad = requestAad({
    requestId: command.request_id,
    hostId,
    deviceId: 'device-1',
    kind: command.kind,
    commandId: command.command_id,
    expiresAt: command.expires_at_unix_ms,
  })
  const commandSealed = device.cipher.seal('c2h', commandAad, plaintext)
  const commandBody = codec.encode('whalerein.v1.EncryptedFrame', {
    channel_id: device.channelId,
    nonce: commandSealed.nonce,
    ciphertext: commandSealed.ciphertext,
  })
  delete command.grant_generation
  delete command.enrollment_generation
  const commandOpened = await authenticate(command, commandBody)
  assert.equal(commandOpened.ok, true)
  assert.deepEqual([...opened.plaintext], [...plaintext])

  assert.equal((await authenticate({ ...request, channel_id: 'other' }, body)).ok, false)
  assert.equal((await authenticate({ ...request, device_id: 'other' }, body)).ok, false)
  assert.equal((await authenticate(request, new Uint8Array([1, 2]))).ok, false)
  const tampered = new Uint8Array(body)
  tampered[tampered.length - 1] ^= 1
  assert.equal((await authenticate(request, tampered)).ok, false)

  const resultBody = security.sealResult({ request, plaintext: new Uint8Array([4, 5]) })
  const frame = codec.decode('whalerein.v1.EncryptedFrame', resultBody)
  const resultPlain = openFrame({ cipher: device.cipher, direction: 'h2c', aad, nonce: frame.nonce, ciphertext: frame.ciphertext })
  assert.deepEqual([...resultPlain], [4, 5])
  assert.equal(security.sealResult({ request: { ...request, channel_id: 'other' }, plaintext }), null)

  const eventPlaintext = codec.encode('whalerein.v1.EventPayload', { session_id: 's1' })
  const sealedEvent = security.sealEvent({ deviceId: 'device-1', channelId: device.channelId, plaintext: eventPlaintext, createdAt: 900 })
  const eventPlain = openFrame({ cipher: device.cipher, direction: 'h2c', aad: eventAad({ hostId, deviceId: 'device-1', channelId: device.channelId, createdAt: 900 }), nonce: sealedEvent.nonce, ciphertext: sealedEvent.ciphertext })
  assert.deepEqual([...eventPlain], [...eventPlaintext])
  assert.equal(security.sealEvent({ deviceId: 'device-1', channelId: 'other', plaintext: eventPlaintext, createdAt: 900 }), null)
})

test('when a record is replayed or uses a retired nonce, the channel rejects it without side effects', async () => {
  const { security, store } = makeSecurity()
  const device = establishChannel({ security, store, hostId, deviceId: 'device-1', codec })
  const request = {
    request_id: 'request-replay',
    host_id: hostId,
    device_id: 'device-1',
    kind: 'REQUEST_KIND_QUERY',
    command_id: '',
    expires_at_unix_ms: 5000,
    channel_id: device.channelId,
    protocol_version: 1,
    relay_version: 5,
    connection_ref: device.connectionRef,
    grant_generation: device.grantGeneration,
    enrollment_generation: device.enrollmentGeneration,
  }
  const aad = requestAad({
    requestId: request.request_id, hostId, deviceId: 'device-1', kind: request.kind,
    commandId: '', expiresAt: request.expires_at_unix_ms,
  })
  const sealed = device.cipher.seal('c2h', aad, new TextEncoder().encode('payload'))
  const body = codec.encode('whalerein.v1.EncryptedFrame', {
    channel_id: device.channelId, nonce: sealed.nonce, ciphertext: sealed.ciphertext,
  })
  const authenticate = (request, body) => security.withRequest({ ...request, body }, result => result)
  assert.equal((await authenticate(request, body)).ok, true)
  const replay = await authenticate(request, body)
  assert.equal(replay.ok, false)
  assert.equal(replay.reason, 'replay_detected')
  assert.equal(replay.plaintext, undefined, 'a replayed frame releases no plaintext')

  const frame = codec.decode('whalerein.v1.EncryptedFrame', body)
  frame.nonce = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12])
  const retired = codec.encode('whalerein.v1.EncryptedFrame', frame)
  const rejected = await authenticate({ ...request, request_id: 'request-legacy' }, retired)
  assert.equal(rejected.ok, false)
  assert.equal(rejected.reason, 'invalid_nonce')
  assert.equal(rejected.plaintext, undefined, 'a retired nonce releases no plaintext')
})

test('a query admitted on a retired client key is rejected after rotation', async () => {
  const { security, store } = makeSecurity()
  const retiredIdentity = generateKeypair()
  const retired = establishChannel({ security, store, hostId, deviceId: 'device-1', codec, identity: retiredIdentity })
  const rotated = establishChannel({
    security, store, hostId, deviceId: 'device-1', codec,
    identity: generateKeypair(), connectionRef: '0123456789abcdef:2',
  })
  const sealedQuery = (channel, requestId) => {
    const request = {
      request_id: requestId, host_id: hostId, device_id: 'device-1', kind: 'REQUEST_KIND_QUERY',
      command_id: '', expires_at_unix_ms: 5000, channel_id: channel.channelId,
      protocol_version: 1, relay_version: 5, connection_ref: channel.connectionRef,
      grant_generation: channel.grantGeneration, enrollment_generation: channel.enrollmentGeneration,
    }
    const aad = requestAad({
      requestId, hostId, deviceId: 'device-1', kind: request.kind, commandId: '',
      expiresAt: request.expires_at_unix_ms,
    })
    const frame = channel.cipher.seal('c2h', aad, new TextEncoder().encode('payload'))
    return {
      request,
      body: codec.encode('whalerein.v1.EncryptedFrame', {
        channel_id: channel.channelId, nonce: frame.nonce, ciphertext: frame.ciphertext,
      }),
    }
  }
  const authenticate = ({ request, body }) => security.withRequest({ ...request, body }, result => result)
  const inFlight = sealedQuery(retired, 'request-retired')
  // The Broker connection that owned the retired key closes while its query is
  // still in flight.
  security.closeConnection('device-1', retired.connectionRef)
  const rejected = await authenticate(inFlight)
  assert.equal(rejected.ok, false, 'a retired key cannot deliver its in-flight query')
  assert.equal(rejected.code, 'unauthorized')
  const current = await authenticate(sealedQuery(rotated, 'request-rotated'))
  assert.equal(current.ok, true, 'the rotated key keeps serving its own query')
})

test('channel admission bounds preserve active channels and release idle capacity', (t) => {
  let now = 1000
  const closures = []
  const { security, store } = makeSecurity({
    now: () => now,
    channelIdleMs: 100,
    maxDeviceChannels: 2,
    maxHostChannels: 3,
    onClosedChannels: (channels, reason) => closures.push({ channels, reason }),
  })
  t.after(() => store.close())
  const options = { security, store, hostId, deviceId: 'device-1', identity: generateKeypair(), codec }
  const first = establishChannel(options)
  now += 50
  const second = establishChannel(options)
  assert.throws(() => establishChannel(options), error => error.code === 'rate_limited')
  assert.throws(() => establishChannel({ ...options, channelId: second.channelId }), error => error.code === 'conflict')
  const other = establishChannel({ ...options, deviceId: 'device-2', identity: generateKeypair() })
  assert.throws(() => establishChannel({ ...options, deviceId: 'device-3', identity: generateKeypair() }), error => error.code === 'rate_limited')
  assert.deepEqual(security.listChannels().map(channel => channel.channelId), [first.channelId, second.channelId, other.channelId])

  now += 50
  const fresh = establishChannel(options)
  assert.deepEqual(closures, [{
    channels: [{ deviceId: 'device-1', channelId: first.channelId }],
    reason: 'channel_replaced',
  }])
  assert.equal(security.getChannel(first.channelId), null)
  security.clearChannels()
  assert.deepEqual(closures[1], {
    channels: [
      { deviceId: 'device-1', channelId: second.channelId },
      { deviceId: 'device-2', channelId: other.channelId },
      { deviceId: 'device-1', channelId: fresh.channelId },
    ],
    reason: 'unavailable',
  })
  assert.equal(security.listChannels().length, 0)
  security.clearChannels()
  assert.equal(closures.length, 2)
})

test('when revocation notices arrive, peers/channels are removed and replay ends only on the final notice', () => {
  const { security, store } = makeSecurity()
  const channel = establishChannel({ security, store, hostId, deviceId: 'device-1', codec })
  const sibling = establishChannel({ security, store, hostId, deviceId: 'device-1', identity: channel.identity, codec })
  security.setRevocationsPending(true)
  const partial = security.handleRevocation({
    notice: { replay_id: 'r1', device_ids: ['device-1'], final: false, revoked_at_unix_ms: 5 },
  })
  assert.deepEqual(partial.ack.device_ids, ['device-1'])
  assert.equal(partial.final, false)
  assert.equal(security.revocationsPending, true)
  assert.equal(store.getPeerKey('device-1'), null)
  assert.equal(security.getChannel(channel.channelId), null)
  assert.equal(security.getChannel(sibling.channelId), null)
  assert.deepEqual(partial.closed, [channel, sibling].map(({ channelId }) => ({ deviceId: 'device-1', channelId })))
  const final = security.handleRevocation({ notice: { replay_id: 'r2', device_ids: [], final: true } })
  assert.equal(final.final, true)
  assert.equal(security.revocationsPending, false)
  assert.equal(store.countRevocations(), 1)
})

test('after Access-confirmed host revocation, local peer cleanup closes its channel and marks it revoked without ending replay', () => {
  const notifications = []
  const { security, store } = makeSecurity({ onRevokedChannels: channels => notifications.push(channels) })
  const channel = establishChannel({ security, store, hostId, deviceId: 'device-1', codec })
  security.setRevocationsPending(true)
  const dropped = security.forgetPairedDevice('device-1')
  assert.deepEqual(dropped.closed, [{ deviceId: 'device-1', channelId: channel.channelId }])
  assert.deepEqual(notifications, [dropped.closed])
  assert.equal(store.getPeerKey('device-1'), null)
  assert.equal(store.isRevoked('device-1'), true)
  assert.equal(security.revocationsPending, true)
  assert.equal(security.getChannel(channel.channelId), null)
  const reconnect = new NoiseXX({ initiator: true, staticKeypair: channel.identity, prologue: channelPrologue({ hostId, deviceId: 'device-1', channelId: 'revoked-reconnect' }) })
  security.setRevocationsPending(false)
  assert.throws(() => security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test',
    channel_id: 'revoked-reconnect', device_id: 'device-1', protocol_version: 1,
    body: reconnect.writeMessage(codec.encode('whalerein.v1.ChannelHello', { host_id: hostId, device_id: 'device-1', protocol_version: 1 })),
  }), error => error.code === 'unauthorized' || error.code === 'forbidden')
  assert.equal(security.forgetPairedDevice(''), null)
})

test('when host identity is created, credentials retain the private key and SQLite retains only the pinned public identity', async () => {
  const store = new StateStore({ path: ':memory:' })
  const adapter = new MemoryHostCredentials({ store })
  const identity = await loadOrCreateHostIdentity({ credentials: adapter, store })
  assert.equal(identity.privateKey.length, 32)
  assert.equal(identity.publicKey.length, 32)
  assert.equal(store.getHostPublicKey().fingerprint, identity.fingerprint)
  assert.equal(await adapter.getCredential(HOST_KEY_CREDENTIAL).then((value) => typeof value), 'string')
  const again = await loadOrCreateHostIdentity({ credentials: adapter, store })
  assert.deepEqual([...again.publicKey], [...identity.publicKey])

  const emptyStore = new StateStore({ path: ':memory:' })
  emptyStore.saveHostPublicKey(identity.publicKey, identity.fingerprint)
  await assert.rejects(loadOrCreateHostIdentity({ credentials: new MemoryHostCredentials(), store: emptyStore }), (error) => {
    assert.equal(error.code, 'internal')
    assert.match(error.message, /restore the credential store/)
    assert.match(error.message, /revoke every paired device/)
    assert.match(error.message, /do not delete the state database/)
    assert.doesNotMatch(error.message, /[A-Za-z0-9+/]{40,}/)
    return true
  })

  const otherIdentity = generateKeypair()
  const otherAdapter = new MemoryHostCredentials()
  await otherAdapter.setCredential(HOST_KEY_CREDENTIAL, Buffer.from(otherIdentity.privateKey).toString('base64'))
  const mismatchStore = new StateStore({ path: ':memory:' })
  mismatchStore.saveHostPublicKey(identity.publicKey, identity.fingerprint)
  await assert.rejects(loadOrCreateHostIdentity({ credentials: otherAdapter, store: mismatchStore }), (error) => {
    assert.equal(error.code, 'internal')
    assert.match(error.message, /does not match the recorded public key/)
    assert.match(error.message, /revoke every paired device/)
    return true
  })

  const bytesAdapter = new MemoryHostCredentials()
  const bytesIdentity = generateKeypair()
  await bytesAdapter.setCredential(HOST_KEY_CREDENTIAL, bytesIdentity.privateKey)
  const bytesLoaded = await loadOrCreateHostIdentity({ credentials: bytesAdapter, store: new StateStore({ path: ':memory:' }) })
  assert.deepEqual([...bytesLoaded.publicKey], [...bytesIdentity.publicKey])

  const badAdapter = new MemoryHostCredentials()
  await badAdapter.setCredential(HOST_KEY_CREDENTIAL, 'short')
  await assert.rejects(loadOrCreateHostIdentity({ credentials: badAdapter, store: new StateStore({ path: ':memory:' }) }), (error) => error.code === 'internal')
})

test('when a prepared pairing survives manager restart, repeated COMMIT persists it only once', () => {
  const first=makeSecurity()
  const start=pairingStart()
  const request={device_id:start.deviceId,protocol_version:1}
  const hello=first.security.handlePairingClaim({request,claim:helloClaim(start)})
  start.noise.readMessage(hello.noise_message)
  finishPairing({security:first.security,start})
  const claim={claim_id:start.claimId,phase:'PAIRING_PHASE_AUTHORIZE',device_id:start.deviceId,device_public_key:start.identity.publicKey}
  first.security.handlePairingClaim({request,claim})
  assert.equal(first.store.getPeerKey(start.deviceId),null)
  const restarted=new SecurityManager({hostId,identity:first.identity,store:first.store,codec,now:()=>1000})
  const committed={...claim,phase:'PAIRING_PHASE_COMMIT'}
  assert.equal(restarted.handlePairingClaim({request,claim:committed}).authorized,true)
  assert.equal(restarted.handlePairingClaim({request,claim:committed}).authorized,true)
  assert.equal(first.store.countPeerKeys(),1)
  assert.throws(()=>restarted.handlePairingClaim({request,claim:{...committed,device_public_key:generateKeypair().publicKey}}),error=>error.code==='unauthorized')
})

test('when revocation arrives before delayed pairing commit, the prepared pairing is cancelled', () => {
  const first=makeSecurity();const start=pairingStart();const request={device_id:start.deviceId,protocol_version:1}
  const hello=first.security.handlePairingClaim({request,claim:helloClaim(start)});start.noise.readMessage(hello.noise_message)
  finishPairing({security:first.security,start})
  first.security.handleRevocation({notice:{replay_id:'cancel',device_ids:[start.deviceId],final:true,revoked_at_unix_ms:1000}})
  assert.throws(()=>first.security.handlePairingClaim({request,claim:{claim_id:start.claimId,phase:'PAIRING_PHASE_COMMIT',device_id:start.deviceId,device_public_key:start.identity.publicKey}}),error=>error.code==='unauthorized')
})


test('a revoked grant cannot reactivate and stale revocations do not clear a repaired channel',()=>{
 const {security,store}=makeSecurity()
 try {
  const start=pairingStart()
  const request={device_id:start.deviceId,protocol_version:1}
  const hello=security.handlePairingClaim({request,claim:helloClaim(start)})
  start.noise.readMessage(hello.noise_message)
  finishPairing({security,start})
  const commit={claim_id:start.claimId,phase:'PAIRING_PHASE_COMMIT',device_id:start.deviceId,device_public_key:start.identity.publicKey,grant_generation:1}
  security.handlePairingClaim({request,claim:commit})
  const channelOptions={security,store,hostId,deviceId:start.deviceId,identity:start.identity,persistPeer:false}
  const previous=establishChannel(channelOptions)
  // A fresh re-enrollment requires the old Host grant to be revoked first; one
  // active grant per device key is the durable invariant.
  security.revokeGrants({grantIds:[store.getPeerKey(start.deviceId).grantId]})
  assert.equal(security.getChannel(previous.channelId),null)
  const repair=pairingStart({deviceId:start.deviceId,identity:start.identity,claimId:'repair'})
  const repairHello=security.handlePairingClaim({request,claim:helloClaim(repair)})
  repair.noise.readMessage(repairHello.noise_message)
  finishPairing({security,start:repair})
  security.handlePairingClaim({request,claim:{...commit,claim_id:repair.claimId,grant_generation:2}})
  assert.equal(security.getChannel(previous.channelId),null)
  const current=establishChannel(channelOptions)
  const stale=security.handleRevocation({notice:{device_ids:[start.deviceId],grant_generations:{[start.deviceId]:1}}})
  assert.deepEqual(stale.applied,[])
  assert.deepEqual(stale.ack.device_ids,[start.deviceId])
  assert.equal(store.getPeerKey(start.deviceId).grantGeneration,2)
  assert.ok(security.getChannel(current.channelId))
  assert.throws(()=>security.handlePairingClaim({request,claim:commit}),error=>error.code==='unauthorized')
 } finally {store.close()}
})

test('a staged FINISH survives manager restart and rejects a changed credential hash', () => {
  const {store,identity,security}=makeSecurity()
  try {
    const start=pairingStart()
    const hello=security.handlePairingClaim({request:{},claim:helloClaim(start)})
    start.noise.readMessage(hello.noise_message)
    const authHash=new Uint8Array(32).fill(7)
    const finish=codec.encode('whalerein.v1.PairingFinish',{
      device_id:start.deviceId,host_id:hostId,host_fingerprint:security.hostFingerprint,device_auth_hash:authHash,
    })
    const claim={claim_id:start.claimId,token_hash:start.tokenHash,device_id:start.deviceId,
      phase:'PAIRING_PHASE_FINISH',noise_message:start.noise.writeMessage(finish),device_auth_hash:authHash}
    const first=security.handlePairingClaim({request:{},claim})
    const restarted=new SecurityManager({hostId,identity,store,codec,now:()=>1000})
    const replay=restarted.handlePairingClaim({request:{},claim})
    assert.deepEqual(Buffer.from(replay.device_public_key),Buffer.from(first.device_public_key))
    assert.deepEqual(Buffer.from(replay.device_auth_hash),Buffer.from(first.device_auth_hash))
    assert.throws(()=>restarted.handlePairingClaim({request:{},claim:{...claim,device_auth_hash:new Uint8Array(32)}}),/binding mismatch/)
    assert.equal(store.countPeerKeys(),0)
  } finally {store.close()}
})

test('Noise FINISH cannot authorize a substituted transport credential', () => {
  const {store,security}=makeSecurity()
  try {
    const start=pairingStart()
    const hello=security.handlePairingClaim({request:{},claim:helloClaim(start)})
    start.noise.readMessage(hello.noise_message)
    const finish=codec.encode('whalerein.v1.PairingFinish',{
      device_id:start.deviceId,host_id:hostId,host_fingerprint:security.hostFingerprint,device_auth_hash:new Uint8Array(32).fill(7),
    })
    assert.throws(()=>security.handlePairingClaim({request:{},claim:{claim_id:start.claimId,token_hash:start.tokenHash,
      device_id:start.deviceId,phase:'PAIRING_PHASE_FINISH',noise_message:start.noise.writeMessage(finish),device_auth_hash:new Uint8Array(32).fill(9)}}),/binding mismatch/)
    assert.equal(store.getPendingPeer(start.claimId),null)
  } finally {store.close()}
})

test('an expired staged peer cannot be activated by a late COMMIT after restart', () => {
  let now = 1000
  const first = makeSecurity({now:()=>now, pendingTtlMs:50})
  const start = pairingStart()
  const request = {device_id:start.deviceId,protocol_version:1}
  const hello = first.security.handlePairingClaim({request,claim:helloClaim(start)})
  start.noise.readMessage(hello.noise_message)
  finishPairing({security:first.security,start})
  const commit = {claim_id:start.claimId,phase:'PAIRING_PHASE_COMMIT',device_id:start.deviceId,device_public_key:start.identity.publicKey,grant_generation:1}
  now += 51
  const restarted = new SecurityManager({hostId,identity:first.identity,store:first.store,codec,now:()=>now})
  assert.throws(()=>restarted.handlePairingClaim({request,claim:commit}), error=>error.code==='unauthorized')
  assert.equal(first.store.getPeerKey(start.deviceId), null)
})

test('a device receive advertisement keeps its own channel-scoped generation, independent of the Host broker generation', () => {
  const store = new StateStore({ path: ':memory:', now: () => 1000 })
  const identity = generateKeypair()
  const security = new SecurityManager({
    hostId, identity, store, codec, now: () => 1000,
    receiveAdvertisement: () => defaultReceiveAdvertisement(7),
  })
  try {
    const compact = establishChannel({
      security, store, hostId, deviceId: 'device-1', codec,
      receive: {
        profile: 'TRANSPORT_PROFILE_COMPACT',
        budget: transportProfileBudget('compact'),
        connection_generation: 3,
        relay_version: 5,
      },
    })
    // HostReady's broker generation (7) is never compared with the device's
    // channel-scoped generation (3), and ChannelReady mints its own value.
    assert.equal(compact.ready.receive.connection_generation, 1)
    assert.equal(compact.ready.receive.profile, 'TRANSPORT_PROFILE_DEFAULT')
    assert.equal(compact.ready.receive.budget.envelope_bytes, 16 * 1024 * 1024)
    const bound = security.getChannelReceive(compact.channelId)
    assert.equal(bound.connection_generation, 3)
    assert.equal(bound.profile, 'TRANSPORT_PROFILE_COMPACT')
    assert.equal(bound.budget.envelope_bytes, 8 * 1024 * 1024)
    assert.throws(() => { bound.budget.envelope_bytes = 1 })
    assert.throws(() => { bound.connection_generation = 4 })
    assert.equal(security.getChannelReceive(compact.channelId).budget.envelope_bytes, 8 * 1024 * 1024,
      'an installed advertisement is immutable')
  } finally {
    store.close()
  }
})

test('a reconnect installs a fresh advertisement and old channel data never refreshes a newer channel', () => {
  const { store, security } = makeSecurity()
  try {
    const first = establishChannel({
      security, store, hostId, deviceId: 'device-1', codec,
      receive: {
        profile: 'TRANSPORT_PROFILE_COMPACT',
        budget: transportProfileBudget('compact'),
        connection_generation: 11,
        relay_version: 5,
      },
    })
    const second = establishChannel({
      security, store, hostId, deviceId: 'device-1', codec,
      receive: {
        profile: 'TRANSPORT_PROFILE_ROOMY',
        budget: transportProfileBudget('roomy'),
        connection_generation: 12,
        relay_version: 5,
      },
    })
    assert.notEqual(first.ready.receive.connection_generation, second.ready.receive.connection_generation,
      'each channel handshake mints its own Host generation')
    assert.equal(security.getChannelReceive(first.channelId).connection_generation, 11)
    assert.equal(security.getChannelReceive(first.channelId).budget.envelope_bytes, 8 * 1024 * 1024)
    assert.equal(security.getChannelReceive(second.channelId).connection_generation, 12)
    assert.equal(security.getChannelReceive(second.channelId).budget.envelope_bytes, 32 * 1024 * 1024)

    // A new channel that re-sends the older generation still binds that value
    // to itself; it can never refresh the newer channel.
    const stale = establishChannel({
      security, store, hostId, deviceId: 'device-1', codec,
      receive: {
        profile: 'TRANSPORT_PROFILE_COMPACT',
        budget: transportProfileBudget('compact'),
        connection_generation: 11,
        relay_version: 5,
      },
    })
    assert.equal(security.getChannelReceive(stale.channelId).connection_generation, 11)
    assert.equal(security.getChannelReceive(second.channelId).connection_generation, 12)
    assert.equal(security.getChannelReceive(second.channelId).budget.envelope_bytes, 32 * 1024 * 1024)

    // A reconnect without an advertisement gets no cap; it never inherits the
    // prior channel's frozen budget.
    const absent = establishChannel({ security, store, hostId, deviceId: 'device-1', codec, omitReceive: true })
    assert.equal(security.getChannelReceive(absent.channelId), null)
    assert.equal(security.getChannelReceive(second.channelId).connection_generation, 12)

    // An installed channel identity is never renegotiated in place.
    const replacement = startChannelHello(security, store, { channelId: second.channelId })
    assert.throws(() => security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test',
      channel_id: replacement.channelId, device_id: replacement.deviceId, body: replacement.body,
    }), (error) => error.code === 'conflict')
    assert.equal(security.getChannelReceive(second.channelId).connection_generation, 12)
  } finally {
    store.close()
  }
})

test('an absent channel receive advertisement adds no cap and a malformed one fails before the channel is usable', () => {
  const { store, security } = makeSecurity()
  try {
    const omitted = establishChannel({ security, store, hostId, deviceId: 'device-1', codec, omitReceive: true })
    assert.equal(security.getChannelReceive(omitted.channelId), null)
    const attempt = (receive) => {
      const channelId = `channel-${Math.random()}`
      const identity = generateKeypair()
      persistPeerWithGrant(security, store, { deviceId: 'device-1', identity, claimId: 'claim' })
      const prologue = channelPrologue({ hostId, deviceId: 'device-1', channelId })
      const noise = new NoiseXX({ initiator: true, staticKeypair: identity, prologue })
      const hello = codec.encode('whalerein.v1.ChannelHello', {
        device_id: 'device-1', host_id: hostId, protocol_version: 1,
        capabilities: { capabilities: ['sessions'], protocol_version: 1 }, client_nonce: 'nonce',
        receive,
      })
      return () => security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test', channel_id: channelId, device_id: 'device-1', protocol_version: 1, body: noise.writeMessage(hello) })
    }
    // A per-leg generation is accepted without any cross-leg comparison.
    assert.ok(attempt({ profile: 'TRANSPORT_PROFILE_DEFAULT', budget: transportProfileBudget('default'), connection_generation: 2, relay_version: 5 })() instanceof Uint8Array)
    // Malformed, contradictory, missing and unknown-profile advertisements
    // fail explicitly before the channel becomes usable.
    assert.throws(attempt({ profile: 'TRANSPORT_PROFILE_DEFAULT', connection_generation: 1 }), (error) => error.code === 'invalid_argument')
    assert.throws(attempt({ profile: 'TRANSPORT_PROFILE_DEFAULT', budget: { ...transportProfileBudget('default'), envelope_bytes: 1 }, connection_generation: 1, relay_version: 5 }), (error) => error.code === 'invalid_argument')
    assert.throws(attempt({ profile: 'TRANSPORT_PROFILE_DEFAULT', budget: transportProfileBudget('default'), connection_generation: 0 }), (error) => error.code === 'invalid_argument')
    assert.throws(() => attempt({ profile: 'TRANSPORT_PROFILE_FUTURE', budget: transportProfileBudget('default'), connection_generation: 1 }), (error) => error.code === 'protocol_version_incompatible')
    assert.equal(security.listChannels().length, 1, 'only the advertisement-free channel was installed')
  } finally {
    store.close()
  }
})

function prepareFinish(security, start, { finishOverrides = {}, claimOverrides = {} } = {}) {
  const hello = security.handlePairingClaim({
    request: { device_id: start.deviceId, protocol_version: 1 },
    claim: helloClaim(start),
  })
  start.noise.readMessage(hello.noise_message)
  const authHash = new Uint8Array(32).fill(7)
  const finish = codec.encode('whalerein.v1.PairingFinish', {
    device_id: start.deviceId,
    host_id: hostId,
    host_fingerprint: security.hostFingerprint,
    device_auth_hash: authHash,
    ...finishOverrides,
  })
  return {
    claim_id: start.claimId,
    token_hash: start.tokenHash,
    phase: 'PAIRING_PHASE_FINISH',
    noise_message: start.noise.writeMessage(finish),
    device_auth_hash: authHash,
    device_id: start.deviceId,
    ...claimOverrides,
  }
}

test('a FINISH with an unknown claim, wrong scope, host, or fingerprint is rejected before staging', () => {
  const { security, store } = makeSecurity()
  try {
    assert.throws(() => security.handlePairingClaim({
      request: { device_id: 'device-1', protocol_version: 1 },
      claim: { claim_id: 'unknown', token_hash: 'ab'.repeat(32), phase: 'PAIRING_PHASE_FINISH', noise_message: new Uint8Array(1), device_id: 'device-1' },
    }), (error) => error.code === 'invalid_argument')

    const wrongClaimDevice = pairingStart({ claimId: 'wrong-claim-device' })
    const claim = prepareFinish(security, wrongClaimDevice, { claimOverrides: { device_id: 'other-device' } })
    assert.throws(() => security.handlePairingClaim({
      request: { device_id: wrongClaimDevice.deviceId, protocol_version: 1 }, claim,
    }), (error) => error.code === 'forbidden')

    const wrongRequestDevice = pairingStart({ claimId: 'wrong-request-device' })
    const requestClaim = prepareFinish(security, wrongRequestDevice)
    assert.throws(() => security.handlePairingClaim({
      request: { device_id: 'other-device', protocol_version: 1 }, claim: requestClaim,
    }), (error) => error.code === 'forbidden')

    const wrongFinishDevice = pairingStart({ claimId: 'wrong-finish-device' })
    const finishClaim = prepareFinish(security, wrongFinishDevice, { finishOverrides: { device_id: 'other-device' } })
    assert.throws(() => security.handlePairingClaim({
      request: { device_id: wrongFinishDevice.deviceId, protocol_version: 1 }, claim: finishClaim,
    }), (error) => error.code === 'forbidden')

    const wrongHost = pairingStart({ claimId: 'wrong-host' })
    const hostClaim = prepareFinish(security, wrongHost, { finishOverrides: { host_id: 'other-host' } })
    assert.throws(() => security.handlePairingClaim({
      request: { device_id: wrongHost.deviceId, protocol_version: 1 }, claim: hostClaim,
    }), (error) => error.code === 'forbidden')

    const wrongFingerprint = pairingStart({ claimId: 'wrong-fingerprint' })
    const fingerprintClaim = prepareFinish(security, wrongFingerprint, { finishOverrides: { host_fingerprint: '00' } })
    assert.throws(() => security.handlePairingClaim({
      request: { device_id: wrongFingerprint.deviceId, protocol_version: 1 }, claim: fingerprintClaim,
    }), (error) => error.code === 'unauthorized')

    // A storage failure while staging is surfaced and never leaves a reusable
    // completed entry behind.
    const staging = pairingStart({ claimId: 'staging-failure' })
    const stagingClaim = prepareFinish(security, staging)
    const stagePeer = store.stagePeer
    store.stagePeer = () => { throw new Error('storage unavailable') }
    try {
      assert.throws(() => security.handlePairingClaim({
        request: { device_id: staging.deviceId, protocol_version: 1 }, claim: stagingClaim,
      }), /storage unavailable/)
    } finally {
      store.stagePeer = stagePeer
    }
    assert.equal(store.getPendingPeer(staging.claimId), null)
  } finally {
    store.close()
  }
})

function startChannelHello(security, store, { deviceId = 'device-1', channelId = 'channel-1', identity = generateKeypair(), hello = {} } = {}) {
  persistPeerWithGrant(security, store, { deviceId, identity, claimId: `claim-${deviceId}-${channelId}` })
  const prologue = channelPrologue({ hostId, deviceId, channelId })
  const noise = new NoiseXX({ initiator: true, staticKeypair: identity, prologue })
  const message = codec.encode('whalerein.v1.ChannelHello', {
    device_id: deviceId,
    host_id: hostId,
    protocol_version: 1,
    capabilities: { capabilities: ['sessions'], protocol_version: 1 },
    client_nonce: 'nonce',
    ...hello,
  })
  return { noise, identity, channelId, deviceId, body: noise.writeMessage(message) }
}

test('channel handshake rejects invalid host advertisements, pending limits, scope mismatches, and substituted peers', () => {
  // The Host's own receive advertisement must validate before any channel is
  // admitted; an invalid one is a configuration failure, not a partial channel.
  const badHostStore = new StateStore({ path: ':memory:' })
  const badHost = new SecurityManager({
    hostId, identity: generateKeypair(), store: badHostStore, codec,
    receiveAdvertisement: () => ({ profile: 'BROKEN', budget: {}, connection_generation: 0 }),
  })
  try {
    assert.throws(() => establishChannel({
      security: badHost, store: badHostStore, hostId, deviceId: 'device-1', codec,
    }), (error) => error.code === 'invalid_argument')
    assert.equal(badHost.listChannels().length, 0)
  } finally {
    badHostStore.close()
  }

  const { security, store } = makeSecurity({ maxPending: 1 })
  try {
    const first = startChannelHello(security, store, { channelId: 'channel-pending' })
    security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test', channel_id: first.channelId, device_id: first.deviceId, body: first.body })
    const second = startChannelHello(security, store, { channelId: 'channel-other' })
    assert.throws(() => security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test',
      channel_id: second.channelId, device_id: second.deviceId, body: second.body,
    }), (error) => error.code === 'rate_limited')
  } finally {
    store.close()
  }

  const scoped = makeSecurity()
  try {
    const hello = startChannelHello(scoped.security, scoped.store, { channelId: 'channel-scope' })
    scoped.security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test', channel_id: hello.channelId, device_id: hello.deviceId, body: hello.body })
    scoped.store.upsertPeerKey({ deviceId: 'device-other', publicKey: generateKeypair().publicKey, fingerprint: 'f', claimId: 'other', pairedAt: 1 })
    assert.throws(() => scoped.security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test',
      channel_id: hello.channelId, device_id: 'device-other', body: new Uint8Array([1]),
    }), (error) => error.code === 'forbidden')
  } finally {
    scoped.store.close()
  }

  const mismatchedHello = makeSecurity()
  try {
    const identity = generateKeypair()
    const hello = startChannelHello(mismatchedHello.security, mismatchedHello.store, { channelId: 'channel-id', identity, hello: { device_id: 'device-other' } })
    assert.throws(() => mismatchedHello.security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test',
      channel_id: hello.channelId, device_id: hello.deviceId, body: hello.body,
    }), (error) => error.code === 'forbidden')
  } finally {
    mismatchedHello.store.close()
  }

  // A non-empty handshake payload is a framing error even when Noise verifies.
  const payload = makeSecurity()
  try {
    const hello = startChannelHello(payload.security, payload.store, { channelId: 'channel-payload' })
    const reply = payload.security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test', channel_id: hello.channelId, device_id: hello.deviceId, body: hello.body })
    hello.noise.readMessage(reply)
    assert.throws(() => payload.security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test',
      channel_id: hello.channelId, device_id: hello.deviceId, body: hello.noise.writeMessage(new Uint8Array([1])),
    }), (error) => error.code === 'invalid_argument')
  } finally {
    payload.store.close()
  }

  // The Noise peer must be the stored peer key, not merely any valid key.
  const substituted = makeSecurity()
  try {
    const storedIdentity = generateKeypair()
    const attacker = generateKeypair()
    const hello = startChannelHello(substituted.security, substituted.store, {
      channelId: 'channel-substituted', identity: attacker,
    })
    persistPeerWithGrant(substituted.security, substituted.store, {
      deviceId: hello.deviceId, identity: storedIdentity, claimId: 'stored',
    })
    const reply = substituted.security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test', channel_id: hello.channelId, device_id: hello.deviceId, body: hello.body })
    hello.noise.readMessage(reply)
    assert.throws(() => substituted.security.handleChannelHandshake({ connection_ref: '0123456789abcdef:test',
      channel_id: hello.channelId, device_id: hello.deviceId, body: hello.noise.writeMessage(new Uint8Array(0)),
    }), (error) => error.code === 'unauthorized')
  } finally {
    substituted.store.close()
  }

  const idle = makeSecurity()
  try {
    idle.security.closeChannel('missing-channel')
    assert.equal(idle.security.listChannels().length, 0)
  } finally {
    idle.store.close()
  }
})
