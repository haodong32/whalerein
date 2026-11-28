import assert from 'node:assert/strict'
import test from 'node:test'
import {
  generateHostId,
  HOST_ID_ALPHABET,
  HOST_ID_PREFIX,
  HOST_ID_RANDOM_LENGTH,
  isNameDerivedHostId,
  slugHostName,
} from '../src/host-id.js'

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

test('a generated host id is opaque and independent of the computer name', () => {
  const zero = generateHostId(() => Buffer.alloc(HOST_ID_RANDOM_LENGTH, 0))
  assert.equal(zero, HOST_ID_PREFIX + HOST_ID_ALPHABET[0].repeat(HOST_ID_RANDOM_LENGTH))
  assert.doesNotMatch(zero, /macbook|haodong/i)

  const first = generateHostId(() => Buffer.from(Array.from({ length: HOST_ID_RANDOM_LENGTH }, (_, index) => index)))
  const second = generateHostId(() => Buffer.from(Array.from({ length: HOST_ID_RANDOM_LENGTH }, (_, index) => index + 1)))
  assert.match(first, new RegExp(`^${HOST_ID_PREFIX}[${HOST_ID_ALPHABET}]{${HOST_ID_RANDOM_LENGTH}}$`))
  assert.notEqual(first, second)
})

test('only the retired name-derived route format is recognized as legacy', () => {
  const hostname = 'Haodong的MacBook Pro.local'
  assert.equal(isNameDerivedHostId('haodong-macbook-pro-k7m2', hostname), true)
  // A truncation of a long computer-name slug keeps its suffix.
  assert.equal(isNameDerivedHostId('haodong-macbook-pr-k7m2', hostname), true)

  assert.equal(isNameDerivedHostId(generateHostId(), hostname), false)
  assert.equal(isNameDerivedHostId('host-aaaaaaaaaaaaaaaaaaaa', hostname), false)
  assert.equal(isNameDerivedHostId('opaque-managed-id', hostname), false)
  // Same suffix but a different computer's name is not this Host's legacy id.
  assert.equal(isNameDerivedHostId('someone-else-k7m2', hostname), false)
  assert.equal(isNameDerivedHostId('', hostname), false)
})

test('a generated id fits the topic id bound', () => {
  const id = generateHostId()
  assert.ok(id.length <= 128)
  assert.equal(id.includes('/'), false)
})
