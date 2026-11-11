import { MemoryHostCredentials } from '../src/security/credentials.js'
import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { startHarness, waitFor } from './support/harness.js'
import { HostRuntime } from '../src/host.js'
import { StateStore } from '../src/state/store.js'
import { TestSource } from './support/source.js'
import { FakeBroker } from './support/fake-mqtt.js'
import { FakeAccessSimulator } from './support/wire-client.js'
import { MqttAccess } from '../src/access/mqtt.js'
import { ProtoCodec } from '../src/protocol.js'
import { testConfig } from './support/config.js'
import { silentLogger } from '../src/log.js'
import { sha256Bytes } from '../src/security/digest.js'

test('when the plugin reloads, identity and dedupe state persist while the channel is replaced', async () => {
  const harness = await startHarness({
    sessions: [{ sessionId: 's1' }],
  })
  try {
    const device = await harness.newPairedDevice('device-1')
    await device.command({ send_message: { session_id: 's1', text: 'before reload' } }, { commandId: 'cmd-reload' })
    const publicKey = harness.host.security.identity.publicKey
    const fingerprint = harness.host.security.identity.fingerprint
    await waitFor(() => harness.host.diagnostics().pendingCiphertext.count === 0)

    await harness.host.reload()
    await waitFor(() => harness.host.access.describe().connected === true, 5000)
    await waitFor(() => harness.host.security.revocationsPending === false, 5000)
    assert.deepEqual([...harness.host.security.identity.publicKey], [...publicKey])
    assert.equal(harness.host.security.identity.fingerprint, fingerprint)
    assert.ok(harness.host.security.hasPeer('device-1'))
    assert.equal(harness.host.diagnostics().activeChannels, 0)

    const rejected = await device.query({ list_sessions: {} })
    assert.equal(rejected.status, 'rejected')
    assert.equal(rejected.errorCode, 'unauthorized')
    await device.connectChannel()
    const list = await device.query({ list_sessions: {} })
    assert.equal(list.status, 'succeeded')

    const replay = await device.command({
      send_message: { session_id: 's1', text: 'before reload' },
    }, { commandId: 'cmd-reload' })
    assert.equal(replay.status, 'rejected')
    assert.equal(replay.errorCode, 'payload_digest_mismatch')
  } finally {
    await harness.stop()
  }
})

test('when the plugin stops normally, it releases the adapter subscription and preserves the cursor', async () => {
  const harness = await startHarness({ sessions: [{ sessionId: 's1' }] })
  const before = harness.adapter
  const device = await harness.newPairedDevice('device-1')
  await device.command({ send_message: { session_id: 's1', text: 'hello' } }, { commandId: 'cmd-stop' })
  await waitFor(() => harness.host.diagnostics().pendingCiphertext.count === 0)
  const store = new StateStore({ path: join(harness.stateDir, 'state.sqlite') })
  before.metadata.set('fixture-bookmark:s1', 'bookmark:committed')
  const cursor = store.getMeta('source:fixture-bookmark:s1')
  assert.equal(cursor, 'bookmark:committed')
  await harness.host.stop()
  assert.equal(harness.host.started, false)
  assert.equal(harness.host.access, null)
  assert.equal(harness.host.events, null)
  assert.equal(before.watchers.size, 0)

  before.publish('s1', { state_changed: true })
  await new Promise((resolve) => setTimeout(resolve, 10))
  const reopened = new StateStore({ path: join(harness.stateDir, 'state.sqlite') })
  assert.equal(reopened.getMeta('source:fixture-bookmark:s1'), cursor)
  store.close()
  reopened.close()
  await device.close()
  await harness.stop()
})

test('when startup finds an accepted unfinished command, recovery marks it unknown', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-crash-'))
  try {
    const broker = new FakeBroker()
    const codec = new ProtoCodec()
    const hostId = 'host-crash'
    const access = new FakeAccessSimulator({ broker, codec, hostId })
    await access.ready
    const config = testConfig({
      adapter: 'fake',
      mqtt_url: 'mqtts://fake-broker.test',
      host_id: hostId,
      host_token: 'token',
      state_dir: dir,
      event_batch_ms: 0,
      shutdown_timeout_ms: 100,
      reconnect_min_ms: 50,
      reconnect_max_ms: 100,
    }, {})
    const credentials = join(dir, 'credentials.json')
    const store = new StateStore({ path: join(dir, 'state.sqlite') })
    const adapter = new TestSource({ sessions: [{ sessionId: 's1' }] })
    const mqtt = new MqttAccess({ url: config.mqttUrl, hostId, token: config.hostToken, connect: broker.connectFn(), logger: silentLogger, codec })
    const host = new HostRuntime({ credentials: new MemoryHostCredentials({credentialFile: credentials}), config, adapter, store, access: mqtt, logger: silentLogger, codec })
    await host.start()
    await mqtt.waitForConnected(5000)
    await waitFor(() => host.security.revocationsPending === false, 5000)
    store.createOrGetCommand({
      deviceId: 'd1',
      commandId: 'crash-cmd',
      requestId: 'r1',
      digest: sha256Bytes(new Uint8Array([1])),
      expiresAt: 100000,
    })

    const store2 = new StateStore({ path: join(dir, 'state.sqlite') })
    const adapter2 = new TestSource({ sessions: [{ sessionId: 's1' }] })
    const mqtt2 = new MqttAccess({ url: config.mqttUrl, hostId, token: config.hostToken, connect: broker.connectFn(), logger: silentLogger, codec })
    const host2 = new HostRuntime({ credentials: new MemoryHostCredentials({credentialFile: credentials}), config, adapter: adapter2, store: store2, access: mqtt2, logger: silentLogger, codec })
    await host2.start()
    assert.equal(store2.getCommand('d1', 'crash-cmd').status, 'unknown')
    assert.equal(store2.getCommand('d1', 'crash-cmd').errorCode, 'unavailable')
    assert.equal(store.getCommand('d1', 'crash-cmd').status, 'unknown')
    await host2.stop()
    await host.stop()
    store2.close()
    store.close()
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('when host keys are created, private key bytes and encodings stay out of SQLite and diagnostics while public identity remains available', async () => {
  const harness = await startHarness({ sessions: [{ sessionId: 's1' }] })
  try {
    await harness.newPairedDevice('device-1')
    const privateKeyB64 = Buffer.from(harness.host.security.identity.privateKey).toString('base64')
    const privateKeyBytes = Buffer.from(harness.host.security.identity.privateKey)
    const privateKeyHex = privateKeyBytes.toString('hex')
    assert.equal(privateKeyB64.length, 44)
    for (const file of readdirSync(harness.stateDir)) {
      const bytes = readFileSync(join(harness.stateDir, file))
      assert.equal(bytes.includes(Buffer.from(privateKeyB64)), false, file)
      assert.equal(bytes.includes(privateKeyBytes), false, file)
      assert.equal(bytes.includes(Buffer.from(privateKeyHex)), false, file)
      assert.equal(bytes.includes(Buffer.from(harness.host.config.hostToken)), false, file)
    }
    const diagnostics = JSON.stringify(harness.host.diagnostics())
    assert.equal(diagnostics.includes(harness.host.config.hostToken), false)
    assert.equal(diagnostics.includes(privateKeyB64), false)
    assert.equal(diagnostics.includes(privateKeyHex), false)
    assert.equal(harness.host.store.getHostPublicKey().fingerprint, harness.host.security.hostFingerprint)
    assert.deepEqual(Object.keys(JSON.parse(diagnostics).source).sort(), ['available', 'capabilities', 'mode'])
    assert.equal(harness.host.diagnostics().cursors.sessions >= 0, true)
  } finally {
    await harness.stop()
  }
})

