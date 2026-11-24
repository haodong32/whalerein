// Regression: the settings projection retains the removal-confirmation target
// from the runtime snapshot, so a page reload or Host restart can reopen the
// pending removal flow after remove().

import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createConnectionManager } from 'whalerein-host/manager.js'
import { MemoryHostCredentials } from 'whalerein-host/security/credentials.js'
import { ProfileCredentialStore, CREDENTIAL_PURPOSES } from 'whalerein-host/credentials.js'
import { HostProfileRegistry } from 'whalerein-host/registry.js'
import { settingsStatus } from '../src/settings.js'

const NOW = 1_800_000_000_000

function ensureConfig(stateRoot) {
  const path = join(stateRoot, 'config.toml')
  writeFileSync(path, 'version = 1\nenvironment = "production"\n')
  return path
}

function fastAccess() {
  return {
    connected: false,
    start() {},
    async waitForConnected() {
      this.connected = true
    },
    async stop() {
      this.connected = false
    },
  }
}

function runtimeFactory() {
  return () => ({
    async start() {},
    async stop() {},
  })
}

test('the settings snapshot keeps the removal-confirmation target after remove()', async () => {
  const stateRoot = mkdtempSync(join(tmpdir(), 'whalerein-repro-removal-'))
  const credentials = new MemoryHostCredentials()
  try {
    const registry = new HostProfileRegistry({ stateRoot, now: () => NOW })
    registry.ensure()
    const id = registry.active().id
    registry.setBindings(id, {
      hostId: id,
      operationId: 'op-1',
      accessOrigin: 'https://access.example',
      enrollmentRevision: '1',
    })
    const store = new ProfileCredentialStore({ credentials, profileId: id, installationId: 'test-install' })
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

    const manager = createConnectionManager({
      stateRoot,
      configFile: ensureConfig(stateRoot),
      credentials,
      source: {},
      now: () => NOW,
      hostName: 'repro-host',
      mqtt: () => fastAccess(),
      runtimeFactory: runtimeFactory(),
      log: null,
      installationId: 'test-install',
    })
    await manager.ready
    assert.equal(manager.snapshot().connected, true)

    const removal = await manager.remove()
    assert.match(removal.url, /\?removal=1$/)
    const host = {
      managed: true,
      service: 'whalerein_cloud',
      manager,
      config: { service: 'whalerein_cloud' },
      started: true,
    }
    const reloaded = settingsStatus(host)
    assert.equal(
      reloaded.removalUrl,
      removal.url,
      'a settings reload after remove() must retain the browser-confirmation target',
    )

    await manager.dispose()
    const restarted = createConnectionManager({
      stateRoot,
      configFile: ensureConfig(stateRoot),
      credentials,
      source: {},
      now: () => NOW,
      hostName: 'repro-host',
      mqtt: () => fastAccess(),
      runtimeFactory: runtimeFactory(),
      log: null,
      installationId: 'test-install',
    })
    await restarted.ready
    const afterRestart = settingsStatus({ ...host, manager: restarted })
    assert.equal(
      afterRestart.removalUrl,
      removal.url,
      'a Host restart must retain the browser-confirmation target',
    )
    await restarted.dispose()
  } finally {
    rmSync(stateRoot, { recursive: true, force: true })
  }
})
