import assert from 'node:assert/strict'
import test from 'node:test'
import { Context } from '@deepseek-ai/cordis'
import { remoteMethods } from '@deepseek-ai/dsh-typert-protocol'
import { RemoteError } from '@deepseek-ai/dsh-typert-protocol'
import { startHarness } from './support/harness.js'
import { EMPTY_SETTINGS_STATUS, settingsStatus, WhaleReinSettingsService } from '../src/settings.js'

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
    { method: 'invite', invocation: { kind: 'direct' } },
    { method: 'revoke', invocation: { kind: 'direct' } },
    { method: 'testConnection', invocation: { kind: 'direct' } },
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
