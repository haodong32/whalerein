// MQTT 5 over TLS transport. This is the plugin's only network connection:
// no HTTP server and no inbound listener. It subscribes only to its own
// request topic and publishes only its own result/event topics.

import { readFileSync } from 'node:fs'
import mqtt from 'mqtt'
import { ConfigError, topics, PROTOCOL_VERSION } from '../config.js'
import { PluginError } from '../errors.js'
import { LogicalTransport, MESSAGE_BYTES, TRANSPORT_VERSION, PACKET_BYTES } from './transport.js'
import { codec as defaultCodec, resolveCodec } from '../protocol.js'

// Leave room for protobuf/MQTT headers and HTTP base64 wrapping in Access.
export const MAX_APPLICATION_MESSAGE_BYTES = MESSAGE_BYTES
export const MAX_MQTT_PACKET_BYTES = PACKET_BYTES

export class MqttAccess {
  #client = null
  #generation = 0
  #reconnectTimer = null
  #connectWaiters = new Set()
  #backoffMs
  #stopped = true
  #connected = false
  #reconnects = 0
  #attempts = 0
  #published = 0
  #publishErrors = 0
  #lastErrorCode = ''
  #handlers = {}
  #disconnectReason = -1
  #transport
  #peerCapable = false
  #lastPublish = { kind: 'none', bytes: 0 }

  /**
   * @param {object} options
   * @param {string} options.url mqtt:// or mqtts:// URL.
   * @param {string} options.hostId
   * @param {string} options.token broker password.
   * @param {string} [options.caFile]
   * @param {number} [options.reconnectMinMs]
   * @param {number} [options.reconnectMaxMs]
   * @param {(url: string, opts: object) => object} [options.connect] test seam.
   * @param {object} [options.logger]
   * @param {() => number} [options.now]
   * @param {Uint8Array} [options.authorizationPublicKey] Ed25519 public key announced in HostReady.
   * @param {import('../protocol.js').ProtoCodec} [options.codec]
   */
  constructor({
    url,
    hostId,
    token,
    authorizationPublicKey = null,
    caFile = '',
    reconnectMinMs = 500,
    reconnectMaxMs = 30_000,
    publishTimeoutMs = 5000,
    connect = mqtt.connect,
    logger,
    now = Date.now,
    codec = defaultCodec,
  }) {
    this.authorizationPublicKey = authorizationPublicKey instanceof Uint8Array && authorizationPublicKey.length === 32
      ? authorizationPublicKey
      : null
    this.url = url
    this.hostId = hostId
    this.token = token
    this.topics = topics(hostId)
    this.reconnectMinMs = reconnectMinMs
    this.reconnectMaxMs = reconnectMaxMs
    this.publishTimeoutMs = publishTimeoutMs
    this.connectFn = connect
    this.logger = logger
    this.now = now
    this.codec = resolveCodec(codec)
    this.#transport = new LogicalTransport({ codec: this.codec, onDeliveryError: () => this.logger?.error('logical_delivery_failed',{code:'internal'}), publish: (topic, bytes) => this.#publish(topic, bytes), now })
    this.#backoffMs = reconnectMinMs
    if (caFile) {
      try {
        this.ca = readFileSync(caFile)
      } catch {
        throw new ConfigError('invalid_config', 'ca_file unreadable')
      }
    } else {
      this.ca = undefined
    }
  }

  get connected() {
    return this.#connected
  }

  get requestTopic() {
    return this.topics.request
  }

  get resultTopic() {
    return this.topics.result
  }

  get eventTopic() {
    return this.topics.event
  }

  /** Begin connecting; reconnects continue in the background after failures. */
  start(handlers = {}) {
    if (!this.#stopped) return this
    this.#stopped = false
    this.#handlers = handlers
    this.#connect()
    return this
  }

  async stop() {
    this.#stopped = true
    this.#connected = false
    this.#generation++
    this.#transport.reset()
    this.#peerCapable = false
    if (this.#reconnectTimer) {
      clearTimeout(this.#reconnectTimer)
      this.#reconnectTimer = null
    }
    for (const waiter of this.#connectWaiters) waiter.reject?.(new PluginError('unavailable', 'access stopped'))
    this.#connectWaiters.clear()
    const client = this.#client
    this.#client = null
    if (!client) return
    await new Promise((resolve) => {
      let settled = false
      const done = () => {
        if (settled) return
        settled = true
        resolve()
      }
      try {
        client.end(false, {}, done)
      } catch {
        done()
      }
      const timer = setTimeout(done, 1000)
      timer.unref?.()
    })
  }

  #options() {
    const options = {
      protocolVersion: 5,
      properties: { maximumPacketSize: MAX_MQTT_PACKET_BYTES },
      clientId: `whalerein-host-${this.hostId}`,
      username: `host:${this.hostId}`,
      password: this.token,
      clean: true,
      reconnectPeriod: 0,
      connectTimeout: 10_000,
      keepalive: 30,
    }
    if (this.ca) {
      options.ca = this.ca
      options.rejectUnauthorized = true
    }
    return options
  }

  #connect() {
    if (this.#stopped) return
    const generation = ++this.#generation
    this.#attempts++
    let client
    try {
      client = this.connectFn(this.url, this.#options())
    } catch {
      this.#lastErrorCode = 'unavailable'
      this.#scheduleReconnect()
      return
    }
    this.#client = client
    this.#disconnectReason = -1
    this.#lastPublish = { kind: 'none', bytes: 0 }
    client.on('disconnect', (packet) => {
      if (generation !== this.#generation) return
      this.#disconnectReason = Number.isInteger(packet?.reasonCode) ? packet.reasonCode : -1
    })
    client.on('connect', (connack) => {
      if (generation !== this.#generation || this.#stopped) return
      this.#onConnected(client, generation, connack)
    })
    client.on('message', (topic, payload) => {
      if (generation !== this.#generation) return
      this.#onMessage(topic, payload)
    })
    client.on('error', (error) => {
      if (generation !== this.#generation) return
      this.#lastErrorCode = 'unavailable'
      this.logger?.warn('mqtt_error', {
        code: this.#lastErrorCode,
        reason: ['ECONNRESET', 'ETIMEDOUT', 'ECONNREFUSED', 'ENOTFOUND', 'EPIPE'].includes(error?.code) ? error.code : 'transport_error',
        generation,
      })
      // A CONNACK refusal can surface as `error` without a following `close`.
      // Keep the reconnect loop alive; otherwise waitForConnected can hang
      // while the broker's auth resource is temporarily down.
      if (!this.#connected) this.#scheduleReconnect()
    })
    client.on('close', () => {
      if (generation !== this.#generation) return
      const wasConnected = this.#connected
      this.#connected = false
      this.#transport.reset()
      this.#peerCapable = false
      if (wasConnected) {
        this.logger?.warn('mqtt_disconnected', { code: 'unavailable', reasonCode: this.#disconnectReason, reason: this.#disconnectReason < 0 ? 'transport_closed' : 'broker_disconnect', generation, ...this.#lastPublish })
        this.#handlers.onDisconnected?.()
      }
      this.#scheduleReconnect()
    })
  }

  async #onConnected(client, generation, connack) {
    const maximum = connack?.properties?.maximumPacketSize
    if (maximum !== undefined && maximum < PACKET_BYTES) {
      this.#lastErrorCode = 'protocol_version_incompatible'
      for (const waiter of this.#connectWaiters) waiter.reject?.(new PluginError('protocol_version_incompatible', 'broker packet budget is below transport minimum'))
      this.#connectWaiters.clear()
      client.end(true)
      return
    }
    this.#connected = true
    if (this.#attempts > 1) this.#reconnects++
    this.#backoffMs = this.reconnectMinMs
    this.#lastErrorCode = ''
    // The host sets revocations_pending synchronously here, before the
    // request subscription can deliver a channel handshake.
    this.#handlers.onConnected?.()
    this.logger?.info('mqtt_connected', { reconnects: this.#reconnects, generation })
    try {
      await new Promise((resolve, reject) => {
        client.subscribe(this.topics.request, { qos: 1 }, (error, grants) => (error || grants?.some((grant) => grant.qos >= 128) ? reject(error ?? new Error('subscription rejected')) : resolve()))
      })
    } catch {
      this.#lastErrorCode = 'unavailable'
      this.logger?.error('mqtt_subscribe_failed', { code: this.#lastErrorCode })
      try {
        client.end(true)
      } catch {
        // close handler schedules the retry
      }
      return
    }
    if (generation !== this.#generation || this.#stopped) return
    try {
      await this.#publishReady(client)
    } catch (error) {
      this.logger?.warn('host_ready_failed', {
        code: 'unavailable',
        reason: 'publish_failed', generation,
      })
      client.end(true)
      return
    }
    if (generation !== this.#generation || this.#stopped || !this.#connected) return
    for (const waiter of this.#connectWaiters) waiter.resolve?.()
    this.#connectWaiters.clear()
    this.#handlers.onReady?.()
  }

  async #publishReady(client) {
    const ready = {
      host_id: this.hostId,
      connected_at_unix_ms: this.now(),
      transport_version: TRANSPORT_VERSION,
    }
    if (this.authorizationPublicKey) ready.authorization_public_key = this.authorizationPublicKey
    const body = this.codec.encode('whalerein.v1.HostReady', ready)
    const result = this.codec.encode('whalerein.v1.HostResult', {
      host_id: this.hostId,
      status: 'succeeded',
      result_body: body,
      completed_at_unix_ms: this.now(),
      protocol_version: PROTOCOL_VERSION,
    })
    await this.#publish(this.topics.result, result)
  }

  #onMessage(topic, payload) {
    if (topic !== this.topics.request) return
    this.#transport.receive(topic, payload, this.topics.result, (bytes) => this.#dispatchRequest(bytes))
      .then((consumed) => { if (!consumed) this.#dispatchRequest(payload) })
      .catch(() => this.logger?.warn('transport_receive_failed', { code: 'invalid_argument' }))
  }

  #dispatchRequest(payload) {
    let request
    try {
      request = this.codec.decode('whalerein.v1.HostRequest', payload)
    } catch {
      this.logger?.warn('request_decode_failed', { code: 'invalid_argument' })
      return
    }
    this.#peerCapable = this.#peerCapable || request.relay_transport_version === TRANSPORT_VERSION
    return Promise.resolve()
      .then(() => this.#handlers.onRequest?.(request))
      .catch(() => this.logger?.error('request_handler_failed', { code: 'internal', request: request.request_id }))
  }

  #scheduleReconnect() {
    if (this.#stopped || this.#reconnectTimer) return
    const base = Math.min(this.#backoffMs, this.reconnectMaxMs)
    const jitter = base * 0.2 * Math.random()
    const delay = Math.round(base + jitter)
    this.#backoffMs = Math.min(this.#backoffMs * 2, this.reconnectMaxMs)
    this.#reconnectTimer = setTimeout(() => {
      this.#reconnectTimer = null
      this.#connect()
    }, delay)
    this.#reconnectTimer.unref?.()
    this.logger?.info('mqtt_reconnect_scheduled', { ms: delay })
  }

  async #publish(topic, payload) {
    const client = this.#client
    if (!client || !this.#connected) throw new PluginError('unavailable', 'mqtt not connected')
    if (payload.length > PACKET_BYTES) {
      this.#publishErrors++
      this.logger?.warn('mqtt_payload_too_large', { code: 'unavailable', kind: topic === this.topics.result ? 'result' : 'event', bytes: payload.length, limit: PACKET_BYTES, generation: this.#generation })
      throw new PluginError('unavailable', 'mqtt payload exceeds application limit')
    }
    const publication = { kind: topic === this.topics.result ? 'result' : 'event', bytes: payload.length }
    const generation = this.#generation
    this.#lastPublish = publication
    this.logger?.debug('mqtt_publish_started', { ...publication, generation })
    let timer
    try {
      await new Promise((resolve, reject) => {
        timer=setTimeout(()=>{
          this.logger?.warn('mqtt_publish_timeout',{code:'unavailable',generation,ms:this.publishTimeoutMs,...publication})
          client.end(true)
          reject(new PluginError('unavailable','MQTT acknowledgement timed out'))
        },this.publishTimeoutMs)
        client.publish(topic, Buffer.from(payload), { qos: 1 }, (error) => (error ? reject(error) : resolve()))
      })
      this.#published++
    } catch {
      this.#publishErrors++
      this.logger?.warn('mqtt_publish_failed', { code: 'unavailable', ...publication, generation })
      throw new PluginError('unavailable', 'mqtt publish failed')
    } finally { clearTimeout(timer) }
  }

  #sendLogical(topic, payload) {
    if (payload.length > MESSAGE_BYTES) this.logger?.warn('mqtt_payload_too_large', {code:'payload_too_large', bytes:payload.length, limit:MESSAGE_BYTES})
    return this.#transport.send(topic, payload, this.topics.request, this.#peerCapable)
  }

  publishResult(payload) {
    return this.#sendLogical(this.topics.result, payload)
  }

  publishEvent(payload) {
    return this.#sendLogical(this.topics.event, payload)
  }

  /** Wait until the first successful connect+subscribe+Ready cycle. */
  waitForConnected(timeoutMs = 5000) {
    if (this.#connected) return Promise.resolve()
    return new Promise((resolve, reject) => {
      const waiter = {
        resolve: () => {
          clearTimeout(timer)
          this.#connectWaiters.delete(waiter)
          resolve()
        },
        reject: (error) => {
          clearTimeout(timer)
          this.#connectWaiters.delete(waiter)
          reject(error)
        },
      }
      const timer = setTimeout(() => {
        this.#connectWaiters.delete(waiter)
        reject(new PluginError('unavailable', 'mqtt connect timeout'))
      }, timeoutMs)
      timer.unref?.()
      this.#connectWaiters.add(waiter)
    })
  }

  describe() {
    return {
      connected: this.#connected,
      reconnects: this.#reconnects,
      published: this.#published,
      publishErrors: this.#publishErrors,
      lastErrorCode: this.#lastErrorCode,
    }
  }
}
