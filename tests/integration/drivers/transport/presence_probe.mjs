// Container-owned presence helper for multi-Host transport cases.
//
// Modes keep the harness free of a Python MQTT dependency:
//   hold <clientId> <username> <password>  keep one broker connection open
//   device <deviceId> <token>              connect as the device, then disconnect
//
// Client ids must match the product's canonical identity rule; Access rejects a
// mismatched id before policy installation. Credentials are never printed.

import { createRequire } from 'node:module'

const require = createRequire(
  new URL('../../../../host/runtime/package.json', import.meta.url),
)
const mqtt = require('mqtt')

const url = process.env.WHALEREIN_TEST_MQTT_URL ?? 'mqtt://127.0.0.1:1883'
const [mode, ...args] = process.argv.slice(2)

function options(clientId, username, password) {
  return {
    protocolVersion: 5,
    reconnectPeriod: 0,
    connectTimeout: 5000,
    clientId,
    username,
    password,
  }
}

function fail(message, code = 3) {
  console.error(message)
  process.exit(code)
}

async function hold(clientId, username, password) {
  try {
    const client = await mqtt.connectAsync(url, options(clientId, username, password))
    console.log('probe ready')
    setTimeout(() => {
      client.endAsync(true).finally(() => process.exit(0))
    }, Number(process.env.WHALEREIN_PROBE_TIMEOUT_MS ?? 15000))
  } catch {
    fail('hold connect failed')
  }
}

async function device(deviceId, token) {
  try {
    const client = await mqtt.connectAsync(
      url,
      options(`whalerein-device-${deviceId}`, `device:${deviceId}`, token),
    )
    console.log('device ready')
    setTimeout(() => {
      // A clean MQTT DISCONNECT still produces the broker disconnect edge that
      // Access fans out, while not publishing a last will.
      client.endAsync(true).finally(() => process.exit(0))
    }, 300)
  } catch {
    fail('device connect failed')
  }
}

if (mode === 'hold' && args.length === 3) {
  await hold(args[0], args[1], args[2])
} else if (mode === 'device' && args.length === 2) {
  await device(args[0], args[1])
} else {
  fail('usage: presence_probe.mjs hold <clientId> <user> <pass> | device <id> <token>', 2)
}
