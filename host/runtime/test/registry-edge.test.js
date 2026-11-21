import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { HostProfileRegistry } from '../src/registry.js'
import { officialService } from '../src/services.js'

const NOW = 1_800_000_000_000

function registryFixture() {
  const stateRoot = mkdtempSync(join(tmpdir(), 'whalerein-registry-'))
  const registry = new HostProfileRegistry({ stateRoot, now: () => NOW })
  registry.ensure()
  return { registry, stateRoot }
}

test('registry mutations refuse a stale expected revision without applying the change', () => {
  const { registry, stateRoot } = registryFixture()
  try {
    const initial = registry.read()
    const profileId = initial.active
    const stale = initial.revision - 1
    assert.throws(
      () => registry.setPending(profileId, { paused: true }, { expectedRevision: stale }),
      (error) => error.code === 'registry_write_failed',
    )
    assert.throws(
      () => registry.clearPendingPhase(profileId, { expectedRevision: stale }),
      (error) => error.code === 'registry_write_failed',
    )
    assert.equal(registry.get(profileId).pending.paused, undefined)
    assert.equal(registry.read().revision, initial.revision)
  } finally {
    rmSync(stateRoot, { recursive: true, force: true })
  }
})

test('activation prepare, abort and commit reject stale revisions and clear prepared markers', () => {
  const { registry, stateRoot } = registryFixture()
  try {
    const first = registry.read().active
    const second = registry.addProfile({
      label: 'Second Host',
      environment: 'production',
      service: officialService('production'),
    })
    const revision = registry.read().revision
    assert.throws(
      () => registry.prepareActivation(second.id, revision - 1),
      (error) => error.code === 'registry_write_failed',
    )
    assert.throws(
      () => registry.commitActivation(second.id, revision - 1),
      (error) => error.code === 'registry_write_failed',
    )
    assert.throws(
      () => registry.abortActivation(revision - 1),
      (error) => error.code === 'registry_write_failed',
    )
    assert.equal(registry.read().prepared, null)
    assert.equal(registry.read().active, first)

    const preparedRevision = registry.read().revision
    registry.prepareActivation(second.id, preparedRevision)
    assert.equal(registry.read().prepared.profileId, second.id)
    assert.equal(registry.abortActivation(registry.read().revision), null)
    assert.equal(registry.read().prepared, null)
    assert.equal(registry.abortActivation(registry.read().revision), null)

    const commitRevision = registry.read().revision
    registry.prepareActivation(second.id, commitRevision)
    const committed = registry.commitActivation(second.id, registry.read().revision)
    assert.equal(committed.id, second.id)
    assert.equal(registry.read().active, second.id)
  } finally {
    rmSync(stateRoot, { recursive: true, force: true })
  }
})

test('registry exposes its state root and file path without writing', () => {
  const { registry, stateRoot } = registryFixture()
  try {
    assert.equal(registry.stateRoot, stateRoot)
    assert.match(registry.filePath, /host-profiles\.json$/)
    assert.equal(registry.stateDirFor(registry.read().active).startsWith(join(stateRoot, 'profiles')), true)
  } finally {
    rmSync(stateRoot, { recursive: true, force: true })
  }
})

test('registry rejects corrupt stored service descriptors on read', async () => {
  const { readFileSync, writeFileSync } = await import('node:fs')
  const { registry, stateRoot } = registryFixture()
  try {
    const path = registry.filePath
    const baseline = readFileSync(path, 'utf8')
    const cases = [
      ['official service identity', (service) => { service.id = '' }],
      ['non-string issuer', (service) => { service.issuer = 42 }],
      ['custom service environment', (service, profile) => {
        profile.environment = 'custom'
        service.environment = 'standalone'
      }],
    ]
    for (const [name, mutate] of cases) {
      const document = JSON.parse(baseline)
      mutate(document.profiles[0].service, document.profiles[0])
      writeFileSync(path, JSON.stringify(document))
      assert.throws(
        () => new HostProfileRegistry({ stateRoot }).read(),
        (error) => error.code === 'registry_corrupt',
        name,
      )
    }
    writeFileSync(path, baseline)
    assert.equal(new HostProfileRegistry({ stateRoot }).read().profiles.length, 1)
  } finally {
    rmSync(stateRoot, { recursive: true, force: true })
  }
})
