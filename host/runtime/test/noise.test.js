import test from 'node:test'
import assert from 'node:assert/strict'
import {
  NoiseXX,
  NoiseError,
  MAX_MESSAGE,
  equal,
  eventAad,
  fingerprint,
  formatFingerprint,
  generateKeypair,
  parseKeypair,
  pairingPrologue,
  channelPrologue,
  requestAad,
  deriveChannelKeys,
  seal,
  open,
  unhex,
  hex,
} from '../src/security/noise.js'

function handshake({ prologueA = 'prologue', prologueB = prologueA } = {}) {
  const initiatorStatic = generateKeypair()
  const responderStatic = generateKeypair()
  const initiator = new NoiseXX({ initiator: true, staticKeypair: initiatorStatic, prologue: prologueA })
  const responder = new NoiseXX({ initiator: false, staticKeypair: responderStatic, prologue: prologueB })
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

test('when handshake hash or prologue changes, derived channel keys change', () => {
  const hash = new Uint8Array(32).fill(5)
  const first = deriveChannelKeys(hash, 'prologue')
  const second = deriveChannelKeys(hash, 'prologue')
  const other = deriveChannelKeys(hash, 'other')
  assert.deepEqual([...first.c2h], [...second.c2h])
  assert.deepEqual([...first.h2c], [...second.h2c])
  assert.notDeepEqual([...first.c2h], [...other.c2h])
  assert.equal(first.c2h.length, 32)
  assert.equal(first.h2c.length, 32)
  assert.equal(first.channelId, undefined)
  assert.throws(() => deriveChannelKeys(new Uint8Array(3), 'p'), (error) => error.code === 'invalid_handshake_hash')
  const text = deriveChannelKeys(hash, new TextEncoder().encode('prologue'))
  assert.deepEqual([...text.c2h], [...first.c2h])
})

test('when an application frame has an invalid key, AAD, nonce, or MAC, seal/open rejects it', () => {
  const { c2h, h2c } = deriveChannelKeys(new Uint8Array(32).fill(1), 'p')
  const aad = requestAad({ requestId: 'r', hostId: 'h', deviceId: 'd', kind: 'REQUEST_KIND_QUERY', commandId: '', expiresAt: 10 })
  const plaintext = new Uint8Array([1, 2, 3, 4])
  const sealed = seal(c2h, aad, plaintext)
  assert.deepEqual([...open(c2h, aad, sealed.nonce, sealed.ciphertext)], [...plaintext])
  assert.equal(open(h2c, aad, sealed.nonce, sealed.ciphertext), null)
  assert.equal(open(c2h, new Uint8Array([9]), sealed.nonce, sealed.ciphertext), null)
  assert.equal(open(c2h, aad, sealed.nonce.slice(0, 8), sealed.ciphertext), null)
  const tampered = new Uint8Array(sealed.ciphertext)
  tampered[0] ^= 1
  assert.equal(open(c2h, aad, sealed.nonce, tampered), null)
})

test('when building AAD and prologues, the values match the wire contract', () => {
  assert.equal(
    new TextDecoder().decode(requestAad({
      requestId: 'req',
      hostId: 'host',
      deviceId: 'dev',
      kind: 'REQUEST_KIND_COMMAND',
      commandId: 'cmd',
      expiresAt: 123,
    })),
    'whalerein/v1/request|req|host|dev|REQUEST_KIND_COMMAND|cmd|123',
  )
  assert.equal(
    new TextDecoder().decode(eventAad({ hostId: 'host', deviceId: 'dev', channelId: 'ch', createdAt: 456 })),
    'whalerein/v1/event|host|dev|ch|456',
  )
  assert.equal(pairingPrologue({ hostId: 'h', tokenHash: 'ab', claimId: 'c' }), 'whalerein/v1/pairing|h|ab|c')
  assert.equal(channelPrologue({ hostId: 'h', deviceId: 'd', channelId: 'c' }), 'whalerein/v1/channel|h|d|c')
})
