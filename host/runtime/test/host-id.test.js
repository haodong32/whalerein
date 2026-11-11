import assert from 'node:assert/strict'
import test from 'node:test'
import { generateHostId, HOST_ID_ALPHABET, slugHostName } from '../src/host-id.js'

test('when a computer name is slugged, the result is safe for MQTT topics', () => {
  assert.equal(slugHostName('Haodong的MacBook Pro.local'), 'haodong-macbook-pro')
  assert.equal(slugHostName('HOST.local'), 'host')
  assert.equal(slugHostName('Already-safe_1'), 'already-safe_1')
  assert.equal(slugHostName('---'), 'host')
  assert.equal(slugHostName(''), 'host')
  assert.equal(slugHostName(undefined), 'host')
  assert.equal(slugHostName('的'), 'host')
  assert.equal(slugHostName('a--b..c'), 'a-b..c')
})

test('when a host ID is generated, it has a four-character suffix and stays within 128 characters', () => {
  const random = () => Buffer.from([9, 28, 10, 23])
  assert.equal(generateHostId('Haodong的MacBook Pro.local', random), 'haodong-macbook-pro-k7m2')
  const longName = `${'n'.repeat(200)}.local`
  const generated = generateHostId(longName, random)
  assert.equal([...generated].length, 128)
  assert.match(generated, /-k7m2$/)
  assert.equal(generated.includes('--'), false)
  const suffix = generateHostId('---name---', () => Buffer.from([30, 30, 30, 30]))
  assert.equal(suffix, `name-${HOST_ID_ALPHABET.at(-1).repeat(4)}`)
})
