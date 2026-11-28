// The private self-status service lane on the Host MQTT transport.
//
// Access publishes one bounded SelfStatusRequest frame on
// h/{host}/svc/self-status/request and reads one HostResult from
// h/{host}/svc/self-status/reply. The Host must subscribe to that request in
// the same QoS 1 subscription as its ordinary request/presence topics, decode
// it through the complete-message transport, dispatch it separately from the
// ordinary request router, and reply inline with a short expiry.

import test from 'node:test'
import assert from 'node:assert/strict'
import { MqttAccess } from '../src/access/mqtt.js'
import { codec } from '../src/protocol.js'
import { FakeBroker } from './support/fake-mqtt.js'
import { waitFor } from './support/harness.js'

const proto = codec()
const HOST_ID = 'host-1'
const REQUEST_TOPIC = `whalerein/v1/h/${HOST_ID}/request`
const RESULT_TOPIC = `whalerein/v1/h/${HOST_ID}/result`
const PRESENCE_TOPIC = `whalerein/v1/h/${HOST_ID}/presence`
const SELF_STATUS_REQUEST_TOPIC = `whalerein/v1/h/${HOST_ID}/svc/self-status/request`
const SELF_STATUS_REPLY_TOPIC = `whalerein/v1/h/${HOST_ID}/svc/self-status/reply`

const CHALLENGE = Buffer.from('0123456789abcdef', 'utf8')

function makeAccess(overrides = {}) {
  const broker = overrides.broker ?? new FakeBroker()
  const accesses = []
  const subscribes = []
  const connect = (url, opts) => {
    const client = broker.connectFn()(url, opts)
    const original = client.subscribe.bind(client)
    client.subscribe = (topics, options, callback) => {
      subscribes.push({ topics, options })
      return original(topics, options, callback)
    }
    accesses.push(client)
    return client
  }
  const access = new MqttAccess({
    url: 'mqtts://broker.test:8883',
    hostId: HOST_ID,
    token: 'secret-token',
    connect,
    reconnectMinMs: 50,
    reconnectMaxMs: 100,
    ...overrides,
  })
  return { broker, access, accesses, subscribes }
}

function selfStatusRequest({ grantId = 'grant-1', challenge = CHALLENGE, requestId = 's1', overrides = {} } = {}) {
  return proto.encode('whalerein.v1.HostRequest', {
    request_id: requestId,
    host_id: HOST_ID,
    kind: 'REQUEST_KIND_SELF_STATUS',
    protocol_version: 1,
    relay_version: 5,
    expires_at_unix_ms: Date.now() + 5_000,
    body: proto.encode('whalerein.v1.SelfStatusRequest', { grant_id: grantId, challenge }),
    ...overrides,
  })
}

function selfStatusReply({ requestId = 's1', grantId = 'grant-1', active = true } = {}) {
  return proto.encode('whalerein.v1.HostResult', {
    request_id: requestId,
    host_id: HOST_ID,
    status: 'succeeded',
    result_body: proto.encode('whalerein.v1.SelfStatus', {
      grant_id: grantId,
      challenge: CHALLENGE,
      active,
    }),
    protocol_version: 1,
    relay_version: 5,
  })
}

async function publisherFor(broker) {
  const publisher = broker.connectFn()('mqtts://access.test', { clientId: 'access' })
  await new Promise((resolve) => publisher.once('connect', resolve))
  return publisher
}

async function publishTo(broker, topic, payload, properties = { messageExpiryInterval: 5 }) {
  const publisher = await publisherFor(broker)
  await new Promise((resolve, reject) => {
    publisher.publish(topic, payload, { qos: 1, properties }, (error) => (error ? reject(error) : resolve()))
  })
}

async function subscribeTo(broker, topic, sink) {
  const subscriber = await publisherFor(broker)
  await new Promise((resolve) => subscriber.subscribe(topic, { qos: 1 }, resolve))
  subscriber.on('message', (received, payload, packet) => {
    sink({ topic: received, payload: Buffer.from(payload), qos: packet?.qos, properties: packet?.properties })
  })
  return subscriber
}

test('the Host subscribes to request, self-status request and presence in one QoS 1 subscription', async () => {
  const { access, broker, subscribes } = makeAccess()
  access.start()
  try {
    await access.waitForConnected(2000)
    assert.equal(broker.subscriberCount(REQUEST_TOPIC), 1)
    assert.equal(broker.subscriberCount(SELF_STATUS_REQUEST_TOPIC), 1)
    assert.equal(broker.subscriberCount(PRESENCE_TOPIC), 1)
    assert.equal(subscribes.length, 1, 'one subscription carries every Host topic')
    assert.deepEqual(subscribes[0].topics, [REQUEST_TOPIC, SELF_STATUS_REQUEST_TOPIC, PRESENCE_TOPIC])
    assert.equal(subscribes[0].options.qos, 1)
  } finally {
    await access.stop()
  }
})

test('a self-status request is decoded by the transport and dispatched on the self-status handler only', async () => {
  const { access, broker } = makeAccess()
  const requests = []
  const ordinary = []
  access.start({
    onSelfStatus: (request) => requests.push(request),
    onRequest: (request) => ordinary.push(request),
  })
  try {
    await access.waitForConnected(2000)
    await publishTo(broker, SELF_STATUS_REQUEST_TOPIC, selfStatusRequest())
    await waitFor(() => requests.length === 1)
    const request = requests[0]
    assert.equal(request.kind, 'REQUEST_KIND_SELF_STATUS')
    assert.equal(request.request_id, 's1')
    assert.equal(request.host_id, HOST_ID)
    const frame = proto.decode('whalerein.v1.SelfStatusRequest', request.body)
    assert.equal(frame.grant_id, 'grant-1')
    assert.deepEqual(Buffer.from(frame.challenge), CHALLENGE)
    assert.equal(ordinary.length, 0, 'the ordinary request router never sees this lane')
  } finally {
    await access.stop()
  }
})

test('a malformed self-status frame is contained and the lane keeps working', async () => {
  const records = []
  const { createLogger } = await import('../src/log.js')
  const logger = createLogger({ salt: 'aa'.repeat(16), level: 'debug', sink: (record) => records.push(record) })
  const { access, broker } = makeAccess({ logger })
  const requests = []
  access.start({ onSelfStatus: (request) => requests.push(request) })
  try {
    await access.waitForConnected(2000)
    await publishTo(broker, SELF_STATUS_REQUEST_TOPIC, Buffer.from('not a HostRequest'))
    await waitFor(() => records.some((record) => record.event === 'transport_receive_failed'))
    assert.equal(requests.length, 0)
    await publishTo(broker, SELF_STATUS_REQUEST_TOPIC, selfStatusRequest())
    await waitFor(() => requests.length === 1)
    assert.equal(JSON.stringify(records).includes('grant-1'), false)
    assert.equal(JSON.stringify(records).includes('0123456789abcdef'), false)
  } finally {
    await access.stop()
  }
})

test('a throwing self-status handler is contained and the next request is still dispatched', async () => {
  const records = []
  const { createLogger } = await import('../src/log.js')
  const logger = createLogger({ salt: 'aa'.repeat(16), level: 'debug', sink: (record) => records.push(record) })
  const { access, broker } = makeAccess({ logger })
  let calls = 0
  access.start({
    onSelfStatus: () => {
      calls += 1
      if (calls === 1) throw new Error('private handler detail')
    },
  })
  try {
    await access.waitForConnected(2000)
    await publishTo(broker, SELF_STATUS_REQUEST_TOPIC, selfStatusRequest({ requestId: 's1' }))
    await waitFor(() => records.some((record) => record.event === 'self_status_handler_failed'))
    await publishTo(broker, SELF_STATUS_REQUEST_TOPIC, selfStatusRequest({ requestId: 's2' }))
    await waitFor(() => calls === 2)
    const failed = records.find((record) => record.event === 'self_status_handler_failed')
    assert.equal(failed.code, 'internal')
    assert.equal(JSON.stringify(records).includes('private handler detail'), false)
    assert.equal(access.connected, true)
  } finally {
    await access.stop()
  }
})

test('a self-status reply is one inline QoS 1 HostResult on the service reply topic with a short expiry', async () => {
  const { access, broker } = makeAccess()
  const seen = []
  access.start()
  try {
    await access.waitForConnected(2000)
    const resultLaneBefore = broker.messages.filter((message) => message.topic === RESULT_TOPIC).length
    await subscribeTo(broker, SELF_STATUS_REPLY_TOPIC, (message) => seen.push(message))
    const payload = selfStatusReply()
    await access.publishSelfStatusReply(payload)
    await waitFor(() => seen.length === 1)
    assert.equal(seen[0].topic, SELF_STATUS_REPLY_TOPIC)
    assert.equal(seen[0].qos, 1)
    const expiry = seen[0].properties?.messageExpiryInterval
    assert.equal(Number.isSafeInteger(expiry), true)
    assert.equal(expiry > 0 && expiry <= 5, true, 'the reply cannot outlive the bounded exchange')
    assert.equal(seen[0].properties?.responseTopic, undefined)
    const result = proto.decode('whalerein.v1.HostResult', seen[0].payload)
    assert.equal(result.request_id, 's1')
    assert.equal(result.host_id, HOST_ID)
    assert.equal(result.status, 'succeeded')
    assert.equal(result.relay_version, 5)
    const status = proto.decode('whalerein.v1.SelfStatus', result.result_body)
    assert.equal(status.grant_id, 'grant-1')
    assert.deepEqual(Buffer.from(status.challenge), CHALLENGE)
    assert.equal(status.active, true)
    assert.equal(broker.messages.filter((message) => message.topic === RESULT_TOPIC).length, resultLaneBefore, 'the reply never rides the ordinary result lane')
  } finally {
    await access.stop()
  }
})

test('an empty or oversized self-status reply is refused without publishing', async () => {
  const budget = {
    packet_bytes: 1024,
    envelope_bytes: 2048,
    bulk_concurrency: 4,
    reserved_bytes: 32 * 1024,
    transfer_timeout_ms: 120_000,
  }
  const { access, broker } = makeAccess({ transportProfile: budget })
  access.start()
  try {
    await access.waitForConnected(2000)
    await assert.rejects(access.publishSelfStatusReply(new Uint8Array(0)), (error) => error.code === 'invalid_argument')
    await assert.rejects(access.publishSelfStatusReply(new Uint8Array([1, 2, 3]).buffer), (error) => error.code === 'invalid_argument')
    const before = broker.messages.length
    await assert.rejects(
      access.publishSelfStatusReply(new Uint8Array(4096)),
      (error) => error.code === 'payload_too_large',
    )
    assert.equal(broker.messages.length, before)
    assert.equal(access.connected, true)
  } finally {
    await access.stop()
  }
})

test('a self-status reply before the link is ready is unavailable, never staged', async () => {
  const { access } = makeAccess()
  await assert.rejects(
    access.publishSelfStatusReply(selfStatusReply()),
    (error) => error.code === 'unavailable',
  )
})