import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import {
  ERROR_CODES,
  ERROR_EXIT_CODES,
  ONLINE_STATUSES,
  PluginError,
  AdapterError,
  DefinitiveExecutionError,
  assertErrorCode,
  isErrorCode,
  publicMessage,
} from '../src/errors.js'

test('when plugin error codes are enumerated, every generated ErrorCode name is accepted and every pairing code is present', () => {
  // Read the generated descriptor instead of a hand-picked sample: a new wire
  // code must be added here or the plugin would silently degrade it to internal.
  const descriptor = JSON.parse(
    readFileSync(new URL('../src/generated/whalerein.json', import.meta.url), 'utf8'),
  )
  const generated = Object.keys(
    descriptor.nested.whalerein.nested.v1.nested.ErrorCode.values,
  )
  assert.deepEqual([...ERROR_CODES].sort(), [...generated].sort())
  for (const code of ['pairing_fenced', 'pairing_committed_unavailable', 'pairing_committed_unconfirmed']) {
    assert.equal(ERROR_CODES.includes(code), true, `${code} missing`)
    assert.equal(isErrorCode(code), true)
    assert.ok(publicMessage(code).length > 0)
  }
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
    if (code === 'error_code_unspecified' || code === 'internal') continue
    assert.equal(typeof publicMessage(code), 'string')
    assert.ok(publicMessage(code).length > 0)
    assert.equal(typeof ERROR_EXIT_CODES[code], 'number', `${code} exit code`)
    assert.notEqual(publicMessage(code), 'internal error', `${code} needs a fixed message`)
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

