import assert from 'node:assert/strict'
import test from 'node:test'
import { sanitizeOs } from '../src/os.js'

test('when given a known OS token, sanitizeOs preserves it', () => {
  assert.equal(sanitizeOs(' macOS '), 'macos')
  assert.equal(sanitizeOs('Windows'), 'windows')
  assert.equal(sanitizeOs('linux'), 'linux')
  assert.equal(sanitizeOs('browser'), '')
  assert.equal(sanitizeOs(''), '')
  assert.equal(sanitizeOs(undefined), '')
})
