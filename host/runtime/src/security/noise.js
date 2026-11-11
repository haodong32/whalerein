// Noise XX (25519 / ChaChaPoly / SHA-256) for the WhaleRein secure channel.
//
// The ecosystem library `noise-handshake` uses BLAKE2b, while the WhaleRein
// contract fixes SHA-256. This module implements only the XX pattern on top of
// audited primitives from @noble, and is covered by Rust<->Node interop tests.

import { x25519 } from '@noble/curves/ed25519.js'
import { chacha20poly1305 } from '@noble/ciphers/chacha.js'
import { sha256 } from '@noble/hashes/sha2.js'
import { hkdf } from '@noble/hashes/hkdf.js'
import { randomBytes } from 'node:crypto'

export const PROTOCOL_NAME = 'Noise_XX_25519_ChaChaPoly_SHA256'
export const HASHLEN = 32
export const TAGLEN = 16
export const MAX_MESSAGE = 65535
export const PROLOGUE_PREFIX = 'whalerein/v1'

const EMPTY = new Uint8Array(0)

function concat(...parts) {
  const length = parts.reduce((n, part) => n + part.length, 0)
  const out = new Uint8Array(length)
  let offset = 0
  for (const part of parts) {
    out.set(part, offset)
    offset += part.length
  }
  return out
}

function equal(a, b) {
  if (a.length !== b.length) return false
  let diff = 0
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i]
  return diff === 0
}

function nonceBytes(counter) {
  const nonce = new Uint8Array(12)
  let value = counter
  for (let i = 4; i < 12; i++) {
    nonce[i] = value & 0xff
    value = Math.floor(value / 256)
  }
  return nonce
}

function generateKeypair() {
  const privateKey = x25519.utils.randomSecretKey()
  return { privateKey, publicKey: x25519.getPublicKey(privateKey) }
}

function dh(privateKey, publicKey) {
  return x25519.getSharedSecret(privateKey, publicKey)
}

export class NoiseError extends Error {
  constructor(code) {
    super(code)
    this.name = 'NoiseError'
    this.code = code
  }
}

/** Exact protocol name bytes, used for both h and ck initialization. */
function protocolBytes() {
  return new TextEncoder().encode(PROTOCOL_NAME)
}

export class NoiseXX {
  /**
   * @param {object} options
   * @param {boolean} options.initiator
   * @param {{privateKey: Uint8Array, publicKey: Uint8Array}} [options.staticKeypair]
   * @param {string|Uint8Array} [options.prologue]
   * @param {{privateKey: Uint8Array, publicKey: Uint8Array}} [options.ephemeralKeypair]
   *   Test-only injection; never set in production.
   */
  constructor({ initiator, staticKeypair, prologue = '', ephemeralKeypair } = {}) {
    if (typeof initiator !== 'boolean') throw new NoiseError('invalid_initiator')
    this.initiator = initiator
    this.s = staticKeypair || generateKeypair()
    if (this.s.privateKey.length !== 32 || this.s.publicKey.length !== 32) {
      throw new NoiseError('invalid_static_key')
    }
    this.ephemeralKeypair = ephemeralKeypair || null
    this.e = null
    this.re = null
    this.rs = null
    this.messageIndex = 0
    this.complete = false
    this.handshakeHash = null
    this.sentMessages = 0
    this.receivedMessages = 0

    const protocol = protocolBytes()
    this.h = protocol.length <= HASHLEN
      ? (() => { const h = new Uint8Array(HASHLEN); h.set(protocol); return h })()
      : sha256(protocol)
    this.ck = this.h.slice()
    this.k = null
    this.n = 0

    const prologueBytes = typeof prologue === 'string'
      ? new TextEncoder().encode(prologue)
      : prologue
    this.prologue = prologueBytes
    this.mixHash(prologueBytes)
  }

  mixHash(data) {
    this.h = sha256(concat(this.h, data))
  }

  mixKey(ikm) {
    const out = hkdf(sha256, ikm, this.ck, EMPTY, 64)
    this.ck = out.slice(0, 32)
    this.k = out.slice(32, 64)
    this.n = 0
  }

  encryptAndHash(plaintext) {
    let ciphertext
    if (this.k) {
      const nonce = nonceBytes(this.n++)
      ciphertext = chacha20poly1305(this.k, nonce, this.h).encrypt(plaintext)
    } else {
      ciphertext = plaintext
    }
    this.mixHash(ciphertext)
    return ciphertext
  }

  decryptAndHash(ciphertext) {
    let plaintext
    if (this.k) {
      const nonce = nonceBytes(this.n++)
      try {
        plaintext = chacha20poly1305(this.k, nonce, this.h).decrypt(ciphertext)
      } catch {
        throw new NoiseError('decrypt_failed')
      }
    } else {
      plaintext = ciphertext
    }
    this.mixHash(ciphertext)
    return plaintext
  }

  /**
   * Write the next handshake message. Returns the raw Noise message bytes.
   * @param {Uint8Array} [payload]
   */
  writeMessage(payload = EMPTY) {
    if (this.complete) throw new NoiseError('handshake_complete')
    if (this.initiator !== (this.messageIndex % 2 === 0)) {
      throw new NoiseError('unexpected_write')
    }

    const chunks = []
    if (this.messageIndex === 0) {
      this.e = this.ephemeralKeypair || generateKeypair()
      chunks.push(this.e.publicKey)
      this.mixHash(this.e.publicKey)
      chunks.push(this.encryptAndHash(payload))
    } else if (this.messageIndex === 1) {
      this.e = this.ephemeralKeypair || generateKeypair()
      chunks.push(this.e.publicKey)
      this.mixHash(this.e.publicKey)
      this.mixKey(dh(this.e.privateKey, this.re))
      chunks.push(this.encryptAndHash(this.s.publicKey))
      this.mixKey(dh(this.s.privateKey, this.re))
      chunks.push(this.encryptAndHash(payload))
    } else if (this.messageIndex === 2) {
      chunks.push(this.encryptAndHash(this.s.publicKey))
      this.mixKey(dh(this.s.privateKey, this.re))
      chunks.push(this.encryptAndHash(payload))
    } else {
      throw new NoiseError('unexpected_write')
    }

    this.messageIndex++
    this.sentMessages++
    if (this.messageIndex === 3) this.finalize()
    return concat(...chunks)
  }

  /**
   * Read the next handshake message and return its plaintext payload.
   * @param {Uint8Array} message
   */
  readMessage(message) {
    if (this.complete) throw new NoiseError('handshake_complete')
    if (this.initiator === (this.messageIndex % 2 === 0)) {
      throw new NoiseError('unexpected_read')
    }
    if (!(message instanceof Uint8Array)) throw new NoiseError('invalid_message')
    if (message.length > MAX_MESSAGE) throw new NoiseError('message_too_large')

    let offset = 0
    const take = (n) => {
      if (offset + n > message.length) throw new NoiseError('truncated_message')
      const part = message.subarray(offset, offset + n)
      offset += n
      return part
    }

    let payload
    if (this.messageIndex === 0) {
      this.re = take(32)
      this.mixHash(this.re)
      payload = this.decryptAndHash(message.subarray(offset))
      offset = message.length
    } else if (this.messageIndex === 1) {
      this.re = take(32)
      this.mixHash(this.re)
      this.mixKey(dh(this.e.privateKey, this.re))
      this.rs = this.decryptAndHash(take(32 + TAGLEN))
      this.mixKey(dh(this.e.privateKey, this.rs))
      payload = this.decryptAndHash(message.subarray(offset))
      offset = message.length
    } else if (this.messageIndex === 2) {
      this.rs = this.decryptAndHash(take(32 + TAGLEN))
      this.mixKey(dh(this.e.privateKey, this.rs))
      payload = this.decryptAndHash(message.subarray(offset))
      offset = message.length
    } else {
      throw new NoiseError('unexpected_read')
    }
    if (offset !== message.length) throw new NoiseError('trailing_bytes')

    this.messageIndex++
    this.receivedMessages++
    if (this.messageIndex === 3) this.finalize()
    return payload
  }

  finalize() {
    this.complete = true
    this.handshakeHash = this.h.slice()
    this.k = null
  }

  /** Remote static public key learned during the handshake. */
  get remoteStaticKey() {
    return this.rs ? this.rs.slice() : null
  }
}

export function fingerprint(publicKey) {
  const digest = sha256(publicKey)
  return hex(digest)
}

export function hex(bytes) {
  let out = ''
  for (const byte of bytes) out += byte.toString(16).padStart(2, '0')
  return out
}

export function unhex(text) {
  if (typeof text !== 'string' || text.length % 2 !== 0) throw new NoiseError('invalid_hex')
  const out = new Uint8Array(text.length / 2)
  for (let i = 0; i < out.length; i++) {
    const byte = Number.parseInt(text.slice(i * 2, i * 2 + 2), 16)
    if (Number.isNaN(byte)) throw new NoiseError('invalid_hex')
    out[i] = byte
  }
  return out
}

/** Group a hex fingerprint into 4-character blocks for human comparison. */
export function formatFingerprint(publicKey) {
  return fingerprint(publicKey).match(/.{1,4}/g).join(' ')
}

/** Derive the application keys after a completed XX handshake. */
export function deriveChannelKeys(handshakeHash, prologue) {
  if (handshakeHash.length !== 32) throw new NoiseError('invalid_handshake_hash')
  const prologueBytes = typeof prologue === 'string'
    ? new TextEncoder().encode(prologue)
    : prologue
  const root = hkdf(
    sha256,
    handshakeHash,
    prologueBytes,
    new TextEncoder().encode(`${PROLOGUE_PREFIX}/channel-keys`),
    64,
  )
  // WIRE.md: channel_id is the client-chosen random id bound through the
  // Noise prologue, not a second value derived from the handshake hash.
  return {
    c2h: root.slice(0, 32),
    h2c: root.slice(32, 64),
  }
}

/** Seal one application message with ChaCha20-Poly1305 and a random nonce. */
export function seal(key, aad, plaintext) {
  const nonce = randomBytes(12)
  const ciphertext = chacha20poly1305(key, nonce, aad).encrypt(plaintext)
  return { nonce, ciphertext }
}

/** Open one application message. Returns null for any authentication failure. */
export function open(key, aad, nonce, ciphertext) {
  if (nonce.length !== 12) return null
  try {
    return chacha20poly1305(key, nonce, aad).decrypt(ciphertext)
  } catch {
    return null
  }
}

export function requestAad({ requestId, hostId, deviceId, kind, commandId = '', expiresAt }) {
  return new TextEncoder().encode(
    `${PROLOGUE_PREFIX}/request|${requestId}|${hostId}|${deviceId}|${kind}|${commandId}|${expiresAt}`,
  )
}

export function eventAad({ hostId, deviceId, channelId, createdAt }) {
  return new TextEncoder().encode(
    `${PROLOGUE_PREFIX}/event|${hostId}|${deviceId}|${channelId}|${createdAt}`,
  )
}

export function pairingPrologue({ hostId, tokenHash, claimId }) {
  return `${PROLOGUE_PREFIX}/pairing|${hostId}|${tokenHash}|${claimId}`
}

export function channelPrologue({ hostId, deviceId, channelId }) {
  return `${PROLOGUE_PREFIX}/channel|${hostId}|${deviceId}|${channelId}`
}

export function parseKeypair(privateKey) {
  if (privateKey.length !== 32) throw new NoiseError('invalid_private_key')
  return { privateKey, publicKey: x25519.getPublicKey(privateKey) }
}

export { generateKeypair, equal }
