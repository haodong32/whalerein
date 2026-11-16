// Host long-term X25519 identity. The private key lives only behind the DSH
// adapter credential store; SQLite keeps just the public key and fingerprint.

import { fingerprint, formatFingerprint, generateKeypair, parseKeypair } from './noise.js'
import { bytesEqual } from './digest.js'
import { PluginError } from '../errors.js'

export const HOST_KEY_CREDENTIAL = 'whalerein.host-private-key.v1'

function decodePrivateKey(value) {
  if (value instanceof Uint8Array) return value
  if (typeof value !== 'string' || value.length === 0) throw new PluginError('internal', 'host identity credential is unreadable; restore the credential store backup instead of rotating the Host identity')
  try {
    return new Uint8Array(Buffer.from(value, 'base64'))
  } catch {
    throw new PluginError('internal', 'host identity credential is unreadable; restore the credential store backup instead of rotating the Host identity')
  }
}

/**
 * Load the host identity, creating it on first use. A public key already
 * recorded in SQLite must match the credential; a missing credential is a
 * hard failure rather than a silent identity rotation that would strand every
 * paired device.
 *
 * @param {object} options
 * @param {{getCredential: Function, setCredential: Function}} options.credentials Host credential binding.
 * @param {import('../state/store.js').StateStore} options.store public reference.
 * @param {{info: Function, warn: Function}} [options.logger]
 * @returns {Promise<{publicKey: Uint8Array, privateKey: Uint8Array, fingerprint: string, fingerprintDisplay: string}>}
 */
export async function loadOrCreateHostIdentity({ credentials, store, logger }) {
  const recorded = store.getHostPublicKey()
  const stored = await credentials.getCredential(HOST_KEY_CREDENTIAL)
  if (!stored) {
    if (recorded) {
      logger?.warn('host_identity_missing', { code: 'internal' })
      throw new PluginError('internal', 'host identity private key is missing; restore the credential store backup, or revoke every paired device and create a new Host identity; do not delete the state database')
    }
    const keypair = generateKeypair()
    await credentials.setCredential(HOST_KEY_CREDENTIAL, Buffer.from(keypair.privateKey).toString('base64'))
    const fp = fingerprint(keypair.publicKey)
    store.saveHostPublicKey(keypair.publicKey, fp)
    logger?.info('host_identity_created', { host: 'self' })
    return { ...keypair, fingerprint: fp, fingerprintDisplay: formatFingerprint(keypair.publicKey) }
  }
  const privateKey = decodePrivateKey(stored)
  if (privateKey.length !== 32) throw new PluginError('internal', 'host identity credential is unreadable; restore the credential store backup instead of rotating the Host identity')
  const keypair = parseKeypair(privateKey)
  const fp = fingerprint(keypair.publicKey)
  if (recorded && !bytesEqual(recorded.publicKey, keypair.publicKey)) {
    logger?.error('host_identity_mismatch', { code: 'internal' })
    throw new PluginError('internal', 'host identity does not match the recorded public key; restore the matching credential store, or revoke every paired device before creating a new identity')
  }
  if (!recorded) store.saveHostPublicKey(keypair.publicKey, fp)
  return { ...keypair, fingerprint: fp, fingerprintDisplay: formatFingerprint(keypair.publicKey) }
}
