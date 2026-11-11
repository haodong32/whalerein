// Container-owned check. All credentials come from the disposable scenario.
import assert from 'node:assert/strict'
import { randomBytes } from 'node:crypto'
import { createRequire } from 'node:module'
import { probeMqtt } from '../../../../host/dsh/src/connection-test.js'
const require = createRequire(new URL('../../../../host/dsh/package.json', import.meta.url))
const mqtt = require('mqtt')

const url = 'mqtt://127.0.0.1:1883'
const hostId = 'probe-unregistered-host'
const token = process.env.WHALEREIN_HOST_PROVISIONING_TOKEN
let client
try {
  assert.equal(typeof token, 'string')
  assert.deepEqual(await probeMqtt(url, undefined, 5000, { hostId, token }), { ok: true, authenticated: true })
  assert.deepEqual(await probeMqtt(url, undefined, 5000, { hostId, token: 'incorrect-test-credential' }), { ok: false, code: 'credentials' })
  client = await mqtt.connectAsync(url, {
    clientId: `whalerein-probe-${randomBytes(16).toString('hex')}`,
    username: `probe:${hostId}`, password: token,
    protocolVersion: 5, clean: true, reconnectPeriod: 0, connectTimeout: 5000,
    properties: { sessionExpiryInterval: 0 },
  })
  client.on('error', () => {})
  // EMQX must deny topic operations, even for an authenticated probe.
  await assert.rejects(client.subscribeAsync(`whalerein/v1/h/${hostId}/request`, { qos: 1 }))
  await assert.rejects(client.publishAsync(`whalerein/v1/h/${hostId}/result`, '', { qos: 1 }))
  console.log('probe-login-ok')
} catch {
  console.error('probe-login-check-failed')
  process.exitCode = 1
} finally {
  if (client) await client.endAsync(true)
}
