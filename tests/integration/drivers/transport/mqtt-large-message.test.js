// AC-05.oversize / .no-fragments: a two MiB result is one packet-sized
// reference and one complete browser fetch, never a fragment train.
//
// The real broker and the real Host RelayTransport are used. The in-process
// directory stub makes the exact ciphertext readable only after finish, so a
// reader proves the reference was published after the object was complete.

import test from 'node:test'
import assert from 'node:assert/strict'
import { randomUUID } from 'node:crypto'
import { MqttAccess } from '../../../../host/runtime/src/access/mqtt.js'
import { codec } from '../../../../host/runtime/src/protocol.js'
import {
  capturePublishes,
  createRelayStub,
  hasLegacyFraming,
  nextMessage,
  subscribe,
} from './relay-fixture.mjs'
import { createRequire } from 'node:module'

const require = createRequire(new URL('../../../../host/runtime/package.json', import.meta.url))
const mqtt = require('mqtt')
const MQTT_URL = process.env.WHALEREIN_TEST_MQTT_URL ?? 'mqtt://127.0.0.1:1884'
const proto = codec()

test('AC-05 a two MiB result crosses the real broker as one reference packet without reconnecting', { timeout: 30000 }, async () => {
  const hostId = `h-ac05-${randomUUID().slice(0, 12)}`
  const topic = `whalerein/v1/h/${hostId}/result`
  const stub = createRelayStub()
  const subscriber = await subscribe(MQTT_URL, topic)
  const access = new MqttAccess({
    url: MQTT_URL,
    hostId,
    token: process.env.WHALEREIN_TEST_HOST_PASSWORD ?? 'host-pass',
    relayClient: stub.relay,
    connect: (target, options) => {
      const client = mqtt.connect(target, options)
      packets = capturePublishes(client)
      return client
    },
  })
  let packets = []
  try {
    access.start({})
    await access.waitForConnected(5000)
    const limit = access.sendPacketLimit()
    const body = new Uint8Array(2 * 1024 * 1024)
    for (let index = 0; index < body.length; index++) body[index] = index % 251
    const requestId = randomUUID()
    const received = nextMessage(subscriber, (_, bytes) => proto.decode('whalerein.v1.HostResult', bytes).request_id === requestId)
    const result = await access.publishResult(proto.encode('whalerein.v1.HostResult', {
      request_id: requestId,
      host_id: hostId,
      device_id: 'device-fixture',
      channel_id: 'channel-fixture',
      status: 'succeeded',
      result_body: body,
      completed_at_unix_ms: 1,
      protocol_version: 1,
      relay_version: 4,
    }))
    assert.equal(result.representation, 'reference')
    assert.ok(result.packetBytes <= limit)
    const message = await received
    assert.ok(message.bytes.length <= limit)
    const delivered = proto.decode('whalerein.v1.HostResult', message.bytes)
    assert.equal((delivered.result_body ?? new Uint8Array()).length, 0)
    assert.equal(Number(delivered.result_reference.ciphertext_bytes), body.length)
    assert.deepEqual(await stub.relay.fetch({ reference: delivered.result_reference }), body)
    const resultPackets = packets.filter((packet) => packet.topic === topic && proto.decode('whalerein.v1.HostResult', packet.raw).request_id === requestId)
    assert.equal(resultPackets.length, 1, 'one complete PUBLISH, not a fragment train')
    assert.ok(resultPackets.every((packet) => packet.packetBytes <= limit))
    assert.equal(hasLegacyFraming(packets), false)
    assert.equal(access.connected, true)
    assert.equal(access.describe().reconnects, 0)
  } finally {
    await access.stop()
    await subscriber.endAsync(true)
  }
})
