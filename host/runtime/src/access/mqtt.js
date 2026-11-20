// MQTT 5 over TLS transport. This is the plugin's only network connection:
// no HTTP server and no inbound listener. It subscribes to its own request
// topic and its exact host presence topic, and publishes only its own
// result/event topics.

import { readFileSync } from 'node:fs'
import mqtt from 'mqtt'
import { ConfigError, topics, PROTOCOL_VERSION } from '../config.js'
import { PluginError } from '../errors.js'
import { LogicalTransport, MESSAGE_BYTES, TRANSPORT_VERSION, PACKET_BYTES } from './transport.js'
import { codec as defaultCodec, resolveCodec } from '../protocol.js'
import {
  resolveTransportProfile,
  transportProfileBudget,
  validateReceiveAdvertisement,
  validateTransportBudget,
  validateTransportProfiles,
} from '../contracts/transport.js'

// Leave room for protobuf/MQTT headers and HTTP base64 wrapping in Access.
export const MAX_MQTT_PACKET_BYTES = PACKET_BYTES


const TLS_TRUST_CODES = new Set([
  'CERT_HAS_EXPIRED', 'CERT_NOT_YET_VALID', 'CERT_UNTRUSTED', 'DEPTH_ZERO_SELF_SIGNED_CERT',
  'SELF_SIGNED_CERT_IN_CHAIN', 'UNABLE_TO_VERIFY_LEAF_SIGNATURE', 'UNABLE_TO_GET_ISSUER_CERT',
  'UNABLE_TO_GET_ISSUER_CERT_LOCALLY', 'ERR_TLS_CERT_ALTNAME_INVALID', 'ERR_SSL_WRONG_VERSION_NUMBER',
  'ERR_SSL_TLSV1_ALERT_UNKNOWN_CA', 'ERR_SSL_SSLV3_ALERT_BAD_CERTIFICATE',
])

/** Classify a transport failure for honest status and retry policy. */
export function classifyTransportFailure(errorCode, reasonCode) {
  const reason = Number(reasonCode)
  // MQTT 5 CONNACK: bad user name/password (4/134) or not authorized (5/135).
  if ([4, 5, 134, 135].includes(reason)) return 'unauthorized'
  if (TLS_TRUST_CODES.has(String(errorCode))) return 'tls_trust'
  return 'unavailable'
}

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
   * @param {string|object} [options.transportProfile] packaged profile id or budget.
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
    transportProfile = 'default',
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
    validateTransportProfiles()
    const resolved = resolveTransportProfile(typeof transportProfile === 'string' ? transportProfile : 'default')
    this.profile = resolved.wireName
    this.budget = typeof transportProfile === 'object' && transportProfile !== null
      ? validateTransportBudget(transportProfile)
      : transportProfileBudget(resolved.name)
    this.#transport = new LogicalTransport({
      codec: this.codec,
      onDeliveryError: () => this.logger?.error('logical_delivery_failed', { code: 'internal' }),
      publish: (topic, bytes) => this.#publish(topic, bytes),
      now,
      fragmentPayloadBytes: this.budget.fragment_payload_bytes,
      envelopeBytes: this.budget.envelope_bytes,
      profile: this.profile,
      logger: this.logger,
    })
    this.#verifyControlEnvelopes()
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

  /**
   * Host receive limits for the authenticated broker connection generation.
   * It is rebuilt from the live access generation so a reconnect or a delayed
   * readiness cannot advertise limits for an older connection.
   */
  receiveAdvertisement() {
    const generation = this.#generation > 0 ? this.#generation : 1
    return validateReceiveAdvertisement({
      profile: this.profile,
      budget: this.budget,
      connection_generation: generation,
    })
  }

  #verifyControlEnvelopes() {
    const limit = this.budget.envelope_bytes
    const samples = [
      ['host_ready', 'whalerein.v1.HostReady', {
        host_id: this.hostId, connected_at_unix_ms: 1, transport_version: TRANSPORT_VERSION,
        receive: { profile: this.profile, budget: this.budget, connection_generation: 1 },
      }],
      ['channel_ready', 'whalerein.v1.ChannelReady', {
        host_id: this.hostId, device_id: 'device', protocol_version: PROTOCOL_VERSION,
        capabilities: { capabilities: ['sessions', 'events'], protocol_version: PROTOCOL_VERSION },
        host_nonce: 'nonce',
        receive: { profile: this.profile, budget: this.budget, connection_generation: 1 },
      }],
      ['watch_ready', 'whalerein.v1.WatchReady', {
        watch_id: 'watch', lease_ms: 30000,
        state: { session: { host_id: this.hostId, session_id: 'session' }, live_epoch: 'epoch', revision: 1, history_head: 'head', projection_id: 'projection' },
      }],
      ['watch_closed', 'whalerein.v1.WatchClosed', {
        watch_id: 'watch', session: { host_id: this.hostId, session_id: 'session' }, reason: 'session_deleted',
      }],
      ['history_reset', 'whalerein.v1.HistoryReset', { reason: 'snapshot_required' }],
      ['result_too_large', 'whalerein.v1.HostResult', {
        request_id: 'request', host_id: this.hostId, device_id: 'device', status: 'failed',
        error_code: 'payload_too_large', protocol_version: PROTOCOL_VERSION, channel_id: 'channel',
      }],
      ['attachment_control', 'whalerein.v1.OpenAttachmentResult', {
        session: { host_id: this.hostId, session_id: 'session' }, attachment_ref: 'attachment',
        media_type: 'application/octet-stream', size: 0, transfer_id: 'transfer', expires_at_unix_ms: 1,
        policy: { max_total_bytes: 0, max_chunk_bytes: 1, max_concurrent: 1, idle_timeout_ms: 1, overall_timeout_ms: 1 },
      }],
    ]
    for (const [kind, type, value] of samples) {
      let size
      try { size = this.codec.encode(type, value).length } catch { throw new ConfigError('invalid_config', `control envelope ${kind} is not encodable`) }
      if (size > limit) {
        this.logger?.warn('transport_control_too_large', { code: 'invalid_config', kind, bytes: size, limit })
        throw new ConfigError('invalid_config', `control envelope ${kind} exceeds the receive budget`)
      }
    }
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
    // A deployment that shares 443 between HTTPS and MQTT routes the TLS
    // connection by ALPN. Only offer the MQTT protocol on that explicit port;
    // a normal mqtts://host:8883 deployment keeps a plain TLS handshake.
    if (this.#alpnProtocols()) options.ALPNProtocols = this.#alpnProtocols()
    return options
  }

  #alpnProtocols() {
    try {
      const url = new URL(this.url)
      if (url.protocol === 'mqtts:' && url.port === '443') return ['mqtt']
    } catch {
      // The URL was validated when the credential was accepted.
    }
    return null
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
      const code = classifyTransportFailure(error?.code, this.#disconnectReason)
      this.#lastErrorCode = code
      this.logger?.warn('mqtt_error', {
        code,
        reason: ['ECONNRESET', 'ETIMEDOUT', 'ECONNREFUSED', 'ENOTFOUND', 'EPIPE'].includes(error?.code) ? error.code : 'transport_error',
        generation,
      })
      if (code !== 'unavailable') {
        // A definitive credential or trust failure must not retry forever.
        this.#failDefinitive(code, generation)
        return
      }
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
        const code = classifyTransportFailure(undefined, this.#disconnectReason)
        this.logger?.warn('mqtt_disconnected', { code, reasonCode: this.#disconnectReason, reason: this.#disconnectReason < 0 ? 'transport_closed' : 'broker_disconnect', generation, ...this.#lastPublish })
        this.#handlers.onDisconnected?.()
        if (code !== 'unavailable') {
          this.#failDefinitive(code, generation)
          return
        }
      }
      if (!this.#connected) {
        const code = classifyTransportFailure(undefined, this.#disconnectReason)
        if (code !== 'unavailable') {
          this.#failDefinitive(code, generation)
          return
        }
      }
      this.#scheduleReconnect()
    })
  }

  #failDefinitive(code, generation) {
    if (generation !== this.#generation) return
    this.#stopped = true
    this.#connected = false
    if (this.#reconnectTimer) {
      clearTimeout(this.#reconnectTimer)
      this.#reconnectTimer = null
    }
    for (const waiter of this.#connectWaiters) waiter.reject?.(new PluginError(code, 'transport refused the saved credential or trust configuration'))
    this.#connectWaiters.clear()
    this.#handlers.onFailure?.(code)
    try { this.#client?.end(true) } catch { /* contained */ }
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
        client.subscribe([this.topics.request, this.topics.presence], { qos: 1 }, (error, grants) => (error || grants?.some((grant) => grant.qos >= 128) ? reject(error ?? new Error('subscription rejected')) : resolve()))
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
      await this.#publishReady()
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

  // Re-announce readiness when a reconnect raced Access's own resubscription.
  async publishReady() {
    if (!this.#connected) throw new PluginError('unavailable', 'mqtt not connected')
    await this.#publishReady()
  }

  async #publishReady() {
    const receive = this.receiveAdvertisement()
    const ready = {
      host_id: this.hostId,
      connected_at_unix_ms: this.now(),
      transport_version: TRANSPORT_VERSION,
      receive,
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
    if (topic === this.topics.presence) {
      let event
      try { event = JSON.parse(payload.toString('utf8')) } catch { return }
      if (!['device_offline', 'request_stale'].includes(event?.event)) return
      if (event.event === 'device_offline' && (typeof event?.device_id !== 'string' || !event.device_id)) return
      if (event.event === 'request_stale' && (typeof event?.request_id !== 'string' || !event.request_id)) return
      try { this.#handlers.onPresence?.(event) } catch { /* presence must not break the MQTT loop */ }
      return
    }
    if (topic !== this.topics.request) return
    this.#transport.receive(topic, payload, this.topics.result, (bytes) => this.#dispatchRequest(bytes))
      .then((consumed) => { if (!consumed) this.#dispatchRequest(payload) })
      .catch((error) => this.logger?.warn('transport_receive_failed', {
        code: typeof error?.code === 'string' ? error.code : 'invalid_argument',
        bytes: payload?.length ?? 0,
        limit: PACKET_BYTES,
        generation: this.#generation,
      }))
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
