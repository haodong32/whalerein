import { test } from 'node:test'
import assert from 'node:assert/strict'
import { generateKeyPairSync, sign } from 'node:crypto'
import { createAuthorizationVerifier } from '../src/verifier.js'

function fixture() {
  const pair = generateKeyPairSync('ed25519')
  const publicKey = new Uint8Array(pair.publicKey.export({ type: 'spki', format: 'der' }).subarray(-32))
  const canonical = 'whalerein/v1/ext-auth-v1\n|auth-1\n|https://issuer.example'
  const signature = sign(null, Buffer.from(canonical), pair.privateKey).toString('base64url')
  return {
    publicKey: Buffer.from(publicKey).toString('base64url'),
    canonical,
    authorization: { issuer: 'https://issuer.example', signature },
  }
}

test('a pinned key verifies the exact canonical authorization bytes', () => {
  const { publicKey, canonical, authorization } = fixture()
  const verify = createAuthorizationVerifier({ publicKey, expectedIssuer: 'https://issuer.example' })
  assert.equal(typeof verify, 'function')
  assert.equal(verify(authorization, canonical), true)
  assert.equal(verify(authorization, `${canonical}\n|tampered`), false)
  assert.equal(verify({ ...authorization, signature: 'not-base64' }, canonical), false)
  assert.equal(verify({ ...authorization, issuer: 'https://evil.example' }, canonical), false)
})

test('verification is unavailable without a well-formed pinned key', () => {
  assert.equal(createAuthorizationVerifier({}), null)
  assert.equal(createAuthorizationVerifier({ publicKey: 'short' }), null)
  const verify = createAuthorizationVerifier({ publicKey: fixture().publicKey })
  assert.equal(verify({}, ''), false)
  assert.equal(verify(null, 'text'), false)
  assert.equal(verify({ signature: 'x' }, 'text'), false)
})
