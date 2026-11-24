// Regression: a broker that accepts the MQTT connection but keeps rejecting
// the Host subscription must still escalate the reconnect backoff. A successful
// CONNACK alone does not complete the ready cycle.

import test from 'node:test'
import assert from 'node:assert/strict'
import { EventEmitter } from 'node:events'
import { MqttAccess } from '../src/access/mqtt.js'

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

test('a repeated post-CONNACK subscription failure backs off instead of hot-looping', async () => {
  const delays = []
  const logger = {
    info(event, data) {
      if (event === 'mqtt_reconnect_scheduled') delays.push(data.ms)
    },
    warn() {},
    error() {},
    debug() {},
  }
  const connect = () => {
    const client = new EventEmitter()
    client.subscribe = (_topics, _options, callback) => {
      queueMicrotask(() => callback(new Error('not authorized to subscribe')))
      return client
    }
    client.end = (force, opts, callback) => {
      const done = typeof force === 'function' ? force : callback
      queueMicrotask(() => {
        client.emit('close')
        done?.()
      })
      return client
    }
    // The broker accepts the connection; only the subscription keeps failing.
    queueMicrotask(() => client.emit('connect', {}))
    return client
  }
  const access = new MqttAccess({
    url: 'mqtts://broker.test:8883',
    hostId: 'host-1',
    token: 'host-token',
    connect,
    reconnectMinMs: 10,
    reconnectMaxMs: 500,
    logger,
  })
  try {
    access.start({})
    const deadline = Date.now() + 2000
    while (delays.length < 4 && Date.now() < deadline) await sleep(10)
    assert.ok(delays.length >= 4, `observed ${delays.length} reconnect schedules`)
    assert.ok(
      delays.at(-1) > delays[0] * 2,
      `repeated subscribe failures must grow the reconnect delay, got ${delays.join(', ')}`,
    )
  } finally {
    await access.stop()
  }
})
