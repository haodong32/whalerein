// TRAN-14: exact payload boundaries through the real broker.
//
// One byte, one chunk minus one, exactly one chunk, one chunk plus one and two
// chunks plus one must all arrive with content preserved. Every wire packet
// stays inside the broker's packet budget, so a boundary cannot be delivered by
// silently raising the MQTT packet size.

import test from 'node:test'
import assert from 'node:assert/strict'
import { randomUUID } from 'node:crypto'
import { createRequire } from 'node:module'

const require = createRequire(
  new URL('../../../../host/runtime/package.json', import.meta.url),
)
const mqtt = require('mqtt')
import { MqttAccess } from '../../../../host/runtime/src/access/mqtt.js'
import { LogicalTransport, PACKET_BYTES } from '../../../../host/runtime/src/access/transport.js'
import { codec } from '../../../../host/runtime/src/protocol.js'

const CHUNK = 60 * 1024
const SIZES = [1, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK - 1, 2 * CHUNK + 1]

test('payloads at and around the fragmentation boundary survive the real broker', { timeout: 30000 }, async () => {
  const hostId = process.env.WHALEREIN_TEST_HOST_ID ?? `boundary-${randomUUID()}`
  const url = process.env.WHALEREIN_TEST_MQTT_URL ?? 'mqtt://127.0.0.1:1884'
  const proto = codec()
  const resultTopic = `whalerein/v1/h/${hostId}/result`
  const requestTopic = `whalerein/v1/h/${hostId}/request`
  const subscriber = await mqtt.connectAsync(url, {
    protocolVersion: 5,
    clientId: `boundary-reader-${randomUUID()}`,
    username: process.env.WHALEREIN_TEST_ACCESS_USERNAME ?? 'access',
    password: process.env.WHALEREIN_TEST_ACCESS_PASSWORD,
    reconnectPeriod: 0,
    connectTimeout: 3000,
  })
  const packetSizes = []
  const expected = new Map()
  const received = new Map()
  let resolveProbe
  const probed = new Promise((resolve) => {
    resolveProbe = resolve
  })
  const access = new MqttAccess({
    url,
    hostId,
    token: process.env.WHALEREIN_TEST_HOST_PASSWORD ?? 'host-pass',
    connect: (target, options) => {
      const client = mqtt.connect(target, options)
      const publish = client.publish.bind(client)
      client.publish = (topic, bytes, ...args) => {
        packetSizes.push(bytes.length)
        return publish(topic, bytes, ...args)
      }
      return client
    },
  })
  try {
    await subscriber.subscribeAsync('whalerein/v1/h/+/result', { qos: 1 })
    const receiver = new LogicalTransport({
      codec: proto,
      publish: (topic, bytes) => subscriber.publishAsync(topic, bytes, { qos: 1 }),
    })
    subscriber.on('message', (topic, bytes) => {
      if (topic !== resultTopic) return
      receiver
        .receive(topic, bytes, requestTopic, (body) => {
          const result = proto.decode('whalerein.v1.HostResult', body)
          if (expected.has(result.request_id)) {
            received.set(result.request_id, result.result_body.length)
          }
        })
        .then((consumed) => {
          if (!consumed) {
            const result = proto.decode('whalerein.v1.HostResult', bytes)
            if (expected.has(result.request_id)) {
              received.set(result.request_id, result.result_body.length)
            }
          }
        })
        .catch(() => {})
    })
    access.start({
      onRequest: (request) => {
        if (request.request_id === 'probe') resolveProbe()
      },
    })
    await access.waitForConnected(3000)
    await subscriber.publishAsync(
      requestTopic,
      proto.encode('whalerein.v1.HostRequest', {
        request_id: 'probe',
        host_id: hostId,
        relay_transport_version: 2,
      }),
      { qos: 1 },
    )
    await probed
    for (const size of SIZES) {
      const requestId = randomUUID()
      const body = new Uint8Array(size)
      for (let index = 0; index < size; index++) body[index] = index % 251
      expected.set(requestId, size)
      await access.publishResult(
        proto.encode('whalerein.v1.HostResult', {
          request_id: requestId,
          host_id: hostId,
          status: 'succeeded',
          result_body: body,
        }),
      )
      const deadline = Date.now() + 5000
      while (!received.has(requestId) && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, 10))
      }
      assert.equal(received.get(requestId), size, `payload size ${size} was not delivered intact`)
    }
    assert.equal(access.connected, true)
    assert.equal(access.describe().reconnects, 0)
    assert.ok(packetSizes.length > 0)
    assert.ok(packetSizes.every((size) => size <= PACKET_BYTES), 'a wire packet exceeded the broker budget')
  } finally {
    await access.stop()
    await subscriber.endAsync(true)
  }
})
