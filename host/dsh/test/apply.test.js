import { HostRuntime } from 'whalerein-host'
import test from 'node:test'
import assert from 'node:assert/strict'
import { existsSync, mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import plugin, { name, inject, Config, apply, configFailure, createHost, createManagedHost } from '../index.js'
import { Context, Service } from '@deepseek-ai/cordis'
import { ConfigError } from '../src/config.js'
import { ActivationOwner } from 'whalerein-host/activation.js'

test('when a profile has a host ID, apply preserves it; otherwise it generates one', async () => {
  const dirs = [0, 1, 2].map((index) => mkdtempSync(join(tmpdir(), `whalerein-apply-host-${index}-`)))
  const disposers = []
  const committed = []
  const base = {
    service: 'standalone',
    adapter: 'fake',
    mqtt_url: 'mqtts://127.0.0.1:1',
    host_token: 'token',
    reconnect_min_ms: 50,
    reconnect_max_ms: 100,
  }
  const effect = (effectFn) => {
    disposers.push(effectFn())
    return () => {}
  }
  try {
    const saved = apply({
      fiber: { name: 'saved' },
      emit(event, fiber, config) {
        committed.push({ event, fiber, hostId: config.host_id })
      },
      effect,
    }, { ...base, host_id: 'host-real', state_dir: dirs[0] })
    assert.equal(saved.config.hostId, 'host-real')
    assert.deepEqual(committed, [])

    const generated = apply({
      fiber: { name: 'generated' },
      emit(event, fiber, config) {
        committed.push({ event, fiber, hostId: config.host_id })
      },
      effect,
    }, { ...base, state_dir: dirs[1] })
    assert.match(generated.config.hostId, /^[a-z0-9_.:-]+-[abcdefghjkmnpqrstuvwxyz23456789]{4}$/)
    assert.equal(committed.length, 1)
    assert.equal(committed[0].event, 'internal/commit')
    assert.equal(committed[0].hostId, generated.config.hostId)

    const started = apply({
      fiber: { name: 'uncommitted' },
      emit() {
        throw new Error('profile entry missing')
      },
      effect,
    }, { ...base, host_id: '   ', state_dir: dirs[2] })
    assert.match(started.config.hostId, /^[a-z0-9_.:-]+-[abcdefghjkmnpqrstuvwxyz23456789]{4}$/)
    assert.equal(committed.length, 1)
  } finally {
    for (const dispose of disposers.reverse()) await dispose()
    for (const dir of dirs) rmSync(dir, { recursive: true, force: true })
  }
})

test('when loaded, the plugin exports Cordis metadata and a default plugin object', () => {
  assert.equal(name, 'whalerein-dsh')
  assert.deepEqual(inject, ['sessions', 'sessionController', 'credentials'])
  assert.equal(typeof apply, 'function')
  assert.equal(typeof createHost, 'function')
  assert.equal(plugin.name, 'whalerein-dsh')
  assert.equal(plugin.Config, Config)
  assert.equal(plugin.apply, apply)
})

test('when plugin configuration is invalid, apply logs a safe configuration error without the token or raw URL and never starts a host', () => {
  const original = process.stderr.write.bind(process.stderr)
  let written = ''
  process.stderr.write = (chunk) => { written += String(chunk); return true }
  try {
    const result = apply({}, {
      service: 'standalone',
      mqtt_url: 'not-a-url-with-private-input',
      host_id: 'host-apply',
      host_token: 'test-token',
    })
    assert.equal(result, undefined)
    assert.match(written, /plugin_config_failed/)
    assert.match(written, /"reason":"mqtt_url must be a URL"/)
    assert.equal(written.includes('host_token'), false)
    assert.equal(written.includes('test-token'), false)
    assert.equal(written.includes('not-a-url-with-private-input'), false)
  } finally {
    process.stderr.write = original
  }
})

test('when apply runs, it starts the host in a Cordis effect and disposes it on unload', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-apply-'))
  try {
    const disposers = []
    const ctx = {
      effect(effect) {
        const dispose = effect()
        disposers.push(dispose)
        return () => {}
      },
    }
    const host = apply(ctx, {
      adapter: 'fake',
      service: 'standalone',
      mqtt_url: 'mqtts://127.0.0.1:1',
      host_id: 'host-apply',
      host_token: 'token',
      state_dir: dir,
      reconnect_min_ms: 50,
      reconnect_max_ms: 100,
    })
    assert.equal(host instanceof HostRuntime, true)
    await new Promise((resolve) => setTimeout(resolve, 100))
    assert.equal(disposers.length, 1)
    await disposers[0]()
    assert.equal(host.started, false)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('when createHost receives valid configuration, it resolves it; invalid input is rejected', () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-create-'))
  try {
    const host = createHost({
      adapter: 'fake',
      service: 'standalone',
      mqtt_url: 'mqtts://broker.test',
      host_id: 'host-1',
      host_token: 'token',
      state_dir: dir,
    })
    assert.equal(host.config.hostId, 'host-1')
    assert.equal(host.config.adapter, 'fake')
    assert.equal(host instanceof HostRuntime, true)
    assert.throws(() => createHost({ mqtt_url: 'mqtts://x' }), (error) => error.code === 'invalid_config')
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})


test('a stored-config failure names its field for the settings page without exposing values', () => {
  const failure = configFailure(new ConfigError('invalid_config', 'mqtt_url is required'))
  assert.equal(failure.code, 'invalid_config')
  assert.equal(failure.field, 'mqtt_url')
  assert.equal(failure.reason, 'mqtt_url is required')
  const boolean = configFailure(new ConfigError('invalid_config', 'boolean value expected'))
  assert.equal(boolean.field, '')
  assert.equal(boolean.reason, 'boolean value expected')
})

test('configuration helpers tolerate schemastery getters, plain errors and failing diagnostics', () => {
  const plain = configFailure(new Error('invalid_config: host_token is required'))
  assert.equal(plain.code, 'invalid_config')
  assert.equal(plain.field, '')
  assert.equal(plain.reason, 'configuration could not be resolved')

  const original = process.stderr.write.bind(process.stderr)
  process.stderr.write = () => { throw new Error('closed stderr') }
  try {
    assert.equal(apply({}, { service: 'standalone', mqtt_url: 'bad', host_id: 'host-apply', host_token: 't' }), undefined)
  } finally {
    process.stderr.write = original
  }
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-no-fiber-'))
  try {
    const generated = apply({ emit() {} }, { service: 'standalone', adapter: 'fake', mqtt_url: 'mqtts://127.0.0.1:1', host_token: 'token', state_dir: directory, host_id: '' })
    assert.match(generated.config.hostId, /^[a-z0-9_.:-]+-[abcdefghjkmnpqrstuvwxyz23456789]{4}$/)
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
})

test('documented values wrapped by schemastery are materialized through the same config path', async () => {
  const { materializeConfig } = await import('../index.js')
  const wrapped = Object.fromEntries([
    ['approval_mode', 'delegate'], ['event_batch_ms', '3'], ['shutdown_timeout_ms', '153'],
  ].map(([key, value]) => [key, { get: () => value }]))
  assert.deepEqual(materializeConfig(wrapped), {
    approval_mode: 'delegate', event_batch_ms: '3', shutdown_timeout_ms: '153',
  })
})


test('managed hosting constructs the connection manager with profile-scoped credentials and manager options', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-managed-'))
  try {
    const calls = []
    const manager = {
      snapshot: () => ({ state: 'idle', environment: 'production' }),
      async dispose() { calls.push(['dispose']) },
    }
    const credentials = { name: 'credential-adapter' }
    const host = await createManagedHost({
      state_root: dir,
    }, {
      ctx: { credentials: {} },
      credentials,
      managerFactory: async (options) => {
        calls.push(['create', options])
        return manager
      },
    })
    assert.equal(host.managed, true)
    assert.equal(host.config.connectionMode, 'managed')
    assert.equal(host.config.stateRoot, dir)
    assert.equal(host.manager, manager)
    assert.equal(host.credentials, credentials)
    assert.equal(calls[0][0], 'create')
    assert.equal(calls[0][1].stateRoot, dir)
    assert.equal(calls[0][1].credentials, credentials)
    assert.equal(calls[0][1].ctx.credentials.name, undefined)
    assert.equal(host.started, true)
    await host.stop()
    assert.deepEqual(calls.at(-1), ['dispose'])
    assert.equal(host.started, false)
    await host.stop()
    assert.equal(calls.filter(([name]) => name === 'dispose').length, 1)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('a managed manager contract failure is closed and reported without exposing internals', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-managed-bad-'))
  try {
    await assert.rejects(createManagedHost({ state_root: dir }, {
      ctx: {},
      managerFactory: async () => ({ snapshot: 'not-a-function' }),
    }), (error) => error.code === 'manager_unavailable')
    await assert.rejects(createManagedHost({ state_root: dir }, {
      ctx: {},
      managerFactory: async () => { throw Object.assign(new Error('private manager detail'), { code: 'manager_unavailable' }) },
    }), (error) => error.code === 'manager_unavailable' && !error.message.includes('private'))
    await assert.rejects(createManagedHost({ state_root: dir }, {
      ctx: {},
      managerFactory: async () => ({
        snapshot: () => ({ state: 'idle' }),
        ready: Promise.reject(new Error('private initialization detail')),
      }),
    }), (error) => error.code === 'manager_unavailable' && !error.message.includes('private'))
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('apply on a fresh managed profile registers settings and disposes the manager with the plugin effect', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-managed-apply-'))
  const root = new Context()
  let disposed = 0
  class TypertGateway extends Service {
    constructor(ctx) { super(ctx, 'typertGateway') }
  }
  try {
    await root.plugin({
      name: 'fixtures',
      apply(ctx) { new TypertGateway(ctx) },
    })
    const manager = {
      snapshot: () => ({ state: 'idle', environment: 'production', profileId: 'profile-1', connected: false }),
      async dispose() { disposed += 1 },
    }
    const host = await apply(root, {
      state_root: dir,
    }, { managerFactory: async () => manager })
    assert.equal(host.managed, true)
    assert.equal(existsSync(join(dir, 'host-id')), false, 'a managed install does not generate a visible Host ID')
    assert.equal(root.whalereinSettings.snapshot().managed, true)
    assert.equal(root.whalereinSettings.snapshot().state, 'idle')
    await root.fiber.dispose()
    assert.equal(disposed, 1)
  } finally {
    await root.fiber.dispose()
    rmSync(dir, { recursive: true, force: true })
  }
})

test('apply keeps the settings surface available when the runtime manager cannot be constructed', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-managed-missing-'))
  const root = new Context()
  const original = process.stderr.write.bind(process.stderr)
  let written = ''
  process.stderr.write = (chunk) => { written += String(chunk); return true }
  class TypertGateway extends Service {
    constructor(ctx) { super(ctx, 'typertGateway') }
  }
  try {
    await root.plugin({ name: 'fixtures', apply(ctx) { new TypertGateway(ctx) } })
    const result = await apply(root, { state_root: dir }, {
      managerFactory: async () => { throw Object.assign(new Error('private module path'), { code: 'manager_unavailable' }) },
    })
    assert.equal(result, undefined)
    assert.match(written, /manager_unavailable/)
    assert.equal(written.includes('private module path'), false)
    const snapshot = root.whalereinSettings.snapshot()
    assert.equal(snapshot.managed, true)
    assert.equal(snapshot.available, false)
    assert.deepEqual(snapshot.error, { code: 'manager_unavailable' })
    assert.equal(snapshot.configError.code, 'manager_unavailable')
  } finally {
    process.stderr.write = original
    await root.fiber.dispose()
    rmSync(dir, { recursive: true, force: true })
  }
})


test('a pinned issuer key becomes the manager authorization verifier', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-managed-verifier-'))
  try {
    const { generateKeyPairSync, sign } = await import('node:crypto')
    const pair = generateKeyPairSync('ed25519')
    const publicKey = Buffer.from(pair.publicKey.export({ type: 'spki', format: 'der' }).subarray(-32)).toString('base64url')
    const canonical = 'whalerein/v1/ext-auth-v1\n|auth-1'
    const signature = sign(null, Buffer.from(canonical), pair.privateKey).toString('base64url')
    let captured
    const host = await createManagedHost({
      state_root: dir,
      enrollment_issuer_public_key: publicKey,
      enrollment_issuer: 'https://issuer.example',
    }, {
      ctx: {},
      managerFactory: async (options) => {
        captured = options
        return { snapshot: () => ({ state: 'idle' }), async dispose() {} }
      },
    })
    assert.equal(typeof captured.verifyAuthorization, 'function')
    assert.equal(captured.expectedIssuer, 'https://issuer.example')
    assert.equal(captured.verifyAuthorization({ issuer: 'https://issuer.example', signature }, canonical), true)
    assert.equal(captured.verifyAuthorization({ issuer: 'https://issuer.example', signature }, `${canonical}x`), false)
    await host.stop()
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})


async function createGatewayRoot() {
  const root = new Context()
  class TypertGateway extends Service {
    constructor(ctx) { super(ctx, 'typertGateway') }
  }
  await root.plugin({ name: 'fixtures', apply(ctx) { new TypertGateway(ctx) } })
  return root
}

function standaloneConfig(stateDir) {
  return {
    adapter: 'fake',
    service: 'standalone',
    mqtt_url: 'mqtts://127.0.0.1:1',
    host_id: 'host-standalone',
    host_token: 'token',
    state_dir: stateDir,
    reconnect_min_ms: 50,
    reconnect_max_ms: 100,
  }
}

test('a competing activation owner is reported as settings state and never starts a second host', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-competing-'))
  const first = new ActivationOwner({ stateRoot: dir }).acquire()
  const root = await createGatewayRoot()
  const original = process.stderr.write.bind(process.stderr)
  let written = ''
  process.stderr.write = (chunk) => { written += String(chunk); return true }
  try {
    const host = apply(root, standaloneConfig(dir))
    assert.equal(host instanceof HostRuntime, true)
    assert.equal(host.started, false)
    assert.match(written, /"code":"conflict"/)
    assert.equal(root.whalereinSettings.snapshot().configError.code, 'conflict')
    assert.equal(written.includes('token'), false, 'activation conflict never logs credentials')
  } finally {
    process.stderr.write = original
    first.release()
    await root.fiber.dispose()
    rmSync(dir, { recursive: true, force: true })
  }
})

test('cold start acquires the activation owner inside the plugin effect and starts the host', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-cold-start-'))
  const root = await createGatewayRoot()
  try {
    const host = apply(root, standaloneConfig(dir))
    assert.equal(host.started, false)
    await new Promise((resolve) => setTimeout(resolve, 120))
    assert.equal(host.started, true)
    assert.throws(() => new ActivationOwner({ stateRoot: dir }).acquire(), (error) => error.code === 'conflict')
    await root.fiber.dispose()
    assert.equal(host.started, false)
    // Disposal released the lock.
    const next = new ActivationOwner({ stateRoot: dir }).acquire()
    next.release()
  } finally {
    await root.fiber.dispose()
    rmSync(dir, { recursive: true, force: true })
  }
})

test('plugin disposal releases the activation owner even when host stop throws', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-reload-race-'))
  const root = await createGatewayRoot()
  try {
    const host = apply(root, standaloneConfig(dir))
    await new Promise((resolve) => setTimeout(resolve, 80))
    host.stop = async () => { throw Object.assign(new Error('private teardown detail'), { code: 'internal' }) }
    await root.fiber.dispose()
    const next = new ActivationOwner({ stateRoot: dir })
    next.acquire()
    next.release()
  } finally {
    await root.fiber.dispose()
    rmSync(dir, { recursive: true, force: true })
  }
})

test('an invalid Standalone configuration never starts managed enrollment or another mode', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-invalid-standalone-'))
  const root = await createGatewayRoot()
  let managerCalls = 0
  const original = process.stderr.write.bind(process.stderr)
  let written = ''
  process.stderr.write = (chunk) => { written += String(chunk); return true }
  try {
    const result = apply(root, {
      service: 'standalone', adapter: 'fake', mqtt_url: 'not-a-url', host_id: 'host-invalid', host_token: 'private-token', state_dir: dir,
    }, { managerFactory: async () => { managerCalls += 1; throw new Error('should not be called') } })
    assert.equal(result, undefined)
    assert.equal(managerCalls, 0, 'Standalone configuration never falls back to managed enrollment')
    const snapshot = root.whalereinSettings.snapshot()
    assert.notEqual(snapshot.managed, true)
    assert.equal(snapshot.configError.code, 'invalid_config')
    assert.equal(written.includes('private-token'), false)
  } finally {
    process.stderr.write = original
    await root.fiber.dispose()
    rmSync(dir, { recursive: true, force: true })
  }
})

test('a failed managed startup never falls back to Standalone settings or transport', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-managed-no-fallback-'))
  const root = await createGatewayRoot()
  try {
    const result = await apply(root, { service: 'whalerein_cloud', state_root: dir }, {
      managerFactory: async () => { throw Object.assign(new Error('private manager path'), { code: 'manager_unavailable' }) },
    })
    assert.equal(result, undefined)
    const snapshot = root.whalereinSettings.snapshot()
    assert.equal(snapshot.managed, true)
    assert.equal(snapshot.service, 'whalerein_cloud')
    assert.equal(snapshot.available, false)
    assert.equal(snapshot.configError.code, 'manager_unavailable')
    assert.equal(existsSync(join(dir, 'host-id')), false, 'no Standalone identity is generated for a managed profile')
  } finally {
    await root.fiber.dispose()
    rmSync(dir, { recursive: true, force: true })
  }
})

test('a managed activation conflict surfaces on settings instead of throwing out of apply', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-managed-conflict-'))
  const root = await createGatewayRoot()
  try {
    const result = await apply(root, { service: 'whalerein_cloud', state_root: dir }, {
      managerFactory: async () => { throw Object.assign(new Error('private lock detail'), { code: 'conflict' }) },
    })
    assert.equal(result, undefined)
    const snapshot = root.whalereinSettings.snapshot()
    assert.equal(snapshot.configError.code, 'conflict')
    assert.deepEqual(snapshot.error, { code: 'conflict' })
    assert.equal(JSON.stringify(snapshot).includes('private lock detail'), false)
  } finally {
    await root.fiber.dispose()
    rmSync(dir, { recursive: true, force: true })
  }
})
