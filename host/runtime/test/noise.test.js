import test from 'node:test'
import assert from 'node:assert/strict'
import { inspect } from 'node:util'
import { chacha20poly1305 } from '@noble/ciphers/chacha.js'
import { hkdf } from '@noble/hashes/hkdf.js'
import { sha256 } from '@noble/hashes/sha2.js'
import {
  NoiseXX,
  NoiseError,
  MAX_MESSAGE,
  REPLAY_WINDOW,
  channelPrologue,
  equal,
  eventAad,
  fingerprint,
  formatFingerprint,
  generateKeypair,
  parseKeypair,
  pairingPrologue,
  requestAad,
  unhex,
  hex,
} from '../src/security/noise.js'

function handshake({ prologueA = 'prologue', prologueB = prologueA, testVector = false } = {}) {
  const initiatorStatic = generateKeypair()
  const responderStatic = generateKeypair()
  const initiator = new NoiseXX({
    initiator: true,
    staticKeypair: initiatorStatic,
    prologue: prologueA,
    ...(testVector ? { ephemeralKeypair: generateKeypair() } : {}),
  })
  const responder = new NoiseXX({
    initiator: false,
    staticKeypair: responderStatic,
    prologue: prologueB,
    ...(testVector ? { ephemeralKeypair: generateKeypair() } : {}),
  })
  const fromInitiator = initiator.writeMessage(new TextEncoder().encode('hello'))
  const hello = responder.readMessage(fromInitiator)
  const fromResponder = responder.writeMessage(new TextEncoder().encode('ready'))
  const ready = initiator.readMessage(fromResponder)
  const finish = initiator.writeMessage(new TextEncoder().encode('finish'))
  const finished = responder.readMessage(finish)
  return {
    initiator,
    responder,
    initiatorStatic,
    responderStatic,
    payloads: [hello, ready, finished].map((bytes) => new TextDecoder().decode(bytes)),
  }
}

function aadFor(requestId = 'r') {
  return requestAad({
    requestId,
    hostId: 'h',
    deviceId: 'd',
    kind: 'REQUEST_KIND_QUERY',
    commandId: '',
    expiresAt: 10,
  })
}

test('when both peers complete Noise XX, they derive the same handshake hash and remote static keys', () => {
  const result = handshake()
  assert.deepEqual(result.payloads, ['hello', 'ready', 'finish'])
  assert.equal(result.initiator.complete, true)
  assert.equal(result.responder.complete, true)
  assert.deepEqual([...result.initiator.handshakeHash], [...result.responder.handshakeHash])
  assert.deepEqual([...result.initiator.remoteStaticKey], [...result.responderStatic.publicKey])
  assert.deepEqual([...result.responder.remoteStaticKey], [...result.initiatorStatic.publicKey])
  assert.throws(() => result.initiator.writeMessage(), (error) => error.code === 'handshake_complete')
  assert.throws(() => result.responder.readMessage(new Uint8Array(1)), (error) => error.code === 'handshake_complete')
})

test('when a MAC, prologue, direction, or message is invalid, Noise rejects the exchange', () => {
  const a = new NoiseXX({ initiator: true, staticKeypair: generateKeypair() })
  const b = new NoiseXX({ initiator: false, staticKeypair: generateKeypair() })
  assert.throws(() => b.writeMessage(), (error) => error instanceof NoiseError && error.code === 'unexpected_write')
  assert.throws(() => new NoiseXX({ initiator: true }).readMessage(new Uint8Array(1)), (error) => error.code === 'unexpected_read')

  const flow = (() => {
    const i = new NoiseXX({ initiator: true, staticKeypair: generateKeypair(), prologue: 'a' })
    const r = new NoiseXX({ initiator: false, staticKeypair: generateKeypair(), prologue: 'a' })
    const m1 = i.writeMessage()
    r.readMessage(m1)
    return { i, r, m2: r.writeMessage() }
  })()
  const badMac = new Uint8Array(flow.m2)
  badMac[badMac.length - 1] ^= 1
  assert.throws(() => flow.i.readMessage(badMac), (error) => error.code === 'decrypt_failed')

  const wrongPrologue = (() => {
    const i = new NoiseXX({ initiator: true, staticKeypair: generateKeypair(), prologue: 'a' })
    const r = new NoiseXX({ initiator: false, staticKeypair: generateKeypair(), prologue: 'b' })
    const m1 = i.writeMessage()
    r.readMessage(m1)
    return { i, m2: r.writeMessage() }
  })()
  assert.throws(() => wrongPrologue.i.readMessage(wrongPrologue.m2), (error) => error.code === 'decrypt_failed')
})

test('a retired v1 prologue cannot complete a v2 channel handshake', () => {
  const initiator = new NoiseXX({ initiator: true, staticKeypair: generateKeypair(), prologue: 'whalerein/v1/channel|h|d|n' })
  const responder = new NoiseXX({ initiator: false, staticKeypair: generateKeypair(), prologue: 'whalerein/v2/channel|h|d|n' })
  responder.readMessage(initiator.writeMessage(new TextEncoder().encode('hello')))
  assert.throws(
    () => initiator.readMessage(responder.writeMessage(new TextEncoder().encode('ready'))),
    (error) => error.code === 'decrypt_failed',
  )
})

test('when Noise inputs are malformed, validation rejects them', () => {
  assert.throws(() => new NoiseXX({ initiator: 'yes' }), (error) => error.code === 'invalid_initiator')
  assert.throws(() => new NoiseXX({ initiator: true, staticKeypair: { privateKey: new Uint8Array(3), publicKey: new Uint8Array(3) } }), (error) => error.code === 'invalid_static_key')
  const responder = new NoiseXX({ initiator: false })
  assert.throws(() => responder.readMessage('not bytes'), (error) => error.code === 'invalid_message')
  assert.throws(() => responder.readMessage(new Uint8Array(MAX_MESSAGE + 1)), (error) => error.code === 'message_too_large')
  assert.throws(() => responder.readMessage(new Uint8Array(4)), (error) => error.code === 'truncated_message')
  assert.throws(() => unhex('abc'), (error) => error.code === 'invalid_hex')
  assert.throws(() => unhex('zz'), (error) => error.code === 'invalid_hex')
  assert.throws(() => parseKeypair(new Uint8Array(4)), (error) => error.code === 'invalid_private_key')
  assert.equal(equal(new Uint8Array([1, 2]), new Uint8Array([1, 2])), true)
  assert.equal(equal(new Uint8Array([1, 2]), new Uint8Array([1, 3])), false)
  assert.equal(equal(new Uint8Array([1]), new Uint8Array([1, 2])), false)
})

test('when a public key is fingerprinted, the SHA-256 hex value is stable and grouped', () => {
  const keypair = generateKeypair()
  const expected = fingerprint(keypair.publicKey)
  assert.equal(expected.length, 64)
  assert.equal(expected, fingerprint(keypair.publicKey))
  assert.equal(hex(unhex(expected)), expected)
  const formatted = formatFingerprint(keypair.publicKey)
  assert.equal(formatted.replace(/ /g, ''), expected)
  assert.match(formatted, /^[0-9a-f]{4}( [0-9a-f]{4}){15}$/)
})

test('when both peers finish, their secret Split keys agree and differ per connection', () => {
  const first = handshake()
  const second = handshake()
  const firstKeys = first.initiator.takeChannelCipher()
  const hostFirst = first.responder.takeChannelCipher()
  const secondKeys = second.initiator.takeChannelCipher()
  assert.equal(firstKeys.keyFingerprint(), hostFirst.keyFingerprint())
  assert.notEqual(firstKeys.keyFingerprint(), secondKeys.keyFingerprint(), 'fresh ephemerals change the Split keys')
  assert.notEqual(hex(firstKeys.seal('c2h', aadFor(), new Uint8Array([1])).ciphertext),
    hex(firstKeys.seal('c2h', aadFor('other'), new Uint8Array([1])).ciphertext))
  assert.throws(() => first.initiator.takeChannelCipher(), (error) => error.code === 'handshake_incomplete')
})

test('a transcript observer with the retired HKDF schedule cannot open a Split-key record', () => {
  const result = handshake({ testVector: true })
  const cipher = result.initiator.takeChannelCipher()
  const host = result.responder.takeChannelCipher()
  const aad = aadFor()
  const sealed = cipher.seal('c2h', aad, new Uint8Array([9, 8, 7]))

  // The retired v1 suite derived application keys from the observable hash.
  const root = hkdf(
    sha256,
    result.initiator.handshakeHash,
    new TextEncoder().encode('prologue'),
    new TextEncoder().encode('whalerein/v1/channel-keys'),
    64,
  )
  const retiredC2h = root.slice(0, 32)
  assert.notEqual(hex(retiredC2h), host.keysHexForTest().c2h)
  assert.throws(
    () => chacha20poly1305(retiredC2h, sealed.nonce, aad).decrypt(sealed.ciphertext),
    'a transcript-only observer cannot authenticate the record',
  )
  assert.equal(host.open('c2h', aad, sealed.nonce, sealed.ciphertext).length, 3)
})

test('when an application frame has an invalid direction, nonce, MAC, or AAD, open rejects it', () => {
  const result = handshake()
  const device = result.initiator.takeChannelCipher()
  const host = result.responder.takeChannelCipher()
  const aad = aadFor()
  const plaintext = new Uint8Array([1, 2, 3, 4])
  const sealed = device.seal('c2h', aad, plaintext)
  assert.deepEqual([...host.open('c2h', aad, sealed.nonce, sealed.ciphertext)], [...plaintext])
  // Wrong direction uses the other key.
  assert.throws(
    () => host.open('h2c', aad, sealed.nonce, sealed.ciphertext),
    (error) => error.code === 'authentication_failed',
  )
  for (const badAad of [new Uint8Array([9])]) {
    const fresh = device.seal('c2h', aad, plaintext)
    assert.throws(() => host.open('c2h', badAad, fresh.nonce, fresh.ciphertext), (error) => error.code === 'authentication_failed')
  }
  assert.throws(() => host.open('c2h', aad, sealed.nonce.slice(0, 8), sealed.ciphertext), (error) => error.code === 'invalid_nonce')
  assert.throws(() => host.open('nope', aad, sealed.nonce, sealed.ciphertext), (error) => error.code === 'invalid_direction')
})

test('out-of-order records within the window are accepted, duplicates and stale counters are rejected', () => {
  const result = handshake()
  const device = result.initiator.takeChannelCipher()
  const host = result.responder.takeChannelCipher()
  const aad = aadFor()
  const frames = [0, 1, 2].map((index) => device.seal('c2h', aad, new Uint8Array([index])))
  assert.deepEqual([...host.open('c2h', aad, frames[2].nonce, frames[2].ciphertext)], [2])
  assert.deepEqual([...host.open('c2h', aad, frames[0].nonce, frames[0].ciphertext)], [0])
  assert.deepEqual([...host.open('c2h', aad, frames[1].nonce, frames[1].ciphertext)], [1])
  assert.throws(() => host.open('c2h', aad, frames[1].nonce, frames[1].ciphertext), (error) => error.code === 'replay_detected')

  // A failed authentication never advances the window.
  const tampered = device.seal('c2h', aad, new Uint8Array([3]))
  const corrupted = new Uint8Array(tampered.ciphertext)
  corrupted[0] ^= 1
  assert.throws(() => host.open('c2h', aad, tampered.nonce, corrupted), (error) => error.code === 'authentication_failed')
  assert.deepEqual([...host.open('c2h', aad, tampered.nonce, tampered.ciphertext)], [3])

  const many = []
  for (let index = 0; index < Number(REPLAY_WINDOW) + 2; index++) many.push(device.seal('c2h', aad, new Uint8Array([1])))
  const highest = many[many.length - 1]
  host.open('c2h', aad, highest.nonce, highest.ciphertext)
  assert.throws(() => host.open('c2h', aad, many[0].nonce, many[0].ciphertext), (error) => error.code === 'stale_counter')
})

test('counter exhaustion fails closed and never reuses a nonce', () => {
  const result = handshake({ testVector: true })
  const device = result.initiator.takeChannelCipher()
  const aad = aadFor()
  device.forceSendCounterForTest('c2h', 0xffffffffffffffffn - 1n)
  const last = device.seal('c2h', aad, new Uint8Array([1]))
  assert.equal(hex(last.nonce), '00000000fffffffffffffffe')
  assert.throws(() => device.seal('c2h', aad, new Uint8Array([1])), (error) => error.code === 'nonce_exhausted')
  assert.throws(() => device.seal('c2h', aad, new Uint8Array([1])), (error) => error.code === 'nonce_exhausted')
})

test('channel key material is never serialized and test affordances require a test handshake', () => {
  const production = handshake()
  const cipher = production.initiator.takeChannelCipher()
  assert.equal(JSON.stringify({ cipher }), '{"cipher":"[redacted]"}')
  assert.match(inspect(cipher), /ChannelCipher\(\.\.\)/)
  assert.equal(Object.keys(cipher).length, 0)
  assert.throws(() => cipher.keysHexForTest(), (error) => error.code === 'key_material_unavailable')
  assert.throws(() => cipher.readerForTest(), (error) => error.code === 'key_material_unavailable')
  assert.throws(() => cipher.forceSendCounterForTest('c2h', 1n), (error) => error.code === 'key_material_unavailable')

  const vector = handshake({ testVector: true })
  const exported = vector.initiator.takeChannelCipher().keysHexForTest()
  assert.equal(exported.c2h.length, 64)
  assert.equal(exported.h2c.length, 64)
})

test('when building AAD and prologues, the values match the v2 channel contract', () => {
  assert.equal(
    new TextDecoder().decode(requestAad({
      requestId: 'req',
      hostId: 'host',
      deviceId: 'dev',
      kind: 'REQUEST_KIND_COMMAND',
      commandId: 'cmd',
      expiresAt: 123,
    })),
    'whalerein/v2/request|req|host|dev|REQUEST_KIND_COMMAND|cmd|123',
  )
  assert.equal(
    new TextDecoder().decode(eventAad({ hostId: 'host', deviceId: 'dev', channelId: 'ch', createdAt: 456 })),
    'whalerein/v2/event|host|dev|ch|456',
  )
  assert.equal(pairingPrologue({ hostId: 'h', tokenHash: 'ab', claimId: 'c' }), 'whalerein/v1/pairing|h|ab|c')
  assert.equal(channelPrologue({ hostId: 'h', deviceId: 'd', channelId: 'c' }), 'whalerein/v2/channel|h|d|c')
})