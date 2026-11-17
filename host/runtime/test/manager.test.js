import test from 'node:test'
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createConnectionManager } from '../src/manager.js'
import { MemoryHostCredentials } from '../src/security/credentials.js'
import { ProfileCredentialStore, CREDENTIAL_PURPOSES } from '../src/credentials.js'
import { HostProfileRegistry } from '../src/registry.js'
import { OFFICIAL_SERVICES } from '../src/services.js'

const NOW = 1_800_000_000_000
const HOST_NAME = 'test-mac'
const b64 = (value) => Buffer.from(value).toString('base64url')

function json(status, body, headers = {}) {
  return {
    status,
    redirected: false,
    headers: { get: (key) => headers[key.toLowerCase()] ?? null },
    async json() {
      return body
    },
  }
}

function metadataDocument(overrides = {}) {
  return {
    service: 'whalerein-cloud',
    environment: 'production',
    protocol: { version: 1, capabilities: ['host-enrollment-v1'] },
    approval: { origin: 'https://app.whalerein.com', path: '/approve/{id}' },
    enrollment: { createPath: '/api/enrollment-requests', pollAfterMs: 100 },
    ...overrides,
  }
}

async function waitFor(predicate, timeoutMs = 3000) {
  const deadline = Date.now() + timeoutMs
  for (;;) {
    const value = await predicate()
    if (value) return value
    if (Date.now() > deadline) throw new Error('condition timeout')
    await new Promise((resolve) => setTimeout(resolve, 5))
  }
}

function createFetch({ metadata = metadataDocument(), pollReply = 'approved', onCreate, onPoll, onEnroll, onMetadata } = {}) {
  const calls = []
  let intentBody = null
  const fetch = async (url, options = {}) => {
    const method = options.method ?? 'GET'
    calls.push({ url, method, options })
    if (url.endsWith('/api/host-service')) {
      if (onMetadata) return onMetadata(url, options)
      return json(200, metadata)
    }
    if (url.endsWith('/api/enrollment-requests') && method === 'POST') {
      intentBody = JSON.parse(options.body)
      if (onCreate) {
        const reply = await onCreate(intentBody)
        if (reply !== undefined) return reply
      }
      return json(201, {
        id: 'request-1',
        state: 'pending',
        approveUrl: `${metadata.approval.origin}/approve/request-1`,
        pollToken: 'poll-token-1',
        expiresAt: new Date(NOW + 300_000).toISOString(),
        pollAfterMs: 100,
        service: metadata.service,
        environment: metadata.environment,
        fingerprint: 'FP',
        displayFingerprint: 'FP-DISPLAY',
      })
    }
    if (url.includes('/api/enrollment-requests/')) {
      if (onPoll) {
        const reply = await onPoll(intentBody)
        if (reply !== undefined) return reply
      }
      if (pollReply === 'denied') return json(200, { id: 'request-1', state: 'denied' })
      return json(200, {
        id: 'request-1',
        state: 'approved',
        authorization: {
          authVersion: 'whalerein/v1/ext-auth-v1',
          authorizationId: 'auth-1',
          issuer: 'https://issuer.example',
          audience: 'https://access.example',
          purpose: 'host_enrollment',
          workspaceId: 'ws-1',
          regionId: 'r1',
          operationId: 'op-1',
          enrollmentRevision: '1',
          hostId: intentBody.hostId,
          hostPublicKeys: [intentBody.signingKey, intentBody.noiseKey],
          issuedAtUnixMs: NOW - 1000,
          expiresAtUnixMs: NOW + 300_000,
          nonce: b64(Buffer.alloc(16, 1)),
          keyId: 'key-1',
          signature: b64(Buffer.alloc(64, 1)),
        },
        route: { region: 'r1', accessOrigin: 'https://access.example', expiresAt: new Date(NOW + 300_000).toISOString() },
      })
    }
    if (url.endsWith('/credential')) {
      const authorization = JSON.parse(options.body).authorization
      return json(200, {
        state: 'enrollment_ready',
        revision: '2',
        credential: {
          hostId: authorization.hostId,
          mqttUrl: 'mqtts://broker.example:8883',
          username: `host:${authorization.hostId}`,
          password: 'recovered-password',
        },
      })
    }
    if (url.endsWith('/v1/enrollments/hosts')) {
      if (onEnroll) {
        const reply = await onEnroll(intentBody)
        if (reply !== undefined) return reply
      }
      return json(200, {
        status: {
          hostId: intentBody.hostId,
          enrollmentRevision: '1',
          state: 'enrollment_pending',
          hostSigningPublicKey: intentBody.signingKey,
          hostNoisePublicKey: intentBody.noiseKey,
        },
        credential: {
          hostId: intentBody.hostId,
          mqttUrl: 'mqtts://broker.example:8883',
          username: `host:${intentBody.hostId}`,
          password: 'broker-password',
        },
      })
    }
    if (url.includes('/v1/hosts/')) {
      return json(200, { hostId: intentBody?.hostId, state: 'enrollment_pending', enrollmentRevision: '1' })
    }
    throw new TypeError(`unexpected request ${method} ${url}`)
  }
  return { fetch, calls, intent: () => intentBody }
}

function createMqtt(mqtts) {
  return (options) => {
    const access = {
      options,
      connected: false,
      start() {
        access.started = true
        return access
      },
      async waitForConnected() {
        access.connected = true
      },
      async stop() {
        access.connected = false
      },
      describe() {
        return { connected: access.connected }
      },
    }
    mqtts.push(access)
    return access
  }
}

function createRuntimes({ failStartFor = null, failStop = false } = {}) {
  const runtimes = []
  const factory = ({ config, adapter, credentials, access }) => {
    const runtime = {
      config,
      adapter,
      credentials,
      access,
      started: false,
      stopped: 0,
      async start() {
        if (failStartFor && failStartFor(config)) throw new Error('runtime start failed')
        runtime.started = true
      },
      async stop() {
        runtime.stopped += 1
        if (failStop) throw new Error('runtime stop failed')
        runtime.started = false
      },
    }
    runtimes.push(runtime)
    return runtime
  }
  return { factory, runtimes }
}

function fixture(options = {}) {
  const stateRoot = options.stateRoot ?? mkdtempSync(join(tmpdir(), 'whalerein-manager-'))
  const credentials = options.credentials ?? new MemoryHostCredentials()
  const mqtts = []
  const runtimes = createRuntimes(options.runtimeOptions)
  const network = options.network ?? createFetch(options.fetchOptions)
  const manager = createConnectionManager({
    stateRoot,
    credentials,
    source: options.source === false ? null : {},
    legacy: options.legacy,
    now: () => NOW,
    hostName: HOST_NAME,
    fetch: options.fetch ?? network.fetch,
    mqtt: options.mqtt ?? createMqtt(mqtts),
    runtimeFactory: options.runtimeFactory ?? runtimes.factory,
    log: null,
    allowLoopback: true,
    installationId: 'test-install',
    standalone: options.standalone,
    removalStatus: options.removalStatus,
    locale: 'en',
    ...options.managerOptions,
  })
  return { manager, stateRoot, credentials, mqtts, runtimes: runtimes.runtimes, network, cleanup: () => rmSync(stateRoot, { recursive: true, force: true }) }
}

test('fresh default is official production, starts no network, and never reads CLI state', async () => {
  const cliRoot = mkdtempSync(join(tmpdir(), 'whalerein-cli-'))
  writeFileSync(join(cliRoot, 'profiles.json'), '{"cli":true}')
  const network = createFetch()
  let fetchCalls = 0
  const stateRoot = mkdtempSync(join(tmpdir(), 'whalerein-manager-'))
  try {
    const manager = createConnectionManager({
      stateRoot,
      credentials: new MemoryHostCredentials(),
      source: {},
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: (...args) => {
        fetchCalls += 1
        return network.fetch(...args)
      },
      mqtt: () => { throw new Error('must not connect') },
      runtimeFactory: () => { throw new Error('must not start') },
      installationId: 'test-install',
    })
    await manager.ready
    const snapshot = manager.snapshot()
    assert.equal(snapshot.state, 'idle')
    assert.equal(snapshot.environment, 'production')
    assert.match(snapshot.profileId, /^[a-f0-9]{32}$/)
    assert.equal(snapshot.label, 'WhaleRein Cloud')
    assert.equal(snapshot.connected, false)
    assert.equal(snapshot.pendingOperation, false)
    assert.equal(snapshot.developerToolsEnabled, false)
    assert.equal(snapshot.stagingEnabled, false)
    assert.equal(fetchCalls, 0)
    assert.deepEqual(JSON.parse(readFileSync(join(cliRoot, 'profiles.json'), 'utf8')), { cli: true })
    await manager.dispose()
  } finally {
    rmSync(cliRoot, { recursive: true, force: true })
    rmSync(stateRoot, { recursive: true, force: true })
  }
})

test('startConnect runs create, poll, proof-bound enrollment and real readiness before connected', async () => {
  const f = fixture()
  try {
    const first = await f.manager.startConnect()
    assert.equal(first.state, 'awaiting_approval')
    assert.equal(first.approvalUrl, 'https://app.whalerein.com/approve/request-1?locale=en')
    await waitFor(() => f.manager.snapshot().state === 'connected')
    const snapshot = f.manager.snapshot()
    assert.equal(snapshot.connected, true)
    assert.equal(snapshot.approvalUrl, null)
    assert.equal(snapshot.pendingOperation, false)
    const details = f.manager.details()
    assert.equal(details.accessOrigin, 'https://access.example')
    assert.equal(details.region, 'r1')
    assert.equal(details.revision, '1')
    assert.equal(details.environment, 'production')
    assert.equal(f.mqtts.length, 1)
    assert.equal(f.mqtts[0].options.token, 'broker-password')
    assert.equal(f.runtimes.length, 1)
    assert.equal(f.runtimes[0].started, true)
    // Secrets never appear in snapshots or details.
    const serialized = JSON.stringify({ snapshot, details })
    for (const secret of ['poll-token-1', 'broker-password', b64(Buffer.alloc(32, 0)), 'auth-1']) {
      assert.equal(serialized.includes(secret), false)
    }
    assert.equal(serialized.includes(f.network.intent().signingKey), false)
    assert.equal(details.hostId, undefined)
    // Duplicate start does nothing.
    const duplicate = await f.manager.startConnect()
    assert.equal(duplicate.state, 'connected')
    assert.equal(f.runtimes.length, 1)
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})

test('disconnect persists paused intent; restart does not auto-reconnect and reconnect reuses identity', async () => {
  const f = fixture()
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'connected')
    const hostId = f.network.intent().hostId
    const before = await f.manager.disconnect()
    assert.equal(before.state, 'disconnected')
    assert.equal(before.connected, false)
    assert.equal(f.runtimes[0].stopped > 0, true)
    await waitFor(async () => {
      const registry = new HostProfileRegistry({ stateRoot: f.stateRoot }).read()
      return registry.profiles.find((profile) => profile.id === registry.active)?.pending?.paused === true
    })
    const second = createConnectionManager({
      stateRoot: f.stateRoot,
      credentials: f.credentials,
      source: {},
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: f.network.fetch,
      mqtt: createMqtt([]),
      runtimeFactory: createRuntimes().factory,
      installationId: 'test-install',
    })
    await second.ready
    assert.equal(second.snapshot().state, 'disconnected')
    assert.equal(second.snapshot().connected, false)
    const reconnected = await second.reconnect()
    assert.equal(reconnected.state, 'connected')
    assert.equal(f.network.intent().hostId, hostId)
    await second.disconnect()
    const creates = f.network.calls.filter((call) => call.method === 'POST' && call.url.endsWith('/api/enrollment-requests')).length
    const resumed = await second.startConnect()
    assert.equal(resumed.state, 'connected')
    assert.equal(f.network.calls.filter((call) => call.method === 'POST' && call.url.endsWith('/api/enrollment-requests')).length, creates)
    await second.dispose()
  } finally {
    f.cleanup()
  }
})

test('staging enablement does not activate or autostart, persists, and failure does not fall back', async () => {
  const f = fixture()
  try {
    const activeBefore = f.manager.snapshot().profileId
    const staging = await f.manager.enableStaging()
    assert.equal(staging.environment, 'staging')
    assert.equal(f.manager.snapshot().profileId, activeBefore)
    assert.equal(f.manager.snapshot().stagingEnabled, true)
    assert.equal(f.manager.snapshot().developerToolsEnabled, true)
    assert.equal(f.runtimes.length, 0)
    const second = createConnectionManager({
      stateRoot: f.stateRoot,
      credentials: f.credentials,
      source: null,
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: f.network.fetch,
      mqtt: createMqtt([]),
      runtimeFactory: createRuntimes().factory,
      installationId: 'test-install',
    })
    assert.equal(second.snapshot().stagingEnabled, true)
    assert.equal(second.snapshot().developerToolsEnabled, true)
    await second.dispose()
    const switched = await f.manager.switchProfile(staging.id)
    assert.equal(switched.environment, 'staging')
    assert.equal(switched.state, 'idle')
    assert.equal(switched.label, 'WhaleRein Cloud Staging')
    assert.equal(f.runtimes.length, 0)
    // A staging service failure remains selected and labeled; no fallback.
    await assert.rejects(
      f.manager.startConnect(),
      (error) => error.code === 'forbidden',
    )
    const failed = f.manager.snapshot()
    assert.equal(failed.environment, 'staging')
    assert.equal(failed.state, 'action_required')
    assert.equal(failed.error.code, 'forbidden')
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})

test('switch stops the old runtime before publishing and a stop failure preserves selection', async () => {
  const f = fixture()
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'connected')
    const productionId = f.manager.snapshot().profileId
    const staging = await f.manager.enableStaging()
    const switched = await f.manager.switchProfile(staging.id)
    assert.equal(switched.profileId, staging.id)
    assert.equal(switched.environment, 'staging')
    assert.equal(f.runtimes[0].stopped > 0, true)

    // Switch back to a bound profile starts it again after commit.
    const back = await f.manager.switchProfile(productionId)
    assert.equal(back.profileId, productionId)
    await waitFor(() => f.manager.snapshot().state === 'connected')

    const failingStateRoot = mkdtempSync(join(tmpdir(), 'whalerein-manager-'))
    try {
      const runtimes = createRuntimes({ failStop: true })
      const manager = createConnectionManager({
        stateRoot: failingStateRoot,
        credentials: f.credentials,
        source: {},
        now: () => NOW,
        hostName: HOST_NAME,
        fetch: f.network.fetch,
        mqtt: createMqtt([]),
        runtimeFactory: runtimes.factory,
        installationId: 'test-install',
      })
      await manager.startConnect()
      await waitFor(() => manager.snapshot().state === 'connected')
      const before = manager.snapshot().profileId
      const stagingId = (await manager.enableStaging()).id
      await assert.rejects(manager.switchProfile(stagingId), (error) => error.code === 'conflict')
      assert.equal(manager.snapshot().profileId, before)
      await manager.dispose()
    } finally {
      rmSync(failingStateRoot, { recursive: true, force: true })
    }
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})

test('a second process cannot own the active installation', async () => {
  const f = fixture()
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'connected')
    const second = createConnectionManager({
      stateRoot: f.stateRoot,
      credentials: new MemoryHostCredentials(),
      source: {},
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: f.network.fetch,
      mqtt: createMqtt([]),
      runtimeFactory: createRuntimes().factory,
      installationId: 'test-install',
    })
    await second.ready
    await assert.rejects(second.startConnect(), (error) => error.code === 'conflict')
    await second.dispose()
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})

test('cancel fences a late approval and never enrolls or connects', async () => {
  let releasePoll
  const f = fixture({
    fetchOptions: {
      onPoll: () => new Promise((resolve) => {
        releasePoll = () => resolve(json(200, {
          id: 'request-1',
          state: 'approved',
          authorization: {
            authVersion: 'whalerein/v1/ext-auth-v1',
            authorizationId: 'auth-1',
            issuer: 'https://issuer.example',
            audience: 'whalerein-cloud',
            purpose: 'host_enrollment',
            workspaceId: 'ws-1',
            regionId: 'r1',
            operationId: 'op-1',
            enrollmentRevision: '1',
            hostId: f.network?.intent()?.hostId,
            hostPublicKeys: [f.network?.intent()?.signingKey, f.network?.intent()?.noiseKey],
            issuedAtUnixMs: NOW - 1000,
            expiresAtUnixMs: NOW + 300_000,
            nonce: b64(Buffer.alloc(16, 1)),
            signature: b64(Buffer.alloc(64, 1)),
          },
          route: { region: 'r1', accessOrigin: 'https://access.example', expiresAt: new Date(NOW + 300_000).toISOString() },
        }))
      }),
    },
  })
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'awaiting_approval')
    assert.equal(typeof releasePoll, 'function')
    await f.manager.cancel()
    assert.equal(f.manager.snapshot().connected, false)
    assert.equal(f.manager.snapshot().pendingOperation, false)
    releasePoll()
    await new Promise((resolve) => setTimeout(resolve, 30))
    assert.equal(f.manager.snapshot().state !== 'connected', true)
    assert.equal(f.runtimes.length, 0)
    const profileId = f.manager.snapshot().profileId
    assert.equal(existsSync(join(f.stateRoot, 'profiles', profileId, 'enrollment-intent.json')), false)
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})

test('retry after denial creates a new approval intent', async () => {
  let creates = 0
  const f = fixture({
    fetchOptions: {
      pollReply: 'denied',
      onCreate: () => {
        creates += 1
        return undefined
      },
    },
  })
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'denied')
    assert.equal(f.manager.snapshot().error.code, 'forbidden')
    const retried = await f.manager.retry()
    assert.equal(retried.state, 'awaiting_approval')
    assert.equal(creates, 2)
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})

test('custom managed service requires fetched metadata, stays inactive, and deletes locally only', async () => {
  let metadataCalls = 0
  const f = fixture({
    fetchOptions: {
      onMetadata: async (url) => {
        metadataCalls += 1
        return json(200, metadataDocument({
          service: 'lab-service',
          environment: 'custom',
          approval: { origin: 'https://lab.example', path: '/approve/{id}' },
        }))
      },
    },
  })
  try {
    const before = f.manager.snapshot().profileId
    const custom = await f.manager.addCustomService({ label: 'Lab', apiOrigin: 'https://lab.example' })
    assert.equal(custom.environment, 'custom')
    assert.equal(custom.active, false)
    assert.equal(f.manager.snapshot().profileId, before)
    assert.equal(metadataCalls, 1)
    const customStore = new ProfileCredentialStore({ credentials: f.credentials, profileId: custom.id, installationId: 'test-install' })
    await customStore.set(CREDENTIAL_PURPOSES.recoverySecret, Buffer.alloc(32).toString('base64url'))
    const deleted = await f.manager.deleteProfile(custom.id)
    assert.equal(await customStore.get(CREDENTIAL_PURPOSES.recoverySecret), null)
    assert.equal(deleted.remoteRegistration, 'not_revoked')
    assert.match(deleted.message, /remote Host registration remains/i)
    await assert.rejects(f.manager.deleteProfile(custom.id), (error) => error.code === 'not_found')
    await assert.rejects(f.manager.deleteProfile(before), (error) => error.code === 'conflict')
    await assert.rejects(f.manager.addCustomService({ label: 'Bad', apiOrigin: 'http://lab.example' }), (error) => error.code === 'invalid_argument')
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})

test('removal opens the management page, stays pending and never claims local revocation', async () => {
  const f = fixture({ removalStatus: async () => ({ state: 'removed' }) })
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'connected')
    const removal = await f.manager.remove()
    assert.equal(removal.state, 'removing')
    assert.equal(removal.remoteRevocation, 'pending')
    assert.match(removal.url, /^https:\/\/app\.whalerein\.com\/hosts\//)
    assert.equal(f.manager.snapshot().state, 'removing')
    await assert.rejects(f.manager.startConnect(), (error) => error.code === 'removal_pending')
    await assert.rejects(f.manager.retry(), (error) => error.code === 'removal_pending')
    const reconciled = await f.manager.reconcileRemoval()
    assert.equal(reconciled.state, 'removed')
    assert.equal(reconciled.connected, false)
    const store = new ProfileCredentialStore({ credentials: f.credentials, profileId: reconciled.profileId, installationId: 'test-install' })
    assert.equal(await store.getJson(CREDENTIAL_PURPOSES.transportCredential), null)
    // Re-add requires explicit fresh approval.
    const readd = await f.manager.startConnect()
    assert.equal(readd.state, 'awaiting_approval')
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})

test('legacy standalone configuration remains a separate manual mode', async () => {
  const stateRoot = mkdtempSync(join(tmpdir(), 'whalerein-manager-legacy-'))
  writeFileSync(join(stateRoot, 'state.sqlite'), 'legacy')
  const runtimes = createRuntimes()
  try {
    const manager = createConnectionManager({
      stateRoot,
      credentials: new MemoryHostCredentials(),
      source: null,
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: async () => { throw new Error('legacy mode must not call Cloud') },
      mqtt: () => { throw new Error('legacy mode builds its own transport') },
      runtimeFactory: runtimes.factory,
      installationId: 'test-install',
      standalone: { config: { hostId: 'legacy-host', hostToken: 'legacy-token', mqttUrl: 'mqtts://legacy.example', stateDir: stateRoot } },
    })
    await manager.ready
    const snapshot = manager.snapshot()
    assert.equal(snapshot.environment, 'standalone')
    assert.equal(snapshot.profileId, 'standalone')
    assert.equal(snapshot.state, 'disconnected')
    const connected = await manager.startConnect({ source: {} })
    assert.equal(connected.state, 'connected')
    assert.equal(runtimes.runtimes.length, 1)
    const details = manager.details()
    assert.equal(details.mode, 'standalone')
    assert.equal(JSON.stringify(details).includes('legacy-token'), false)
    await assert.rejects(manager.remove(), (error) => error.code === 'unsupported_capability')
    await manager.dispose()
  } finally {
    rmSync(stateRoot, { recursive: true, force: true })
  }
})

test('corrupt registry, moved state and missing committed keys stay actionable', async () => {
  const corruptRoot = mkdtempSync(join(tmpdir(), 'whalerein-manager-corrupt-'))
  try {
    writeFileSync(join(corruptRoot, 'host-profiles.json'), '{bad')
    assert.throws(() => createConnectionManager({
      stateRoot: corruptRoot,
      credentials: new MemoryHostCredentials(),
      installationId: 'test-install',
    }), (error) => error.code === 'registry_corrupt')
  } finally {
    rmSync(corruptRoot, { recursive: true, force: true })
  }

  const f = fixture()
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'connected')
    const profileId = f.manager.snapshot().profileId
    const registry = new HostProfileRegistry({ stateRoot: f.stateRoot })
    assert.match(registry.get(profileId).bindings.signingPublicKey, /^[A-Za-z0-9_-]+$/)
    await f.manager.dispose()
    // A fresh credential store has no committed key while the registry does.
    const missing = createConnectionManager({
      stateRoot: f.stateRoot,
      credentials: new MemoryHostCredentials(),
      source: {},
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: f.network.fetch,
      mqtt: createMqtt([]),
      runtimeFactory: createRuntimes().factory,
      installationId: 'test-install',
    })
    await missing.ready
    await waitFor(() => missing.snapshot().state === 'action_required')
    assert.ok(['internal'].includes(missing.snapshot().error.code))
    await missing.dispose()
    // Moving the state directory is rejected by a new manager.
    rmSync(join(f.stateRoot, 'profiles', profileId), { recursive: true, force: true })
    assert.throws(() => createConnectionManager({
      stateRoot: f.stateRoot,
      credentials: f.credentials,
      installationId: 'test-install',
    }), (error) => error.code === 'registry_unsafe_state')
  } finally {
    f.cleanup()
  }
})

test('metadata failure leaves the selected profile intact without fallback', async () => {
  const f = fixture({
    fetchOptions: {
      onMetadata: async () => json(503, {}),
    },
  })
  try {
    const profileId = f.manager.snapshot().profileId
    await assert.rejects(f.manager.startConnect(), (error) => error.code === 'unavailable')
    assert.equal(f.manager.snapshot().profileId, profileId)
    assert.equal(f.manager.snapshot().state, 'action_required')
    assert.equal(f.runtimes.length, 0)
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})

test('dispose stops the runtime and fences later operations', async () => {
  const f = fixture()
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'connected')
    await f.manager.dispose()
    await f.manager.dispose()
    assert.equal(f.runtimes[0].stopped > 0, true)
    await assert.rejects(f.manager.startConnect(), (error) => error.code === 'disposed')
  } finally {
    f.cleanup()
  }
})

function wrapCredentials(base, { failTransport = false, failRemove = false } = {}) {
  return {
    async getCredential(name) {
      return base.getCredential(name)
    },
    async setCredential(name, value) {
      if (failTransport && name.includes('transport-credential')) throw new Error('credential write failed')
      return base.setCredential(name, value)
    },
    async deleteCredential(name) {
      if (failRemove) throw new Error('delete failed')
      return base.deleteCredential?.(name)
    },
  }
}

test('manager constructor rejects missing state root and credential adapter', () => {
  assert.throws(() => createConnectionManager({ credentials: new MemoryHostCredentials(), installationId: 'x' }), (error) => error.code === 'invalid_argument')
  assert.throws(() => createConnectionManager({ stateRoot: '/tmp/x', installationId: 'x' }), (error) => error.code === 'invalid_argument')
})

test('snapshot exposes secret-free profile list and allowlisted capabilities', async () => {
  const f = fixture()
  try {
    const initial = f.manager.snapshot()
    assert.equal(Array.isArray(initial.profiles), true)
    assert.equal(initial.profiles.length, 1)
    assert.equal(initial.profiles[0].active, true)
    assert.equal(initial.profiles[0].managed, true)
    assert.deepEqual(initial.capabilities, [])
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'connected')
    const snapshot = f.manager.snapshot()
    assert.deepEqual(snapshot.capabilities, ['host-enrollment-v1'])
    assert.deepEqual(snapshot.profiles[0].capabilities, ['host-enrollment-v1'])
    const staging = await f.manager.enableStaging()
    const withStaging = f.manager.snapshot()
    const stagingProfile = withStaging.profiles.find((profile) => profile.id === staging.id)
    assert.equal(stagingProfile.environment, 'staging')
    assert.equal(stagingProfile.active, false)
    assert.equal(stagingProfile.managed, true)
    assert.equal(JSON.stringify(withStaging).includes('broker-password'), false)
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})

test('cancel after an uncertain credential write retains recovery identity', async () => {
  const f = fixture({ credentials: wrapCredentials(new MemoryHostCredentials(), { failTransport: true }) })
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'action_required')
    assert.equal(f.manager.snapshot().error.code, 'recovery_required')
    const registry = new HostProfileRegistry({ stateRoot: f.stateRoot }).read()
    const active = registry.profiles.find((profile) => profile.id === registry.active)
    assert.equal(active.pending.uncertain, true)
    const cancelled = await f.manager.cancel()
    assert.equal(cancelled.state, 'action_required')
    assert.equal(cancelled.error.code, 'recovery_required')
    assert.equal(f.runtimes.length, 0)
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})

test('retry resume paths reconnect or start a new approval without fallback', async () => {
  const f = fixture()
  try {
    // idle -> retry starts a fresh approval
    const fresh = await f.manager.retry()
    assert.equal(fresh.state, 'awaiting_approval')
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
  const g = fixture()
  try {
    await g.manager.startConnect()
    await waitFor(() => g.manager.snapshot().state === 'connected')
    await g.manager.disconnect()
    assert.equal(g.manager.snapshot().state, 'disconnected')
    const reconnected = await g.manager.retry()
    assert.equal(reconnected.state, 'connected')
    await g.manager.dispose()
  } finally {
    g.cleanup()
  }
})

test('retry reconciles a persisted uncertain operation only when the credential exists', async () => {
  const f = fixture()
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'connected')
    await f.manager.disconnect()
    const profileId = f.manager.snapshot().profileId
    const registry = new HostProfileRegistry({ stateRoot: f.stateRoot })
    registry.setPending(profileId, { uncertain: true, phase: 'enrollment' })
    await f.manager.dispose()

    const reopened = createConnectionManager({
      stateRoot: f.stateRoot,
      credentials: f.credentials,
      source: {},
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: f.network.fetch,
      mqtt: createMqtt([]),
      runtimeFactory: createRuntimes().factory,
      installationId: 'test-install',
    })
    await reopened.ready
    assert.equal(reopened.snapshot().state, 'recovering')
    const afterCancel = await reopened.cancel()
    assert.equal(afterCancel.state, 'action_required')
    const retried = await reopened.retry()
    assert.equal(retried.state, 'connected')
    await reopened.dispose()

    // Without a persisted credential the same retry stays explicitly unresolved.
    const registry2 = new HostProfileRegistry({ stateRoot: f.stateRoot })
    const profileId2 = registry2.read().active
    registry2.setPending(profileId2, { uncertain: true, phase: 'enrollment' })
    const store = new ProfileCredentialStore({ credentials: f.credentials, profileId: profileId2, installationId: 'test-install' })
    await store.remove(CREDENTIAL_PURPOSES.transportCredential)
    const unresolved = createConnectionManager({
      stateRoot: f.stateRoot,
      credentials: f.credentials,
      source: {},
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: f.network.fetch,
      mqtt: createMqtt([]),
      runtimeFactory: createRuntimes().factory,
      installationId: 'test-install',
    })
    await unresolved.ready
    await unresolved.cancel()
    await assert.rejects(unresolved.retry(), (error) => error.code === 'recovery_required')
    await unresolved.dispose()
  } finally {
    f.cleanup()
  }
})

test('removal reconciliation preserves pending on outage, failure and removing states', async () => {
  let status = 'failed'
  const f = fixture({ removalStatus: async () => status })
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'connected')
    await f.manager.remove()
    let snapshot = await f.manager.reconcileRemoval()
    assert.equal(snapshot.state, 'action_required')
    assert.equal(f.manager.details().removalState, 'failed')
    status = 'removing'
    snapshot = await f.manager.reconcileRemoval()
    assert.equal(snapshot.state, 'removing')
    status = 'removed'
    snapshot = await f.manager.reconcileRemoval()
    assert.equal(snapshot.state, 'removed')
    // A later observation with no pending removal is a no-op.
    assert.equal((await f.manager.reconcileRemoval()).state, 'removed')
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
  const noProvider = fixture()
  try {
    await noProvider.manager.startConnect()
    await waitFor(() => noProvider.manager.snapshot().state === 'connected')
    await noProvider.manager.remove()
    assert.equal((await noProvider.manager.reconcileRemoval()).state, 'removing')
    await noProvider.manager.dispose()
  } finally {
    noProvider.cleanup()
  }
})

test('reconnect refuses a pending removal and an expired assigned route', async () => {
  const f = fixture()
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'connected')
    await f.manager.disconnect()
    const profileId = f.manager.snapshot().profileId
    const registry = new HostProfileRegistry({ stateRoot: f.stateRoot })
    registry.setPending(profileId, { removal: 'removing' })
    await assert.rejects(f.manager.reconnect(), (error) => error.code === 'removal_pending')
    registry.setPending(profileId, { removal: null })
    const store = new ProfileCredentialStore({ credentials: f.credentials, profileId, installationId: 'test-install' })
    const credential = await store.getJson(CREDENTIAL_PURPOSES.transportCredential)
    await store.setJson(CREDENTIAL_PURPOSES.transportCredential, { ...credential, routeExpiresAt: NOW - 1 })
    await assert.rejects(f.manager.reconnect(), (error) => error.code === 'expired')
    assert.equal(f.manager.snapshot().environment, 'production')
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})

test('restart resumes a persisted approval through an injected source provider', async () => {
  const f = fixture({
    fetchOptions: {
      onPoll: async () => json(200, { id: 'request-1', state: 'pending' }),
    },
  })
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'awaiting_approval')
    await f.manager.dispose()
    const resumed = createConnectionManager({
      stateRoot: f.stateRoot,
      credentials: f.credentials,
      source: null,
      getSource: () => ({}),
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: async (url, options) => {
        if (url.includes('/api/enrollment-requests/')) {
          const intent = JSON.parse(readFileSync(join(f.stateRoot, 'profiles', f.manager.snapshot().profileId, 'enrollment-intent.json'), 'utf8'))
          return createFetch().fetch(url, { ...options, method: 'GET' })
            ?? json(200, { id: intent.id, state: 'pending' })
        }
        return f.network.fetch(url, options)
      },
      mqtt: createMqtt([]),
      runtimeFactory: createRuntimes().factory,
      installationId: 'test-install',
    })
    await resumed.ready
    await new Promise((resolve) => setTimeout(resolve, 20))
    assert.equal(resumed.snapshot().state, 'awaiting_approval')
    assert.equal(resumed.snapshot().pendingOperation, true)
    await resumed.dispose()
  } finally {
    f.cleanup()
  }
})

test('a source provider failure leaves persisted approval dormant without inventing a runtime', async () => {
  const f = fixture({ fetchOptions: { onPoll: async () => json(200, { id: 'request-1', state: 'pending' }) } })
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'awaiting_approval')
    await f.manager.dispose()
    const dormant = createConnectionManager({
      stateRoot: f.stateRoot,
      credentials: f.credentials,
      source: null,
      getSource: () => { throw new Error('source unavailable') },
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: f.network.fetch,
      mqtt: createMqtt([]),
      runtimeFactory: createRuntimes().factory,
      installationId: 'test-install',
    })
    await dormant.ready
    assert.equal(dormant.snapshot().state, 'awaiting_approval')
    assert.equal(dormant.snapshot().connected, false)
    await dormant.dispose()
  } finally {
    f.cleanup()
  }
})

test('persisted approved intent resumes through polling and uncertain intent is fenced', async () => {
  const f = fixture()
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'awaiting_approval')
    const profileId = f.manager.snapshot().profileId
    const path = join(f.stateRoot, 'profiles', profileId, 'enrollment-intent.json')
    const intent = JSON.parse(readFileSync(path, 'utf8'))
    await f.manager.dispose()

    writeFileSync(path, JSON.stringify({ ...intent, state: 'approved' }))
    const noAuth = createConnectionManager({
      stateRoot: f.stateRoot,
      credentials: f.credentials,
      source: {},
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: f.network.fetch,
      mqtt: createMqtt([]),
      runtimeFactory: createRuntimes().factory,
      installationId: 'test-install',
    })
    await noAuth.ready
    await noAuth.startConnect()
    await waitFor(() => noAuth.snapshot().state === 'connected')
    assert.equal(noAuth.snapshot().connected, true)
    await noAuth.dispose()

    writeFileSync(path, JSON.stringify({ ...intent, state: 'uncertain' }))
    const registry = new HostProfileRegistry({ stateRoot: f.stateRoot })
    registry.setPending(profileId, { uncertain: true, phase: 'enrollment' })
    const uncertain = createConnectionManager({
      stateRoot: f.stateRoot,
      credentials: f.credentials,
      source: {},
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: f.network.fetch,
      mqtt: createMqtt([]),
      runtimeFactory: createRuntimes().factory,
      installationId: 'test-install',
    })
    await uncertain.ready
    const result = await uncertain.startConnect()
    assert.equal(result.state, 'recovering')
    await uncertain.dispose()
  } finally {
    f.cleanup()
  }
})

test('runtime failures and missing readiness produce action-required without fallback', async () => {
  const f = fixture({ runtimeOptions: { failStartFor: () => true } })
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'action_required')
    assert.equal(f.manager.snapshot().error.code, 'internal')
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
  const noReadiness = fixture({ mqtt: () => ({ start() {}, connected: false, async stop() {} }) })
  try {
    await noReadiness.manager.startConnect()
    await waitFor(() => noReadiness.manager.snapshot().state === 'action_required')
    assert.equal(noReadiness.manager.snapshot().error.code, 'unavailable')
    await noReadiness.manager.dispose()
  } finally {
    noReadiness.cleanup()
  }
})

test('a switched target failure stays selected and never falls back', async () => {
  const stateRoot = mkdtempSync(join(tmpdir(), 'whalerein-manager-'))
  const stagingDoc = metadataDocument({
    service: 'whalerein-cloud-staging',
    environment: 'staging',
    approval: { origin: 'https://app.staging.whalerein.com', path: '/approve/{id}' },
  })
  const productionFetch = createFetch()
  const stagingFetch = createFetch({ metadata: stagingDoc })
  const credentials = new MemoryHostCredentials()
  try {
    const first = createConnectionManager({
      stateRoot,
      credentials,
      source: {},
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: productionFetch.fetch,
      mqtt: createMqtt([]),
      runtimeFactory: createRuntimes().factory,
      installationId: 'test-install',
    })
    await first.startConnect()
    await waitFor(() => first.snapshot().state === 'connected')
    const staging = await first.enableStaging()
    await first.switchProfile(staging.id)
    await first.dispose()

    const second = createConnectionManager({
      stateRoot,
      credentials,
      source: {},
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: stagingFetch.fetch,
      mqtt: createMqtt([]),
      runtimeFactory: createRuntimes().factory,
      installationId: 'test-install',
    })
    await second.startConnect()
    await waitFor(() => second.snapshot().state === 'connected' && second.snapshot().environment === 'staging')
    const registry = new HostProfileRegistry({ stateRoot })
    const stagingHostId = registry.get(staging.id).bindings.hostId
    const productionId = registry.list().find((profile) => profile.environment === 'production').id
    await second.switchProfile(productionId)
    await waitFor(() => second.snapshot().state === 'connected' && second.snapshot().environment === 'production')
    await second.dispose()

    const failing = createConnectionManager({
      stateRoot,
      credentials,
      source: {},
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: productionFetch.fetch,
      mqtt: createMqtt([]),
      runtimeFactory: createRuntimes({ failStartFor: (config) => config.hostId === stagingHostId }).factory,
      installationId: 'test-install',
    })
    await failing.ready
    await waitFor(() => failing.snapshot().state === 'connected')
    assert.equal(failing.snapshot().profileId, productionId)
    const switched = await failing.switchProfile(staging.id)
    assert.equal(switched.profileId, staging.id)
    assert.equal(switched.environment, 'staging')
    assert.equal(switched.state, 'action_required')
    assert.equal(switched.connected, false)
    await failing.dispose()
  } finally {
    rmSync(stateRoot, { recursive: true, force: true })
  }
})

test('a profile state directory made unsafe is reported without identity regeneration', async () => {
  const f = fixture()
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'connected')
    await f.manager.disconnect()
    const profileId = f.manager.snapshot().profileId
    const stateDir = join(f.stateRoot, 'profiles', profileId)
    rmSync(stateDir, { recursive: true, force: true })
    const target = join(f.stateRoot, 'moved')
    mkdirSync(target)
    symlinkSync(target, stateDir)
    await assert.rejects(f.manager.reconnect(), (error) => error.code === 'registry_unsafe_state')
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})


test('credential recovery uses a fresh recovery authorization without re-enrolling', async () => {
  const f = fixture()
  try {
    await f.manager.startConnect()
    await waitFor(() => f.manager.snapshot().state === 'connected')
    await f.manager.disconnect()
    const profileId = f.manager.snapshot().profileId
    const store = new ProfileCredentialStore({ credentials: f.credentials, profileId, installationId: 'test-install' })
    await store.remove(CREDENTIAL_PURPOSES.transportCredential)
    const registry = new HostProfileRegistry({ stateRoot: f.stateRoot })
    registry.setPending(profileId, { uncertain: true, phase: 'credential' })
    const enrollCallsBefore = f.network.calls.filter((call) => call.url.endsWith('/v1/enrollments/hosts')).length
    const recovered = await f.manager.recoverCredential({
      authorization: {
        authVersion: 'whalerein/v1/ext-auth-v1',
        authorizationId: 'recovery-1',
        issuer: 'https://issuer.example',
        audience: 'https://access.example',
        purpose: 'host_credential_recovery',
        hostId: registry.get(profileId).bindings.hostId,
        issuedAtUnixMs: NOW - 1000,
        expiresAtUnixMs: NOW + 60_000,
        nonce: Buffer.alloc(16, 1).toString('base64url'),
      },
    })
    assert.equal(recovered.state, 'connected')
    assert.equal(f.mqtts.at(-1).options.token, 'recovered-password')
    assert.equal(f.network.calls.filter((call) => call.url.endsWith('/v1/enrollments/hosts')).length, enrollCallsBefore)
    await f.manager.dispose()
  } finally {
    f.cleanup()
  }
})

test('live transport disconnects and readiness are reflected in manager state', async () => {
  let handlers = null
  const access = {
    connected: false,
    start(received) {
      handlers = received
      access.connected = true
      return access
    },
    async waitForConnected() {
      access.connected = true
    },
    async stop() {
      access.connected = false
    },
  }
  const runtimeFactory = ({ access: injected }) => ({
    async start() {
      injected.start({})
    },
    async stop() {},
  })
  const stateRoot = mkdtempSync(join(tmpdir(), 'whalerein-manager-'))
  try {
    const manager = createConnectionManager({
      stateRoot,
      credentials: new MemoryHostCredentials(),
      source: {},
      now: () => NOW,
      hostName: HOST_NAME,
      fetch: createFetch().fetch,
      mqtt: () => access,
      runtimeFactory,
      installationId: 'test-install',
    })
    await manager.startConnect()
    await waitFor(() => manager.snapshot().state === 'connected')
    handlers.onDisconnected()
    assert.equal(manager.snapshot().state, 'reconnecting')
    assert.equal(manager.snapshot().connected, false)
    handlers.onReady()
    assert.equal(manager.snapshot().state, 'connected')
    assert.equal(manager.snapshot().connected, true)
    await manager.dispose()
  } finally {
    rmSync(stateRoot, { recursive: true, force: true })
  }
})
