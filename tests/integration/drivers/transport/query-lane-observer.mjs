// Observe the epoch-5 MQTT query lane on the real broker.
//
// The observer authenticates as the shared Access test account and records the
// four hops of the request/reply lane:
//   Device -> Access   whalerein/v1/h/{host}/d/+/query   (QoS 1)
//   Access -> Host     whalerein/v1/h/{host}/request     (QoS 1)
//   Host   -> Access   whalerein/v1/h/{host}/result      (QoS 1)
//   Access -> Device   whalerein/v1/h/{host}/d/+/reply   (QoS 1)
//
// Only routing metadata is printed: topic, QoS, size, Correlation Data as
// UTF-8, Response Topic, Message Expiry Interval and reserved user properties.
// Payload bytes are never decoded or printed.
//
//   {"op":"snapshot"} -> {"type":"snapshot","query":[...],"request":[...],"result":[...],"reply":[...]}
//   {"op":"stop"}     -> exits

import { createRequire } from 'node:module'
import { createInterface } from 'node:readline'
import { randomBytes } from 'node:crypto'
import { codec as createCodec } from '../../../../host/runtime/src/protocol.js'

const require = createRequire(new URL('../../../../host/dsh/package.json', import.meta.url))
const mqtt = require('mqtt')

const url = process.env.WHALEREIN_MQTT_URL ?? 'mqtt://127.0.0.1:1883'
const password = process.env.WHALEREIN_TEST_ACCESS_PASSWORD
const hostId = process.env.WHALEREIN_OBSERVER_HOST_ID
if (!password || !hostId) {
  console.error('query observer requires access credentials and a host id')
  process.exit(2)
}

const filters = {
  query: `whalerein/v1/h/${hostId}/d/+/query`,
  request: `whalerein/v1/h/${hostId}/request`,
  result: `whalerein/v1/h/${hostId}/result`,
  reply: `whalerein/v1/h/${hostId}/d/+/reply`,
}
const messages = { query: [], request: [], result: [], reply: [] }

function text(value) {
  if (value == null) return null
  if (typeof value === 'string') return value
  return Buffer.from(value).toString('utf8')
}

function userProperties(packet) {
  const pairs = packet?.properties?.userProperties
  if (!pairs || typeof pairs !== 'object') return {}
  return { ...pairs }
}

const proto = createCodec()

const client = await mqtt.connectAsync(url, {
  protocolVersion: 5,
  clientId: `query-observer-${randomBytes(8).toString('hex')}`,
  username: 'access',
  password,
  clean: true,
  reconnectPeriod: 0,
  connectTimeout: 5000,
  properties: { sessionExpiryInterval: 0 },
})

client.on('message', (topic, payload, packet) => {
  const lane = Object.keys(filters).find((name) => {
    const filter = filters[name].split('/')
    const parts = topic.split('/')
    return filter.length === parts.length && filter.every((part, index) => part === '+' || part === parts[index])
  })
  if (!lane) return
  const properties = packet?.properties ?? {}
  const entry = {
    topic,
    qos: packet?.qos ?? null,
    bytes: payload.length,
    at: Date.now(),
    correlation: text(properties.correlationData),
    response_topic: properties.responseTopic ?? null,
    expiry_interval: properties.messageExpiryInterval ?? null,
    user_properties: userProperties(packet),
  }
  if (lane === 'result') {
    try {
      entry.request_id = proto.decode('whalerein.v1.HostResult', payload).request_id ?? null
    } catch {}
  }
  messages[lane].push(entry)
})

await client.subscribeAsync(Object.values(filters), { qos: 1 })

function snapshot() {
  process.stdout.write(JSON.stringify({ type: 'snapshot', ...messages }) + '\n')
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
