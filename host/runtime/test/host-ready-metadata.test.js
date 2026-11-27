import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { hostname, tmpdir } from 'node:os'
import { join } from 'node:path'
import { HostRuntime } from '../src/host.js'
import { MemoryHostCredentials } from '../src/security/credentials.js'
import { TestSource } from './support/source.js'
import { FakeBroker } from './support/fake-mqtt.js'
import { ProtoCodec } from '../src/protocol.js'
import { testConfig } from './support/config.js'
import { silentLogger } from '../src/log.js'
import { hostOsFamily } from '../src/os.js'
import { sanitizeReadyDisplayName } from '../src/access/mqtt.js'
import { waitFor } from './support/harness.js'

function readyFrom(broker, codec) {
  const message = broker.messages.filter((entry) => entry.topic.endsWith('/result')).at(-1)
  assert.ok(message, 'a HostReady result was published')
  return codec.decode('whalerein.v1.HostReady', codec.decode('whalerein.v1.HostResult', message.payload).result_body)
}

function runtime({ broker, config, codec, credentials }) {
  return new HostRuntime({
    config,
    adapter: new TestSource(),
    credentials,
    accessConnect: broker.connectFn(),
    logger: silentLogger,
    codec,
  })
}

test('HostRuntime publishes bounded display metadata without feeding the Host id, route or fingerprint', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-ready-metadata-'))
  const broker = new FakeBroker()
  const codec = new ProtoCodec()
  const hostId = 'host-ready-metadata'
  const credentialFile = join(dir, 'credentials.json')
  const base = testConfig({
    mqtt_url: 'mqtts://fake-broker.test',
    host_id: hostId,
    host_token: 'token',
    state_dir: dir,
    reconnect_min_ms: 20,
    reconnect_max_ms: 40,
  }, {
    displayName: '  Mac\u0000Book\u009F Pro  ',
    osFamily: 'macOS',
  })
  const first = runtime({ broker, config: base, codec, credentials: new MemoryHostCredentials({ credentialFile }) })
  try {
    await first.start()
    await first.access.waitForConnected(5000)
    const ready = readyFrom(broker, codec)
    assert.equal(ready.host_id, hostId)
    assert.equal(ready.display_name, 'MacBook Pro')
    assert.equal(ready.os_family, 'macOS')
    assert.equal(first.access.hostId, hostId)
    assert.equal(first.access.requestTopic, `whalerein/v1/h/${hostId}/request`)
    assert.equal(first.access.resultTopic, `whalerein/v1/h/${hostId}/result`)
    assert.deepEqual(first.access.route, {
      nodeId: '', incarnation: '', service: '', environment: '', generation: 0, noisePublicKey: null,
    })
    const route = first.access.route
    const fingerprint = first.security.hostFingerprint

    // A reconnect republishes the same directory metadata for the same Host.
    broker.disconnectClientId(`whalerein-host-${hostId}`)
    await waitFor(() => first.access.connected === false)
    await waitFor(() => first.access.connected === true, 5000)
    const reconnected = readyFrom(broker, codec)
    assert.equal(reconnected.host_id, hostId)
    assert.equal(reconnected.display_name, 'MacBook Pro')
    assert.equal(reconnected.os_family, 'macOS')
    await first.stop()

    // Different display metadata for the same durable identity changes only
    // those fields, never the id, route or host fingerprint.
    const second = runtime({
      broker,
      config: { ...base, displayName: 'Other Name', osFamily: 'Linux' },
      codec,
      credentials: new MemoryHostCredentials({ credentialFile }),
    })
    try {
      await second.start()
      await second.access.waitForConnected(5000)
      const changed = readyFrom(broker, codec)
      assert.equal(changed.host_id, hostId)
      assert.equal(changed.display_name, 'Other Name')
      assert.equal(changed.os_family, 'Linux')
      assert.equal(changed.route_generation, undefined)
      assert.equal(second.security.hostFingerprint, fingerprint)
      assert.deepEqual(second.access.route, route)
    } finally {
      await second.stop()
    }
  } finally {
    if (first.started) await first.stop()
    rmSync(dir, { recursive: true, force: true })
  }
})

test('HostRuntime falls back to the instance label, machine name and local OS family', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-ready-fallback-'))
  const broker = new FakeBroker()
  const codec = new ProtoCodec()
  const config = testConfig({
    mqtt_url: 'mqtts://fake-broker.test',
    host_id: 'host-ready-fallback',
    host_token: 'token',
    state_dir: dir,
  }, { instanceLabel: 'Work Mac' })
  const host = runtime({ broker, config, codec, credentials: new MemoryHostCredentials() })
  try {
    await host.start()
    await host.access.waitForConnected(5000)
    const ready = readyFrom(broker, codec)
    assert.equal(ready.display_name, 'Work Mac')
    assert.equal(ready.os_family, hostOsFamily())
    assert.equal(host.access.displayName, 'Work Mac')
    assert.equal(host.access.osFamily, hostOsFamily())
  } finally {
    await host.stop()
    rmSync(dir, { recursive: true, force: true })
  }

  // Without an instance label the machine hostname is only a display prefill.
  const plainDir = mkdtempSync(join(tmpdir(), 'whalerein-ready-hostname-'))
  const plainBroker = new FakeBroker()
  const plain = runtime({
    broker: plainBroker,
    config: testConfig({
      mqtt_url: 'mqtts://fake-broker.test',
      host_id: 'host-ready-hostname',
      host_token: 'token',
      state_dir: plainDir,
    }),
    codec,
    credentials: new MemoryHostCredentials(),
  })
  try {
    await plain.start()
    await plain.access.waitForConnected(5000)
    const ready = readyFrom(plainBroker, codec)
    assert.equal(ready.display_name, sanitizeReadyDisplayName(hostname()))
    assert.equal(ready.os_family, hostOsFamily())
  } finally {
    await plain.stop()
    rmSync(plainDir, { recursive: true, force: true })
  }
})