// One outbound call to Access that mints a pairing URI. The admin token and the
// one-time invitation token stay out of logs; callers receive only the URI.

import { RemoteError } from '@deepseek-ai/dsh-typert-protocol'
import { validateTopicId } from 'whalerein-host/config.js'
import { signHostAuth } from 'whalerein-host/security/host-auth.js'

function fail(code) {
  return new RemoteError(code, code, {})
}

function hostSignature(privateKey, purpose, hostId, deviceId, signedAtUnixMs, now) {
  if (!(privateKey instanceof Uint8Array) || privateKey.length !== 32) throw fail('whalerein/host-key-missing')
  const signedAt = Number.isSafeInteger(signedAtUnixMs) ? signedAtUnixMs : (now ?? Date.now)()
  let signature
  try {
    signature = signHostAuth(privateKey, purpose, hostId, deviceId, signedAt)
  } catch {
    throw fail('whalerein/host-key-missing')
  }
  return {
    signedAtUnixMs: signedAt,
    signature: Buffer.from(signature).toString('base64'),
  }
}

/**
 * Ask Access for a one-time pairing invitation for this host.
 * The request is signed by the host key and carries no administrator token.
 * @param {object} options
 * @param {string} options.accessUrl public Access origin.
 * @param {string} options.hostId host the invitation is bound to.
 * @param {Uint8Array} options.privateKey host Noise private key.
 * @param {number} [options.signedAtUnixMs]
 * @param {() => number} [options.now]
 * @param {typeof fetch} [options.fetchImpl] HTTP implementation.
 * @returns {Promise<{ pairingUri: string, expiresAtUnixMs: number }>}
 */
export async function createInvitation({ accessUrl, hostId, privateKey, signedAtUnixMs, now, fetchImpl = globalThis.fetch } = {}) {
  if (!accessUrl || !hostId) throw fail('whalerein/server-missing')
  const proof = hostSignature(privateKey, 'invitation', hostId, '', signedAtUnixMs, now)
  let response
  try {
    response = await fetchImpl(new URL('/v1/pairing/invitations', accessUrl), {
      method: 'POST',
      redirect: 'error',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json',
      },
      body: JSON.stringify({ hostId, ...proof }),
      signal: AbortSignal.timeout(10_000),
    })
  } catch {
    throw fail('whalerein/unavailable')
  }
  if (response.status === 401 || response.status === 403 || response.status === 410) throw fail('whalerein/unauthorized')
  if (response.status === 404) throw fail('whalerein/host-not-found')
  if (!response.ok) throw fail('whalerein/unavailable')
  let body
  try {
    body = await response.json()
  } catch {
    throw fail('whalerein/unavailable')
  }
  const pairingUri = typeof body?.pairingUri === 'string' ? body.pairingUri : ''
  if (!pairingUri.startsWith('whalerein://pair?')) throw fail('whalerein/unavailable')
  const expiresAtUnixMs = Number(body?.expiresAtUnixMs)
  return {
    pairingUri,
    expiresAtUnixMs: Number.isSafeInteger(expiresAtUnixMs) ? expiresAtUnixMs : 0,
  }
}

/**
 * Ask Access to revoke this host's grant for one device.
 * The request is signed by the host key. Callers receive only the device id.
 * @param {object} options
 * @param {string} options.accessUrl public Access origin.
 * @param {string} options.hostId host whose grant is revoked.
 * @param {string} options.deviceId device to revoke.
 * @param {Uint8Array} options.privateKey host Noise private key.
 * @param {number} [options.signedAtUnixMs]
 * @param {() => number} [options.now]
 * @param {typeof fetch} [options.fetchImpl] HTTP implementation.
 * @returns {Promise<{ deviceId: string }>}
 */
export async function revokeDevice({ accessUrl, hostId, deviceId, privateKey, signedAtUnixMs, now, fetchImpl = globalThis.fetch } = {}) {
  if (!accessUrl || !hostId) throw fail('whalerein/server-missing')
  let id = ''
  try {
    id = validateTopicId(deviceId, 'device_id')
    validateTopicId(hostId, 'host_id')
  } catch {
    throw fail('whalerein/not-found')
  }
  const proof = hostSignature(privateKey, 'revoke', hostId, id, signedAtUnixMs, now)
  let response
  try {
    response = await fetchImpl(new URL(`/v1/hosts/${encodeURIComponent(hostId)}/devices/${encodeURIComponent(id)}/revocation`, accessUrl), {
      method: 'POST',
      redirect: 'error',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json',
      },
      body: JSON.stringify(proof),
      signal: AbortSignal.timeout(10_000),
    })
  } catch {
    throw fail('whalerein/unavailable')
  }
  if (response.status === 401 || response.status === 403 || response.status === 410) throw fail('whalerein/unauthorized')
  if (response.status === 404) throw fail('whalerein/not-found')
  if (!response.ok) throw fail('whalerein/unavailable')
  return { deviceId: id }
}
