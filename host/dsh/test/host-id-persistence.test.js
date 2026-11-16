import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { withHostId } from '../index.js'

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
