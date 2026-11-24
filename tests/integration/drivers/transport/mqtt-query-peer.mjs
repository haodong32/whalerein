// Raw MQTT 5 peer for the epoch-5 reserved-property and reply-target rules.
//
// Modes:
//   forge <hostId> <deviceId> <deviceToken> <accessPassword>
//     Connect as the paired Device and publish one query PUBLISH with a
//     client-supplied `whalerein-connection` property, then the same payload
//     without the reserved property. Only PUBACK reason codes and counts are
//     printed; the payload is synthetic and never decoded from the wire.
//
//   target <hostId> <deviceId> <deviceToken> <accessPassword> <replyTopic>
//     Reconnect the Device with the same MQTT client id and prove that a reply
//     directed at the replaced connection's `whalerein-target` is not delivered
//     while the same reply directed at the current connection is delivered.
//
// Only routing facts, stable reason codes and counts are printed.

import { randomBytes } from 'node:crypto'
import { createRequire } from 'node:module'
import { codec } from '../../../../host/runtime/src/protocol.js'

const require = createRequire(new URL('../../../../host/runtime/package.json', import.meta.url))
const mqtt = require('mqtt')

const url = process.env.WHALEREIN_TEST_MQTT_URL ?? 'mqtt://127.0.0.1:1883'
const mode = process.argv[2]
const proto = codec()

function fail(message, code = 2) {
  console.error(message)
  process.exit(code)
}

function deviceOptions(deviceId, token) {
  return {
    protocolVersion: 5,
    clientId: `whalerein-device-${deviceId}`,
    username: `device:${deviceId}`,
    password: token,
    clean: true,
    reconnectPeriod: 0,
    connectTimeout: 5000,
    properties: { sessionExpiryInterval: 0 },
  }
}

function accessOptions(accessPassword) {
  return {
    protocolVersion: 5,
    clientId: `query-peer-access-${randomBytes(8).toString('hex')}`,
    username: 'access',
    password: accessPassword,
    clean: true,
    reconnectPeriod: 0,
    connectTimeout: 5000,
    properties: { sessionExpiryInterval: 0 },
  }
}

function hostRequest(hostId, deviceId) {
  return proto.encode('whalerein.v1.HostRequest', {
    request_id: randomBytes(16).toString('hex'),
    host_id: hostId,
    device_id: deviceId,
    kind: 'REQUEST_KIND_QUERY',
    protocol_version: 1,
    relay_version: 5,
    expires_at_unix_ms: Date.now() + 30000,
    body: randomBytes(32),
  })
}

async function publishReason(client, topic, payload, properties) {
  let error = null
  const acknowledged = new Promise((resolve) => {
    const timer = setTimeout(() => resolve(null), 3000)
    client.once('packetreceive', (packet) => {
      if (packet?.cmd !== 'puback') return
      clearTimeout(timer)
      resolve(packet.reasonCode ?? 0)
    })
  })
  await client.publishAsync(topic, payload, { qos: 1, properties }).catch((value) => {
    error = value?.message ?? String(value)
  })
  return { reason: await acknowledged, error }
}

function snapshotRef(payload, clientId) {
  for (const line of payload.split('\n')) {
    if (!line.startsWith('client ')) continue
    const parts = line.slice('client '.length).split(' ')
    if (parts[0] === clientId && parts.length >= 11) return parts[10]
  }
  return null
}

function snapshotEpoch(payload) {
  for (const line of payload.split('\n')) {
    if (line.startsWith('epoch ')) return line.slice('epoch '.length)
  }
  return ''
}

async function brokerSnapshot(accessPassword) {
  const client = await mqtt.connectAsync(url, accessOptions(accessPassword))
  const id = `query-peer-snapshot-${randomBytes(8).toString('hex')}`
  try {
    const reply = new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('snapshot timeout')), 5000)
      client.on('message', (_topic, payload) => {
        const text = payload.toString('utf8')
        if (text.includes(`id ${id}`)) {
          clearTimeout(timer)
          resolve(text)
        }
      })
    })
    await client.subscribeAsync('whalerein/v1/control/reply', { qos: 1 })
    await client.publishAsync('$CONTROL/whalerein/v1', `whalerein/1\nid ${id}\nop snapshot\n`, { qos: 1 })
    return await reply
  } finally {
    await client.endAsync(true)
  }
}

async function waitFor(check, timeout = 8000) {
  const deadline = Date.now() + timeout
  while (Date.now() < deadline) {
    const value = await check()
    if (value) return value
    await new Promise((resolve) => setTimeout(resolve, 50))
  }
  return null
}

async function forge(hostId, deviceId, token) {
  const queryTopic = `whalerein/v1/h/${hostId}/d/${deviceId}/query`
  const client = await mqtt.connectAsync(url, deviceOptions(deviceId, token))
  client.on('error', () => {})
  try {
    const forged = await publishReason(client, queryTopic, hostRequest(hostId, deviceId), {
      userProperties: { 'whalerein-connection': 'forged:1' },
    })
    await new Promise((resolve) => setTimeout(resolve, 250))
    const accepted = await publishReason(client, queryTopic, hostRequest(hostId, deviceId), {})
    process.stdout.write(JSON.stringify({
      forgedReason: forged.reason,
      forgedError: forged.error,
      acceptedReason: accepted.reason,
      acceptedError: accepted.error,
    }) + '\n')
  } finally {
    await client.endAsync(true)
  }
}

async function target(hostId, deviceId, token, accessPassword) {
  const replyTopic = `whalerein/v1/h/${hostId}/d/${deviceId}/reply`
  const clientId = `whalerein-device-${deviceId}`

  const first = await mqtt.connectAsync(url, deviceOptions(deviceId, token))
  first.on('error', () => {})
  const firstSnapshot = await waitFor(async () => {
    const payload = await brokerSnapshot(accessPassword)
    return snapshotRef(payload, clientId)
  })
  const epoch = snapshotEpoch(await brokerSnapshot(accessPassword))
  await first.subscribeAsync(replyTopic, { qos: 1 })
  await first.endAsync(true)

  const received = []
  const second = await mqtt.connectAsync(url, deviceOptions(deviceId, token))
  second.on('error', () => {})
  const secondSnapshot = await waitFor(async () => {
    const payload = await brokerSnapshot(accessPassword)
    const ref = snapshotRef(payload, clientId)
    return ref && ref !== firstSnapshot ? ref : null
  })
  await second.subscribeAsync(replyTopic, { qos: 1 })
  second.on('message', () => received.push(Date.now()))

  const access = await mqtt.connectAsync(url, accessOptions(accessPassword))
  access.on('error', () => {})
  try {
    const result = (requestId) => proto.encode('whalerein.v1.HostResult', {
      request_id: requestId,
      host_id: hostId,
      device_id: deviceId,
      status: 'succeeded',
      protocol_version: 1,
      relay_version: 5,
      result_body: randomBytes(16),
    })
    await access.publishAsync(replyTopic, result('stale-target'), {
      qos: 1,
      properties: { userProperties: { 'whalerein-target': firstSnapshot } },
    })
    await new Promise((resolve) => setTimeout(resolve, 500))
    const staleDelivered = received.length
    await access.publishAsync(replyTopic, result('current-target'), {
      qos: 1,
      properties: { userProperties: { 'whalerein-target': secondSnapshot } },
    })
    await waitFor(async () => received.length > staleDelivered, 3000)
    process.stdout.write(JSON.stringify({
      replaced: firstSnapshot !== secondSnapshot,
      epochMatches: firstSnapshot.startsWith(`${epoch}:`) && secondSnapshot.startsWith(`${epoch}:`),
      staleDelivered,
      currentDelivered: received.length - staleDelivered,
    }) + '\n')
  } finally {
    await access.endAsync(true)
    await second.endAsync(true)
  }
}

if (mode === 'forge' && process.argv.length === 6) {
  await forge(process.argv[3], process.argv[4], process.argv[5])
} else if (mode === 'target' && process.argv.length === 7) {
  await target(process.argv[3], process.argv[4], process.argv[5], process.argv[6])
} else {
  fail('usage: mqtt-query-peer.mjs forge <host> <device> <token> | target <host> <device> <token> <accessPassword>')
}
