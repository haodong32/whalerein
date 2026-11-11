import test from 'node:test'
import assert from 'node:assert/strict'
import { approvalDigest, bytesEqual, commandDigest, randomId, sha256Bytes, sha256Text } from '../src/security/digest.js'

test('when hashing the published empty and abc vectors, byte, text and command digests match SHA-256', () => {
  const vectors = [
    ['', 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'],
    ['abc', 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'],
  ]
  for (const [text, expected] of vectors) {
    const bytes = new TextEncoder().encode(text)
    assert.equal(Buffer.from(sha256Bytes(bytes)).toString('hex'), expected)
    assert.equal(Buffer.from(sha256Text(text)).toString('hex'), expected)
    assert.equal(Buffer.from(commandDigest(bytes)).toString('hex'), expected)
  }
})

test('when random IDs are generated, they use lowercase hex with the requested byte length and distinct values', () => {
  assert.notEqual(randomId(), randomId())
  assert.equal(randomId(8).length, 16)
  assert.match(randomId(8), /^[0-9a-f]{16}$/)
})

test('when approval fields use either casing, the digest uses the canonical field order', () => {
  const canonical = {
    approvalId: 'a',
    sessionId: 's',
    toolName: 'bash',
    callId: 'c',
    reason: 'r',
    version: 2,
    expiresAt: 99,
  }
  const snake = {
    approval_id: 'a',
    session_id: 's',
    tool_name: 'bash',
    call_id: 'c',
    reason: 'r',
    version: 2,
    expires_at_unix_ms: 99,
  }
  assert.deepEqual([...approvalDigest(canonical)], [...approvalDigest(snake)])
  assert.deepEqual([...approvalDigest({})], [...sha256Text('whalerein/v1/approval||||||0|0')])
})

test('when byte values are invalid or lengths differ, constant-time comparison returns false', () => {
  assert.equal(bytesEqual(new Uint8Array([1]), new Uint8Array([1])), true)
  assert.equal(bytesEqual(new Uint8Array([1]), new Uint8Array([2])), false)
  assert.equal(bytesEqual(new Uint8Array([1]), new Uint8Array([1, 2])), false)
  assert.equal(bytesEqual('a', new Uint8Array([1])), false)
  assert.equal(bytesEqual(null, null), false)
})
