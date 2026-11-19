import test from 'node:test'
import assert from 'node:assert/strict'
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  HostProfileRegistry,
  LEGACY_PROFILE_ID,
  RegistryError,
  atomicWriteJson,
  hasLegacyMarkers,
} from '../src/registry.js'
import {
  OFFICIAL_SERVICES,
  customService,
  officialService,
  validateServiceMetadata,
  validateServiceOrigin,
} from '../src/services.js'
import { PluginError } from '../src/errors.js'

function tempRoot() {
  return mkdtempSync(join(tmpdir(), 'whalerein-registry-'))
}

test('official descriptors are frozen non-secret production and staging presets', () => {
  assert.equal(OFFICIAL_SERVICES.production.id, 'official-production')
  assert.equal(OFFICIAL_SERVICES.production.apiOrigin, 'https://app.whalerein.com')
  assert.equal(OFFICIAL_SERVICES.staging.id, 'official-staging')
  assert.equal(OFFICIAL_SERVICES.staging.apiOrigin, 'https://app.staging.whalerein.com')
  assert.ok(Object.isFrozen(OFFICIAL_SERVICES))
  const serialized = JSON.stringify(OFFICIAL_SERVICES)
  assert.ok(!/token|secret|password|region|mqtt|port/i.test(serialized))
  assert.equal(officialService('production'), OFFICIAL_SERVICES.production)
  assert.equal(officialService('staging'), OFFICIAL_SERVICES.staging)
  assert.throws(() => officialService('custom'), (error) => error.code === 'invalid_argument')
})

test('service origin validation enforces HTTPS, canonical origin and explicit loopback', () => {
  assert.equal(validateServiceOrigin('https://service.example'), 'https://service.example')
  assert.equal(validateServiceOrigin('http://127.0.0.1:8080', { allowLoopback: true }), 'http://127.0.0.1:8080')
  assert.throws(() => validateServiceOrigin('http://service.example'), (error) => error.code === 'invalid_argument')
  assert.throws(() => validateServiceOrigin('https://user:pass@service.example'), (error) => error.code === 'invalid_argument')
  assert.throws(() => validateServiceOrigin('https://service.example/path'), (error) => error.code === 'invalid_argument')
  assert.throws(() => validateServiceOrigin('https://service.example?query=1'), (error) => error.code === 'invalid_argument')
  assert.throws(() => validateServiceOrigin('not a url'), (error) => error.code === 'invalid_argument')
  assert.throws(() => validateServiceOrigin(''), (error) => error.code === 'invalid_argument')
})

test('custom service descriptors require explicit label and origin', () => {
  const custom = customService({ label: 'Lab', apiOrigin: 'https://lab.example' })
  assert.equal(custom.label, 'Lab')
  assert.equal(custom.environment, 'custom')
  assert.equal(custom.apiOrigin, 'https://lab.example')
  assert.throws(() => customService({ label: '', apiOrigin: 'https://lab.example' }), (error) => error.code === 'invalid_argument')
  assert.throws(() => customService({ label: ' Lab', apiOrigin: 'https://lab.example' }), (error) => error.code === 'invalid_argument')
  assert.throws(() => customService({ label: 'Lab', apiOrigin: 'https://lab.example', environment: 'standalone' }), (error) => error.code === 'invalid_argument')
})

test('service metadata validation accepts the frozen document and rejects redirects', () => {
  const metadata = {
    service: 'whalerein-cloud',
    environment: 'production',
    protocol: { version: 1, capabilities: ['host-enrollment-v1'] },
    approval: { origin: 'https://app.whalerein.com', path: '/approve/{id}' },
    enrollment: { createPath: '/api/enrollment-requests', pollAfterMs: 2000 },
  }
  const validated = validateServiceMetadata(metadata, { expectedEnvironment: 'production' })
  assert.equal(validated.service, 'whalerein-cloud')
  assert.equal(validated.pollAfterMs, 2000)
  assert.ok(Object.isFrozen(validated.capabilities))
  assert.throws(() => validateServiceMetadata(null), (error) => error.code === 'protocol_version_incompatible')
  assert.throws(() => validateServiceMetadata({ ...metadata, service: '' }), (error) => error.code === 'protocol_version_incompatible')
  assert.throws(() => validateServiceMetadata({ ...metadata, environment: 'standalone' }), (error) => error.code === 'protocol_version_incompatible')
  assert.throws(() => validateServiceMetadata(metadata, { expectedService: 'other' }), (error) => error.code === 'forbidden')
  assert.throws(() => validateServiceMetadata(metadata, { expectedEnvironment: 'staging' }), (error) => error.code === 'forbidden')
  assert.throws(() => validateServiceMetadata({ ...metadata, protocol: { version: 2, capabilities: ['host-enrollment-v1'] } }), (error) => error.code === 'protocol_version_incompatible')
  assert.throws(() => validateServiceMetadata({ ...metadata, protocol: { version: 1, capabilities: [] } }), (error) => error.code === 'unsupported_capability')
  assert.throws(() => validateServiceMetadata({ ...metadata, protocol: { version: 1, capabilities: 'x' } }), (error) => error.code === 'protocol_version_incompatible')
  assert.throws(() => validateServiceMetadata({ ...metadata, protocol: { version: 0, capabilities: ['host-enrollment-v1'] } }), (error) => error.code === 'protocol_version_incompatible')
  // An older Host reads a newer descriptor that advertises additional
  // capabilities. Unknown extras are retained, not treated as a failure.
  const extended = validateServiceMetadata({ ...metadata, protocol: { version: 1, capabilities: ['host-enrollment-v1', 'host-status', 'future-capability'] } })
  assert.deepEqual(extended.capabilities, ['host-enrollment-v1', 'host-status', 'future-capability'])
  assert.throws(() => validateServiceMetadata({ ...metadata, approval: null }), (error) => error.code === 'protocol_version_incompatible')
  assert.throws(() => validateServiceMetadata({ ...metadata, approval: { ...metadata.approval, path: 'approve' } }), (error) => error.code === 'protocol_version_incompatible')
  assert.throws(() => validateServiceMetadata({ ...metadata, enrollment: {} }), (error) => error.code === 'protocol_version_incompatible')
  assert.throws(() => validateServiceMetadata({ ...metadata, enrollment: { createPath: '/x', pollAfterMs: 1 } }), (error) => error.code === 'protocol_version_incompatible')
})

test('fresh bootstrap records one opaque production profile without network or CLI state', () => {
  const root = tempRoot()
  try {
    const registry = new HostProfileRegistry({ stateRoot: root })
    const state = registry.ensure()
    assert.equal(state.active, state.profiles[0].id)
    assert.match(state.active, /^[a-f0-9]{32}$/)
    assert.equal(state.profiles[0].environment, 'production')
    assert.equal(state.profiles[0].mode, 'managed')
    assert.equal(state.profiles[0].service.apiOrigin, OFFICIAL_SERVICES.production.apiOrigin)
    assert.equal(state.revision, 1)
    assert.equal(state.activationGeneration, 1)
    assert.equal(state.developerToolsEnabled, false)
    assert.equal(registry.read().revision, 1)
    assert.deepEqual(registry.list().length, 1)
    assert.equal(registry.get('missing'), null)
    assert.throws(() => registry.stateDirFor('missing'), (error) => error.code === 'profile_not_found')
    assert.equal(registry.stateDirFor(state.active), join(root, 'profiles', state.active))
    assert.equal(registry.active().id, state.active)
    // Calling ensure again is idempotent and does not mint a second profile.
    const again = registry.ensure()
    assert.equal(again.profiles.length, 1)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('staging enablement is explicit, idempotent and never activates', () => {
  const root = tempRoot()
  try {
    const registry = new HostProfileRegistry({ stateRoot: root })
    const before = registry.ensure()
    const staging = registry.enableStaging()
    assert.equal(staging.environment, 'staging')
    assert.notEqual(staging.id, before.active)
    const after = registry.read()
    assert.equal(after.active, before.active)
    assert.equal(after.developerToolsEnabled, true)
    assert.equal(after.profiles.length, 2)
    const repeated = registry.enableStaging()
    assert.equal(repeated.id, staging.id)
    assert.equal(registry.read().profiles.length, 2)
    assert.equal(registry.markDeveloperTools(false).developerToolsEnabled, false)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('profile labels are labels, never storage keys, and duplicate labels conflict', () => {
  const root = tempRoot()
  try {
    const registry = new HostProfileRegistry({ stateRoot: root })
    registry.ensure()
    const custom = registry.addProfile({
      label: 'Lab',
      environment: 'custom',
      service: { id: 'lab-service', environment: 'custom', label: 'Lab', apiOrigin: 'https://lab.example' },
    })
    assert.match(custom.id, /^[a-f0-9]{32}$/)
    assert.notEqual(custom.stateDir, custom.label)
    assert.equal(custom.stateDir, `profiles/${custom.id}`)
    assert.throws(() => registry.addProfile({
      label: 'lab',
      environment: 'custom',
      service: { id: 'other', environment: 'custom', label: 'lab', apiOrigin: 'https://other.example' },
    }), (error) => error.code === 'profile_conflict')
    // A compatible configured service may report staging while the local
    // profile stays a custom managed profile.
    const stagingLab = registry.addProfile({
      label: 'Staging Lab',
      environment: 'custom',
      service: { id: 'lab-staging', environment: 'staging', label: 'Staging Lab', apiOrigin: 'https://lab-staging.example' },
    })
    assert.equal(stagingLab.environment, 'custom')
    assert.throws(() => registry.addProfile({ label: '', environment: 'custom' }), (error) => error.code === 'profile_conflict')
    assert.throws(() => registry.addProfile({ label: 'X', environment: 'custom', mode: 'standalone' }), (error) => error.code === 'profile_conflict')
    assert.throws(() => registry.addProfile({
      label: 'Bad',
      environment: 'custom',
      service: { id: 'x', environment: 'custom', label: 'Bad', apiOrigin: 'https://user@bad.example' },
    }), (error) => error.code === 'registry_corrupt')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('bindings and pending flags are validated and revision guarded', () => {
  const root = tempRoot()
  try {
    const registry = new HostProfileRegistry({ stateRoot: root })
    const state = registry.ensure()
    const revision = state.revision
    const updated = registry.setBindings(state.active, { hostId: 'host-1234', region: 'r1' })
    assert.equal(updated.bindings.hostId, 'host-1234')
    assert.throws(() => registry.setBindings(state.active, { hostId: 'host 2' }), (error) => error.code === 'registry_corrupt')
    assert.throws(() => registry.setBindings(state.active, { password: 'nope' }), (error) => error.code === 'registry_corrupt')
    assert.throws(() => registry.setBindings(state.active, { accessOrigin: 'http://evil.example' }), (error) => error.code === 'registry_corrupt')
    assert.throws(() => registry.setBindings('missing', { hostId: 'x' }), (error) => error.code === 'profile_not_found')
    assert.throws(() => registry.setBindings(state.active, { hostId: 'x' }, { expectedRevision: revision }), (error) => error.code === 'registry_write_failed')
    const pending = registry.setPending(state.active, { paused: true, phase: 'approval' })
    assert.equal(pending.pending.paused, true)
    assert.equal(pending.pending.phase, 'approval')
    const cleared = registry.clearPendingPhase(state.active)
    assert.equal(cleared.pending.phase, undefined)
    assert.equal(cleared.pending.paused, true)
    assert.throws(() => registry.setPending(state.active, { phase: 'nope' }), (error) => error.code === 'registry_corrupt')
    assert.throws(() => registry.setPending('missing', { paused: true }), (error) => error.code === 'profile_not_found')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('activation is prepared then committed atomically and a prepared marker never selects', () => {
  const root = tempRoot()
  try {
    const registry = new HostProfileRegistry({ stateRoot: root })
    const state = registry.ensure()
    const staging = registry.enableStaging()
    const revision = registry.read().revision
    const prepared = registry.prepareActivation(staging.id, revision)
    assert.equal(prepared.profileId, staging.id)
    let current = registry.read()
    assert.equal(current.active, state.active)
    assert.notEqual(current.prepared, null)
    assert.equal(registry.recoverPrepared(), null)
    assert.equal(registry.read().prepared, null)
    // Prepare again and commit.
    const revision2 = registry.read().revision
    registry.prepareActivation(staging.id, revision2)
    const preparedRevision = registry.read().revision
    registry.commitActivation(staging.id, preparedRevision)
    current = registry.read()
    assert.equal(current.active, staging.id)
    assert.equal(current.prepared, null)
    assert.equal(current.activationGeneration, state.activationGeneration + 1)
    assert.throws(() => registry.prepareActivation(staging.id, current.revision), (error) => error.code === 'profile_conflict')
    assert.throws(() => registry.prepareActivation('missing', current.revision), (error) => error.code === 'profile_not_found')
    assert.throws(() => registry.prepareActivation(state.active, current.revision - 1), (error) => error.code === 'registry_write_failed')
    assert.throws(() => registry.commitActivation(state.active, current.revision), (error) => error.code === 'registry_write_failed')
    assert.throws(() => registry.abortActivation(current.revision - 1), (error) => error.code === 'registry_write_failed')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('local deletion refuses the active profile and never claims remote revocation', () => {
  const root = tempRoot()
  try {
    const registry = new HostProfileRegistry({ stateRoot: root })
    const state = registry.ensure()
    const staging = registry.enableStaging()
    assert.throws(() => registry.removeProfile(state.active), (error) => error.code === 'profile_conflict')
    const removed = registry.removeProfile(staging.id)
    assert.equal(removed.remoteRevocation, 'not_attempted')
    assert.equal(registry.get(staging.id), null)
    assert.throws(() => registry.removeProfile('missing'), (error) => error.code === 'profile_not_found')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('legacy manual state is adopted in place with a reserved standalone profile', () => {
  const root = tempRoot()
  try {
    writeFileSync(join(root, 'state.sqlite'), 'legacy')
    const registry = new HostProfileRegistry({ stateRoot: root })
    const state = registry.ensure()
    assert.equal(state.active, LEGACY_PROFILE_ID)
    assert.equal(state.profiles.length, 1)
    assert.equal(state.profiles[0].legacy, true)
    assert.equal(state.profiles[0].mode, 'standalone')
    assert.equal(state.profiles[0].stateDir, '.')
    assert.equal(state.profiles[0].service, null)
    assert.equal(hasLegacyMarkers(root), true)
    // Repeated adoption does not duplicate or move anything.
    const again = new HostProfileRegistry({ stateRoot: root }).ensure()
    assert.equal(again.profiles.length, 1)
    assert.equal(readFileSync(join(root, 'state.sqlite'), 'utf8'), 'legacy')
    assert.throws(() => registry.removeProfile(LEGACY_PROFILE_ID), (error) => error.code === 'profile_conflict')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('legacy adoption interrupted before the registry write repeats without changing identity', () => {
  const root = tempRoot()
  try {
    writeFileSync(join(root, 'state.sqlite'), 'legacy')
    let calls = 0
    const failing = new HostProfileRegistry({
      stateRoot: root,
      atomicWrite: () => {
        calls += 1
        throw new RegistryError('registry_write_failed', 'disk full')
      },
    })
    assert.throws(() => failing.ensure(), (error) => error.code === 'registry_write_failed')
    assert.equal(calls, 1)
    assert.equal(hasLegacyMarkers(root), true)
    const registry = new HostProfileRegistry({ stateRoot: root })
    const state = registry.ensure()
    assert.equal(state.active, LEGACY_PROFILE_ID)
    assert.equal(state.profiles[0].id, LEGACY_PROFILE_ID)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('ambiguous partial state and unsafe state roots fail closed', () => {
  const root = tempRoot()
  try {
    mkdirSync(join(root, 'profiles'))
    assert.throws(() => new HostProfileRegistry({ stateRoot: root }).ensure(), (error) => error.code === 'registry_corrupt')
    assert.throws(() => new HostProfileRegistry({ stateRoot: 'relative/root' }), (error) => error.code === 'registry_unsafe_state')
    assert.throws(() => new HostProfileRegistry({ stateRoot: '/' }), (error) => error.code === 'registry_unsafe_state')
    assert.throws(() => new HostProfileRegistry({}), (error) => error.code === 'registry_unsafe_state')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('corrupt and future registries are rejected without creating profiles', () => {
  const root = tempRoot()
  try {
    const registry = new HostProfileRegistry({ stateRoot: root })
    const state = registry.ensure()
    const file = join(root, 'host-profiles.json')
    const original = readFileSync(file, 'utf8')
    const mutations = [
      { ...state, schema: 'whalerein-host/v2/profiles' },
      { ...state, schemaVersion: 2 },
      { ...state, revision: 0 },
      { ...state, activationGeneration: -1 },
      { ...state, active: 'missing' },
      { ...state, profiles: [] },
      { ...state, developerToolsEnabled: 'yes' },
      { ...state, extra: true },
      { ...state, profiles: [{ ...state.profiles[0], extra: true }] },
      { ...state, profiles: [{ ...state.profiles[0], id: 'not-opaque' }] },
      { ...state, profiles: [{ ...state.profiles[0], stateDir: '../escape' }] },
      { ...state, profiles: [{ ...state.profiles[0], service: { ...state.profiles[0].service, apiOrigin: 'http://evil' } }] },
      { ...state, profiles: [state.profiles[0], { ...state.profiles[0] }] },
    ]
    for (const mutation of mutations) {
      writeFileSync(file, JSON.stringify(mutation))
      assert.throws(() => new HostProfileRegistry({ stateRoot: root }).read(), (error) => error instanceof RegistryError)
    }
    writeFileSync(file, '{not json')
    assert.throws(() => new HostProfileRegistry({ stateRoot: root }).read(), (error) => error.code === 'registry_corrupt')
    writeFileSync(file, original)
    assert.equal(new HostProfileRegistry({ stateRoot: root }).read().profiles.length, 1)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('missing, moved and symlinked managed state is rejected', () => {
  const root = tempRoot()
  try {
    const registry = new HostProfileRegistry({ stateRoot: root })
    const state = registry.ensure()
    const stateDir = join(root, 'profiles', state.active)
    rmSync(stateDir, { recursive: true, force: true })
    assert.throws(() => new HostProfileRegistry({ stateRoot: root }).read(), (error) => error.code === 'registry_unsafe_state')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
  const symlinkRoot = tempRoot()
  try {
    const registry = new HostProfileRegistry({ stateRoot: symlinkRoot })
    const state = registry.ensure()
    const stateDir = join(symlinkRoot, 'profiles', state.active)
    const target = join(symlinkRoot, 'elsewhere')
    mkdirSync(target)
    rmSync(stateDir, { recursive: true, force: true })
    symlinkSync(target, stateDir)
    assert.throws(() => new HostProfileRegistry({ stateRoot: symlinkRoot }).read(), (error) => error.code === 'registry_unsafe_state')
  } finally {
    rmSync(symlinkRoot, { recursive: true, force: true })
  }
})

test('registry write failures preserve the previous committed file', () => {
  const root = tempRoot()
  try {
    const registry = new HostProfileRegistry({ stateRoot: root })
    registry.ensure()
    const before = readFileSync(join(root, 'host-profiles.json'), 'utf8')
    const failing = new HostProfileRegistry({
      stateRoot: root,
      atomicWrite: () => {
        throw new RegistryError('registry_write_failed', 'disk full')
      },
    })
    assert.throws(() => failing.enableStaging(), (error) => error.code === 'registry_write_failed')
    assert.equal(readFileSync(join(root, 'host-profiles.json'), 'utf8'), before)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('atomic json writer fsyncs and replaces a file', () => {
  const root = tempRoot()
  try {
    atomicWriteJson(join(root, 'nested', 'value.json'), { ok: true })
    assert.deepEqual(JSON.parse(readFileSync(join(root, 'nested', 'value.json'), 'utf8')), { ok: true })
    assert.throws(() => atomicWriteJson(join(root, 'x.json'), { ok: true }, { fs: { openSync: () => { throw new Error('no') } } }), (error) => error.code === 'registry_write_failed')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('sync and async registry errors expose stable typed codes', () => {
  const error = new RegistryError('registry_future_schema', 'future')
  assert.equal(error.code, 'registry_future_schema')
  assert.equal(error.detail, 'future')
  assert.ok(new PluginError('invalid_argument') instanceof Error)
})

test('registry validates every documented record and binding failure mode', () => {
  const root = tempRoot()
  try {
    const registry = new HostProfileRegistry({ stateRoot: root })
    const state = registry.ensure()
    assert.equal(registry.filePath, join(root, 'host-profiles.json'))
    const base = { ...state, profiles: [{ ...state.profiles[0] }], prepared: null }
    const record = () => ({ ...state.profiles[0], bindings: {}, pending: {} })
    const cases = [
      { ...base, profiles: [{ ...record(), environment: 'Invalid Env' }] },
      { ...base, profiles: [{ ...record(), mode: 'invalid' }] },
      { ...base, profiles: [{ ...record(), legacy: 'no' }] },
      { ...base, profiles: [{ ...record(), createdAt: -1 }] },
      { ...base, profiles: [{ ...record(), legacy: true }] },
      { ...base, profiles: [{ ...record(), legacy: true, id: 'standalone', environment: 'standalone', mode: 'standalone', service: null, stateDir: 'elsewhere' }] },
      { ...base, profiles: [{ ...record(), mode: 'standalone' }] },
      { ...base, profiles: [{ ...record(), environment: 'standalone' }] },
      { ...base, profiles: [{ ...record(), service: { ...record().service, environment: 'staging' } }] },
      { ...base, profiles: [{ ...record(), service: { ...record().service, environment: 'standalone' } }] },
      { ...base, profiles: [{ ...record(), service: { ...record().service, apiOrigin: 'http://insecure.example' } }] },
      { ...base, profiles: [{ ...record(), service: { ...record().service, id: 'official-staging' } }] },
      { ...base, profiles: [{ ...record(), labels: undefined, label: 'x'.repeat(65) }] },
      { ...base, profiles: [{ ...record(), bindings: { signingPublicKey: 'short' } }] },
      { ...base, profiles: [{ ...record(), bindings: { noisePublicKey: 'short' } }] },
      { ...base, profiles: [{ ...record(), bindings: { accessOrigin: 'not a url' } }] },
      { ...base, profiles: [{ ...record(), bindings: { hostId: 7 } }] },
      { ...base, profiles: [{ ...record(), pending: { paused: 'yes' } }] },
      { ...base, profiles: [{ ...record(), pending: { intentId: 'bad id' } }] },
      { ...base, profiles: [{ ...record(), pending: { phase: 'unknown' } }] },
      { ...base, profiles: [{ ...record(), pending: { removal: 'unknown' } }] },
    ]
    const file = join(root, 'host-profiles.json')
    for (const mutation of cases) {
      writeFileSync(file, JSON.stringify(mutation))
      assert.throws(() => new HostProfileRegistry({ stateRoot: root }).read(), (error) => error instanceof RegistryError)
    }
    // Prepared marker validation.
    const prepared = {
      ...base,
      profiles: [state.profiles[0], {
        ...record(),
        id: 'b'.repeat(32),
        label: 'Second',
        stateDir: `profiles/${'b'.repeat(32)}`,
        createdAt: 1,
      }],
    }
    mkdirSync(join(root, 'profiles', 'b'.repeat(32)))
    for (const marker of [
      { profileId: 'missing', expectedRevision: 1, generation: 2 },
      { profileId: 'b'.repeat(32), expectedRevision: 'x', generation: 2 },
      { profileId: state.active, expectedRevision: 1, generation: 2 },
      { profileId: 'b'.repeat(32), expectedRevision: 1, generation: 2, extra: true },
    ]) {
      writeFileSync(file, JSON.stringify({ ...prepared, prepared: marker }))
      assert.throws(() => new HostProfileRegistry({ stateRoot: root }).read(), (error) => error.code === 'registry_corrupt')
    }
    // A directory at the registry path is not readable.
    writeFileSync(file, JSON.stringify(base))
    rmSync(file)
    mkdirSync(file)
    assert.throws(() => new HostProfileRegistry({ stateRoot: root }).read(), (error) => error.code === 'registry_corrupt')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('a busy registry lock fails closed with an actionable code', () => {
  const root = tempRoot()
  try {
    const registry = new HostProfileRegistry({ stateRoot: root })
    registry.ensure()
    const { DatabaseSync } = process.getBuiltinModule('node:sqlite')
    const database = new DatabaseSync(join(root, 'profiles.lock.sqlite'))
    database.exec('PRAGMA busy_timeout=0; BEGIN EXCLUSIVE;')
    try {
      const blocked = new HostProfileRegistry({ stateRoot: root, lockTimeoutMs: 1 })
      assert.throws(() => blocked.enableStaging(), (error) => error.code === 'registry_lock_failed')
    } finally {
      database.exec('ROLLBACK')
      database.close()
    }
    assert.equal(new HostProfileRegistry({ stateRoot: root }).enableStaging().environment, 'staging')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('official services are derived from the canonical catalog with no duplicate endpoint table', () => {
  const catalogPath = new URL('../src/configuration/catalog.json', import.meta.url)
  const catalog = JSON.parse(readFileSync(catalogPath, 'utf8'))
  for (const [name, entry] of Object.entries(catalog.environments)) {
    assert.equal(OFFICIAL_SERVICES[name].id, entry.id)
    assert.equal(OFFICIAL_SERVICES[name].environment, entry.environment)
    assert.equal(OFFICIAL_SERVICES[name].apiOrigin, entry.api_origin)
    assert.equal(OFFICIAL_SERVICES[name].issuer, entry.issuer)
    assert.equal(OFFICIAL_SERVICES[name].nativeClientId, entry.native_client_id)
  }
  const source = readFileSync(new URL('../src/services.js', import.meta.url), 'utf8')
  for (const endpoint of Object.values(catalog.environments).map((entry) => entry.api_origin)) {
    assert.equal(source.includes(endpoint), false, 'services.js must not duplicate catalog endpoints')
  }
  assert.equal(existsSync(new URL('../src/official-services.json', import.meta.url)), false)
  assert.equal(existsSync(new URL('../src/local-environment.js', import.meta.url)), false)
})
