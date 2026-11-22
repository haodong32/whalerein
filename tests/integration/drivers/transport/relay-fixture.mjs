// Shared fixture for the epoch-4 inline/reference and live transport probes.
//
// The relay object client is deliberately in-process: these probes exercise the
// real Host RelayTransport and a real broker, while the HTTPS directory is a
// deterministic stub that records the exact reserve/upload/finish order and
// makes the object readable only after finish. Real Access directory coverage
// lives in the Python chain/storage cases.

import { randomBytes } from 'node:crypto'
import { createRequire } from 'node:module'
import { sha256Bytes } from '../../../../host/runtime/src/security/digest.js'
import { publishPacketBytes } from '../../../../host/runtime/src/access/packet.js'

const require = createRequire(new URL('../../../../host/runtime/package.json', import.meta.url))
export const mqtt = require('mqtt')

const OBJECT_ID = () => randomBytes(16).toString('hex')

export function createRelayStub({ now = Date.now } = {}) {
  const objects = new Map()
  const events = []
  let holdFinish = null

  const relay = {
    async reserve(input) {
      const objectId = OBJECT_ID()
      const object = {
        objectId,
        scope: input,
        ciphertext: null,
        readable: false,
        digest: sha256Bytes(input.ciphertext),
        expiresAt: input.expiresAt,
      }
      objects.set(objectId, object)
      events.push({ event: 'reserve', objectId, purpose: input.purpose, bytes: input.ciphertext.length })
      return {
        object_id: objectId,
        owner_node_id: 'node-fixture',
        upload_path: `/v1/relay/objects/${objectId}`,
        max_bytes: input.ciphertext.length,
        expires_at_unix_ms: input.expiresAt,
        upload_timeout_ms: 10_000,
      }
    },
    async upload({ reservation, ciphertext }) {
      const object = objects.get(reservation.object_id)
      if (!object) throw new Error('upload before reserve')
      object.ciphertext = Buffer.from(ciphertext)
      events.push({ event: 'upload', objectId: reservation.object_id, bytes: ciphertext.length })
    },
    async finish({ objectId }) {
      if (holdFinish) await holdFinish
      const object = objects.get(objectId)
      if (!object || !object.ciphertext) throw new Error('finish before upload')
      object.readable = true
      events.push({ event: 'finish', objectId, readable: true })
      return { object_id: objectId }
    },
    async fetch({ reference }) {
      const object = objects.get(reference.object_id)
      if (!object || !object.readable) throw new Error('reference published before the object was readable')
      return new Uint8Array(object.ciphertext)
    },
    async delete({ objectId }) {
      events.push({ event: 'delete', objectId })
      objects.delete(objectId)
      return { cancelled: true }
    },
  }

  return {
    relay,
    objects,
    events,
    get finishEvents() {
      return events.filter((entry) => entry.event === 'finish').length
    },
    holdNextFinish() {
      let release
      holdFinish = new Promise((resolve) => { release = resolve })
      return () => {
        holdFinish = null
        release()
      }
    },
  }
}

/** Patch one mqtt.js client so every publish records its exact PUBLISH size. */
export function capturePublishes(client) {
  const packets = []
  const publish = client.publish.bind(client)
  client.publish = (topic, bytes, ...args) => {
    packets.push({
      topic,
      payloadBytes: bytes.length,
      packetBytes: publishPacketBytes(topic, bytes.length),
      raw: Buffer.from(bytes),
    })
    return publish(topic, bytes, ...args)
  }
  return packets
}

export function hasLegacyFraming(packets) {
  return packets.some((packet) => packet.raw.subarray(0, 7).toString('latin1') === 'WRMQTT2')
    || packets.some((packet) => /\/ack$|\/receipt$/.test(packet.topic))
}

/** Connect a broker subscriber using the shared Access test account. */
export async function subscribe(url, filter, { qos = 1 } = {}) {
  const client = await mqtt.connectAsync(url, {
    protocolVersion: 5,
    clientId: `relay-probe-${randomBytes(8).toString('hex')}`,
    username: process.env.WHALEREIN_TEST_ACCESS_USERNAME ?? 'access',
    password: process.env.WHALEREIN_TEST_ACCESS_PASSWORD,
    reconnectPeriod: 0,
    connectTimeout: 3000,
  })
  await client.subscribeAsync(filter, { qos })
  return client
}

/** Wait for one message matching a predicate, never longer than timeoutMs. */
export function nextMessage(client, predicate, timeoutMs = 5000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      client.removeListener('message', onMessage)
      reject(new Error('expected relay message was not observed'))
    }, timeoutMs)
    timer.unref?.()
    function onMessage(topic, bytes, packet) {
      if (!predicate(topic, bytes)) return
      clearTimeout(timer)
      client.removeListener('message', onMessage)
      resolve({ topic, bytes: Buffer.from(bytes), qos: packet?.qos })
    }
    client.on('message', onMessage)
  })
}

/** Record every message that arrives on one subscriber. */
export function collectMessages(client) {
  const messages = []
  client.on('message', (topic, bytes, packet) => {
    messages.push({ topic, bytes: Buffer.from(bytes), qos: packet?.qos })
  })
  return messages
}
