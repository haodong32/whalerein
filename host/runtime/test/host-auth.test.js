import assert from 'node:assert/strict'
import test from 'node:test'
import { ed25519 } from '@noble/curves/ed25519.js'

// The same seed and empty message are checked in access/internal/auth.
// Noble signs it; Go crypto/ed25519 verifies the identical RFC 8032 bytes.
test('when signing the RFC 8032 empty message, the host signature matches the fixed vector', () => {
  const seed = Buffer.from('9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60', 'hex')
  const want = Buffer.from('e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b', 'hex')
  const signed = ed25519.sign(new Uint8Array(), seed)
  assert.equal(Buffer.from(signed).equals(want), true)
  assert.equal(ed25519.verify(signed, new Uint8Array(), ed25519.getPublicKey(seed), { zip215: false }), true)
})
