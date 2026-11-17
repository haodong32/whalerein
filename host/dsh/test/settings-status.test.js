import assert from 'node:assert/strict'
import test from 'node:test'
import { Context } from '@deepseek-ai/cordis'
import { remoteMethods } from '@deepseek-ai/dsh-typert-protocol'
import { RemoteError } from '@deepseek-ai/dsh-typert-protocol'
import { startHarness } from './support/harness.js'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { EMPTY_SETTINGS_STATUS, sanitizeConfigurationView, settingsStatus, WhaleReinSettingsService } from '../src/settings.js'

test('when settings status is requested, it returns the host pairing code and paired device summaries', async () => {
  const harness = await startHarness()
  try {
    assert.deepEqual(settingsStatus(null), { ...EMPTY_SETTINGS_STATUS, devices: [] })
    const initial = settingsStatus(harness.host)
    assert.equal(initial.available, true)
    assert.equal(initial.connected, true)
    assert.equal(initial.hostId, 'host-1')
    assert.equal(initial.pairingCode, harness.host.security.hostFingerprintDisplay)
    assert.deepEqual(initial.devices, [])
    assert.equal(initial.diagnostics.identityReady, true)
    assert.equal(initial.diagnostics.dshAvailable, true)
    assert.equal(initial.diagnostics.pendingCiphertext, 0)

    await harness.newPairedDevice('device-1')
    const status = settingsStatus(harness.host)
    assert.equal(status.devices.length, 1)
    assert.equal(status.devices[0].deviceId, 'device-1')
    assert.equal(status.devices[0].displayName, 'test device')
    assert.equal(status.devices[0].os, 'macos')
    assert.equal(status.devices[0].connected, true)
    assert.equal(typeof status.devices[0].fingerprint, 'string')
    assert.equal(typeof status.devices[0].pairedAt, 'number')
    assert.equal('publicKey' in status.devices[0], false)
    const serialized = JSON.stringify(status)
    assert.equal(serialized.includes(harness.config.hostToken), false)
    assert.equal(serialized.includes(Buffer.from(harness.host.security.identity.privateKey).toString('base64')), false)
  } finally {
    await harness.stop()
  }
})

test('runtime diagnostics project only known fields and preserve unavailable values', () => {
  const host = {
    started: true, config: { hostId: 'host-1' },
    diagnostics: () => ({
      source: { available: true, internalDetail: 'unexposed' },
      access: { reconnects: 2, publishErrors: 3, extra: 'unexposed' },
      revocationsPending: true,
      pendingCiphertext: { count: 4 }, commands: { unknown: 5 },
      cursors: { snapshotRequired: 6 }, extra: 'unexposed',
    }),
  }
  assert.deepEqual(settingsStatus(host).diagnostics, {
    identityReady: false, dshAvailable: true, revocationsPending: true,
    reconnects: 2, publishErrors: 3, pendingCiphertext: 4,
    unknownCommands: 5, snapshotRequired: 6,
  })
  host.diagnostics = () => ({ access: { reconnects: -1, publishErrors: 'unexposed' } })
  assert.equal(settingsStatus(host).diagnostics.reconnects, null)
  assert.equal(settingsStatus(host).diagnostics.publishErrors, null)
  assert.equal(settingsStatus(host).diagnostics.dshAvailable, null)
  host.diagnostics = () => { throw new Error('unavailable') }
  assert.equal(settingsStatus(host).diagnostics, null)
  delete host.diagnostics
  assert.equal(settingsStatus(host).diagnostics, null)
})

test('when settings reads remote status, it returns a read-only snapshot compatible with the source interface', async () => {
  const ctx = new Context()
  const host = { started: false }
  const service = new WhaleReinSettingsService(ctx, host)
  WhaleReinSettingsService.initializeRemoteSnapshot.call(service)
  assert.equal(service.name, 'whalereinSettings')
  assert.equal(service.typertRemote.namespace, 'whalerein')
  assert.deepEqual(remoteMethods(service), [
    { method: 'snapshot', invocation: { kind: 'direct' } },
    { method: 'details', invocation: { kind: 'direct' } },
    { method: 'startConnect', invocation: { kind: 'direct' } },
    { method: 'cancel', invocation: { kind: 'direct' } },
    { method: 'retry', invocation: { kind: 'direct' } },
    { method: 'disconnect', invocation: { kind: 'direct' } },
    { method: 'reconnect', invocation: { kind: 'direct' } },
    { method: 'removeComputer', invocation: { kind: 'direct' } },
    { method: 'switchProfile', invocation: { kind: 'direct' } },
    { method: 'enableStaging', invocation: { kind: 'direct' } },
    { method: 'addCustomService', invocation: { kind: 'direct' } },
    { method: 'deleteProfile', invocation: { kind: 'direct' } },
    { method: 'invite', invocation: { kind: 'direct' } },
    { method: 'revoke', invocation: { kind: 'direct' } },
    { method: 'testConnection', invocation: { kind: 'direct' } },
    { method: 'configuration', invocation: { kind: 'direct' } },
    { method: 'saveDefaultEnvironment', invocation: { kind: 'direct' } },
    { method: 'specializeBuiltin', invocation: { kind: 'direct' } },
    { method: 'saveCustomEnvironment', invocation: { kind: 'direct' } },
    { method: 'removeCustomEnvironment', invocation: { kind: 'direct' } },
    { method: 'applyEnvironment', invocation: { kind: 'direct' } },
  ])
  assert.deepEqual(service.snapshot(), { ...EMPTY_SETTINGS_STATUS, devices: [] })
  assert.equal(ctx.whalereinSettings.name, 'whalereinSettings')
  assert.equal(ctx.whalereinSettings.snapshot().available, false)
  await ctx.fiber.dispose()
})

test('when settings creates an invitation, it returns the URI and fingerprint without exposing a separate token', async () => {
  const previous = globalThis.fetch
  const seen = []
  globalThis.fetch = async (url, init) => {
    seen.push({ url: String(url), init })
    return {
      ok: true,
      status: 200,
      json: async () => ({
        pairingUri: 'whalerein://pair?v=1&token=once',
        token: 'once',
        hostId: 'host-1',
        expiresAtUnixMs: 1700000000000,
      }),
    }
  }
  const ctx = new Context()
  try {
    const host = {
      started: true,
      config: { accessUrl: 'https://access.example', hostId: 'host-1' },
      now: () => 1700000000000,
      security: { identity: { privateKey: new Uint8Array(32).fill(4) }, hostFingerprintDisplay: 'abcd ef01' },
    }
    const service = new WhaleReinSettingsService(ctx, host)
    const result = await service.invite()
    assert.equal(result.pairingUri, 'whalerein://pair?v=1&token=once')
    assert.equal(result.expiresAtUnixMs, 1700000000000)
    assert.equal(result.fingerprint, 'abcd ef01')
    assert.equal('token' in result, false)
    assert.equal(seen[0].url, 'https://access.example/v1/pairing/invitations')
    assert.equal(seen[0].init.headers.authorization, undefined)
    assert.equal(JSON.parse(seen[0].init.body).hostId, 'host-1')
    assert.equal(JSON.parse(seen[0].init.body).signedAtUnixMs, 1700000000000)
    assert.equal(seen[0].init.redirect, 'error')
    assert.equal(JSON.stringify(result).includes('admin-secret'), false)
  } finally {
    globalThis.fetch = previous
    await ctx.fiber.dispose()
  }
})

test('after Access confirms the signed host-scoped revocation, settings removes the local peer and returns only its device ID', async () => {
  const previous = globalThis.fetch
  const seen = []
  let confirm
  const confirmation = new Promise(resolve => { confirm = resolve })
  globalThis.fetch = async (url, init) => {
    seen.push({ url: String(url), init })
    await confirmation
    return { ok: true, status: 204, json: async () => { throw new Error('no body') } }
  }
  const ctx = new Context()
  try {
    let forgotten = ''
    let generation = 1
    const service = new WhaleReinSettingsService(ctx, {
      config: { accessUrl: 'https://access.example', hostId: 'host-1' },
      now: () => 1700000000000,
      security: {
        identity: { privateKey: new Uint8Array(32).fill(4) },
        grantGeneration() {return generation},
        forgetPairedDevice(deviceId,expectedGeneration) {assert.equal(expectedGeneration,1);forgotten = deviceId},
      },
    })
    const pending = service.revoke('device-phone')
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(seen.length, 1)
    assert.equal(forgotten, '', 'Local authorization must remain until Access confirms revocation')
    generation = 2
    confirm()
    const result = await pending
    assert.deepEqual(result, { deviceId: 'device-phone' })
    assert.equal(forgotten, 'device-phone')
    assert.equal(seen[0].url, 'https://access.example/v1/hosts/host-1/devices/device-phone/revocation')
    assert.equal(seen[0].init.method, 'POST')
    assert.equal(seen[0].init.headers.authorization, undefined)
    assert.equal(seen[0].init.redirect, 'error')
    assert.equal(JSON.parse(seen[0].init.body).signedAtUnixMs, 1700000000000)
    assert.equal(JSON.stringify(result).includes('admin-secret'), false)
  } finally {
    confirm()
    globalThis.fetch = previous
    await ctx.fiber.dispose()
  }
})

test('when Access rejects revocation, settings maps the error without exposing the administrator token', async () => {
  const previous = globalThis.fetch
  const ctx = new Context()
  const host = {
    config: { accessUrl: 'https://access.example', hostId: 'host-1' },
    security: { identity: { privateKey: new Uint8Array(32).fill(4) } },
  }
  const service = new WhaleReinSettingsService(ctx, host)
  const cases = [
    { status: 401, code: 'whalerein/unauthorized' },
    { status: 403, code: 'whalerein/unauthorized' },
    { status: 410, code: 'whalerein/unauthorized' },
    { status: 404, code: 'whalerein/not-found' },
    { status: 500, code: 'whalerein/unavailable' },
    { network: true, code: 'whalerein/unavailable' },
  ]
  try {
    for (const item of cases) {
      globalThis.fetch = async () => {
        if (item.network) throw new Error('network')
        return { ok: false, status: item.status }
      }
      await assert.rejects(service.revoke('device-1'), (error) => error instanceof RemoteError && error.code === item.code && !String(error.message).includes('admin-secret'))
    }
    let called = false
    globalThis.fetch = async () => {
      called = true
      throw new Error('unexpected')
    }
    await assert.rejects(service.revoke('bad id'), (error) => error instanceof RemoteError && error.code === 'whalerein/not-found' && !String(error.message).includes('admin-secret'))
    host.security = {}
    await assert.rejects(service.revoke('device-1'), (error) => error instanceof RemoteError && error.code === 'whalerein/host-key-missing')
    host.security = { identity: { privateKey: new Uint8Array(32).fill(4) } }
    host.config = { hostId: 'host-1' }
    await assert.rejects(service.revoke('device-1'), (error) => error instanceof RemoteError && error.code === 'whalerein/server-missing')
    assert.equal(called, false)
  } finally {
    globalThis.fetch = previous
    await ctx.fiber.dispose()
  }
})

test('when the host key is missing, settings reports the error without calling Access', async () => {
  const previous = globalThis.fetch
  let called = false
  globalThis.fetch = async () => {
    called = true
    throw new Error('unexpected')
  }
  const ctx = new Context()
  try {
    const service = new WhaleReinSettingsService(ctx, {
      config: { accessUrl: 'https://access.example', hostId: 'host-1' },
      security: { hostFingerprintDisplay: 'abcd ef01' },
    })
    await assert.rejects(service.invite(), (error) => error instanceof RemoteError && error.code === 'whalerein/host-key-missing' && error.message === error.code)
    assert.equal(called, false)
  } finally {
    globalThis.fetch = previous
    await ctx.fiber.dispose()
  }
})

test('a host that failed to start reports the invalid configuration instead of a normal empty status', async () => {
  const { settingsStatus } = await import('../src/settings.js')
  const failure = settingsStatus({
    started: false,
    config: { hostId: 'host-configured' },
    configError: { code: 'invalid_config', field: 'mqtt_url', reason: 'mqtt_url is required' },
  })
  assert.equal(failure.available, false)
  assert.equal(failure.connected, false)
  assert.equal(failure.hostId, 'host-configured')
  assert.deepEqual(failure.devices, [])
  assert.deepEqual(failure.configError, { code: 'invalid_config', field: 'mqtt_url', reason: 'mqtt_url is required' })
  assert.equal(settingsStatus({ started: false }).configError, null)
})

import { sanitizeManagerSnapshot, sanitizeManagerDetails } from '../src/settings.js'

test('a managed manager snapshot is allowlisted and never exposes secrets, host ids or routes', () => {
  const raw = {
    state: 'connected',
    environment: 'staging',
    profileId: 'profile-opaque',
    label: 'WhaleRein Cloud Staging',
    hostName: 'Studio Mac',
    stagingEnabled: true,
    developerToolsEnabled: true,
    approvalUrl: 'https://app.example/approve/request-1',
    error: { code: 'temporarily_unavailable', message: 'private detail' },
    connected: true,
    pendingOperation: false,
    profiles: [
      { id: 'profile-opaque', label: 'Staging', environment: 'staging', active: true, capabilities: ['switching', 'evil'] },
      { id: 'bad id', label: 'drop' },
      null,
    ],
    capabilities: ['pairing', 'removal', 'evil'],
    hostId: 'host-private',
    route: { mqttUrl: 'mqtts://private.example:8883' },
    pollToken: 'poll-secret',
    authorization: { signature: 'auth-secret' },
    signingKey: 'key-secret',
  }
  const snapshot = sanitizeManagerSnapshot(raw, { developerTools: true })
  assert.equal(snapshot.managed, true)
  assert.equal(snapshot.state, 'connected')
  assert.equal(snapshot.environment, 'staging')
  assert.equal(snapshot.connected, true)
  assert.equal(snapshot.error.code, 'temporarily_unavailable')
  assert.equal(snapshot.approvalUrl, 'https://app.example/approve/request-1')
  assert.deepEqual(snapshot.profiles, [{
    id: 'profile-opaque', label: 'Staging', environment: 'staging', active: true, managed: true, capabilities: ['switching'],
  }])
  assert.deepEqual(snapshot.capabilities, ['pairing', 'removal'])
  const serialized = JSON.stringify(snapshot)
  for (const secret of ['host-private', 'private.example', 'poll-secret', 'auth-secret', 'key-secret', 'private detail', 'bad id']) {
    assert.equal(serialized.includes(secret), false, secret)
  }
  assert.equal('hostId' in snapshot, false)
  assert.equal('route' in snapshot, false)
  assert.equal('pollToken' in snapshot, false)
  assert.equal('authorization' in snapshot, false)
})

test('invalid or hostile manager values fail closed without leaking them', () => {
  const snapshot = sanitizeManagerSnapshot({
    state: 'unknown-state',
    environment: 'private',
    approvalUrl: 'javascript:alert(1)',
    error: { code: 'Private Error With Spaces' },
    profiles: 'not-an-array',
    capabilities: ['unknown'],
  })
  assert.equal(snapshot.state, 'action_required')
  assert.equal(snapshot.environment, 'production')
  assert.equal(snapshot.approvalUrl, '')
  assert.deepEqual(snapshot.error, { code: 'internal' })
  assert.deepEqual(snapshot.profiles, [])
  assert.deepEqual(snapshot.capabilities, [])
  const details = sanitizeManagerDetails({
    state: 'connected', environment: 'production', accessOrigin: 'https://access.example',
    region: 'eu-west', revision: 4, lastErrorCode: 'bad code spaces',
    route: 'private', hostId: 'private-host', mqttUrl: 'mqtts://private',
  })
  assert.deepEqual(details, {
    state: 'connected', environment: 'production', label: null, hostName: null,
    accessOrigin: 'https://access.example/', region: 'eu-west', revision: 4,
    lastErrorCode: 'internal', protocolVersion: null,
  })
  assert.equal(sanitizeManagerDetails(null), null)
})

test('managed settings exposes sanitized manager details and refuses missing or failing managers', async () => {
  const ctx = new Context()
  const manager = {
    snapshot: () => ({ state: 'connected', environment: 'production', connected: true }),
    details: () => ({ state: 'connected', region: 'eu-west', route: 'private', hostId: 'secret' }),
  }
  const service = new WhaleReinSettingsService(ctx, { managed: true, manager })
  assert.equal(service.snapshot().managed, true)
  assert.equal(service.snapshot().connected, true)
  assert.deepEqual(service.details(), {
    state: 'connected', environment: null, label: null, hostName: null,
    accessOrigin: null, region: 'eu-west', revision: null, lastErrorCode: null, protocolVersion: null,
  })
  await ctx.fiber.dispose()
  const failingCtx = new Context()
  const failing = new WhaleReinSettingsService(failingCtx, { managed: true, manager: { snapshot: () => ({ state: 'idle' }), details: () => { throw new Error('private') } } })
  assert.throws(() => failing.details(), (error) => error instanceof RemoteError && error.code === 'whalerein/unavailable' && !error.message.includes('private'))
  await failingCtx.fiber.dispose()
  const missingCtx = new Context()
  const missing = new WhaleReinSettingsService(missingCtx, { managed: true, manager: null })
  assert.equal(missing.snapshot().available, false)
  assert.deepEqual(missing.snapshot().error, { code: 'unavailable' })
  await missingCtx.fiber.dispose()
})

test('every frozen manager method forwards once with validated arguments and returns a sanitized snapshot', async () => {
  const ctx = new Context()
  const calls = []
  const manager = {
    snapshot: () => ({ state: 'connected', environment: 'production', profileId: 'p1', connected: true }),
    startConnect: (request) => calls.push(['startConnect', request]),
    cancel: () => calls.push(['cancel']),
    retry: () => calls.push(['retry']),
    disconnect: () => calls.push(['disconnect']),
    reconnect: () => calls.push(['reconnect']),
    remove: () => calls.push(['remove']),
    switchProfile: (id) => calls.push(['switchProfile', id]),
    enableStaging: () => calls.push(['enableStaging']),
    addCustomService: (value) => calls.push(['addCustomService', value]),
    deleteProfile: (id) => calls.push(['deleteProfile', id]),
  }
  const service = new WhaleReinSettingsService(ctx, { managed: true, manager })
  for (const [method, args] of [
    ['startConnect', [{ source: 'browser' }]],
    ['cancel', []], ['retry', []], ['disconnect', []], ['reconnect', []], ['removeComputer', []],
    ['switchProfile', ['profile-1']], ['enableStaging', []],
    ['addCustomService', [{ label: 'Service', apiOrigin: 'https://service.example/path' }]],
    ['deleteProfile', ['profile-2']],
  ]) {
    const result = await service[method](...args)
    assert.equal(result.managed, true, method)
  }
  assert.deepEqual(calls, [
    ['startConnect', { source: 'browser' }], ['cancel'], ['retry'], ['disconnect'], ['reconnect'], ['remove'],
    ['switchProfile', 'profile-1'], ['enableStaging'],
    ['addCustomService', { label: 'Service', apiOrigin: 'https://service.example/path' }],
    ['deleteProfile', 'profile-2'],
  ])
  await assert.rejects(service.switchProfile('bad id'), (error) => error instanceof RemoteError && error.code === 'whalerein/invalid-input')
  await assert.rejects(service.deleteProfile(''), (error) => error instanceof RemoteError && error.code === 'whalerein/invalid-input')
  await assert.rejects(service.addCustomService(null), (error) => error instanceof RemoteError && error.code === 'whalerein/invalid-input')
  await assert.rejects(service.addCustomService({ label: 'x', apiOrigin: 'file:///private' }), (error) => error instanceof RemoteError && error.code === 'whalerein/invalid-input')
  await ctx.fiber.dispose()
})

test('manager failures become stable action errors without exposing private diagnostics', async () => {
  const ctx = new Context()
  const service = new WhaleReinSettingsService(ctx, {
    managed: true,
    manager: { snapshot: () => ({ state: 'requesting' }), cancel: async () => { throw new Error('poll-token-secret and private path') } },
  })
  await assert.rejects(service.cancel(), (error) => error instanceof RemoteError
    && error.code === 'whalerein/action-failed'
    && !error.message.includes('poll-token-secret')
    && !error.message.includes('private path'))
  await ctx.fiber.dispose()
  const standaloneCtx = new Context()
  const standalone = new WhaleReinSettingsService(standaloneCtx, { started: false })
  await assert.rejects(standalone.startConnect({ source: 'settings' }),
    (error) => error instanceof RemoteError && error.code === 'whalerein/unsupported_capability')
  await standaloneCtx.fiber.dispose()
})

test('managed pairing calls are refused with a clear capability error while standalone keeps working', async () => {
  const ctx = new Context()
  const service = new WhaleReinSettingsService(ctx, {
    managed: true,
    manager: { snapshot: () => ({ state: 'connected', capabilities: [] }) },
  })
  await assert.rejects(service.invite(), (error) => error instanceof RemoteError
    && error.code === 'whalerein/unsupported_capability'
    && /pairing/.test(error.message))
  await assert.rejects(service.revoke('device-1'), (error) => error instanceof RemoteError
    && error.code === 'whalerein/unsupported_capability')
  await ctx.fiber.dispose()
})

test('manager action results expose only opaque profile metadata and a validated browser URL', async () => {
  const ctx = new Context()
  const service = new WhaleReinSettingsService(ctx, {
    managed: true,
    manager: {
      snapshot: () => ({ state: 'connected', environment: 'production' }),
      remove: async () => ({ state: 'removing', url: 'https://app.example/hosts/host-1', hostId: 'host-1', token: 'secret' }),
      enableStaging: async () => ({ id: 'staging-id', label: 'Staging', environment: 'staging', developerToolsEnabled: true }),
      addCustomService: async () => ({ id: 'custom-id', label: 'Custom', environment: 'custom', active: false }),
    },
  })
  const removal = await service.removeComputer()
  assert.deepEqual(removal.action, {
    id: '', label: '', environment: '', active: false, deleted: '', remoteRegistration: '', url: 'https://app.example/hosts/host-1',
  })
  const staging = await service.enableStaging()
  assert.equal(staging.action.id, 'staging-id')
  assert.equal(staging.action.environment, 'staging')
  assert.equal(staging.action.active, false)
  assert.equal(JSON.stringify(removal).includes('secret-token'), false)
  assert.equal(JSON.stringify(removal).includes('"hostId"'), false)
  await ctx.fiber.dispose()
})

test('an unsafe removal URL never crosses the settings RPC boundary', async () => {
  const ctx = new Context()
  const service = new WhaleReinSettingsService(ctx, {
    managed: true,
    manager: {
      snapshot: () => ({ state: 'connected' }),
      remove: async () => ({ state: 'removing', url: 'javascript:alert(1)' }),
    },
  })
  const result = await service.removeComputer()
  assert.equal(result.action, null)
  assert.equal(JSON.stringify(result).includes('javascript:'), false)
  await ctx.fiber.dispose()
})

test('settings projects configuration provenance and delegates saves to the Host module', async () => {
  const patches = []
  const configView = {
    path: '/tmp/whalerein/config.toml',
    source_kind: 'explicit',
    revision: `sha256:${'a'.repeat(64)}`,
    selected_environment: 'staging',
    selection_source: 'file',
    definition_source: 'builtin',
    desired_default: 'staging',
    effective_override: null,
    definition: { id: 'official-staging', mode: 'account', label: 'WhaleRein Cloud Staging (testing only)', api_origin: 'https://app.staging.whalerein.com' },
    field_provenance: { id: 'catalog', mode: 'catalog', label: 'catalog', tls: 'catalog', issuer: 'catalog' },
    validation: { scope: 'offline', live_readiness: 'not_checked' },
    errors: [],
  }
  const manager = {
    snapshot: () => ({
      state: 'connected', environment: 'production', desiredEnvironment: 'staging',
      profileId: 'abc', label: 'WhaleRein Cloud', hostName: 'host', stagingEnabled: true,
      developerToolsEnabled: true, approvalUrl: null, error: null, connected: true,
      pendingOperation: false, profiles: [], capabilities: [],
    }),
    configurationView: () => configView,
    updateConfiguration: async (patch) => { patches.push(patch); return configView },
    applyEnvironment: async (environment) => ({ ...manager.snapshot(), environment, profileId: 'def' }),
  }
  const ctx = new Context()
  try {
    const service = new WhaleReinSettingsService(ctx, { managed: true, manager })
    const status = service.snapshot()
    assert.equal(status.configuration.path, '/tmp/whalerein/config.toml')
    assert.equal(status.configuration.selectedEnvironment, 'staging')
    assert.equal(status.configuration.sourceKind, 'explicit')
    assert.equal(status.configuration.definitionSource, 'builtin')
    assert.equal(status.configuration.desiredDefault, 'staging')
    assert.equal(status.configuration.definition.label, 'WhaleRein Cloud Staging (testing only)')
    assert.equal(status.configuration.definition.api_origin, undefined)
    assert.equal(status.configuration.provenance.id, 'catalog')
    assert.equal(status.environment, 'production')
    assert.equal(status.desiredEnvironment, undefined)
    assert.equal(JSON.stringify(status).includes('token'), false)

    const saved = await service.saveDefaultEnvironment({ environment: 'staging' })
    assert.deepEqual(patches.at(-1), { type: 'SetDefaultEnvironment', environment: 'staging' })
    assert.equal(saved.configuration.desiredDefault, 'staging')
    await service.specializeBuiltin({ environment: 'local', label: 'Lab', tls: { ca_provider: 'file', ca_file: '/opt/ca.pem' } })
    assert.deepEqual(patches.at(-1), { type: 'SpecializeBuiltin', environment: 'local', label: 'Lab', tls: { ca_provider: 'file', ca_file: '/opt/ca.pem' } })
    await service.saveCustomEnvironment({ name: 'company', definition: { mode: 'account', label: 'Company', api_origin: 'https://cloud.example.com', issuer: 'https://auth.example.com/oidc', native_client_id: 'whalerein-cli' } })
    assert.equal(patches.at(-1).type, 'UpsertCustomEnvironment')
    assert.equal(patches.at(-1).name, 'company')
    await service.removeCustomEnvironment({ name: 'company' })
    assert.deepEqual(patches.at(-1), { type: 'RemoveCustomEnvironment', environment: 'company' })
    const applied = await service.applyEnvironment({ environment: 'staging' })
    assert.equal(applied.action.environment, 'staging')

    await assert.rejects(service.saveDefaultEnvironment({ environment: 'Bad Name' }), (error) => error.code === 'whalerein/invalid-input')
    await assert.rejects(service.specializeBuiltin({ environment: 'company' }), (error) => error.code === 'whalerein/invalid-input')
    await assert.rejects(service.saveCustomEnvironment({ name: 'lab', definition: { mode: 'account' } }), (error) => error.code === 'whalerein/invalid-input')
    await assert.rejects(service.applyEnvironment({ environment: '../x' }), (error) => error.code === 'whalerein/invalid-input')
    await assert.rejects(service.saveCustomEnvironment({ name: 'lab', definition: { mode: 'standalone', tls: { ca_provider: 'file' } } }), (error) => error.code === 'whalerein/invalid-input')
  } finally {
    await ctx.fiber.dispose()
  }
})

test('settings configuration projection falls back to the plugin reference and stays redacted', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-settings-config-'))
  try {
    const configFile = join(directory, 'config.toml')
    writeFileSync(configFile, 'version = 1\nenvironment = "production"\n')
    const host = { managed: true, manager: null, configPath: configFile, environmentOverride: null }
    const status = settingsStatus(host)
    assert.equal(status.configuration.selectedEnvironment, 'production')
    assert.equal(status.configuration.definitionSource, 'builtin')
    const missing = settingsStatus({ managed: true, manager: null, configPath: join(directory, 'missing.toml') })
    assert.equal(missing.configuration.selectedEnvironment, null)
    assert.equal(missing.configuration.errors[0].code, 'missing_config')
    assert.equal(settingsStatus({ managed: true, manager: null }).configuration, null)
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
})

test('configuration view projection drops unknown provenance and malformed values', () => {
  const view = sanitizeConfigurationView({
    path: 7,
    source_kind: 'remote',
    revision: 'not-a-revision',
    selected_environment: 'Bad Name',
    selection_source: 'invocation',
    definition_source: 'file',
    desired_default: 'Bad Name',
    effective_override: 'Bad Name',
    definition: { id: 7, mode: 'unknown', label: null },
    field_provenance: { id: 'file', tls: 'default', issuer: 'secret-source' },
    validation: { scope: 'live', live_readiness: 'ready' },
    errors: [{ code: 'BAD CODE', message: 'raw private detail' }, { code: 'unknown_field', message: 'x' }],
  })
  assert.equal(view.path, '')
  assert.equal(view.sourceKind, 'explicit')
  assert.equal(view.revision, null)
  assert.equal(view.selectedEnvironment, null)
  assert.equal(view.desiredDefault, null)
  assert.equal(view.effectiveOverride, null)
  assert.equal(view.selectionSource, 'invocation')
  assert.equal(view.definitionSource, 'file')
  assert.equal(view.definition.mode, null)
  assert.deepEqual(view.provenance, { id: 'file', tls: 'default' })
  assert.equal(view.validation.scope, 'live')
  assert.deepEqual(view.errors.map((error) => error.code), ['unreadable_config', 'unknown_field'])
  assert.equal(sanitizeConfigurationView(null), null)
})
