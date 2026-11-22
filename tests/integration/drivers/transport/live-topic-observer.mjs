// Observe the Host live and control event topics on the real broker.
//
// The observer authenticates as the shared Access test account, subscribes to
// the Host live and event topics at QoS 1 (an incoming QoS 0 PUBLISH stays
// QoS 0), and reports only topics, QoS, sizes and timestamps. It never prints
// ciphertext or message content. Python drives it over stdin:
//
//   {"op":"snapshot"} -> {"type":"snapshot","live":[...],"event":[...]}
//   {"op":"stop"}     -> exits
//
// A "live" entry is a PUBLISH observed on .../live; an "event" entry is a
// PUBLISH observed on .../event. The control event topic must never carry a
// preview.

import { createRequire } from 'node:module'
import { createInterface } from 'node:readline'
import { randomBytes } from 'node:crypto'

const require = createRequire(new URL('../../../../host/dsh/package.json', import.meta.url))
const mqtt = require('mqtt')

const url = process.env.WHALEREIN_MQTT_URL ?? 'mqtt://127.0.0.1:1883'
const password = process.env.WHALEREIN_TEST_ACCESS_PASSWORD
const hostId = process.env.WHALEREIN_OBSERVER_HOST_ID
if (!password || !hostId) {
  console.error('observer requires access credentials and a host id')
  process.exit(2)
}

const liveTopic = `whalerein/v1/h/${hostId}/live`
const eventTopic = `whalerein/v1/h/${hostId}/event`
const deviceLiveFilter = `whalerein/v1/h/${hostId}/d/+/live`
const messages = { live: [], event: [], device_live: [] }

const client = await mqtt.connectAsync(url, {
  protocolVersion: 5,
  clientId: `live-observer-${randomBytes(8).toString('hex')}`,
  username: 'access',
  password,
  clean: true,
  reconnectPeriod: 0,
  connectTimeout: 5000,
  properties: { sessionExpiryInterval: 0 },
})

client.on('message', (topic, payload, packet) => {
  const entry = { topic, qos: packet?.qos ?? null, bytes: payload.length, at: Date.now() }
  if (topic === liveTopic) messages.live.push(entry)
  else if (topic === eventTopic) messages.event.push(entry)
  else if (topic.startsWith(`whalerein/v1/h/${hostId}/d/`) && topic.endsWith('/live')) messages.device_live.push(entry)
})

await client.subscribeAsync([liveTopic, eventTopic, deviceLiveFilter], { qos: 1 })

function snapshot() {
  process.stdout.write(JSON.stringify({
    type: 'snapshot',
    live: messages.live,
    event: messages.event,
    device_live: messages.device_live,
  }) + '\n')
}

process.stdout.write(JSON.stringify({ type: 'ready' }) + '\n')

const lines = createInterface({ input: process.stdin })
let stopping = false
lines.on('line', async (line) => {
  let command
  try {
    command = JSON.parse(line)
  } catch {
    return
  }
  if (command.op === 'snapshot') {
    snapshot()
    return
  }
  if (command.op === 'stop' && !stopping) {
    stopping = true
    await client.endAsync(true)
    process.exit(0)
  }
})
