import test from 'node:test'
import assert from 'node:assert/strict'
import { mapAdapterError } from '../src/adapter/errors.js'

test('when common DSH failures reach the adapter boundary, they map to stable codes without leaking text', () => {
  const cases = [
    ['session not found', 'not_found'],
    ['SESSION_QUERY_SESSION_NOT_FOUND', 'not_found'],
    ['unsupported capability', 'unsupported_capability'],
    ['operation aborted', 'unavailable'],
    ['request cancelled', 'unavailable'],
    ['agent is busy', 'conflict'],
    ['session/writer-held', 'conflict'],
    ['request expired', 'expired'],
    ['timeout', 'expired'],
    ['not authenticated', 'unauthorized'],
    ['access denied', 'forbidden'],
    ['provider credentials unavailable', 'unavailable'],
    ['invalid argument', 'invalid_argument'],
    ['mystery', 'internal'],
  ]
  for (const [message, code] of cases) {
    assert.equal(mapAdapterError(new Error(message)), code, message)
  }
  assert.equal(mapAdapterError('session/not-found'), 'not_found')
  assert.equal(mapAdapterError(null), 'internal')
  assert.equal(mapAdapterError({ code: 'custom', name: 'X', message: 'busy' }), 'conflict')
})
