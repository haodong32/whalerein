import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { EventEmitter } from 'node:events'
import { randomBytes } from 'node:crypto'
import { MqttAccess, sanitizeReadyDisplayName, sanitizeReadyOsFamily } from '../src/access/mqtt.js'
import { ConfigError } from '../src/config.js'
import { codec } from '../src/protocol.js'
import { transportProfileBudget } from '../src/contracts/transport.js'
import { FakeBroker } from './support/fake-mqtt.js'
import { waitFor } from './support/harness.js'
import { ed25519 } from '@noble/curves/ed25519.js'
import { attachInput } from '../src/access/route.js'

const proto = codec()
const RESULT_TOPIC = 'whalerein/v1/h/host-1/result'
const EVENT_TOPIC = 'whalerein/v1/h/host-1/event'
const REQUEST_TOPIC = 'whalerein/v1/h/host-1/request'
const DEVICE_LIVE_TOPIC = 'whalerein/v1/h/host-1/d/d1/live'

function resultEnvelope({ body = new Uint8Array([1]), status = 'succeeded' } = {}) {
  return proto.encode('whalerein.v1.HostResult', {
    request_id: 'ready', host_id: 'host-1', device_id: 'd1', status,
    result_body: body, protocol_version: 1, relay_version: 5,
  })
}

function eventEnvelope({ ciphertext = new Uint8Array([1]) } = {}) {
  return proto.encode('whalerein.v1.EventEnvelope', {
    host_id: 'host-1', device_id: 'd1', channel_id: 'c1',
    nonce: randomBytes(12), ciphertext,
    created_at_unix_ms: 1, relay_version: 5,
  })
}

function advertisement(overrides = {}) {
  return proto.encode('whalerein.v1.ReceiveAdvertisement', {
    profile: 'TRANSPORT_PROFILE_DEFAULT',
    budget: transportProfileBudget('default'),
    connection_generation: 1,
    relay_version: 5,
    ...overrides,
  })
}

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

function hostReadyFrom(broker, index = -1) {
  const messages = broker.messages.filter((message) => message.topic === RESULT_TOPIC)
  const message = messages.at(index)
  assert.ok(message, 'a HostReady result was published')
  return proto.decode('whalerein.v1.HostReady', proto.decode('whalerein.v1.HostResult', message.payload).result_body)
}

function request(overrides = {}) {
  return proto.encode('whalerein.v1.HostRequest', {
    request_id: 'r1',
    host_id: 'host-1',
    device_id: 'd1',
    kind: 'REQUEST_KIND_QUERY',
    protocol_version: 1,
    relay_version: 5,
    body: new Uint8Array([1]),
    expires_at_unix_ms: Date.now() + 60_000,
    ...overrides,
  })
}

async function publishRequest(broker, bytes) {
  const publisher = broker.connectFn()('mqtts://access.test', { clientId: 'access' })
  await new Promise((resolve) => publisher.subscribe(REQUEST_TOPIC, { qos: 1 }, resolve))
  await new Promise((resolve) => publisher.publish(REQUEST_TOPIC, bytes, { qos: 1, properties: {messageExpiryInterval: 2} }, resolve))
}

test('only an mqtts URL on port 443 offers the MQTT ALPN protocol', () => {
  const shared = makeAccess({ url: 'mqtts://access.example:443' })
  shared.access.start()
  assert.deepEqual(shared.options[0].opts.ALPNProtocols, ['mqtt'])

  const plainTLS = makeAccess({ url: 'mqtts://broker.test:8883' })
  plainTLS.access.start()
  assert.equal(plainTLS.options[0].opts.ALPNProtocols, undefined)

  const implicit = makeAccess({ url: 'mqtts://broker.test' })
  implicit.access.start()
  assert.equal(implicit.options[0].opts.ALPNProtocols, undefined)

  const loopback = makeAccess({ url: 'mqtt://127.0.0.1:1883' })
  loopback.access.start()
  assert.equal(loopback.options[0].opts.ALPNProtocols, undefined)
})

test('a blocked MQTT port or ALPN negotiation fails closed without a plaintext fallback', async () => {
  for (const code of ['ECONNREFUSED', 'ERR_SSL_ALPN_NEGOTIATION_FAILED']) {
    const attempts = []
    const connect = (url, options) => {
      attempts.push({ url, options })
      const client = new EventEmitter()
      client.end = (force, opts, callback) => {
        const done = typeof force === 'function' ? force : callback
        queueMicrotask(() => { client.emit('close'); done?.() })
        return client
      }
      queueMicrotask(() => client.emit('error', Object.assign(new Error('private transport detail'), { code })))
      return client
    }
    const access = new MqttAccess({
      url: 'mqtts://access.example:443',
      hostId: 'host-1',
      token: 'secret-token',
      connect,
      reconnectMinMs: 60_000,
      reconnectMaxMs: 60_000,
    })
    access.start()
    await assert.rejects(access.waitForConnected(120), (error) => error.code === 'unavailable', code)
    assert.equal(access.describe().connected, false)
    assert.equal(access.describe().lastErrorCode, 'unavailable')
    assert.equal(attempts.length, 1, `${code} does not fall back to another transport`)
    assert.equal(attempts[0].url, 'mqtts://access.example:443')
    assert.deepEqual(attempts[0].options.ALPNProtocols, ['mqtt'])
    assert.equal(attempts[0].options.rejectUnauthorized, undefined)
    await access.stop()
  }
})

test('when MQTT connects, Access uses MQTT 5 identity, subscribes to its request topic, and sends an epoch-5 HostReady', async () => {
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
  assert.equal(options[0].opts.properties.maximumPacketSize, 64 * 1024)
  assert.equal(broker.subscriberCount('whalerein/v1/h/host-1/request'), 1)
  const ready = broker.messages.find((message) => message.topic === RESULT_TOPIC)
  const readyResult = proto.decode('whalerein.v1.HostResult', ready.payload)
  assert.equal(readyResult.status, 'succeeded')
  assert.equal(readyResult.relay_version, 5)
  const readyBody = proto.decode('whalerein.v1.HostReady', readyResult.result_body)
  assert.equal(readyBody.host_id, 'host-1')
  assert.equal(readyBody.relay_version, 5)
  assert.equal(readyBody.receive.relay_version, 5)
  assert.equal(readyBody.receive.budget.packet_bytes, 65536)
  assert.equal(readyBody.receive.budget.envelope_bytes, 16 * 1024 * 1024)

  await publishRequest(broker, request())
  await waitFor(() => calls.requests.length === 1)
  assert.equal(calls.requests[0].request_id, 'r1')
  assert.equal(calls.requests[0].relay_version, 5)
  assert.ok(calls.requests[0].replyDeadline <= Date.now() + 2000)

  await access.stop()
  assert.equal(access.connected, false)
  assert.equal(access.describe().connected, false)
})

test('HostReady carries bounded directory metadata on connect and reconnect', async () => {
  const { broker, access } = makeAccess({
    displayName: '  Hao\u0000dong\u009F MacBook  ',
    osFamily: 'macOS',
  })
  access.start({})
  await access.waitForConnected(2000)
  const first = hostReadyFrom(broker)
  assert.equal(first.host_id, 'host-1')
  assert.equal(first.display_name, 'Haodong MacBook')
  assert.equal(first.os_family, 'macOS')
  assert.equal(access.hostId, 'host-1', 'display metadata never feeds the Host id')
  assert.equal(access.requestTopic, REQUEST_TOPIC)
  assert.equal(access.resultTopic, RESULT_TOPIC)

  broker.disconnectClientId('whalerein-host-host-1')
  await waitFor(() => access.connected === false)
  await waitFor(() => access.connected === true, 5000)
  const second = hostReadyFrom(broker)
  assert.equal(second.host_id, 'host-1')
  assert.equal(second.display_name, 'Haodong MacBook')
  assert.equal(second.os_family, 'macOS')
  assert.ok(second.receive.connection_generation > first.receive.connection_generation)
  await access.stop()
})

test('HostReady omits empty or invalid directory metadata and bounds overlong values', async () => {
  const cases = [
    { label: 'empty', displayName: '', osFamily: '', display: undefined, os: undefined },
    { label: 'null', displayName: null, osFamily: null, display: undefined, os: undefined },
    { label: 'control-only', displayName: '\u0000\u001F\u007F\u009F  ', osFamily: 'lin\u0000ux', display: undefined, os: undefined },
    { label: 'overlong', displayName: 'x'.repeat(80), osFamily: 'a'.repeat(17), display: 'x'.repeat(64), os: undefined },
    { label: 'invalid os characters', displayName: 'Host', osFamily: 'linux/x86_64', display: 'Host', os: undefined },
    { label: 'valid bounded values', displayName: '  Host  ', osFamily: 'Windows 11', display: 'Host', os: 'Windows 11' },
  ]
  for (const entry of cases) {
    const { broker, access } = makeAccess({ displayName: entry.displayName, osFamily: entry.osFamily })
    access.start({})
    await access.waitForConnected(2000)
    const body = hostReadyFrom(broker)
    assert.equal(body.host_id, 'host-1', entry.label)
    assert.equal(body.display_name, entry.display, entry.label)
    assert.equal(body.os_family, entry.os, entry.label)
    await access.stop()
  }
})

test('directory metadata sanitizers stay bounded and drop C0/C1 controls', () => {
  assert.equal(sanitizeReadyDisplayName(undefined), '')
  assert.equal(sanitizeReadyDisplayName('\u0000\u001F\u007F\u009F'), '')
  assert.equal(sanitizeReadyDisplayName('  A\u0000B\u009FC  '), 'ABC')
  assert.equal(sanitizeReadyDisplayName('x'.repeat(80)).length, 64)
  assert.equal(sanitizeReadyOsFamily(undefined), '')
  assert.equal(sanitizeReadyOsFamily('  macOS  '), 'macOS')
  assert.equal(sanitizeReadyOsFamily('a'.repeat(17)), '')
  assert.equal(sanitizeReadyOsFamily('linux/x86_64'), '')
  assert.equal(sanitizeReadyOsFamily('Windows 11'), 'Windows 11')
})

test('a request with another relay epoch is rejected before business dispatch, never framed with WRMQTT2', async () => {
  const { broker, access } = makeAccess()
  const seen = []
  access.start({ onRequest: (request) => seen.push(request) })
  await access.waitForConnected(2000)
  await publishRequest(broker, request({ relay_version: 2 }))
  await publishRequest(broker, request({ relay_version: 5 }))
  await waitFor(() => seen.length === 1)
  assert.equal(seen[0].relay_version, 5)
  const ready = broker.messages.filter((message) => message.topic === RESULT_TOPIC).at(-1)
  assert.doesNotThrow(() => proto.decode('whalerein.v1.HostResult', ready.payload), 'a complete packet is never text-framed')
  await access.stop()
})

test('the Access receive advertisement is recorded for the Host -> Access hop and never answered as business', async () => {
  const { broker, access } = makeAccess()
  const seen = []
  access.start({ onRequest: (request) => seen.push(request) })
  await access.waitForConnected(2000)
  const advertised = transportProfileBudget('default')
  await publishRequest(broker, request({
    request_id: 'ready-advertisement',
    kind: 'REQUEST_KIND_ACCESS_READY',
    device_id: '',
    channel_id: '',
    body: advertisement({ connection_generation: 7, budget: { ...advertised, packet_bytes: 4096 } }),
  }))
  await waitFor(() => access.accessReceiveAdvertisement()?.connection_generation === 7)
  assert.equal(seen.length, 0, 'control information is never business')
  assert.equal(access.accessReceiveAdvertisement().budget.packet_bytes, 4096)
  assert.equal(access.sendPacketLimit(), 4096)
  // The same control notice with another epoch is ignored.
  await publishRequest(broker, request({
    request_id: 'ready-old', kind: 'REQUEST_KIND_ACCESS_READY', device_id: '', channel_id: '',
    body: advertisement({ connection_generation: 9, relay_version: 0 }),
  }))
  await new Promise((resolve) => setTimeout(resolve, 20))
  assert.equal(access.accessReceiveAdvertisement().connection_generation, 7)
  await access.stop()
})

test('a smaller CONNACK maximum lowers the effective packet limit instead of raising it', async () => {
  const broker = new FakeBroker()
  const connect = (url, opts) => {
    const client = broker.connect(url, opts)
    const emit = client.emit.bind(client)
    client.emit = (event, payload) => emit(event, event === 'connect'
      ? { ...payload, properties: { maximumPacketSize: 4096 } }
      : payload)
    return client
  }
  const access = new MqttAccess({
    url: 'mqtts://broker.test:8883', hostId: 'host-1', token: 't', connect,
    reconnectMinMs: 50, reconnectMaxMs: 100,
  })
  access.start()
  await access.waitForConnected(2000)
  assert.equal(access.sendPacketLimit(), 4096)
  await access.stop()
})

test('when MQTT result/event publication succeeds or fails, Access reports the outcome', async () => {
  const { broker, access } = makeAccess()
  await assert.rejects(access.publishResult(resultEnvelope()), (error) => error.code === 'unavailable')
  await assert.rejects(access.publishEvent(eventEnvelope()), (error) => error.code === 'unavailable')
  assert.equal(await access.publishLive(eventEnvelope(), { deviceId: 'd1' }), false, 'a disconnected live publish is a drop')
  const seen = []
  const subscriber = broker.connectFn()('mqtts://access.test', { clientId: 'access' })
  await new Promise((resolve) => subscriber.subscribe(RESULT_TOPIC, { qos: 1 }, resolve))
  await new Promise((resolve) => subscriber.subscribe(EVENT_TOPIC, { qos: 1 }, resolve))
  subscriber.on('message', (topic, payload) => seen.push({ topic, payload: Buffer.from(payload) }))
  access.start({})
  await access.waitForConnected(2000)
  await access.publishResult(resultEnvelope({ body: new Uint8Array([2]) }))
  await access.publishEvent(eventEnvelope({ ciphertext: new Uint8Array([3]) }))
  await waitFor(() => seen.length >= 3)
  const resultMessage = seen.filter((entry) => entry.topic === RESULT_TOPIC).at(-1)
  assert.equal(proto.decode('whalerein.v1.HostResult', resultMessage.payload).result_body[0], 2)
  assert.equal(proto.decode('whalerein.v1.EventEnvelope', seen.find((entry) => entry.topic === EVENT_TOPIC).payload).ciphertext[0], 3)
  broker.online = false
  await assert.rejects(access.publishResult(resultEnvelope({ body: new Uint8Array([4]) })), (error) => error.code === 'unavailable')
  assert.equal(access.describe().publishErrors >= 1, true)
  await access.stop()
})

test('an event above the Access packet advertisement is published as a ready reference', async () => {
  const calls = []
  const relayClient = {
    async reserve({ ciphertext, expiresAt }) {
      calls.push('reserve')
      return {
        object_id: 'a'.repeat(32), owner_node_id: 'node-1',
        upload_path: `/v1/relay/objects/${'a'.repeat(32)}`,
        max_bytes: ciphertext.length, expires_at_unix_ms: expiresAt, upload_timeout_ms: 1000,
      }
    },
    async upload() { calls.push('upload') },
    async finish({ objectId }) { calls.push('finish'); return { object_id: objectId } },
    async delete() { calls.push('delete') },
    async fetch() { throw new Error('not used') },
  }
  const { broker, access } = makeAccess({ relayClient })
  const seen = []
  const subscriber = broker.connectFn()('mqtts://access.test', { clientId: 'access' })
  await new Promise((resolve) => subscriber.subscribe(EVENT_TOPIC, { qos: 1 }, resolve))
  subscriber.on('message', (topic, payload) => seen.push(Buffer.from(payload)))
  access.start({})
  await access.waitForConnected(2000)
  await publishRequest(broker, request({
    request_id: 'advertisement-small', kind: 'REQUEST_KIND_ACCESS_READY', device_id: '', channel_id: '',
    body: advertisement({ connection_generation: 4, budget: { ...transportProfileBudget('default'), packet_bytes: 400 } }),
  }))
  await waitFor(() => access.accessReceiveAdvertisement() !== null)
  await access.publishEvent(eventEnvelope({ ciphertext: new Uint8Array(1024) }))
  await waitFor(() => seen.length === 1)
  const envelope = proto.decode('whalerein.v1.EventEnvelope', seen[0])
  assert.equal((envelope.ciphertext ?? new Uint8Array(0)).length, 0)
  assert.equal(envelope.payload_reference.ciphertext_bytes, 1024)
  assert.equal(envelope.relay_version, 5)
  assert.deepEqual(calls, ['reserve', 'upload', 'finish'])
  await access.stop()
})

test('when the broker disconnects, MQTT Access reconnects with backoff and a fresh generation', async () => {
  const { broker, access } = makeAccess()
  let connections = 0
  access.start({ onConnected: () => connections++ })
  await access.waitForConnected(2000)
  const first = access.receiveAdvertisement().connection_generation
  broker.disconnectClientId('whalerein-host-host-1')
  await waitFor(() => access.connected === false)
  await waitFor(() => access.connected === true, 5000)
  assert.equal(connections, 2)
  assert.equal(access.describe().reconnects, 1)
  assert.ok(access.receiveAdvertisement().connection_generation > first)
  assert.equal(access.accessReceiveAdvertisement(), null, 'a reconnect drops the old hop advertisement')
  const ready = broker.messages.filter((message) => message.topic === RESULT_TOPIC).at(-1)
  const body = proto.decode('whalerein.v1.HostReady', proto.decode('whalerein.v1.HostResult', ready.payload).result_body)
  assert.equal(body.receive.connection_generation, access.receiveAdvertisement().connection_generation)
  await access.stop()
})

test('a trust failure after a successful connection retries instead of ending the MQTT lane', async () => {
  const broker = new FakeBroker()
  let failTrust = false
  let attempts = 0
  const connect = (url, options) => {
    attempts++
    const client = broker.connect(url, options)
    if (failTrust) {
      // Model a broker rotating its leaf: the first TLS handshake of the
      // attempt fails closed, then a later attempt succeeds.
      client.closed = true
      queueMicrotask(() => {
        client.emit('error', Object.assign(new Error('unable to verify leaf'), { code: 'UNABLE_TO_VERIFY_LEAF_SIGNATURE' }))
        client.emit('close')
      })
    }
    return client
  }
  let failures = 0
  const access = new MqttAccess({
    url: 'mqtts://broker.test:8883', hostId: 'host-1', token: 'secret-token', connect,
    reconnectMinMs: 20, reconnectMaxMs: 40,
  })
  access.start({ onFailure: () => failures++ })
  await access.waitForConnected(2000)
  failTrust = true
  broker.disconnectClientId('whalerein-host-host-1')
  await waitFor(() => attempts >= 3, 3000)
  failTrust = false
  await waitFor(() => access.connected === true, 5000)
  assert.equal(failures, 0, 'a post-connection trust failure is retried, not terminal')
  assert.ok(access.describe().reconnects >= 1)
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
  client.emit('message', REQUEST_TOPIC, Buffer.from([255, 255]))
  client.emit('message', REQUEST_TOPIC, Buffer.from('WRMQTT2legacy'))
  await access.stop()
  assert.equal(seen.length, 0)
  assert.equal(broker.messages.length >= 1, true)
})

test('when MQTT has never connected, waiting for readiness reaches its deadline with unavailable', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] })
  const { access } = makeAccess()
  try {
    const expired = assert.rejects(access.waitForConnected(20), error => error.code === 'unavailable')
    t.mock.timers.tick(20)
    await expired
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
  await access.publishEvent(eventEnvelope({ ciphertext: new Uint8Array(321) }))
  accesses[0].emit('disconnect', { reasonCode: 149, properties: { reasonString: 'private-detail' } })
  accesses[0].emit('close')
  const disconnected = records.find((record) => record.event === 'mqtt_disconnected')
  assert.equal(disconnected.reasonCode, 149)
  assert.equal(disconnected.reason, 'broker_disconnect')
  assert.equal(disconnected.kind, 'event')
  assert.equal(disconnected.generation, 1)
  assert.equal(JSON.stringify(records).includes('private-detail'), false)
  await access.stop()
})

test('when concurrent publishes fail, each diagnostic reports its own size and kind', async () => {
  const records = []
  const { createLogger } = await import('../src/log.js')
  const logger = createLogger({ salt: 'aa'.repeat(16), level: 'debug', sink: (record) => records.push(record) })
  const { access, accesses } = makeAccess({ logger })
  access.start({})
  await access.waitForConnected(2000)
  const callbacks = []
  accesses[0].publish = (_topic, _payload, _options, callback) => callbacks.push(callback)
  const failed = assert.rejects(access.publishEvent(eventEnvelope()), (error) => error.code === 'unavailable')
  const succeeded = access.publishResult(resultEnvelope())
  await new Promise((resolve) => setImmediate(resolve))
  callbacks[0](new Error('private detail'))
  callbacks[1](null)
  await Promise.all([failed, succeeded])
  const record = records.find((entry) => entry.event === 'mqtt_publish_failed')
  assert.equal(record.kind, 'event')
  assert.equal(JSON.stringify(records).includes('private detail'), false)
  await access.stop()
})

test('when an application message exceeds the envelope budget, publication fails without disconnecting', async () => {
  const records = []
  const { createLogger } = await import('../src/log.js')
  const logger = createLogger({ salt: 'aa'.repeat(16), level: 'debug', sink: (record) => records.push(record) })
  const budget = {
    packet_bytes: 1024,
    envelope_bytes: 2048,
    bulk_concurrency: 4,
    reserved_bytes: 32 * 1024,
    transfer_timeout_ms: 120_000,
  }
  const { access, broker } = makeAccess({ logger, transportProfile: budget })
  access.start()
  await access.waitForConnected(2000)
  const before = broker.messages.length
  try {
    await assert.rejects(
      access.publishResult(resultEnvelope({ body: new Uint8Array(4096) })),
      (error) => error.code === 'payload_too_large',
    )
    assert.equal(broker.messages.length, before)
    assert.equal(access.connected, true)
  } finally {
    await access.stop()
  }
})

test('missing broker PUBACK is bounded and interrupts only the MQTT link', async () => {
  const { access, accesses } = makeAccess({ publishTimeoutMs: 5 })
  access.start()
  await access.waitForConnected(2000)
  accesses[0].publish = () => {}
  try {
    await assert.rejects(access.publishResult(resultEnvelope()), error => error.code === 'unavailable')
    assert.equal(access.connected, false)
  } finally {
    await access.stop()
  }
})

test('when a connection_closed presence event arrives, Access dispatches it without treating it as a request', async () => {
  const { broker, access } = makeAccess()
  const seen = []
  access.start({ onPresence: (event) => seen.push(event) })
  await access.waitForConnected(2000)
  const publisher = broker.connectFn()('mqtts://access.test', { clientId: 'access' })
  await new Promise((resolve) => publisher.subscribe('whalerein/v1/h/host-1/presence', { qos: 1 }, resolve))
  await new Promise((resolve) => publisher.publish('whalerein/v1/h/host-1/presence', JSON.stringify({ event: 'connection_closed', device_id: 'd1', connection_ref: '0123456789abcdef:7', reason: 'broker' }), { qos: 1 }, resolve))
  await waitFor(() => seen.length === 1)
  assert.deepEqual(seen[0], { event: 'connection_closed', device_id: 'd1', connection_ref: '0123456789abcdef:7', reason: 'broker' })
  await access.stop()
})

test('a configured Access origin and signing key construct the Host-authority relay client', async () => {
  const { generateKeypair } = await import('../src/security/noise.js')
  const identity = generateKeypair()
  const withRelay = makeAccess({ accessOrigin: 'https://access.example', authorizationPrivateKey: identity.privateKey })
  assert.equal(withRelay.access.sendPacketLimit(), 64 * 1024)
  // An origin without a signing key cannot use Host authority, so no client is
  // constructed and oversized messages fail explicitly instead.
  assert.doesNotThrow(() => makeAccess({ accessOrigin: 'https://access.example' }))
})

test('the live connection disables Nagle so small previews do not wait a round trip', async () => {
  const { access, accesses } = makeAccess()
  await access.start()
  await access.waitForConnected(1000)
  assert.equal(accesses[0].stream.noDelay, true)
  await access.stop()
})

test('a receive budget that cannot fit required control envelopes fails configuration before readiness', () => {
  const budget = {
    packet_bytes: 40,
    envelope_bytes: 40,
    bulk_concurrency: 4,
    reserved_bytes: 32 * 1024,
    transfer_timeout_ms: 120_000,
  }
  assert.throws(() => makeAccess({ transportProfile: budget }), (error) => error.code === 'invalid_config')
})

test('topic accessors expose the configured Host topics', () => {
  const { access } = makeAccess()
  assert.equal(access.requestTopic, REQUEST_TOPIC)
  assert.equal(access.resultTopic, RESULT_TOPIC)
  assert.equal(access.eventTopic, EVENT_TOPIC)
  assert.equal(access.deviceLiveTopic('d1'), DEVICE_LIVE_TOPIC)
  assert.equal(access.liveTopic, undefined, 'the shared Host live path is retired')
})

test('an unparseable URL avoids ALPN and a throwing connector keeps retrying without a crash', async () => {
  const parsed = makeAccess({ url: 'not a url' })
  parsed.access.start()
  await parsed.access.waitForConnected(2000)
  assert.equal(parsed.options[0].opts.ALPNProtocols, undefined)
  await parsed.access.stop()

  let attempts = 0
  const access = new MqttAccess({
    url: 'mqtts://broker.test:8883', hostId: 'host-1', token: 't',
    connect: () => { attempts++; throw new Error('secret detail') },
    reconnectMinMs: 10, reconnectMaxMs: 20,
  })
  access.start()
  await assert.rejects(access.waitForConnected(80), (error) => error.code === 'unavailable')
  assert.ok(attempts >= 1)
  assert.equal(JSON.stringify(access.describe()).includes('secret detail'), false)
  await access.stop()
})

test('a rejected subscription or a failed HostReady closes the link and schedules a bounded retry', async () => {
  const rejected = makeAccess({ reconnectMinMs: 60_000, reconnectMaxMs: 60_000 })
  rejected.access.start()
  rejected.accesses[0].subscribe = (_topic, _options, callback) => {
    queueMicrotask(() => callback(new Error('private subscription detail')))
    return rejected.accesses[0]
  }
  await new Promise((resolve) => setTimeout(resolve, 30))
  assert.equal(rejected.access.connected, false)
  await rejected.access.stop()

  const failedReady = makeAccess({ reconnectMinMs: 60_000, reconnectMaxMs: 60_000 })
  failedReady.access.start()
  failedReady.accesses[0].publish = (_topic, _payload, _options, callback) => {
    queueMicrotask(() => callback(new Error('private ready detail')))
  }
  await new Promise((resolve) => setTimeout(resolve, 30))
  assert.equal(failedReady.access.connected, false)
  await failedReady.access.stop()
})

test('a definitive TLS trust failure stops retrying and reports the actionable code', async () => {
  const attempts = []
  const connect = () => {
    attempts.push(1)
    const client = new EventEmitter()
    client.end = (force, opts, callback) => {
      const done = typeof force === 'function' ? force : callback
      queueMicrotask(() => { client.emit('close'); done?.() })
      return client
    }
    queueMicrotask(() => client.emit('error', Object.assign(new Error('private detail'), { code: 'ERR_TLS_CERT_ALTNAME_INVALID' })))
    return client
  }
  const access = new MqttAccess({
    url: 'mqtts://access.example:443', hostId: 'host-1', token: 'secret-token', connect,
    reconnectMinMs: 60_000, reconnectMaxMs: 60_000,
  })
  let failure = ''
  access.start({ onFailure: (code) => { failure = code } })
  await assert.rejects(access.waitForConnected(120), (error) => error.code === 'tls_trust')
  assert.equal(access.describe().lastErrorCode, 'tls_trust')
  assert.equal(failure, 'tls_trust')
  await new Promise((resolve) => setTimeout(resolve, 50))
  assert.equal(attempts.length, 1, 'a TLS trust failure does not keep retrying')
  await access.stop()
})

test('a definitive broker credential refusal stops retrying with unauthorized', async () => {
  const attempts = []
  const connect = () => {
    attempts.push(1)
    const client = new EventEmitter()
    client.end = (force, opts, callback) => {
      const done = typeof force === 'function' ? force : callback
      queueMicrotask(() => { client.emit('close'); done?.() })
      return client
    }
    queueMicrotask(() => {
      client.emit('disconnect', { reasonCode: 5 })
      client.emit('close')
    })
    return client
  }
  const access = new MqttAccess({
    url: 'mqtts://access.example:443', hostId: 'host-1', token: 'refused-token', connect,
    reconnectMinMs: 60_000, reconnectMaxMs: 60_000,
  })
  access.start()
  await assert.rejects(access.waitForConnected(150), (error) => error.code === 'unauthorized')
  assert.equal(access.describe().lastErrorCode, 'unauthorized')
  assert.equal(access.describe().lastErrorCode, 'unauthorized')
  await new Promise((resolve) => setTimeout(resolve, 50))
  assert.equal(attempts.length, 1, 'a refused credential does not retry forever')
  await access.stop()
})

test('a live preview publishes only on the live topic at QoS 0 and never on the control event topic', async () => {
  const { broker, access } = makeAccess()
  const seen = []
  const subscriber = broker.connectFn()('mqtts://access.test', { clientId: 'access' })
  await new Promise((resolve) => subscriber.subscribe(DEVICE_LIVE_TOPIC, { qos: 0 }, resolve))
  subscriber.on('message', (topic, payload, packet) => seen.push({ topic, payload: Buffer.from(payload), qos: packet?.qos }))
  access.start({})
  await access.waitForConnected(2000)
  const envelope = eventEnvelope({ ciphertext: new Uint8Array(512) })
  assert.equal(await access.publishLive(envelope, { deviceId: 'd1' }), true)
  await waitFor(() => seen.length === 1)
  assert.equal(seen[0].topic, DEVICE_LIVE_TOPIC)
  assert.equal(seen[0].qos, 0)
  const published = broker.messages.filter((message) => message.topic === DEVICE_LIVE_TOPIC)
  assert.equal(published.length, 1)
  assert.equal(published[0].qos, 0, 'live is one QoS 0 PUBLISH')
  assert.equal(broker.messages.some((message) => message.topic === EVENT_TOPIC), false, 'no preview on the control topic')
  assert.deepEqual(
    Buffer.from(proto.decode('whalerein.v1.EventEnvelope', published[0].payload).ciphertext),
    Buffer.from(proto.decode('whalerein.v1.EventEnvelope', envelope).ciphertext),
    'the complete EventEnvelope is published unchanged',
  )
  await access.stop()
})

test('a live preview resolves when the client accepts the packet, without a PUBACK timer', async () => {
  const { access, accesses } = makeAccess({ publishTimeoutMs: 5 })
  access.start()
  await access.waitForConnected(2000)
  let qos = null
  accesses[0].publish = (_topic, _payload, options, callback) => {
    qos = options?.qos
    // QoS 0 has no acknowledgement; the client reports acceptance after 20 ms,
    // which is longer than the QoS 1 acknowledgement timeout configured above.
    setTimeout(() => callback?.(null), 20)
  }
  assert.equal(await access.publishLive(eventEnvelope(), { deviceId: 'd1' }), true)
  assert.equal(qos, 0)
  assert.equal(access.connected, true, 'a slow live acceptance never interrupts the link')
  await access.stop()
})

test('a live preview that cannot fit is dropped instead of becoming a reference or control message', async () => {
  const budget = {
    packet_bytes: 1024,
    envelope_bytes: 64 * 1024,
    bulk_concurrency: 4,
    reserved_bytes: 128 * 1024,
    transfer_timeout_ms: 120_000,
  }
  const calls = []
  const relayClient = {
    async reserve() { calls.push('reserve') },
    async upload() { calls.push('upload') },
    async finish() { calls.push('finish') },
    async delete() { calls.push('delete') },
    async fetch() { throw new Error('not used') },
  }
  const { broker, access } = makeAccess({ transportProfile: budget, relayClient })
  access.start()
  await access.waitForConnected(2000)
  const before = broker.messages.length
  const oversized = eventEnvelope({ ciphertext: new Uint8Array(4096) })
  assert.equal(await access.publishLive(oversized, { deviceId: 'd1' }), false)
  const reference = {
    object_id: 'a'.repeat(32), owner_node_id: 'node-1', ciphertext_bytes: 100,
    sha256: new Uint8Array(32), expires_at_unix_ms: Date.now() + 60_000,
    purpose: 'PAYLOAD_PURPOSE_EVENT',
  }
  const referenced = proto.encode('whalerein.v1.EventEnvelope', {
    host_id: 'host-1', device_id: 'd1', channel_id: 'c1', nonce: new Uint8Array(12),
    payload_reference: reference, created_at_unix_ms: 1, relay_version: 5,
  })
  assert.equal(await access.publishLive(referenced, { deviceId: 'd1' }), false, 'live never uses the reference carrier')
  const old = proto.encode('whalerein.v1.EventEnvelope', {
    host_id: 'host-1', device_id: 'd1', channel_id: 'c1', nonce: new Uint8Array(12),
    ciphertext: new Uint8Array([1]), created_at_unix_ms: 1, relay_version: 3,
  })
  assert.equal(await access.publishLive(old, { deviceId: 'd1' }), false, 'epoch 3 is never accepted')
  assert.equal(broker.messages.length, before, 'a dropped preview publishes nothing')
  assert.deepEqual(calls, [], 'a dropped live preview never stages an HTTPS object')
  await access.stop()
})

test('a deadline-bound reply carries Correlation Data and the remaining Message Expiry with no Response Topic', async () => {
  const clock = { now: 1_700_000_000_000 }
  const { broker, access } = makeAccess({ now: () => clock.now })
  access.start()
  await access.waitForConnected(2000)
  await access.publishResult(resultEnvelope(), { requestId: 'request-42', expiresAt: clock.now + 1500 })
  const published = broker.messages.filter((message) => message.topic === RESULT_TOPIC).at(-1)
  assert.equal(String(published.properties.correlationData), 'request-42')
  assert.equal(published.properties.messageExpiryInterval, 2, 'a positive ceiling of the remaining deadline')
  assert.equal(published.properties.responseTopic, undefined, 'a reply never carries a Response Topic')

  const before = broker.messages.length
  clock.now += 3000
  await access.publishResult(resultEnvelope(), { requestId: 'request-elapsed', expiresAt: clock.now - 1 })
  assert.equal(broker.messages.length, before, 'an elapsed reply is never published')
  await access.stop()
})

test('a reply expiry is capped by a shorter upstream MQTT expiry', async () => {
  const clock = { now: 1_700_000_000_000 }
  const { broker, access } = makeAccess({ now: () => clock.now })
  access.start()
  await access.waitForConnected(2000)
  await access.publishResult(resultEnvelope(), {
    requestId: 'request-capped', expiresAt: clock.now + 9000, upstreamExpirySeconds: 3,
  })
  const published = broker.messages.filter((message) => message.topic === RESULT_TOPIC).at(-1)
  assert.equal(published.properties.messageExpiryInterval, 3)
  assert.equal(published.properties.correlationData.toString('utf8'), 'request-capped')
  await access.stop()
})

test('a route-enabled HostReady carries a proof for the serving node', async () => {
  const privateKey = new Uint8Array(32).fill(5)
  const publicKey = ed25519.getPublicKey(privateKey)
  const noiseKey = new Uint8Array(32).fill(6)
  const { broker, access } = makeAccess({
    displayName: 'Route Host',
    osFamily: 'Linux',
    authorizationPublicKey: publicKey,
    authorizationPrivateKey: privateKey,
    routeNodeId: 'node-a',
    routeIncarnation: 'inc-1',
    routeService: 'official',
    routeEnvironment: 'staging',
    routeGeneration: 4,
    routeNoisePublicKey: noiseKey,
  })
  access.start({})
  await access.waitForConnected(2000)
  const ready = broker.messages.find((message) => message.topic === RESULT_TOPIC)
  const body = proto.decode('whalerein.v1.HostReady', proto.decode('whalerein.v1.HostResult', ready.payload).result_body)
  assert.equal(body.display_name, 'Route Host')
  assert.equal(body.os_family, 'Linux')
  assert.equal(body.route_generation, 4)
  assert.equal(body.route_service, 'official')
  assert.equal(body.route_environment, 'staging')
  assert.equal(body.route_challenge.length, 32)
  assert.equal(body.route_signing_public_key.length, 32)
  assert.equal(body.route_noise_public_key.length, 32)
  const expected = attachInput({
    service: 'official', environment: 'staging', hostId: 'host-1', nodeId: 'node-a',
    incarnation: 'inc-1', challenge: body.route_challenge, generation: 4,
    noiseKey, signedAtUnixMs: body.route_signed_at_unix_ms,
  })
  assert.equal(ed25519.verify(body.route_signature, new TextEncoder().encode(expected), publicKey), true)
  await access.stop()
  const closing = broker.messages.filter((message) => message.topic === RESULT_TOPIC).at(-1)
  const closingBody = proto.decode('whalerein.v1.HostReady', proto.decode('whalerein.v1.HostResult', closing.payload).result_body)
  assert.equal(closingBody.route_closing, true)
  assert.equal(closingBody.route_generation, 4)
})
