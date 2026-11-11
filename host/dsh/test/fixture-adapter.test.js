import test from 'node:test'
import assert from 'node:assert/strict'
import { encode, decode } from '../../../tests/integration/fixtures/adapter.mjs'

test('fixture adapter preserves byte arrays and Buffers across the process boundary', () => {
  for (const digest of [new Uint8Array([1, 2, 3]), Buffer.from([1, 2, 3])]) {
    const received = decode(encode({ args: ['approval-fixture', digest] }))
    assert.ok(received.args[1] instanceof Uint8Array)
    assert.deepEqual(Array.from(received.args[1]), [1, 2, 3])
  }
})
