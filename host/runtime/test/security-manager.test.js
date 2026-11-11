import { MemoryHostCredentials } from '../src/security/credentials.js'
import test from 'node:test'
import assert from 'node:assert/strict'
import {
  NoiseXX,
  channelPrologue,
  eventAad,
  fingerprint,
  generateKeypair,
  open,
  pairingPrologue,
  requestAad,
  seal,
} from '../src/security/noise.js'
import { SecurityManager, HOST_CAPABILITIES } from '../src/security/manager.js'
import { loadOrCreateHostIdentity, HOST_KEY_CREDENTIAL } from '../src/security/identity.js'
import { StateStore } from '../src/state/store.js'
import { codec as defaultCodec } from '../src/protocol.js'
import { establishChannel } from './support/channel.js'

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
  })
  return { store, identity, security }
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

function finishPairing({ security, start, request = {}, fingerprintOverride = null }) {
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
  assert.equal(channel.second.established, true)
  assert.equal(channel.second.replacedChannelId, null)
  assert.equal(channel.ready.device_id, 'device-1')
  assert.equal(security.listChannels().length, 1)
  assert.equal(security.listChannels()[0].channelId, channel.channelId)
  assert.equal(security.getChannel(channel.channelId).channelId, channel.channelId)
  assert.equal(security.getChannel('missing'), null)

  const second = establishChannel({ security, store, hostId, deviceId: 'device-1', codec })
  assert.equal(second.second.replacedChannelId, channel.channelId)
  assert.equal(security.listChannels().length, 1)

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
  assert.throws(() => security.handleChannelHandshake({
    request: { channel_id: channelId, device_id: 'device-4', protocol_version: 1 },
    message: noise.writeMessage(hello),
  }), (error) => error.code === 'protocol_version_incompatible')
})

test('when revocation replay, message shape, or TTL checks fail, the channel handshake is rejected', () => {
  let now = 1000
  const { security, store } = makeSecurity({ now: () => now, pendingTtlMs: 10 })
  const identity = generateKeypair()
  store.upsertPeerKey({ deviceId: 'device-1', publicKey: identity.publicKey, fingerprint: 'f' })
  const channelId = 'channel-1'
  const noise = new NoiseXX({ initiator: true, staticKeypair: identity, prologue: channelPrologue({ hostId, deviceId: 'device-1', channelId }) })
  const hello = codec.encode('whalerein.v1.ChannelHello', { device_id: 'device-1', host_id: hostId, protocol_version: 1 })

  security.setRevocationsPending(true)
  assert.throws(() => security.handleChannelHandshake({
    request: { channel_id: channelId, device_id: 'device-1' },
    message: noise.writeMessage(hello),
  }), (error) => error.code === 'revocations_pending')
  security.setRevocationsPending(false)

  assert.throws(() => security.handleChannelHandshake({ request: { channel_id: '', device_id: 'device-1' }, message: hello }), (error) => error.code === 'invalid_argument')
  assert.throws(() => security.handleChannelHandshake({ request: { channel_id: channelId, device_id: 'device-1' }, message: new Uint8Array(0) }), (error) => error.code === 'invalid_argument')

  const noise2 = new NoiseXX({ initiator: true, staticKeypair: identity, prologue: channelPrologue({ hostId, deviceId: 'device-1', channelId }) })
  security.handleChannelHandshake({
    request: { channel_id: channelId, device_id: 'device-1' },
    message: noise2.writeMessage(hello),
  })
  now += 20
  assert.throws(() => security.handleChannelHandshake({
    request: { channel_id: channelId, device_id: 'device-1' },
    message: new Uint8Array(1),
  }), (error) => error.code === 'unauthorized')
})

test('when request frame channel/MAC/AAD checks pass, results are sealed to that same channel', () => {
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
  const sealed = seal(device.c2h, aad, plaintext)
  const body = codec.encode('whalerein.v1.EncryptedFrame', {
    channel_id: device.channelId,
    nonce: sealed.nonce,
    ciphertext: sealed.ciphertext,
  })
  const opened = security.openRequest({ request, body })
  assert.equal(opened.ok, true)
  assert.deepEqual([...opened.plaintext], [...plaintext])

  assert.equal(security.openRequest({ request: { ...request, channel_id: 'other' }, body }).ok, false)
  assert.equal(security.openRequest({ request: { ...request, device_id: 'other' }, body }).ok, false)
  assert.equal(security.openRequest({ request, body: new Uint8Array([1, 2]) }).ok, false)
  const tampered = new Uint8Array(body)
  tampered[tampered.length - 1] ^= 1
  assert.equal(security.openRequest({ request, body: tampered }).ok, false)

  const resultBody = security.sealResult({ request, plaintext: new Uint8Array([4, 5]) })
  const frame = codec.decode('whalerein.v1.EncryptedFrame', resultBody)
  const resultPlain = open(device.h2c, aad, frame.nonce, frame.ciphertext)
  assert.deepEqual([...resultPlain], [4, 5])
  assert.equal(security.sealResult({ request: { ...request, channel_id: 'other' }, plaintext }), null)

  const eventPlaintext = codec.encode('whalerein.v1.EventPayload', { session_id: 's1' })
  const sealedEvent = security.sealEvent({ deviceId: 'device-1', channelId: device.channelId, plaintext: eventPlaintext, createdAt: 900 })
  const eventPlain = open(device.h2c, eventAad({ hostId, deviceId: 'device-1', channelId: device.channelId, createdAt: 900 }), sealedEvent.nonce, sealedEvent.ciphertext)
  assert.deepEqual([...eventPlain], [...eventPlaintext])
  assert.equal(security.sealEvent({ deviceId: 'device-1', channelId: 'other', plaintext: eventPlaintext, createdAt: 900 }), null)
})

test('when a new channel replaces old channels, stale ciphertext is removed and clearChannels closes every channel', () => {
  const { security, store } = makeSecurity()
  const first = establishChannel({ security, store, hostId, deviceId: 'device-1', codec })
  store.addOutbox({ deviceId: 'device-1', channelId: first.channelId, payload: new Uint8Array([1]) })
  const second = establishChannel({ security, store, hostId, deviceId: 'device-1', codec })
  assert.equal(second.second.replacedChannelId, first.channelId)
  assert.equal(store.countOutbox(), 0)
  const closed = security.clearChannels()
  assert.deepEqual(closed, [{ deviceId: 'device-1', channelId: second.channelId }])
  assert.equal(security.listChannels().length, 0)
  assert.deepEqual(security.clearChannels(), [])
})

test('when revocation notices arrive, peers/channels are removed and replay ends only on the final notice', () => {
  const { security, store } = makeSecurity()
  const channel = establishChannel({ security, store, hostId, deviceId: 'device-1', codec })
  security.setRevocationsPending(true)
  const partial = security.handleRevocation({
    notice: { replay_id: 'r1', device_ids: ['device-1'], final: false, revoked_at_unix_ms: 5 },
  })
  assert.deepEqual(partial.ack.device_ids, ['device-1'])
  assert.equal(partial.final, false)
  assert.equal(security.revocationsPending, true)
  assert.equal(store.getPeerKey('device-1'), null)
  assert.equal(security.getChannel(channel.channelId), null)
  assert.deepEqual(partial.closed, [{ deviceId: 'device-1', channelId: channel.channelId }])
  const final = security.handleRevocation({ notice: { replay_id: 'r2', device_ids: [], final: true } })
  assert.equal(final.final, true)
  assert.equal(security.revocationsPending, false)
  assert.equal(store.countRevocations(), 1)
})

test('after Access-confirmed host revocation, local peer cleanup closes its channel and marks it revoked without ending replay', () => {
  const { security, store } = makeSecurity()
  const channel = establishChannel({ security, store, hostId, deviceId: 'device-1', codec })
  security.setRevocationsPending(true)
  const dropped = security.forgetPairedDevice('device-1')
  assert.deepEqual(dropped.closed, [{ deviceId: 'device-1', channelId: channel.channelId }])
  assert.equal(store.getPeerKey('device-1'), null)
  assert.equal(store.isRevoked('device-1'), true)
  assert.equal(security.revocationsPending, true)
  assert.equal(security.getChannel(channel.channelId), null)
  const reconnect = new NoiseXX({ initiator: true, staticKeypair: channel.identity, prologue: channelPrologue({ hostId, deviceId: 'device-1', channelId: 'revoked-reconnect' }) })
  security.setRevocationsPending(false)
  assert.throws(() => security.handleChannelHandshake({
    request: { channel_id: 'revoked-reconnect', device_id: 'device-1', protocol_version: 1 },
    message: reconnect.writeMessage(codec.encode('whalerein.v1.ChannelHello', { host_id: hostId, device_id: 'device-1', protocol_version: 1 })),
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
  await assert.rejects(loadOrCreateHostIdentity({ credentials: new MemoryHostCredentials(), store: emptyStore }), (error) => error.code === 'internal')

  const otherIdentity = generateKeypair()
  const otherAdapter = new MemoryHostCredentials()
  await otherAdapter.setCredential(HOST_KEY_CREDENTIAL, Buffer.from(otherIdentity.privateKey).toString('base64'))
  const mismatchStore = new StateStore({ path: ':memory:' })
  mismatchStore.saveHostPublicKey(identity.publicKey, identity.fingerprint)
  await assert.rejects(loadOrCreateHostIdentity({ credentials: otherAdapter, store: mismatchStore }), (error) => error.code === 'internal')

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
  store.applyRevocations([start.deviceId],{generations:{[start.deviceId]:1}})
  assert.throws(()=>security.handlePairingClaim({request,claim:commit}),error=>error.code==='unauthorized')
  store.upsertPeerKey({deviceId:start.deviceId,publicKey:start.identity.publicKey,fingerprint:'fp',claimId:'repair',grantGeneration:2})
  const stale=security.handleRevocation({notice:{device_ids:[start.deviceId],grant_generations:{[start.deviceId]:1}}})
  assert.deepEqual(stale.applied,[])
  assert.deepEqual(stale.ack.device_ids,[start.deviceId])
  assert.equal(store.getPeerKey(start.deviceId).grantGeneration,2)
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
