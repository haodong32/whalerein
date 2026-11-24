// Regression: mqtt.js reports a refused MQTT 5 CONNACK as an `error` event
// whose `code` is the numeric reason code and never emits `disconnect`. The
// Host must still classify that as a terminal `unauthorized` instead of
// reconnect-looping on a revoked/invalid Host credential.

import test from 'node:test'
import assert from 'node:assert/strict'
import { EventEmitter } from 'node:events'
import { MqttAccess } from '../src/access/mqtt.js'

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

test('a refused MQTT CONNACK stops retrying and reports unauthorized', async () => {
  const attempts = []
  // Shape of mqtt@5 handleConnack for reasonCode > 0: _cleanUp(true) then
  // emit('error', new ErrorWithReasonCode(message, reasonCode)). No
  // 'disconnect' packet is emitted, so MqttAccess's #disconnectReason is -1.
  const connect = () => {
    attempts.push(1)
    const client = new EventEmitter()
    client.end = (force, opts, callback) => {
      const done = typeof force === 'function' ? force : callback
      queueMicrotask(() => {
        client.emit('close')
        done?.()
      })
      return client
    }
    queueMicrotask(() => {
      client.emit('error', Object.assign(new Error('Connection refused: Not authorized'), { code: 135 }))
      queueMicrotask(() => client.emit('close'))
    })
    return client
  }
  const access = new MqttAccess({
    url: 'mqtts://broker.test:8883',
    hostId: 'host-1',
    token: 'revoked-host-token',
    connect,
    reconnectMinMs: 10,
    reconnectMaxMs: 20,
  })
  let failure = ''
  access.start({ onFailure: (code) => { failure = code } })
  await assert.rejects(
    access.waitForConnected(120),
    (error) => error.code === 'unauthorized',
    'a refused CONNACK is a terminal credential failure',
  )
  assert.equal(access.describe().lastErrorCode, 'unauthorized')
  assert.equal(failure, 'unauthorized')
  await sleep(60)
  assert.equal(attempts.length, 1, 'a refused credential must not be retried forever')
  await access.stop()
})
