import { HostRuntime } from 'whalerein-host'
import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import plugin, { name, inject, Config, apply, createHost } from '../index.js'

test('when a profile has a host ID, apply preserves it; otherwise it generates one', async () => {
  const dirs = [0, 1, 2].map((index) => mkdtempSync(join(tmpdir(), `whalerein-apply-host-${index}-`)))
  const disposers = []
  const committed = []
  const base = {
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
