// Observe the retired Host live topic and the current Device-scoped topics.
//
// Epoch 5 retires `whalerein/v1/h/{host}/live`: the Host publishes a QoS 0
// preview directly on `.../d/{device}/live` and Access never republishes it.
// The observer authenticates as the shared Access test account, subscribes to
// the retired Host topic (to prove it stays empty) and the Device topic, and
// reports only topics, QoS, sizes, reserved user properties and timestamps. It
// never prints ciphertext or message content.
//
//   {"op":"snapshot"} -> {"type":"snapshot","live":[...],"event":[...],"device_live":[...],"device_event":[...]}
//   {"op":"stop"}     -> exits

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

const retiredLiveTopic = `whalerein/v1/h/${hostId}/live`
const eventTopic = `whalerein/v1/h/${hostId}/event`
const deviceLiveFilter = `whalerein/v1/h/${hostId}/d/+/live`
const deviceEventFilter = `whalerein/v1/h/${hostId}/d/+/event`
const messages = { live: [], event: [], device_live: [], device_event: [] }

function userProperties(packet) {
  const pairs = packet?.properties?.userProperties
  if (!pairs || typeof pairs !== 'object') return {}
  return { ...pairs }
}

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
  const entry = {
    topic,
    qos: packet?.qos ?? null,
    bytes: payload.length,
    at: Date.now(),
    properties: userProperties(packet),
  }
  if (topic === retiredLiveTopic) messages.live.push(entry)
  else if (topic === eventTopic) messages.event.push(entry)
  else if (topic.startsWith(`whalerein/v1/h/${hostId}/d/`) && topic.endsWith('/live')) messages.device_live.push(entry)
  else if (topic.startsWith(`whalerein/v1/h/${hostId}/d/`) && topic.endsWith('/event')) messages.device_event.push(entry)
})

await client.subscribeAsync(
  [retiredLiveTopic, eventTopic, deviceLiveFilter, deviceEventFilter],
  { qos: 1 },
)

function snapshot() {
  process.stdout.write(JSON.stringify({
    type: 'snapshot',
    live: messages.live,
    event: messages.event,
    device_live: messages.device_live,
    device_event: messages.device_event,
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
