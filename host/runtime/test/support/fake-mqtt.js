// Minimal in-process MQTT 5-ish broker used by tests and the local harness.
// It implements only the surface MqttAccess uses: connect, exact-topic
// subscribe, QoS 1 publish with acknowledgement, end, and forced disconnects.

import { EventEmitter } from 'node:events'

class FakeMqttClient extends EventEmitter {
  constructor(broker, options = {}) {
    super()
    this.broker = broker
    this.options = options
    this.connected = false
    this.closed = false
    this.subscriptions = new Set()
    this.delivered = []
    this.stream = {
      noDelay: false,
      setNoDelay(value) { this.noDelay = value === true },
    }
    queueMicrotask(() => {
      if (this.closed) return
      this.connected = true
      this.emit('connect', {
        sessionPresent: false,
        ...(Number.isFinite(this.broker.maxPacketBytes) ? { properties: { maximumPacketSize: this.broker.maxPacketBytes } } : {}),
      })
    })
  }

  subscribe(topic, options, callback) {
    const cb = typeof options === 'function' ? options : callback
    const topics = Array.isArray(topic) ? topic : [topic]
    queueMicrotask(() => {
      if (this.closed || !this.connected) {
        cb?.(new Error('client closed'))
        return
      }
      for (const item of topics) this.broker.subscribe(this, item)
      cb?.(null, topics.map((item) => ({ topic: item, qos: 1 })))
    })
    return this
  }

  unsubscribe(topic, callback) {
    this.broker.unsubscribe(this, topic)
    queueMicrotask(() => callback?.(null))
    return this
  }

  publish(topic, payload, options, callback) {
    const cb = typeof options === 'function' ? options : callback
    const qos = typeof options === 'object' && options !== null ? (options.qos ?? 0) : 0
    this.broker.publish(this, topic, Buffer.from(payload), (error) => cb?.(error ?? null), qos)
    return this
  }

  end(force, options, callback) {
    const cb = typeof force === 'function' ? force : callback
    if (this.closed) {
      queueMicrotask(() => cb?.())
      return this
    }
    this.closed = true
    this.connected = false
    this.broker.remove(this)
    queueMicrotask(() => {
      this.emit('close')
      cb?.()
    })
    return this
  }
}

export class FakeBroker {
  #clients = new Set()
  #topics = new Map()

  constructor({ online = true, maxPacketBytes = Infinity } = {}) {
    this.online = online
    this.maxPacketBytes = maxPacketBytes
    this.messages = []
    this.connectCount = 0
  }

  connectFn() {
    return (url, options) => {
      this.connectCount++
      return this.connect(url, options)
    }
  }

  connect(url, options = {}) {
    const client = new FakeMqttClient(this, options)
    this.#clients.add(client)
    return client
  }

  /** Deliver one simulated out-of-band (HTTPS-resolved) payload to subscribers. */
  deliverDirect(topic, payload) {
    const bytes = Buffer.from(payload)
    for (const client of this.#topics.get(topic) ?? []) {
      if (!client.closed) queueMicrotask(() => client.emit('message', topic, bytes))
    }
  }

  subscribe(client, topic) {
    client.subscriptions.add(topic)
    let set = this.#topics.get(topic)
    if (!set) {
      set = new Set()
      this.#topics.set(topic, set)
    }
    set.add(client)
  }

  unsubscribe(client, topic) {
    client.subscriptions.delete(topic)
    this.#topics.get(topic)?.delete(client)
  }

  remove(client) {
    this.#clients.delete(client)
    for (const topic of client.subscriptions) this.#topics.get(topic)?.delete(client)
    client.subscriptions.clear()
  }

  publish(client, topic, payload, callback, qos = 1) {
    if (!this.online || client.closed || !client.connected || payload.length > this.maxPacketBytes) {
      queueMicrotask(() => callback(new Error('offline')))
      return
    }
    this.messages.push({ topic, payload, qos, clientId: client.options?.clientId ?? '' })
    queueMicrotask(() => {
      callback(null)
      const subscribers = this.#topics.get(topic)
      if (!subscribers) return
      for (const subscriber of subscribers) {
        if (subscriber.closed || !subscriber.connected) continue
        subscriber.delivered.push({ topic, payload })
        subscriber.emit('message', topic, payload, { qos })
      }
    })
  }

  /** Force one broker client offline without affecting the others. */
  disconnectClientId(clientId) {
    for (const client of [...this.#clients]) {
      if (client.options?.clientId !== clientId || !client.connected) continue
      client.connected = false
      client.emit('close')
    }
  }

  /** Force every live client offline; the plugin should schedule a reconnect. */
  goOffline() {
    this.online = false
    for (const client of [...this.#clients]) {
      if (!client.connected) continue
      client.connected = false
      client.emit('close')
    }
  }

  goOnline() {
    this.online = true
  }

  get clientCount() {
    return this.#clients.size
  }

  subscriberCount(topic) {
    return this.#topics.get(topic)?.size ?? 0
  }
}
