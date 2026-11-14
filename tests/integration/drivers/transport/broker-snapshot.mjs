// Container-owned broker snapshot query for the Mosquitto line.
//
// The integration broker exposes its connection registry only over the private
// control topic. This driver authenticates as the Access identity with the
// shared broker password, requests one snapshot and prints the parsed clients
// as JSON. It never prints credentials or payloads.
import { createRequire } from 'node:module'
import { randomBytes } from 'node:crypto'

const require = createRequire(new URL('../../../../host/dsh/package.json', import.meta.url))
const mqtt = require('mqtt')

const url = process.env.WHALEREIN_MQTT_URL ?? 'mqtt://127.0.0.1:1883'
const password = process.env.WHALEREIN_BROKER_PASSWORD
if (!password) {
  console.error('broker password is required')
  process.exit(2)
}

function parseClients(payload) {
  const clients = []
  for (const line of payload.split('\n')) {
    if (!line.startsWith('client ')) continue
    const parts = line.slice('client '.length).split(' ')
    if (parts.length < 9) continue
    clients.push({
      client_id: parts[0],
      kind: parts[1],
      host_id: parts[2],
      device_id: parts[3],
      conn: Number(parts[4]),
      revision: Number(parts[5]),
      state: parts[6],
      sequence: Number(parts[7]),
      connected_at: Number(parts[8]),
    })
  }
  return clients
}

const id = `integration-snapshot-${randomBytes(8).toString('hex')}`
let client
try {
  client = await mqtt.connectAsync(url, {
    protocolVersion: 5,
    clientId: id,
    username: 'access',
    password,
    clean: true,
    reconnectPeriod: 0,
    connectTimeout: 5000,
    properties: { sessionExpiryInterval: 0 },
  })
  const reply = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('snapshot timeout')), 5000)
    client.on('message', (_topic, payload) => {
      const lines = payload.toString('utf8').split('\n')
      if (!lines.includes(`id ${id}`)) return
      clearTimeout(timer)
      resolve(payload.toString('utf8'))
    })
  })
  await client.subscribeAsync('whalerein/v1/control/reply', { qos: 1 })
  await client.publishAsync(
    '$CONTROL/whalerein/v1',
    `whalerein/1\nid ${id}\nop snapshot\n`,
    { qos: 1 },
  )
  const payload = await reply
  process.stdout.write(JSON.stringify({ clients: parseClients(payload) }))
} catch {
  console.error('broker snapshot query failed')
  process.exitCode = 1
} finally {
  if (client) await client.endAsync(true)
}
