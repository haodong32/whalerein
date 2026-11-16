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

test('managed profiles bind credentials to an opaque per-profile record and never reuse the legacy key', async () => {
  const credentials = store()
  const first = new DshHostCredentials({ ctx: { credentials }, profileId: 'profile-one' })
  await first.setCredential('ignored', 'key-one')
  const second = new DshHostCredentials({ ctx: { credentials }, profileId: 'profile-two' })
  await second.setCredential('ignored', 'key-two')
  const keys = [...credentials.records.keys()]
  assert.equal(keys.length, 2)
  assert.equal(keys.every((key) => /^whalerein-dsh\/profile-[0-9a-f]{24}\/host-key$/.test(key)), true)
  assert.equal(keys.includes('whalerein-dsh/host-key'), false)
  assert.equal(await new DshHostCredentials({ ctx: { credentials }, profileId: 'profile-one' }).getCredential(), 'key-one')
  assert.equal(await new DshHostCredentials({ ctx: { credentials }, profileId: 'profile-two' }).getCredential(), 'key-two')
  assert.equal(await new DshHostCredentials({ ctx: { credentials }, profileId: 'profile-three' }).getCredential(), null)
})

test('a managed profile does not migrate an unrelated legacy host key into its namespace', async () => {
  const credentials = store({ 'whalerein-dsh/host-key': { kind: 'grant', payload: { privateKeyBase64: 'legacy-key' } } })
  const adapter = new DshHostCredentials({ ctx: { credentials }, hostId: 'host-1', profileId: 'profile-one' })
  assert.equal(await adapter.getCredential(), null)
  assert.deepEqual(credentials.writes, [])
})

test('manager purpose keys map to distinct profile-scoped credential records', async () => {
  const credentials = store()
  const adapter = new DshHostCredentials({ ctx: { credentials } })
  const signing = 'whalerein-host/v1/dsh/0123456789abcdef0123456789abcdef/ed25519-signing-key'
  const noise = 'whalerein-host/v1/dsh/0123456789abcdef0123456789abcdef/noise-private-key'
  await adapter.setCredential(signing, 'signing-secret')
  await adapter.setCredential(noise, 'noise-secret')
  assert.equal(await adapter.getCredential(signing), 'signing-secret')
  assert.equal(await adapter.getCredential(noise), 'noise-secret')
  assert.equal(await adapter.getCredential('whalerein-host/v1/dsh/0123456789abcdef0123456789abcdef/poll-credential'), null)
  const keys = [...credentials.records.keys()]
  assert.equal(keys.length, 2)
  assert.equal(keys.every((key) => /^whalerein-dsh\/cred-[0-9a-f]{64}$/.test(key)), true)
  assert.equal(keys.includes('whalerein-dsh/host-key'), false)
  assert.equal(JSON.stringify([...credentials.records.values()]).includes('signing-secret'), true)
})

test('legacy fixed-name host credentials and arbitrary names keep the single legacy record', async () => {
  const credentials = store()
  const adapter = new DshHostCredentials({ ctx: { credentials } })
  await adapter.setCredential('whalerein.host-private-key.v1', 'legacy-host-key')
  assert.equal(await adapter.getCredential('whalerein.host-private-key.v1'), 'legacy-host-key')
  assert.equal(await adapter.getCredential(), 'legacy-host-key')
  await adapter.setCredential('ignored-name', 'still-legacy')
  assert.equal(await adapter.getCredential(), 'still-legacy')
  assert.deepEqual([...credentials.records.keys()], ['whalerein-dsh/host-key'])
})

test('a reference-style store refuses manager purpose scopes instead of collapsing secrets', async () => {
  const adapter = new DshHostCredentials({
    ctx: { credentials: { async resolve() { return { value: 'reference-key' } }, async set() {} } },
  })
  assert.equal(await adapter.getCredential('whalerein-host/v1/dsh/0123456789abcdef0123456789abcdef/noise-private-key'), null)
  await assert.rejects(
    adapter.setCredential('whalerein-host/v1/dsh/0123456789abcdef0123456789abcdef/noise-private-key', 'x'),
    (error) => error instanceof AdapterError && error.code === 'unsupported_capability',
  )
})
