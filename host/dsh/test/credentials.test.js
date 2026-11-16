import test from 'node:test'
import assert from 'node:assert/strict'
import { DshHostCredentials } from '../src/credentials.js'
import { AdapterError } from 'whalerein-host/errors.js'
import { sha256Text } from 'whalerein-host/security/digest.js'

function legacyKey(hostId) {
  return `whalerein-dsh/host-key-${Buffer.from(sha256Text(hostId).slice(0, 8)).toString('hex')}`
}

function store(initial = {}) {
  const records = new Map(Object.entries(initial))
  return {
    records,
    writes: [],
    async readRecord(key) { return records.get(key) ?? null },
    async modifyRecord(key, update) {
      const value = await update(records.get(key) ?? null)
      records.set(key, value)
      this.writes.push([key, value])
      return value
    },
  }
}

test('the credential record key stays stable when host_id changes', async () => {
  const credentials = store()
  const before = new DshHostCredentials({ ctx: { credentials }, hostId: 'office-host' })
  await before.setCredential('ignored', 'private-key-before')
  const after = new DshHostCredentials({ ctx: { credentials }, hostId: 'renamed-host' })
  assert.equal(await after.getCredential(), 'private-key-before')
  assert.deepEqual([...credentials.records.keys()], ['whalerein-dsh/host-key'])
  assert.equal(credentials.writes.length, 1)
})

test('a legacy host-scoped record is read and migrated without changing the identity', async () => {
  const credentials = store({ [legacyKey('host-1')]: { kind: 'grant', payload: { privateKeyBase64: 'legacy-key' } } })
  const adapter = new DshHostCredentials({ ctx: { credentials }, hostId: 'host-1' })
  assert.equal(await adapter.getCredential(), 'legacy-key')
  assert.equal(await adapter.getCredential(), 'legacy-key')
  assert.deepEqual(credentials.writes.map(([key]) => key), ['whalerein-dsh/host-key'])
  assert.equal(credentials.records.get('whalerein-dsh/host-key').payload.privateKeyBase64, 'legacy-key')
})

test('a failed migration still returns the legacy key for that run', async () => {
  const key = legacyKey('host-1')
  const credentials = {
    async readRecord(name) { return name === key ? { kind: 'grant', payload: { privateKeyBase64: 'legacy' } } : null },
    async modifyRecord() { throw new Error('credential store is read-only') },
  }
  const adapter = new DshHostCredentials({ ctx: { credentials }, hostId: 'host-1' })
  assert.equal(await adapter.getCredential(), 'legacy')
})

test('missing, non-grant and malformed records read as absent', async () => {
  for (const record of [null, { kind: 'other', payload: { privateKeyBase64: 'x' } }, { kind: 'grant' }, { kind: 'grant', payload: {} }]) {
    const credentials = store({ 'whalerein-dsh/host-key': record })
    const adapter = new DshHostCredentials({ ctx: { credentials }, hostId: 'host-1' })
    assert.equal(await adapter.getCredential(), null)
  }
  assert.equal(await new DshHostCredentials({ ctx: {}, hostId: 'host-1' }).getCredential(), null)
})

test('a reference-style credential store is read and written without record keys', async () => {
  const calls = []
  const credentials = {
    async resolve(ref) { calls.push(['resolve', ref]); return { value: 'reference-key' } },
    async set(ref, value) { calls.push(['set', ref, value]) },
  }
  const adapter = new DshHostCredentials({ ctx: { credentials }, hostId: 'host-1' })
  assert.equal(await adapter.getCredential(), 'reference-key')
  await adapter.setCredential('ignored', 'replacement')
  assert.deepEqual(calls, [
    ['resolve', 'WHALEREIN_HOST_PRIVATE_KEY'],
    ['set', 'WHALEREIN_HOST_PRIVATE_KEY', 'replacement'],
  ])
  assert.equal(await new DshHostCredentials({ ctx: { credentials: {} }, hostId: 'host-1' }).getCredential(), null)
})

test('credential failures become stable adapter errors without native text', async () => {
  const readFailure = new DshHostCredentials({ ctx: { credentials: { async readRecord() { throw new Error('private native detail') } } }, hostId: 'host-1' })
  await assert.rejects(readFailure.getCredential(), (error) => error instanceof AdapterError && !error.message.includes('private native detail'))
  await assert.rejects(new DshHostCredentials({ ctx: { credentials: {} }, hostId: 'host-1' }).setCredential('ignored', 'x'),
    (error) => error instanceof AdapterError && error.code === 'unsupported_capability')
  await assert.rejects(new DshHostCredentials({ ctx: {}, hostId: 'host-1' }).setCredential('ignored', 'x'),
    (error) => error instanceof AdapterError && error.code === 'unavailable')
  const writeFailure = new DshHostCredentials({ ctx: { credentials: { async modifyRecord() { throw new Error('private write detail') } } }, hostId: 'host-1' })
  await assert.rejects(writeFailure.setCredential('ignored', 'x'), (error) => error instanceof AdapterError && !error.message.includes('private write detail'))
})
