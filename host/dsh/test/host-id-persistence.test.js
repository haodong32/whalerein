import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import os, { tmpdir } from 'node:os'
import { join } from 'node:path'
import { withHostId } from '../index.js'
import { slugHostName } from 'whalerein-host/host-id.js'

function directory(t) {
  const value = mkdtempSync(join(tmpdir(), 'whalerein-host-id-'))
  t.after(() => rmSync(value, { recursive: true, force: true }))
  return value
}

test('when the profile write is rejected, the generated host id survives in the state directory', (t) => {
  const stateDir = directory(t)
  const ctx = { fiber: {}, emit() { throw new Error('profile is read-only') } }
  const first = withHostId(ctx, { state_dir: stateDir })
  assert.ok(first.host_id)
  assert.equal(readFileSync(join(stateDir, 'host-id'), 'utf8').trim(), first.host_id)
  const second = withHostId(ctx, { state_dir: stateDir })
  assert.equal(second.host_id, first.host_id)
})

test('a saved host id wins over the state-directory fallback', (t) => {
  const stateDir = directory(t)
  writeFileSync(join(stateDir, 'host-id'), 'persisted-host\n')
  const config = withHostId({}, { state_dir: stateDir, host_id: 'profile-host' })
  assert.equal(config.host_id, 'profile-host')
  assert.equal(readFileSync(join(stateDir, 'host-id'), 'utf8').trim(), 'persisted-host')
})

test('an unreadable or invalid fallback is replaced once and stays stable', (t) => {
  const stateDir = directory(t)
  writeFileSync(join(stateDir, 'host-id'), 'not a valid/id\n')
  const first = withHostId({}, { state_dir: stateDir })
  assert.doesNotMatch(first.host_id, /[ /]/)
  const second = withHostId({}, { state_dir: stateDir })
  assert.equal(second.host_id, first.host_id)
})

test('a persisted name-derived route is retired once and replaced by an opaque id', (t) => {
  const stateDir = directory(t)
  const legacy = `${slugHostName(os.hostname())}-k7m2`
  writeFileSync(join(stateDir, 'host-id'), `${legacy}\n`)
  const first = withHostId({}, { state_dir: stateDir })
  assert.match(first.host_id, /^host-[a-z2-9]{20}$/)
  assert.notEqual(first.host_id, legacy)
  assert.equal(readFileSync(join(stateDir, 'host-id'), 'utf8').trim(), first.host_id)
  assert.equal(readFileSync(join(stateDir, 'host-id-retired'), 'utf8').trim(), legacy)

  const second = withHostId({}, { state_dir: stateDir })
  assert.equal(second.host_id, first.host_id)
  assert.equal(readFileSync(join(stateDir, 'host-id-retired'), 'utf8').trim(), legacy)
})

test('an explicit name-derived id is replaced at startup', (t) => {
  const stateDir = directory(t)
  const legacy = `${slugHostName(os.hostname())}-k7m2`
  const config = withHostId({}, { state_dir: stateDir, host_id: legacy })
  assert.match(config.host_id, /^host-[a-z2-9]{20}$/)
  assert.notEqual(config.host_id, legacy)
  assert.equal(readFileSync(join(stateDir, 'host-id-retired'), 'utf8').trim(), legacy)
})

test('an explicit name-derived id is stable across restarts when the profile write is rejected', (t) => {
  const stateDir = directory(t)
  const legacy = `${slugHostName(os.hostname())}-k7m2`
  const ctx = { fiber: {}, emit() { throw new Error('profile is read-only') } }
  const first = withHostId(ctx, { state_dir: stateDir, host_id: legacy })
  assert.match(first.host_id, /^host-[a-z2-9]{20}$/)
  assert.notEqual(first.host_id, legacy)
  const second = withHostId(ctx, { state_dir: stateDir, host_id: legacy })
  assert.equal(second.host_id, first.host_id, 'the fallback route must not rotate on restart')
  assert.equal(readFileSync(join(stateDir, 'host-id'), 'utf8').trim(), first.host_id)
  assert.equal(readFileSync(join(stateDir, 'host-id-retired'), 'utf8').trim(), legacy)
})

test('an explicit name-derived id reuses a persisted opaque replacement', (t) => {
  const stateDir = directory(t)
  const legacy = `${slugHostName(os.hostname())}-k7m2`
  writeFileSync(join(stateDir, 'host-id'), 'host-abcdefghjkmnpqrstuvwxyz2345\n')
  const config = withHostId({}, { state_dir: stateDir, host_id: legacy })
  assert.equal(config.host_id, 'host-abcdefghjkmnpqrstuvwxyz2345')
})

test('an explicit opaque id is preserved unchanged', (t) => {
  const stateDir = directory(t)
  const explicit = 'host-abcdefghjkmnpqrstuvwxyz2345'
  const config = withHostId({}, { state_dir: stateDir, host_id: explicit })
  assert.equal(config.host_id, explicit)
})
