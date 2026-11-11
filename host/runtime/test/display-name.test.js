import assert from 'node:assert/strict'
import test from 'node:test'
import { sanitizeDisplayName } from '../src/display-name.js'

test('when sanitizing a short display name, it keeps the label and removes control characters', () => {
  assert.equal(sanitizeDisplayName('  办公室的笔记本\n'), '办公室的笔记本')
  assert.equal(sanitizeDisplayName('a\u0001b\u007F'), 'ab')
  assert.equal(sanitizeDisplayName('   '), '')
  assert.equal(sanitizeDisplayName(undefined), '')
  assert.equal(sanitizeDisplayName('x'.repeat(80)), 'x'.repeat(64))
  assert.equal([...sanitizeDisplayName('你'.repeat(80))].length, 64)
})
