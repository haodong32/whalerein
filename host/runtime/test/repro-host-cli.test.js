// Regression: a resume that becomes ready after the readiness timeout must
// clear the persisted pause, so the next process start reconnects.

import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createConnectionManager } from '../src/manager.js'
import { MemoryHostCredentials } from '../src/security/credentials.js'
import { ProfileCredentialStore, CREDENTIAL_PURPOSES } from '../src/credentials.js'
import { HostProfileRegistry } from '../src/registry.js'
import { PluginError } from '../src/errors.js'

const NOW = 1_800_000_000_000

function ensureConfig(stateRoot) {
  const path = join(stateRoot, 'config.toml')
  writeFileSync(path, 'version = 1\nenvironment = "production"\n')
  return path
}

function runtimeFactory() {
  return ({ access }) => ({
    async start() {
      access.start?.({})
    },
    async stop() {
      access.connected = false
    },
  })
}

function fastAccess() {
  const access = {
    connected: false,
    handlers: null,
    start(handlers) {
      access.handlers = handlers
      return access
    },
    async waitForConnected() {
      access.connected = true
    },
    async stop() {
      access.connected = false
    },
  }
  return access
}

function slowAccess() {
  const access = {
    connected: false,
    handlers: null,
    start(handlers) {
      access.handlers = handlers
      return access
    },
    async waitForConnected() {
      throw new PluginError('unavailable', 'readiness timeout')
    },
    async stop() {
      access.connected = false
    },
  }
  return access
}

async function seedBoundProfile(stateRoot, credentials) {
  const registry = new HostProfileRegistry({ stateRoot, now: () => NOW })
  registry.ensure()
  const id = registry.active().id
  registry.setBindings(id, {
    hostId: id,
    operationId: 'op-1',
    accessOrigin: 'https://access.example',
    enrollmentRevision: '1',
  })
  const store = new ProfileCredentialStore({
    credentials,
    profileId: id,
    installationId: 'test-install',
  })
  await store.setJson(CREDENTIAL_PURPOSES.transportCredential, {
    hostId: id,
    mqttUrl: 'mqtts://broker.example:8883',
    username: `host:${id}`,
    password: 'transport-secret',
    accessOrigin: 'https://access.example',
    enrollmentRevision: '1',
    operationId: 'op-1',
    persistedAt: NOW,
  })
  return id
}

test('a late readiness after Resume leaves the Host paused for the next start', async () => {
  const stateRoot = mkdtempSync(join(tmpdir(), 'whalerein-repro-pause-'))
  const credentials = new MemoryHostCredentials()
  try {
    await seedBoundProfile(stateRoot, credentials)
    let slow = null
    let fast = 0
    const manager = createConnectionManager({
      stateRoot,
      configFile: ensureConfig(stateRoot),
      credentials,
      source: {},
      now: () => NOW,
      hostName: 'repro-host',
      mqtt: () => {
        if (slow === null) {
          fast += 1
          return fastAccess()
        }
        return slow
      },
      runtimeFactory: runtimeFactory(),
      log: null,
      installationId: 'test-install',
    })
    await manager.ready
    assert.equal(manager.snapshot().connected, true, 'the seeded profile connects')
    await manager.disconnect()
    assert.equal(manager.snapshot().state, 'disconnected')

    // The next transport start is slow: the readiness wait times out, but the
    // transport then reports Ready on its own retry loop.
    slow = slowAccess()
    const reconnecting = await manager.reconnect()
    assert.equal(reconnecting.state, 'reconnecting', 'readiness timeout is transient')
    slow.connected = true
    slow.handlers.onConnected?.()
    slow.handlers.onReady?.()
    const live = manager.snapshot()
    assert.equal(live.connected, true, 'the transport became ready')
    assert.equal(live.state, 'connected')

    await manager.dispose()

    // Restart the plugin on the same state. A Host that is connected must not
    // stay locally paused because its readiness was slow once.
    let restartedCalls = 0
    const restarted = createConnectionManager({
      stateRoot,
      configFile: ensureConfig(stateRoot),
      credentials,
      source: {},
      now: () => NOW,
      hostName: 'repro-host',
      mqtt: () => {
        restartedCalls += 1
        return fastAccess()
      },
      runtimeFactory: runtimeFactory(),
      log: null,
      installationId: 'test-install',
    })
    await restarted.ready
    if (restarted.snapshot().state === 'reconnecting') {
      await new Promise((resolve) => setTimeout(resolve, 20))
    }
    assert.ok(
      restartedCalls > 0,
      `the restarted Host must attempt the resumed connection (live snapshot: state=${live.state}, action=${live.action?.kind})`,
    )
    assert.equal(restarted.snapshot().connected, true, 'the restarted Host reconnects')
    const restartedSnapshot = restarted.snapshot()
    assert.notEqual(restartedSnapshot.action?.kind, 'resume', 'a connected Host cannot still be paused')
    await restarted.dispose()
  } finally {
    rmSync(stateRoot, { recursive: true, force: true })
  }
})
