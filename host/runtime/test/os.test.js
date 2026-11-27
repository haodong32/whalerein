import assert from 'node:assert/strict'
import test from 'node:test'
import { hostOsFamily, sanitizeOs } from '../src/os.js'

test('when given a known OS token, sanitizeOs preserves it', () => {
  assert.equal(sanitizeOs(' macOS '), 'macos')
  assert.equal(sanitizeOs('Windows'), 'windows')
  assert.equal(sanitizeOs('linux'), 'linux')
  assert.equal(sanitizeOs('browser'), '')
  assert.equal(sanitizeOs(''), '')
  assert.equal(sanitizeOs(undefined), '')
})

test('the directory OS family is coarse and never includes a build version', () => {
  assert.equal(hostOsFamily('darwin'), 'macOS')
  assert.equal(hostOsFamily('win32'), 'Windows')
  assert.equal(hostOsFamily('linux'), 'Linux')
  assert.equal(hostOsFamily('freebsd'), '')
  assert.equal(hostOsFamily('android'), '')
  assert.equal(hostOsFamily(''), '')
})
