import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { MqttAccess } from '../src/access/mqtt.js'
import { ConfigError } from '../src/config.js'
import { codec } from '../src/protocol.js'
import { FakeBroker } from './support/fake-mqtt.js'
import { waitFor } from './support/harness.js'

const proto = codec()

function makeAccess(overrides = {}) {
  const broker = overrides.broker ?? new FakeBroker()
  const accesses = []
  const options = []
  const connect = (url, opts) => {
    options.push({ url, opts })
    const client = broker.connectFn()(url, opts)
    accesses.push(client)
    return client
  }
  const access = new MqttAccess({
    url: 'mqtts://broker.test:8883',
    hostId: 'host-1',
    token: 'secret-token',
    connect,
    reconnectMinMs: 50,
    reconnectMaxMs: 100,
    ...overrides,
  })
  return { broker, access, options, accesses }
}

test('when MQTT connects, Access uses MQTT 5 identity, subscribes to its request topic, and sends HostReady', async () => {
  const { broker, access, options } = makeAccess()
  const calls = { connected: 0, ready: 0, requests: [] }
  access.start({
    onConnected: () => calls.connected++,
    onReady: () => calls.ready++,
    onRequest: (request) => calls.requests.push(request),
  })
  await access.waitForConnected(2000)
  assert.equal(access.connected, true)
  assert.equal(calls.connected, 1)
  assert.equal(calls.ready, 1)
  assert.equal(options[0].opts.protocolVersion, 5)
  assert.equal(options[0].opts.clientId, 'whalerein-host-host-1')
  assert.equal(options[0].opts.username, 'host:host-1')
  assert.equal(options[0].opts.password, 'secret-token')
  assert.equal(options[0].opts.reconnectPeriod, 0)
  assert.equal(broker.subscriberCount('whalerein/v1/h/host-1/request'), 1)
  const ready = broker.messages.find((message) => message.topic === 'whalerein/v1/h/host-1/result')
  const readyResult = proto.decode('whalerein.v1.HostResult', ready.payload)
  assert.equal(readyResult.status, 'succeeded')
  const readyBody = proto.decode('whalerein.v1.HostReady', readyResult.result_body)
  assert.equal(readyBody.host_id, 'host-1')

  const request = proto.encode('whalerein.v1.HostRequest', {
    request_id: 'r1',
    host_id: 'host-1',
    kind: 'REQUEST_KIND_QUERY',
    protocol_version: 1,
    body: new Uint8Array(0),
  })
  // Deliver through the real broker routing path.
  const publisher = broker.connectFn()('mqtts://access.test', { clientId: 'access' })
  await new Promise((resolve) => publisher.subscribe('whalerein/v1/h/host-1/request', { qos: 1 }, resolve))
  await new Promise((resolve) => publisher.publish('whalerein/v1/h/host-1/request', request, { qos: 1 }, resolve))
  await waitFor(() => calls.requests.length === 1)
  assert.equal(calls.requests[0].request_id, 'r1')

  await access.stop()
  assert.equal(access.connected, false)
  assert.equal(access.describe().connected, false)
})

test('when MQTT result/event publication succeeds or fails, Access reports the outcome', async () => {
  const { broker, access } = makeAccess()
  await assert.rejects(access.publishResult(new Uint8Array([1])), (error) => error.code === 'unavailable')
  await assert.rejects(access.publishEvent(new Uint8Array([1])), (error) => error.code === 'unavailable')
  const seen = []
  const subscriber = broker.connectFn()('mqtts://access.test', { clientId: 'access' })
  await new Promise((resolve) => subscriber.subscribe('whalerein/v1/h/host-1/result', { qos: 1 }, resolve))
  await new Promise((resolve) => subscriber.subscribe('whalerein/v1/h/host-1/event', { qos: 1 }, resolve))
  subscriber.on('message', (topic, payload) => seen.push({ topic, payload: Buffer.from(payload) }))
  access.start({})
  await access.waitForConnected(2000)
  await access.publishResult(new Uint8Array([2]))
  await access.publishEvent(new Uint8Array([3]))
  await waitFor(() => seen.length >= 3)
  assert.equal(seen.some((entry) => entry.topic.endsWith('/result') && entry.payload[0] === 2), true)
  assert.equal(seen.some((entry) => entry.topic.endsWith('/event') && entry.payload[0] === 3), true)
  broker.online = false
  await assert.rejects(access.publishResult(new Uint8Array([4])), (error) => error.code === 'unavailable')
  assert.equal(access.describe().publishErrors >= 1, true)
  await access.stop()
})

test('when the broker disconnects, MQTT Access reconnects with backoff', async () => {
  const { broker, access } = makeAccess()
  let connections = 0
  access.start({ onConnected: () => connections++ })
  await access.waitForConnected(2000)
  broker.disconnectClientId('whalerein-host-host-1')
  await waitFor(() => access.connected === false)
  await waitFor(() => access.connected === true, 5000)
  assert.equal(connections, 2)
  assert.equal(access.describe().reconnects, 1)
  await access.stop()
})

test('when MQTT Access stops, timers are released and a later restart succeeds', async () => {
  const { access } = makeAccess()
  access.start({})
  await access.waitForConnected(2000)
  await access.stop()
  await access.stop()
  access.start({})
  await access.waitForConnected(2000)
  assert.equal(access.connected, true)
  await access.stop()
})

test('when a CA file is configured, MQTT Access loads it or rejects unreadable files', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-ca-'))
  try {
    const ca = join(dir, 'ca.pem')
    writeFileSync(ca, 'CA PENSION')
    const { access, options } = makeAccess({ caFile: ca })
    access.start({})
    await access.waitForConnected(2000)
    assert.equal(Buffer.isBuffer(options[0].opts.ca), true)
    assert.equal(options[0].opts.rejectUnauthorized, true)
    await access.stop()
    assert.throws(() => makeAccess({ caFile: join(dir, 'missing.pem') }), (error) => error instanceof ConfigError)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('when MQTT receives malformed bytes or a foreign topic, it dispatches no application request', async () => {
  const { broker, access, accesses } = makeAccess()
  const seen = []
  access.start({ onRequest: (request) => seen.push(request) })
  await access.waitForConnected(2000)
  const client = accesses[0]
  client.emit('message', 'other/topic', Buffer.from('ignored'))
  client.emit('message', 'whalerein/v1/h/host-1/request', Buffer.from([255, 255]))
  await access.stop()
  assert.equal(seen.length, 0)
  assert.equal(broker.messages.length >= 1, true)
})

test('when MQTT has never connected, waiting for readiness reaches its deadline with unavailable', async () => {
  const { access } = makeAccess()
  try {
    await assert.rejects(access.waitForConnected(20), error => error.code === 'unavailable')
  } finally {
    await access.stop()
  }
})

test('when MQTT disconnects, diagnostics include broker reason and publish size without payloads', async () => {
  const records = []
  const { createLogger } = await import('../src/log.js')
  const logger = createLogger({ salt: 'aa'.repeat(16), level: 'debug', sink: (record) => records.push(record) })
  const { access, accesses } = makeAccess({ logger })
  access.start({})
  await access.waitForConnected(2000)
  await access.publishEvent(new Uint8Array(321))
  accesses[0].emit('disconnect', { reasonCode: 149, properties: { reasonString: 'private-detail' } })
  accesses[0].emit('close')
  const disconnected = records.find((record) => record.event === 'mqtt_disconnected')
  assert.equal(disconnected.reasonCode, 149)
  assert.equal(disconnected.reason, 'broker_disconnect')
  assert.equal(disconnected.kind, 'event')
  assert.equal(disconnected.bytes, 321)
  assert.equal(disconnected.generation, 1)
  assert.equal(JSON.stringify(records).includes('private-detail'), false)
  await access.stop()
})

test('when concurrent publishes fail, each diagnostic reports its own size and kind', async () => {
  const records = []
  const { createLogger } = await import('../src/log.js')
  const logger = createLogger({ salt: 'aa'.repeat(16), sink: (record) => records.push(record) })
  const { access, accesses } = makeAccess({ logger })
  access.start({})
  await access.waitForConnected(2000)
  const callbacks = []
  accesses[0].publish = (_topic, _payload, _options, callback) => callbacks.push(callback)
  const failed = assert.rejects(access.publishEvent(new Uint8Array(2)))
  const succeeded = access.publishResult(new Uint8Array(3))
  callbacks[0](new Error('private detail'))
  callbacks[1](null)
  await Promise.all([failed, succeeded])
  const record = records.find((entry) => entry.event === 'mqtt_publish_failed')
  assert.equal(record.bytes, 2)
  assert.equal(record.kind, 'event')
  assert.equal(JSON.stringify(records).includes('private detail'), false)
  await access.stop()
})

test('when an application result exceeds the size limit, publication fails without disconnecting', async () => {
  const records = []
  const { createLogger } = await import('../src/log.js')
  const logger = createLogger({ salt: 'aa'.repeat(16), sink: (record) => records.push(record) })
  const { access, broker } = makeAccess({ logger })
  access.start()
  await access.waitForConnected(2000)
  const before = broker.messages.length
  try {
    await assert.rejects(access.publishResult(new Uint8Array(16 * 1024 * 1024 + 1)), (error) => error.code === 'payload_too_large')
    assert.equal(broker.messages.length, before)
    assert.equal(access.connected, true)
    const failure = records.find((record) => record.event === 'mqtt_payload_too_large')
    assert.equal(failure.limit, 16 * 1024 * 1024)
    assert.equal(failure.bytes, 16 * 1024 * 1024 + 1)
  } finally {
    await access.stop()
  }
})

test('missing broker PUBACK is bounded and interrupts only the MQTT link', async () => {
  const { access, accesses }=makeAccess({publishTimeoutMs:5})
  access.start();await access.waitForConnected(2000)
  accesses[0].publish=()=>{}
  await assert.rejects(access.publishResult(new Uint8Array([1])),error=>error.code==='unavailable')
  await waitFor(()=>!access.connected)
  await access.stop()
})

test('when a device_offline presence event arrives, Access dispatches it without treating it as a request', async () => {
  const { broker, access } = makeAccess()
  const seen = []
  access.start({ onPresence: (event) => seen.push(event) })
  await access.waitForConnected(2000)
  const publisher = broker.connectFn()('mqtts://access.test', { clientId: 'access' })
  await new Promise((resolve) => publisher.subscribe('whalerein/v1/h/host-1/presence', { qos: 1 }, resolve))
  await new Promise((resolve) => publisher.publish('whalerein/v1/h/host-1/presence', JSON.stringify({ event: 'device_offline', device_id: 'd1', reason: 'will' }), { qos: 1 }, resolve))
  await waitFor(() => seen.length === 1)
  assert.deepEqual(seen[0], { event: 'device_offline', device_id: 'd1', reason: 'will' })
  await access.stop()
})
