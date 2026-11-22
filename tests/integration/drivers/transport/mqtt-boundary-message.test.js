// AC-01.below / .at / .above / .headers against the real broker.
//
// Every HostResult is one complete MQTT PUBLISH. The encoded packet, including
// the topic, packet identifier and properties, is compared with the negotiated
// 64 KiB limit. Exactly at or below it the result stays inline; one encoded byte
// above it the complete ciphertext is uploaded first and only a packet-sized
// reference is published. No fragment, receipt or acknowledgement packet exists.

import test from 'node:test'
import assert from 'node:assert/strict'
import { randomUUID } from 'node:crypto'
import { createRequire } from 'node:module'
import { MqttAccess } from '../../../../host/runtime/src/access/mqtt.js'
import { publishPacketBytes } from '../../../../host/runtime/src/access/packet.js'
import { codec } from '../../../../host/runtime/src/protocol.js'
import {
  capturePublishes,
  createRelayStub,
  hasLegacyFraming,
  nextMessage,
  subscribe,
} from './relay-fixture.mjs'

const MQTT_URL = process.env.WHALEREIN_TEST_MQTT_URL ?? 'mqtt://127.0.0.1:1884'
const TOKEN = process.env.WHALEREIN_TEST_HOST_PASSWORD ?? 'host-pass'
const proto = codec()

function envelope(hostId, requestId, body) {
  return proto.encode('whalerein.v1.HostResult', {
    request_id: requestId,
    host_id: hostId,
    device_id: 'device-fixture',
    channel_id: 'channel-fixture',
    status: 'succeeded',
    result_body: body,
    completed_at_unix_ms: 1,
    protocol_version: 1,
    relay_version: 4,
  })
}

// The exact packet size never needs the topic's message body to be published:
// publishPacketBytes only needs the payload length. Use the real codec shape.
function packetSize(topic, bodyLength, hostId) {
  const size = proto.encode('whalerein.v1.HostResult', {
    request_id: 'r'.repeat(36),
    host_id: hostId,
    device_id: 'device-fixture',
    channel_id: 'channel-fixture',
    status: 'succeeded',
    result_body: new Uint8Array(bodyLength),
    completed_at_unix_ms: 1,
    protocol_version: 1,
    relay_version: 4,
  }).length
  return publishPacketBytes(topic, size)
}

function maxBodyForPacket(topic, limit, hostId) {
  let low = 1
  let high = limit
  if (packetSize(topic, low, hostId) > limit) return 0
  while (low < high) {
    const middle = Math.ceil((low + high) / 2)
    if (packetSize(topic, middle, hostId) <= limit) low = middle
    else high = middle - 1
  }
  return low
}

function body(length, seed = 0) {
  const value = new Uint8Array(length)
  for (let index = 0; index < length; index++) value[index] = (index * 31 + seed) % 251
  return value
}

async function hostAccess(hostId, relay) {
  const access = new MqttAccess({
    url: MQTT_URL,
    hostId,
    token: TOKEN,
    relayClient: relay,
    connect: (target, options) => {
      const client = mqttConnect(target, options)
      access.__packets = capturePublishes(client)
      return client
    },
  })
  access.start({})
  await access.waitForConnected(5000)
  return access
}

const require = createRequire(new URL('../../../../host/runtime/package.json', import.meta.url))
const mqtt = require('mqtt')
function mqttConnect(target, options) {
  return mqtt.connect(target, options)
}

test('AC-01 complete result packets stay inline below and at the encoded boundary', { timeout: 30000 }, async () => {
  const hostId = `h-ac01-${randomUUID().slice(0, 12)}`
  const topic = `whalerein/v1/h/${hostId}/result`
  const stub = createRelayStub()
  const subscriber = await subscribe(MQTT_URL, topic)
  const access = await hostAccess(hostId, stub.relay)
  try {
    const limit = access.sendPacketLimit()
    const at = maxBodyForPacket(topic, limit, hostId)
    const below = maxBodyForPacket(topic, limit - 1, hostId)
    assert.equal(packetSize(topic, below, hostId), limit - 1)
    assert.equal(packetSize(topic, at, hostId), limit)

    for (const [label, size] of [['below', below], ['at', at]]) {
      const requestId = randomUUID()
      const bodyBytes = body(size)
      const received = nextMessage(subscriber, (_, bytes) => proto.decode('whalerein.v1.HostResult', bytes).request_id === requestId)
      const result = await access.publishResult(envelope(hostId, requestId, bodyBytes))
      assert.equal(result.representation, 'inline', `AC-01.${label} must stay inline`)
      assert.equal(result.packetBytes, size === at ? limit : limit - 1)
      const message = await received
      const delivered = proto.decode('whalerein.v1.HostResult', message.bytes)
      assert.deepEqual(Buffer.from(delivered.result_body), Buffer.from(bodyBytes))
      assert.ok(delivered.result_reference == null)
      assert.equal(stub.events.length, 0, 'an inline result never touches the directory')
    }
    const packets = access.__packets
    assert.ok(packets.length >= 2)
    assert.ok(packets.every((packet) => packet.packetBytes <= limit))
    assert.equal(hasLegacyFraming(packets), false)
  } finally {
    await access.stop()
    await subscriber.endAsync(true)
  }
})

test('AC-01.above publishes one packet-sized reference after the object is readable', { timeout: 30000 }, async () => {
  const hostId = `h-ac01-above-${randomUUID().slice(0, 10)}`
  const topic = `whalerein/v1/h/${hostId}/result`
  const stub = createRelayStub()
  const subscriber = await subscribe(MQTT_URL, topic)
  const access = await hostAccess(hostId, stub.relay)
  try {
    const limit = access.sendPacketLimit()
    const at = maxBodyForPacket(topic, limit, hostId)
    const above = at + 1
    assert.equal(packetSize(topic, above, hostId), limit + 1)
    const requestId = randomUUID()
    const bodyBytes = body(above, 7)
    const received = nextMessage(subscriber, (_, bytes) => proto.decode('whalerein.v1.HostResult', bytes).request_id === requestId)
    const result = await access.publishResult(envelope(hostId, requestId, bodyBytes))
    assert.equal(result.representation, 'reference')
    assert.ok(result.packetBytes <= limit, 'the reference itself must fit one packet')
    const message = await received
    assert.ok(publishPacketBytes(topic, message.bytes.length) <= limit)
    const delivered = proto.decode('whalerein.v1.HostResult', message.bytes)
    assert.equal((delivered.result_body ?? new Uint8Array()).length, 0)
    const reference = delivered.result_reference
    assert.ok(reference, 'the oversized result must carry a reference')
    assert.equal(Number(reference.ciphertext_bytes), bodyBytes.length)
    assert.equal(reference.purpose, 'PAYLOAD_PURPOSE_RESULT')
    // The directory saw reserve, upload and finish before any package was sent.
    const names = stub.events.map((entry) => entry.event)
    assert.deepEqual(names, ['reserve', 'upload', 'finish'])
    assert.equal(stub.objects.get(reference.object_id).readable, true)
    assert.deepEqual(await stub.relay.fetch({ reference }), bodyBytes)
    const packets = access.__packets.filter((packet) => packet.topic === topic && proto.decode('whalerein.v1.HostResult', packet.raw).request_id === requestId)
    assert.equal(packets.length, 1, 'exactly one complete reference PUBLISH')
    assert.ok(packets.every((packet) => packet.packetBytes <= limit))
    assert.equal(hasLegacyFraming(access.__packets), false)
  } finally {
    await access.stop()
    await subscriber.endAsync(true)
  }
})

test('AC-01.headers moves the inline boundary by exactly the topic bytes', { timeout: 30000 }, async () => {
  const shortHost = `h-${randomUUID().slice(0, 10)}`
  const longHost = `${shortHost}-topic-suffix`
  const shortTopic = `whalerein/v1/h/${shortHost}/result`
  const longTopic = `whalerein/v1/h/${longHost}/result`
  assert.equal(longTopic.length - shortTopic.length, Buffer.byteLength(longHost) - Buffer.byteLength(shortHost))
  const stub = createRelayStub()
  const shortSubscriber = await subscribe(MQTT_URL, shortTopic)
  const longSubscriber = await subscribe(MQTT_URL, longTopic)
  const shortAccess = await hostAccess(shortHost, stub.relay)
  const longAccess = await hostAccess(longHost, stub.relay)
  try {
    const limit = shortAccess.sendPacketLimit()
    const shortAt = maxBodyForPacket(shortTopic, limit, shortHost)
    const longAt = maxBodyForPacket(longTopic, limit, longHost)
    const delta = packetSize(longTopic, 1, longHost) - packetSize(shortTopic, 1, shortHost)
    assert.ok(delta > longTopic.length - shortTopic.length, 'the topic and encoded host field both count')
    assert.equal(shortAt - longAt, delta, 'the boundary moves by the whole encoded framing delta')
    assert.equal(packetSize(longTopic, shortAt, longHost), limit + delta)
    // The same body that was inline for the short topic is now a reference.
    const requestId = randomUUID()
    const bodyBytes = body(shortAt)
    const received = nextMessage(longSubscriber, (_, bytes) => proto.decode('whalerein.v1.HostResult', bytes).request_id === requestId)
    const result = await longAccess.publishResult(envelope(longHost, requestId, bodyBytes))
    assert.equal(result.representation, 'reference')
    const message = await received
    assert.equal(proto.decode('whalerein.v1.HostResult', message.bytes).result_reference.purpose, 'PAYLOAD_PURPOSE_RESULT')
    assert.ok(result.packetBytes <= limit)
    assert.ok(longAccess.__packets.every((packet) => packet.packetBytes <= limit))
    assert.equal(hasLegacyFraming(longAccess.__packets), false)
  } finally {
    await shortAccess.stop()
    await longAccess.stop()
    await shortSubscriber.endAsync(true)
    await longSubscriber.endAsync(true)
  }
})
