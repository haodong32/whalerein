// Profile-scoped credential adapter boundary.
//
// Every profile owns independent key purposes. Nothing here writes a secret to
// the registry, a snapshot, a log or SQLite: callers receive opaque values only
// through the injected credential adapter (DSH owns the actual secure store).
//
// Ordering: a private key is written before its public binding is recorded in
// the registry. A crash between the two writes leaves the credential readable,
// so the next start records the same public key instead of minting a new
// identity. A registry binding without its credential is a hard failure.

import { randomBytes } from 'node:crypto'
import { ed25519, x25519 } from '@noble/curves/ed25519.js'
import { PluginError } from './errors.js'
import { HOST_KEY_CREDENTIAL } from './security/identity.js'

export const CREDENTIAL_NAMESPACE = 'whalerein-host/v1'
export const CREDENTIAL_PURPOSES = Object.freeze({
  signingKey: 'ed25519-signing-key',
  noiseKey: 'noise-private-key',
  pollCredential: 'poll-credential',
  transportCredential: 'transport-credential',
  recoverySecret: 'recovery-secret',
})
const PURPOSE_VALUES = new Set(Object.values(CREDENTIAL_PURPOSES))
const PROFILE_ID_RE = /^[a-f0-9]{32}$|^standalone$/

function decodeBase64Url(value, label) {
  if (typeof value !== 'string' || value === '') throw new PluginError('internal', `${label} is missing; restore the credential store backup instead of rotating the identity`)
  let bytes
  try {
    bytes = new Uint8Array(Buffer.from(value, 'base64url'))
  } catch {
    throw new PluginError('internal', `${label} is unreadable; restore the credential store backup instead of rotating the identity`)
  }
  return bytes
}

function encode(bytes) {
  return Buffer.from(bytes).toString('base64url')
}

function assertPurpose(purpose) {
  if (!PURPOSE_VALUES.has(purpose)) throw new PluginError('invalid_argument', 'unsupported credential purpose')
}

/** Key names are scoped by plugin installation and opaque profile id. */
export class ProfileCredentialStore {
  #credentials
  #installationId
  #profileId

  constructor({ credentials, installationId = 'default', profileId } = {}) {
    if (!credentials || typeof credentials.getCredential !== 'function' || typeof credentials.setCredential !== 'function') {
      throw new PluginError('invalid_argument', 'credential adapter is required')
    }
    if (typeof profileId !== 'string' || !PROFILE_ID_RE.test(profileId)) {
      throw new PluginError('invalid_argument', 'opaque profile id is required for credential scope')
    }
    if (typeof installationId !== 'string' || !/^[A-Za-z0-9_.:-]{1,64}$/.test(installationId)) {
      throw new PluginError('invalid_argument', 'installation credential scope is invalid')
    }
    this.#credentials = credentials
    this.#installationId = installationId
    this.#profileId = profileId
  }

  key(purpose) {
    assertPurpose(purpose)
    return `${CREDENTIAL_NAMESPACE}/${this.#installationId}/${this.#profileId}/${purpose}`
  }

  async get(purpose) {
    let value
    try {
      value = await this.#credentials.getCredential(this.key(purpose))
    } catch {
      throw new PluginError('internal', 'credential read failed')
    }
    return typeof value === 'string' && value !== '' ? value : null
  }

  async set(purpose, value) {
    if (typeof value !== 'string' || value === '') throw new PluginError('invalid_argument', 'credential value must be a non-empty string')
    try {
      await this.#credentials.setCredential(this.key(purpose), value)
    } catch {
      throw new PluginError('internal', 'credential write failed; the committed identity is preserved')
    }
  }

  async getJson(purpose) {
    const value = await this.get(purpose)
    if (value === null) return null
    try {
      const parsed = JSON.parse(value)
      return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : null
    } catch {
      throw new PluginError('internal', 'stored credential record is unreadable')
    }
  }

  async setJson(purpose, value) {
    await this.set(purpose, JSON.stringify(value))
  }

  /** Remove a scoped credential. Adapters without delete store an empty value. */
  async remove(purpose) {
    const key = this.key(purpose)
    if (typeof this.#credentials.deleteCredential === 'function') {
      try {
        await this.#credentials.deleteCredential(key)
        return
      } catch {
        // fall through to the empty-value convention
      }
    }
    try {
      await this.#credentials.setCredential(key, '')
    } catch {
      throw new PluginError('internal', 'credential removal failed')
    }
  }
}

/**
 * Load or create the independent signing and Noise keys for a managed profile.
 * A recorded public binding without its private credential fails closed.
 */
export async function prepareProfileIdentity({ store, bindings = {}, random = randomBytes } = {}) {
  if (!(store instanceof ProfileCredentialStore)) throw new PluginError('invalid_argument', 'profile credential store is required')
  const signing = await loadOrCreateKey({
    store,
    purpose: CREDENTIAL_PURPOSES.signingKey,
    binding: bindings.signingPublicKey,
    kind: 'signing',
    random,
  })
  const noise = await loadOrCreateKey({
    store,
    purpose: CREDENTIAL_PURPOSES.noiseKey,
    binding: bindings.noisePublicKey,
    kind: 'noise',
    random,
  })
  return {
    signingPrivateKey: signing.privateKey,
    signingPublicKey: signing.publicKey,
    noisePrivateKey: noise.privateKey,
    noisePublicKey: noise.publicKey,
    changed: signing.changed || noise.changed,
  }
}

async function loadOrCreateKey({ store, purpose, binding, kind, random }) {
  const stored = await store.get(purpose)
  if (stored !== null) {
    const privateKey = decodeBase64Url(stored, `${kind} key`)
    if (privateKey.length !== 32) throw new PluginError('internal', `${kind} key is unreadable; restore the credential store backup instead of rotating the identity`)
    const publicKey = kind === 'signing'
      ? ed25519.getPublicKey(privateKey)
      : x25519.getPublicKey(privateKey)
    const encoded = encode(publicKey)
    if (typeof binding === 'string' && binding !== '' && binding !== encoded) {
      throw new PluginError('internal', `${kind} key does not match the committed identity; restore the matching credential store`)
    }
    return { privateKey, publicKey, changed: false }
  }
  if (typeof binding === 'string' && binding !== '') {
    throw new PluginError('internal', `${kind} key is missing; restore the credential store backup instead of rotating the identity`)
  }
  const privateKey = kind === 'signing' ? new Uint8Array(random(32)) : x25519.utils.randomSecretKey()
  if (privateKey.length !== 32) throw new PluginError('internal', 'generated key length is invalid')
  await store.set(purpose, encode(privateKey))
  const publicKey = kind === 'signing' ? ed25519.getPublicKey(privateKey) : x25519.getPublicKey(privateKey)
  return { privateKey, publicKey, changed: true }
}

/** Credential adapter supplied to HostRuntime; it only ever exposes the Noise key. */
export function createRuntimeCredentials({ store, noisePrivateKey }) {
  if (!(store instanceof ProfileCredentialStore)) throw new PluginError('invalid_argument', 'profile credential store is required')
  if (!(noisePrivateKey instanceof Uint8Array) || noisePrivateKey.length !== 32) {
    throw new PluginError('invalid_argument', 'Noise private key is required')
  }
  const value = encode(noisePrivateKey)
  return {
    async getCredential(name) {
      if (name === HOST_KEY_CREDENTIAL) return value
      return store.get(name)
    },
    async setCredential(name, next) {
      if (name === HOST_KEY_CREDENTIAL) {
        // HostRuntime may only confirm the already-committed identity.
        if (next !== value) throw new PluginError('internal', 'refusing to replace the committed Host identity')
        return
      }
      await store.set(name, next)
    },
  }
}

export async function ensureRecoverySecret(store, random = randomBytes) {
  const existing = await store.get(CREDENTIAL_PURPOSES.recoverySecret)
  if (existing !== null) {
    const decoded = decodeBase64Url(existing, 'recovery secret')
    if (decoded.length < 32) throw new PluginError('internal', 'recovery secret is unreadable; restore the credential store backup')
    return existing
  }
  const secret = encode(random(32))
  await store.set(CREDENTIAL_PURPOSES.recoverySecret, secret)
  return secret
}

export function signingPublicKeyFromPrivate(privateKey) {
  if (!(privateKey instanceof Uint8Array) || privateKey.length !== 32) throw new PluginError('internal', 'signing key is invalid')
  return ed25519.getPublicKey(privateKey)
}

export function noisePublicKeyFromPrivate(privateKey) {
  if (!(privateKey instanceof Uint8Array) || privateKey.length !== 32) throw new PluginError('internal', 'Noise key is invalid')
  return x25519.getPublicKey(privateKey)
}

export { decodeBase64Url, encode }
