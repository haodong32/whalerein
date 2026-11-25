// One epoch-5 transport probe per AC subcase. Each invocation prints one JSON
// fact object; the Python case asserts the named identity. Only counts, sizes
// and stable codes are printed, never ciphertext or message content.

import assert from 'node:assert/strict'
import { randomBytes, randomUUID } from 'node:crypto'
import { createRequire } from 'node:module'
import { MqttAccess } from '../../../../host/runtime/src/access/mqtt.js'
import { RelayObjectClient } from '../../../../host/runtime/src/access/relay-client.js'
import { RelayTransport } from '../../../../host/runtime/src/access/transport.js'
import { maxPlaintextBytes, publishPacketBytes } from '../../../../host/runtime/src/access/packet.js'
import { codec } from '../../../../host/runtime/src/protocol.js'
import {
  capturePublishes,
  collectMessages,
  createRelayStub,
  hasLegacyFraming,
  nextMessage,
  subscribe,
} from './relay-fixture.mjs'

const require = createRequire(new URL('../../../../host/runtime/package.json', import.meta.url))
const mqtt = require('mqtt')
const MQTT_URL = process.env.WHALEREIN_TEST_MQTT_URL ?? 'mqtt://127.0.0.1:1884'
const TOKEN = process.env.WHALEREIN_TEST_HOST_PASSWORD ?? 'host-pass'
const proto = codec()
const result = (facts) => process.stdout.write(JSON.stringify({ ok: true, ...facts }) + '\n')

function envelope(hostId, body, requestId = randomUUID(), extra = {}) {
  return proto.encode('whalerein.v1.HostResult', {
    request_id: requestId,
    host_id: hostId,
    device_id: 'device-fixture',
    channel_id: 'channel-fixture',
    status: 'succeeded',
    result_body: body,
    completed_at_unix_ms: 1,
    protocol_version: 1,
    relay_version: 5,
    ...extra,
  })
}

function bytes(length, seed = 3) {
  const value = new Uint8Array(length)
  for (let index = 0; index < length; index++) value[index] = (index + seed) % 251
  return value
}

function sizedEnvelope(hostId, length) {
  return envelope(hostId, bytes(length)).length
}

function eventEnvelope(hostId, length) {
  return proto.encode('whalerein.v1.EventEnvelope', {
    host_id: hostId,
    device_id: 'device-fixture',
    channel_id: 'channel-fixture',
    nonce: Buffer.alloc(12),
    ciphertext: bytes(length),
    created_at_unix_ms: 1,
    relay_version: 5,
  })
}

function maxBodyFor(topic, limit, hostId = 'h') {
  let low = 1
  let high = limit
  while (low < high) {
    const middle = Math.ceil((low + high) / 2)
    if (publishPacketBytes(topic, sizedEnvelope(hostId, middle)) <= limit) low = middle
    else high = middle - 1
  }
  return low
}

function maxEventFor(topic, limit, hostId) {
  let low = 1
  let high = limit
  while (low < high) {
    const middle = Math.ceil((low + high) / 2)
    if (publishPacketBytes(topic, eventEnvelope(hostId, middle).length) <= limit) low = middle
    else high = middle - 1
  }
  return low
}

async function transportFor(hostId, { stub = createRelayStub(), budget = null, receiverLimit } = {}) {
  const subscriber = await subscribe(MQTT_URL, `whalerein/v1/h/${hostId}/#`)
  const transport = new RelayTransport({
    publish: (topic, payload) => subscriber.publishAsync(topic, payload, { qos: 1 }),
    relay: stub.relay,
    budget,
  })
  return { subscriber, transport, stub, receiverLimit }
}

async function caseReceiverBudget() {
  const hostId = `probe-rb-${randomUUID().slice(0, 10)}`
  const topic = `whalerein/v1/h/${hostId}/result`
  const limit = 8192
  const { subscriber, transport, stub } = await transportFor(hostId)
  try {
    const at = maxBodyFor(topic, limit, hostId)
    const below = envelope(hostId, bytes(at - 1))
    const inline = await transport.sendComplete(topic, below, { kind: 'result', receiverLimit: limit })
    assert.equal(inline.representation, 'inline')
    assert.ok(inline.packetBytes <= limit)
    const above = envelope(hostId, bytes(at + 1))
    const referenced = await transport.sendComplete(topic, above, { kind: 'result', receiverLimit: limit })
    assert.equal(referenced.representation, 'reference')
    assert.ok(referenced.packetBytes <= limit, 'the reference fits the smaller advertised hop')
    assert.equal(stub.events.map((entry) => entry.event).join(','), 'reserve,upload,finish')
    result({ limit, inlineBytes: inline.packetBytes, referenceBytes: referenced.packetBytes, at })
  } finally {
    await subscriber.endAsync(true)
  }
}

async function caseSecondHop() {
  const hostId = `probe-hop-${randomUUID().slice(0, 10)}`
  const resultTopic = `whalerein/v1/h/${hostId}/result`
  const deviceEventTopic = `whalerein/v1/h/${hostId}/d/device-fixture/event`
  const { subscriber, transport, stub } = await transportFor(hostId)
  try {
    const at = maxEventFor(deviceEventTopic, 65536, hostId)
    const payload = eventEnvelope(hostId, at - 1)
    const inlineMessage = nextMessage(subscriber, (_, message) => proto.decode('whalerein.v1.EventEnvelope', message).ciphertext?.length === at - 1)
    const first = await transport.sendComplete(resultTopic, payload, { kind: 'event', receiverLimit: 65536 })
    assert.equal(first.representation, 'inline')
    const delivered = await inlineMessage
    const original = proto.decode('whalerein.v1.EventEnvelope', delivered.bytes)
    assert.ok(original.payload_reference == null)
    const second = await transport.sendComplete(deviceEventTopic, delivered.bytes, { kind: 'event', receiverLimit: 32768 })
    assert.equal(second.representation, 'reference')
    assert.equal(stub.events.map((entry) => entry.event).join(','), 'reserve,upload,finish')
    const [objectId] = stub.objects.keys()
    const fetched = await stub.relay.fetch({ reference: { object_id: objectId } })
    assert.equal(fetched.length, at - 1, 'the reference resolves the same complete ciphertext')
    result({ first: first.representation, second: second.representation, at, sameBytes: fetched.length })
  } finally {
    await subscriber.endAsync(true)
  }
}

async function caseReferenceLimit() {
  const hostId = `probe-ref-${randomUUID().slice(0, 10)}`
  const topic = `whalerein/v1/h/${hostId}/result`
  const stub = createRelayStub()
  const transport = new RelayTransport({ publish: async () => { throw new Error('must not publish') }, relay: stub.relay })
  const reference = {
    object_id: 'f'.repeat(32),
    owner_node_id: 'node-fixture',
    ciphertext_bytes: 1024,
    sha256: Buffer.alloc(32),
    expires_at_unix_ms: Date.now() + 60000,
    purpose: 'PAYLOAD_PURPOSE_RESULT',
  }
  const payload = proto.encode('whalerein.v1.HostResult', {
    request_id: randomUUID(), host_id: hostId, status: 'succeeded', relay_version: 5,
    result_reference: reference,
  })
  let code = ''
  try {
    await transport.sendComplete(topic, payload, { kind: 'result', receiverLimit: 200 })
  } catch (error) {
    code = error.code
  }
  assert.equal(code, 'payload_too_large')
  assert.equal(stub.events.length, 0, 'no object is reserved for an unfittable reference')
  result({ code, packets: 0 })
}

async function caseOversize() {
  const hostId = `probe-over-${randomUUID().slice(0, 10)}`
  const topic = `whalerein/v1/h/${hostId}/result`
  let published = 0
  const stub = createRelayStub()
  const transport = new RelayTransport({
    publish: async () => { published++ },
    relay: stub.relay,
    budget: { packet_bytes: 4096, envelope_bytes: 4096, bulk_concurrency: 1, reserved_bytes: 4096, transfer_timeout_ms: 1000 },
  })
  let code = ''
  try {
    await transport.sendComplete(topic, envelope(hostId, bytes(5000)), { kind: 'result' })
  } catch (error) {
    code = error.code
  }
  assert.equal(code, 'payload_too_large')
  assert.equal(published, 0)
  assert.equal(stub.events.length, 0)
  result({ code, packets: published })
}

async function caseLegacy() {
  const hostId = `probe-legacy-${randomUUID().slice(0, 10)}`
  const topic = `whalerein/v1/h/${hostId}/result`
  const stub = createRelayStub()
  const transport = new RelayTransport({ publish: async () => {}, relay: stub.relay })
  const codes = {}
  for (const epoch of [0, 2, 3, 4]) {
    try {
      await transport.sendComplete(topic, proto.encode('whalerein.v1.HostResult', {
        request_id: randomUUID(), host_id: hostId, status: 'succeeded', result_body: bytes(16), relay_version: epoch,
      }), { kind: 'result' })
      codes[epoch] = 'accepted'
    } catch (error) {
      codes[epoch] = error.code
    }
  }
  // A mixed artifact with both representations is never admitted either.
  try {
    await transport.sendComplete(topic, proto.encode('whalerein.v1.HostResult', {
      request_id: randomUUID(), host_id: hostId, status: 'succeeded', result_body: bytes(16), relay_version: 5,
      result_reference: { object_id: 'f'.repeat(32), owner_node_id: 'n', ciphertext_bytes: 16, sha256: Buffer.alloc(32), expires_at_unix_ms: Date.now() + 60000, purpose: 'PAYLOAD_PURPOSE_RESULT' },
    }), { kind: 'result' })
    codes.mixed = 'accepted'
  } catch (error) {
    codes.mixed = error.code
  }

  // The real Host receive path rejects an old epoch already on the wire.
  let delivered = 0
  const access = new MqttAccess({
    url: MQTT_URL, hostId, token: TOKEN, relayClient: stub.relay,
    connect: (target, options) => mqtt.connect(target, options),
  })
  access.start({ onRequest: () => { delivered++ } })
  await access.waitForConnected(5000)
  try {
    const publisher = await subscribe(MQTT_URL, 'ignored/noop')
    try {
      const requestTopic = `whalerein/v1/h/${hostId}/request`
      await publisher.publishAsync(requestTopic, proto.encode('whalerein.v1.HostRequest', {
        request_id: 'legacy', host_id: hostId, kind: 'REQUEST_KIND_QUERY', relay_version: 2,
        body: Buffer.from('WRMQTT2legacy'), protocol_version: 1,
      }), { qos: 1 })
      const validId = 'valid-v5'
      await new Promise((resolve) => setTimeout(resolve, 300))
      assert.equal(delivered, 0, 'a legacy epoch must not reach the Host business handler')
      await publisher.publishAsync(requestTopic, proto.encode('whalerein.v1.HostRequest', {
        request_id: validId, host_id: hostId, kind: 'REQUEST_KIND_QUERY', relay_version: 5,
        body: Buffer.from('valid'), protocol_version: 1, expires_at_unix_ms: Date.now() + 30000,
      }), { qos: 1, properties: {
        messageExpiryInterval: 30, correlationData: Buffer.from(validId),
        responseTopic: `whalerein/v1/h/${hostId}/result`,
      } })
      const deadline = Date.now() + 2000
      while (delivered === 0 && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 20))
      assert.equal(delivered, 1, 'a current epoch request still reaches the handler')
    } finally {
      await publisher.endAsync(true)
    }
  } finally {
    await access.stop()
  }
  result({ codes, legacyDelivered: 0, validDelivered: delivered })
}

async function caseMatchedAndNoFragments() {
  const hostId = `probe-matched-${randomUUID().slice(0, 10)}`
  const topic = `whalerein/v1/h/${hostId}/result`
  const stub = createRelayStub()
  let packets = []
  const subscriber = await subscribe(MQTT_URL, topic)
  const access = new MqttAccess({
    url: MQTT_URL, hostId, token: TOKEN, relayClient: stub.relay,
    connect: (target, options) => {
      const client = mqtt.connect(target, options)
      packets = capturePublishes(client)
      return client
    },
  })
  access.start({})
  await access.waitForConnected(5000)
  try {
    const limit = access.sendPacketLimit()
    const delivered = []
    for (let index = 0; index < 6; index++) {
      const requestId = randomUUID()
      const body = bytes(64 + index)
      const received = nextMessage(subscriber, (_, payload) => proto.decode('whalerein.v1.HostResult', payload).request_id === requestId)
      await access.publishResult(envelope(hostId, body, requestId))
      const message = await received
      const decoded = proto.decode('whalerein.v1.HostResult', message.bytes)
      assert.deepEqual(Buffer.from(decoded.result_body), Buffer.from(body))
      delivered.push(publishPacketBytes(topic, message.bytes.length))
    }
    assert.equal(access.connected, true)
    assert.equal(access.describe().reconnects, 0)
    assert.ok(packets.every((packet) => packet.packetBytes <= limit))
    assert.equal(stub.events.length, 0, 'matched small releases stay inline')
    assert.equal(hasLegacyFraming(packets), false)
    result({ packets: delivered.length, legacy: 0, reconnects: access.describe().reconnects })
  } finally {
    await access.stop()
    await subscriber.endAsync(true)
  }
}

async function caseFetchVerification() {
  const reference = {
    object_id: 'f'.repeat(32),
    owner_node_id: 'node-fixture',
    ciphertext_bytes: 100,
    sha256: randomBytes(32),
    expires_at_unix_ms: Date.now() + 60000,
    purpose: 'PAYLOAD_PURPOSE_RESULT',
  }
  const key = randomBytes(32)
  const make = (body, headers = {}) => new RelayObjectClient({
    origin: 'http://127.0.0.1:1', hostId: 'h-fixture', privateKey: key,
    fetch: async () => new Response(body, { status: 200, headers }),
  })
  const codes = {}
  try {
    await make(Buffer.alloc(100), { 'content-length': '100' }).fetch({ reference, envelopeBytes: 1 << 20 })
    codes.inline = 'accepted'
  } catch (error) { codes.inline = error.code }
  try {
    await make(Buffer.alloc(99), { 'content-length': '100' }).fetch({ reference, envelopeBytes: 1 << 20 })
    codes.truncated = 'accepted'
  } catch (error) { codes.truncated = error.code }
  try {
    await make(Buffer.alloc(100), { 'content-length': '100' }).fetch({ reference, envelopeBytes: 1 << 20 })
    codes.corrupt = codes.inline
  } catch (error) { codes.corrupt = error.code }
  assert.equal(codes.inline, 'payload_digest_mismatch')
  assert.ok(['invalid_argument', 'unavailable'].includes(codes.truncated))
  result({ codes })
}

async function caseBulkFairness() {
  const hostId = `probe-fair-${randomUUID().slice(0, 10)}`
  const resultTopic = `whalerein/v1/h/${hostId}/result`
  const eventTopic = `whalerein/v1/h/${hostId}/event`
  const stub = createRelayStub()
  const subscriber = await subscribe(MQTT_URL, `whalerein/v1/h/${hostId}/#`)
  const smallDelivered = []
  const published = []
  const transport = new RelayTransport({
    publish: async (topic, payload) => {
      published.push({ topic, bytes: payload.length })
      await subscriber.publishAsync(topic, payload, { qos: 1 })
    },
    relay: stub.relay,
  })
  try {
    const release = stub.holdNextFinish()
    const oversized = envelope(hostId, bytes(70000), 'bulk')
    const bulk = transport.sendComplete(resultTopic, oversized, { kind: 'result' })
    await new Promise((resolve) => setTimeout(resolve, 50))
    assert.equal(published.length, 0, 'the bulk reference is not published before the object is readable')
    const small = envelope(hostId, bytes(32), 'small')
    const smallMessage = nextMessage(subscriber, (_, payload) => {
      try { return proto.decode('whalerein.v1.HostResult', payload).request_id === 'small' } catch { return false }
    })
    const smallResult = await transport.sendComplete(eventTopic, small, { kind: 'result' })
    assert.equal(smallResult.representation, 'inline')
    const message = await smallMessage
    assert.ok(message)
    assert.equal(proto.decode('whalerein.v1.HostResult', message.bytes).request_id, 'small')
    assert.equal(published.length, 1, 'the small flow is serviced while the bulk upload waits')
    release()
    const bulkResult = await bulk
    assert.equal(bulkResult.representation, 'reference')
    assert.equal(published.length, 2)
    result({ small: 'inline', bulk: 'reference', smallBeforeBulk: true })
  } finally {
    await subscriber.endAsync(true)
  }
}

async function caseLiveClass() {
  const hostId = `probe-live-${randomUUID().slice(0, 10)}`
  const liveTopic = `whalerein/v1/h/${hostId}/d/device-fixture/live`
  const eventTopic = `whalerein/v1/h/${hostId}/event`
  const stub = createRelayStub()
  const subscriber = await subscribe(MQTT_URL, `whalerein/v1/h/${hostId}/#`, { qos: 1 })
  const messages = collectMessages(subscriber)
  const published = []
  const transport = new RelayTransport({
    publish: async (topic, payload, options) => {
      const qos = options?.qos ?? 1
      published.push({ topic, qos })
      await subscriber.publishAsync(topic, payload, { qos })
    },
    relay: stub.relay,
  })
  try {
    const preview = eventEnvelope(hostId, 64)
    const accepted = await transport.sendLive(liveTopic, preview, { receiverLimit: 65536 })
    assert.equal(accepted, true)
    const delivered = await nextMessage(subscriber, (topic) => topic === liveTopic)
    assert.equal(delivered.qos, 0, 'live previews are published at QoS 0')
    assert.deepEqual(Buffer.from(delivered.bytes), Buffer.from(preview))
    // A live preview never appears on the control event topic.
    await new Promise((resolve) => setTimeout(resolve, 150))
    assert.equal(
      messages.filter((message) => message.topic === eventTopic).length,
      0,
      'a live preview must not be published on the control event topic',
    )
    // The same complete envelope delivered as control uses the event topic at QoS 1.
    const controlReceived = nextMessage(subscriber, (topic) => topic === eventTopic)
    const control = await transport.sendComplete(eventTopic, preview, { kind: 'event', receiverLimit: 65536 })
    assert.equal(control.representation, 'inline')
    const controlMessage = await controlReceived
    assert.equal(controlMessage.qos, 1, 'control events stay QoS 1')
    await new Promise((resolve) => setTimeout(resolve, 150))
    assert.equal(messages.filter((message) => message.topic === liveTopic).length, 1)
    assert.equal(messages.filter((message) => message.topic === eventTopic).length, 1)
    assert.deepEqual(published.map((entry) => entry.qos), [0, 1])
    result({
      liveQos: delivered.qos,
      controlQos: controlMessage.qos,
      livePackets: messages.filter((message) => message.topic === liveTopic).length,
      eventPackets: messages.filter((message) => message.topic === eventTopic).length,
    })
  } finally {
    await subscriber.endAsync(true)
  }
}

async function caseLiveInlineAndDrop() {
  const hostId = `probe-live-drop-${randomUUID().slice(0, 10)}`
  const liveTopic = `whalerein/v1/h/${hostId}/d/device-fixture/live`
  const eventTopic = `whalerein/v1/h/${hostId}/event`
  const stub = createRelayStub()
  const subscriber = await subscribe(MQTT_URL, `whalerein/v1/h/${hostId}/#`, { qos: 1 })
  let livePublishes = 0
  let failLive = true
  const transport = new RelayTransport({
    publish: async (topic, payload, options) => {
      if (topic === liveTopic) {
        livePublishes++
        if (failLive) throw new Error('live delivery is disconnected')
      }
      await subscriber.publishAsync(topic, payload, { qos: options?.qos ?? 1 })
    },
    relay: stub.relay,
  })
  try {
    // A live publish failure drops only the preview; control still works.
    const preview = eventEnvelope(hostId, 64)
    const dropped = await transport.sendLive(liveTopic, preview, { receiverLimit: 65536 })
    assert.equal(dropped, false)
    failLive = false
    const oversized = await transport.sendLive(liveTopic, eventEnvelope(hostId, 5000), { receiverLimit: 4096 })
    assert.equal(oversized, false, 'a preview above the packet budget is dropped, never referenced')
    const publishesBeforeReference = livePublishes
    const referenceEnvelope = proto.encode('whalerein.v1.EventEnvelope', {
      host_id: hostId,
      device_id: 'device-fixture',
      channel_id: 'channel-fixture',
      nonce: Buffer.alloc(12),
      ciphertext: bytes(8),
      created_at_unix_ms: 1,
      relay_version: 5,
      payload_reference: {
        object_id: 'f'.repeat(32),
        owner_node_id: 'node-fixture',
        ciphertext_bytes: 8,
        sha256: Buffer.alloc(32),
        expires_at_unix_ms: Date.now() + 60000,
        purpose: 'PAYLOAD_PURPOSE_EVENT',
      },
    })
    const referenced = await transport.sendLive(liveTopic, referenceEnvelope, { receiverLimit: 65536 })
    assert.equal(referenced, false, 'live never uses the HTTPS reference carrier')
    assert.equal(livePublishes, publishesBeforeReference)
    // Control is delivered after the dropped preview at its own QoS.
    const controlReceived = nextMessage(subscriber, (topic) => topic === eventTopic)
    const control = await transport.sendComplete(eventTopic, preview, { kind: 'event', receiverLimit: 65536 })
    assert.equal(control.representation, 'inline')
    const controlMessage = await controlReceived
    assert.equal(controlMessage.qos, 1)
    result({
      dropped,
      oversized,
      referenced,
      control: control.representation,
      controlQos: controlMessage.qos,
    })
  } finally {
    await subscriber.endAsync(true)
  }
}

const cases = {
  'receiver-budget': caseReceiverBudget,
  'second-hop-reference': caseSecondHop,
  'reference-limit': caseReferenceLimit,
  oversize: caseOversize,
  legacy: caseLegacy,
  'matched-no-fragments': caseMatchedAndNoFragments,
  'fetch-verification': caseFetchVerification,
  'bulk-fairness': caseBulkFairness,
  'live-class': caseLiveClass,
  'live-inline-and-drop': caseLiveInlineAndDrop,
}

const selected = cases[process.argv[2]]
if (!selected) {
  console.error(`unknown relay probe case: ${process.argv[2]}`)
  process.exit(2)
}
await selected()
process.exit(0)
