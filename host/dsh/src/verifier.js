// Pinned authorization verification for the managed cloud connection.
//
// The account service signs one portable authorization per approval. Access
// verifies it independently; the Host checks the same signature before it
// sends any secret, so a compromised transport or a misrouted route cannot
// substitute an authorization from another issuer.

import { ed25519 } from '@noble/curves/ed25519.js'

function decodeBase64(value) {
  if (typeof value !== 'string' || value === '') return null
  try {
    return new Uint8Array(Buffer.from(value, 'base64url'))
  } catch {
    return null
  }
}

/**
 * createAuthorizationVerifier builds the verifier the runtime calls before it
 * trusts an authorization. Without a pinned key the verifier returns null and
 * the runtime keeps its structural checks only (Access still verifies).
 *
 * @param {{ publicKey?: Uint8Array|string, expectedIssuer?: string }} options
 * @returns {((authorization: object, canonical: string) => boolean) | null}
 */
export function createAuthorizationVerifier({ publicKey, expectedIssuer = '' } = {}) {
  let key = null
  if (publicKey instanceof Uint8Array) key = publicKey
  else if (typeof publicKey === 'string') key = decodeBase64(publicKey)
  if (!key || key.length !== 32) return null
  const issuer = typeof expectedIssuer === 'string' ? expectedIssuer : ''
  return (authorization, canonical) => {
    if (!authorization || typeof authorization !== 'object') return false
    if (typeof canonical !== 'string' || canonical === '') return false
    const signature = decodeBase64(authorization.signature)
    if (!signature || signature.length !== 64) return false
    if (issuer !== '' && authorization.issuer !== issuer) return false
    try {
      return ed25519.verify(signature, new TextEncoder().encode(canonical), key)
    } catch {
      return false
    }
  }
}
