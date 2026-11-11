import test from 'node:test'
import assert from 'node:assert/strict'
import {
  ERROR_CODES,
  ONLINE_STATUSES,
  PluginError,
  AdapterError,
  DefinitiveExecutionError,
  assertErrorCode,
  isErrorCode,
  publicMessage,
} from '../src/errors.js'

test('when plugin error codes are enumerated, they match the protobuf enum names', () => {
  assert.equal(ERROR_CODES.includes('payload_digest_mismatch'), true)
  assert.equal(ERROR_CODES.includes('revocations_pending'), true)
  assert.equal(ONLINE_STATUSES.includes('host_accepted'), true)
  assert.equal(isErrorCode('internal'), true)
  assert.equal(isErrorCode('nope'), false)
  assert.equal(isErrorCode(undefined), false)
  assert.equal(assertErrorCode('expired'), 'expired')
  assert.throws(() => assertErrorCode('nope'), (error) => error.code === 'internal')
  assert.equal(publicMessage('approval_expired'), 'approval expired')
  assert.equal(publicMessage('error_code_unspecified'), '')
  assert.equal(publicMessage('unknown_code'), 'internal error')
  for (const code of ERROR_CODES) {
    if (code === 'error_code_unspecified') continue
    assert.equal(typeof publicMessage(code), 'string')
    assert.ok(publicMessage(code).length > 0)
  }
})

test('when a typed error omits a code, it uses the stable default', () => {
  assert.equal(new PluginError('expired').code, 'expired')
  assert.equal(new PluginError('expired').detail, '')
  assert.equal(new PluginError('expired', 'detail').detail, 'detail')
  assert.equal(new AdapterError('bogus').code, 'internal')
  assert.equal(new AdapterError('not_found').code, 'not_found')
  assert.equal(new AdapterError('not_found').detail, '')
})

test('only an explicit definitive execution error carries failure evidence while preserving the shared error vocabulary', () => {
  const failure = new DefinitiveExecutionError('unavailable')
  assert.ok(failure instanceof AdapterError)
  assert.equal(failure.name, 'DefinitiveExecutionError')
  assert.equal(failure.code, 'unavailable')
  assert.equal(new DefinitiveExecutionError('invalid-code').code, 'internal')
  assert.equal(new AdapterError('unavailable') instanceof DefinitiveExecutionError, false)
})

