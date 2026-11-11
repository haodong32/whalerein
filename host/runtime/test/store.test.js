import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { DatabaseSync } from 'node:sqlite'
import { StateStore } from '../src/state/store.js'
import { PluginError } from '../src/errors.js'

function memoryStore(now = () => 100) {
  return new StateStore({ path: ':memory:', now })
}

test('when metadata and public identity are updated, reads return the latest values without exposing private material', () => {
  const store = memoryStore()
  assert.equal(store.getMeta('missing'), null)
  store.setMeta('a', 'b')
  store.setMeta('a', 'c')
  assert.equal(store.getMeta('a'), 'c')
  assert.equal(store.getHostPublicKey(), null)
  store.saveHostPublicKey(new Uint8Array([1, 2, 3]), 'fp', 11)
  assert.deepEqual([...store.getHostPublicKey().publicKey], [1, 2, 3])
  assert.equal(store.getHostPublicKey().fingerprint, 'fp')
  store.saveHostPublicKey(new Uint8Array([4, 5]), 'fp2')
  assert.equal(store.getHostPublicKey().fingerprint, 'fp2')
  store.close()
  store.close()
})

test('when a peer is authorized or removed, the store updates its key and cached revocation', () => {
  const store = memoryStore()
  store.upsertPeerKey({ deviceId: 'd1', publicKey: new Uint8Array([1]), fingerprint: 'f1', claimId: 'c1' })
  assert.equal(store.countPeerKeys(), 1)
  assert.deepEqual(store.listPeerKeys(), [{ deviceId: 'd1', fingerprint: 'f1', pairedAt: 100, displayName: '', os: '' }])
  assert.equal('publicKey' in store.listPeerKeys()[0], false)
  store.upsertPeerKey({ deviceId: 'd1', publicKey: new Uint8Array([9]), fingerprint: 'f9', displayName: '手机', os: 'macos' })
  assert.equal(store.getPeerKey('d1').displayName, '手机')
  assert.equal(store.getPeerKey('d1').os, 'macos')
  assert.equal(store.listPeerKeys()[0].displayName, '手机')
  assert.deepEqual([...store.getPeerKey('d1').publicKey], [9])
  assert.equal(store.getPeerKey('missing'), null)
  store.applyRevocations(['d1'], { replayId: 'r1', revokedAt: 5 })
  assert.equal(store.isRevoked('d1'), true)
  assert.equal(store.getPeerKey('d1'), null)
  store.upsertPeerKey({ deviceId: 'd1', publicKey: new Uint8Array([2]), fingerprint: 'f2', grantGeneration:1 })
  assert.equal(store.isRevoked('d1'), false)
  store.deletePeerKey('d1')
  assert.equal(store.countPeerKeys(), 0)
  store.deletePeerKey('d1')
})

test('when opening an older peer-key schema, migration adds the display-name column', () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-store-'))
  const path = join(dir, 'state.sqlite')
  try {
    const db = new DatabaseSync(path)
    db.exec(`
      CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
      INSERT INTO meta (key, value) VALUES ('schema_version', '1');
      CREATE TABLE peer_keys (
        device_id TEXT PRIMARY KEY,
        public_key BLOB NOT NULL,
        fingerprint TEXT NOT NULL,
        claim_id TEXT,
        paired_at INTEGER NOT NULL
      );
      INSERT INTO peer_keys (device_id, public_key, fingerprint, claim_id, paired_at)
      VALUES ('old', x'01', 'fold', 'c', 1);
    `)
    db.close()
    const store = new StateStore({ path, now: () => 100 })
    assert.equal(store.getPeerKey('old').displayName, '')
    assert.equal(store.getPeerKey('old').os, '')
    store.upsertPeerKey({ deviceId: 'old', publicKey: new Uint8Array([1]), fingerprint: 'fold', displayName: '旧设备' })
    assert.equal(store.listPeerKeys()[0].displayName, '旧设备')
    store.close()
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('when the same revocation is applied twice, the store records it once', () => {
  const store = memoryStore()
  store.upsertPeerKey({ deviceId: 'a', publicKey: new Uint8Array([1]), fingerprint: 'a' })
  store.upsertPeerKey({ deviceId: 'b', publicKey: new Uint8Array([2]), fingerprint: 'b' })
  const applied = store.applyRevocations(['a', 'a', '', 'b'], { replayId: 'r', revokedAt: 9 })
  assert.deepEqual(applied, ['a', 'b'])
  assert.equal(store.countRevocations(), 2)
  assert.equal(store.getRevocation('a').replayId, 'r')
  assert.equal(store.getRevocation('missing'), null)
  store.applyRevocations(['a'], { replayId: 'r2', revokedAt: 10 })
  assert.equal(store.countRevocations(), 2)
  assert.equal(store.getRevocation('a').replayId, 'r2')
})

test('when a command key is reused with a different digest, the store returns the original immutable admission for service-level conflict detection', () => {
  const store = memoryStore()
  const first = store.createOrGetCommand({
    deviceId: 'd1',
    commandId: 'c1',
    requestId: 'r1',
    digest: new Uint8Array(32).fill(1),
    expiresAt: 500,
  })
  assert.equal(first.created, true)
  assert.equal(first.command.status, 'accepted')
  const second = store.createOrGetCommand({
    deviceId: 'd1',
    commandId: 'c1',
    requestId: 'r2',
    digest: new Uint8Array(32).fill(2),
    expiresAt: 600,
  })
  assert.equal(second.created, false)
  assert.equal(second.command.expiresAt, 500)
  assert.deepEqual([...second.command.digest], [...new Uint8Array(32).fill(1)])
  assert.equal(store.getCommand('d1', 'c1').requestId, 'r1')
  assert.equal(store.getCommand('d1', 'missing'), null)
  const equal = store.createOrGetCommand({ deviceId: 'd1', commandId: 'c1', requestId: 'r3', digest: new Uint8Array(32).fill(1), expiresAt: 700 })
  assert.equal(equal.created, false)
  assert.equal(equal.command.requestId, 'r1')
  const otherDevice = store.createOrGetCommand({ deviceId: 'd2', commandId: 'c1', requestId: 'r4', digest: new Uint8Array(32).fill(2), expiresAt: 800 })
  assert.equal(otherDevice.created, true)
  assert.equal(store.countCommands('accepted'), 2)
})

test('when crash recovery sees admitted and succeeded commands, only unfinished admissions become unknown and terminal bytes are preserved', () => {
  const store = memoryStore()
  for (const id of ['a', 'b', 'c']) {
    store.createOrGetCommand({
      deviceId: 'd',
      commandId: id,
      requestId: `r-${id}`,
      digest: new Uint8Array(32),
      expiresAt: 1,
    })
  }
  store.setCommandStatus({ deviceId: 'd', commandId: 'b', status: 'succeeded', resultPayload: new Uint8Array([7, 8]) })
  assert.equal(store.countCommands('accepted'), 2)
  assert.equal(store.countCommands('succeeded'), 1)
  assert.deepEqual([...store.getCommand('d', 'b').resultPayload], [7, 8])
  assert.equal(store.setCommandStatus({ deviceId: 'd', commandId: 'missing', status: 'failed' }), false)
  assert.equal(store.recoverAcceptedCommands(), 2)
  assert.equal(store.countCommands('unknown'), 2)
  assert.equal(store.getCommand('d', 'a').errorCode, 'unavailable')
  assert.equal(store.getCommand('d', 'c').status, 'unknown')
  assert.equal(store.getCommand('d', 'b').status, 'succeeded')
  assert.deepEqual([...store.getCommand('d', 'b').resultPayload], [7, 8])
  assert.equal(store.recoverAcceptedCommands(), 0)
})

test('when outbox limits are reached, inserts are bounded and acknowledged rows can be removed', () => {
  const store = memoryStore()
  const first = store.addOutbox({ deviceId: 'a', channelId: 'c1', payload: new Uint8Array([1]), perDeviceLimit: 2, totalLimit: 3 })
  assert.equal(first.overflow, false)
  store.addOutbox({ deviceId: 'a', channelId: 'c1', payload: new Uint8Array([2]), perDeviceLimit: 2, totalLimit: 3 })
  const overflow = store.addOutbox({ deviceId: 'a', channelId: 'c1', payload: new Uint8Array([3]), perDeviceLimit: 2, totalLimit: 3 })
  assert.equal(overflow.overflow, true)
  assert.equal(overflow.reason, 'device')
  store.addOutbox({ deviceId: 'b', channelId: 'c2', payload: new Uint8Array([4]), perDeviceLimit: 5, totalLimit: 3 })
  const total = store.addOutbox({ deviceId: 'c', channelId: 'c3', payload: new Uint8Array([5]), perDeviceLimit: 5, totalLimit: 3 })
  assert.equal(total.overflow, true)
  assert.equal(total.reason, 'total')
  assert.equal(store.countOutbox(), 3)
  assert.equal(store.countOutbox('a'), 2)
  const rows = store.listOutbox(10)
  assert.equal(rows.length, 3)
  assert.deepEqual([...rows[0].payload], [1])
  store.removeOutbox(rows[0].id)
  assert.equal(store.countOutbox(), 2)
  store.deleteOutboxForChannel('a', 'c1')
  assert.equal(store.countOutbox('a'), 0)
  store.deleteOutboxForDevice('b')
  assert.equal(store.countOutbox(), 0)
})

test('when a file-backed store is reopened, metadata, pinned public identity and complete command admissions survive', () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-store-'))
  try {
    const path = join(dir, 'state.sqlite')
    const first = new StateStore({ path })
    first.setMeta('k', 'v')
    first.saveHostPublicKey(new Uint8Array(32).fill(7), 'public-fingerprint', 11)
    first.createOrGetCommand({ deviceId: 'd', commandId: 'c', requestId: 'r', digest: new Uint8Array(32).fill(3), expiresAt: 123 })
    first.close()
    const second = new StateStore({ path })
    assert.equal(second.getMeta('k'), 'v')
    assert.equal(second.getCommand('d', 'c').status, 'accepted')
    assert.equal(second.getCommand('d', 'c').requestId, 'r')
    assert.equal(second.getCommand('d', 'c').expiresAt, 123)
    assert.deepEqual([...second.getCommand('d', 'c').digest], [...new Uint8Array(32).fill(3)])
    assert.equal(second.getHostPublicKey().fingerprint, 'public-fingerprint')
    assert.deepEqual([...second.getHostPublicKey().publicKey], [...new Uint8Array(32).fill(7)])
    second.close()
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('when a persisted schema version is unknown, opening rejects internal instead of claiming to migrate it', () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-schema-'))
  try {
    const path = join(dir, 'state.sqlite')
    const store = new StateStore({ path })
    store.setMeta('schema_version', '99')
    store.close()
    assert.throws(() => new StateStore({ path }), error => error instanceof PluginError && error.code === 'internal')
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})



test('an old revocation preserves newer peers and its watermark blocks delayed old activation',()=>{
 const store=memoryStore()
 try {
  const key=new Uint8Array([1])
  store.upsertPeerKey({deviceId:'d',publicKey:key,fingerprint:'fp',grantGeneration:2})
  assert.deepEqual(store.applyRevocations(['d'],{generations:{d:1}}),[])
  assert.equal(store.getPeerKey('d').grantGeneration,2)
  assert.equal(store.isRevoked('d'),false)
  assert.equal(store.isGrantRevoked('d',1),true)
  store.applyRevocations(['d'],{generations:{d:2}})
  assert.equal(store.getPeerKey('d'),null)
  store.upsertPeerKey({deviceId:'d',publicKey:key,fingerprint:'fp',grantGeneration:3})
  store.applyRevocations(['d'],{generations:{d:1}})
  assert.equal(store.getPeerKey('d').grantGeneration,3)
  assert.equal(store.getRevocation('d').grantGeneration,2)
  assert.equal(store.isRevoked('d'),false)
 } finally {store.close()}
})
