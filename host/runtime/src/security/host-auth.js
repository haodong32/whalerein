// Host authorization signatures. The Ed25519 seed is the Noise X25519 private
// key. Access stores the Ed25519 public key, which is not the Noise public key.

import { ed25519 } from '@noble/curves/ed25519.js'

export const HOST_AUTH_PREFIX = 'whalerein/v1/host-auth'

export function authorizationPublicKey(privateKey) {
  return ed25519.getPublicKey(privateKey)
}

export function hostAuthMessage(purpose, hostId, deviceId, signedAtUnixMs) {
  return new TextEncoder().encode(`${HOST_AUTH_PREFIX}|${purpose}|${hostId}|${deviceId ?? ''}|${signedAtUnixMs}`)
}

export function signHostAuth(privateKey, purpose, hostId, deviceId, signedAtUnixMs) {
  return ed25519.sign(hostAuthMessage(purpose, hostId, deviceId, signedAtUnixMs), privateKey)
}
