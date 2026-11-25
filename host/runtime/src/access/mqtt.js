// MQTT 5 over TLS transport. This is the plugin's only network connection:
// no HTTP server and no inbound listener. It subscribes to its own request
// topic and its exact host presence topic, and publishes only its own
// result/event control topics plus the QoS 0 live topic.

import { readFileSync } from 'node:fs'
import mqtt from 'mqtt'
import { ConfigError, topics, PROTOCOL_VERSION } from '../config.js'
import { PluginError } from '../errors.js'
import { RelayTransport } from './transport.js'
import { publishPacketBytes } from './packet.js'
import { RelayObjectClient } from './relay-client.js'
import { codec as defaultCodec, resolveCodec } from '../protocol.js'
import {
  BROKER_PACKET_LIMIT_BYTES,
  RELAY_VERSION,
  resolveTransportProfile,
  transportProfileBudget,
  messageExpirySeconds,
  validateReceiveAdvertisement,
  validateTransportBudget,
  validateTransportProfiles,
} from '../contracts/transport.js'

/**
 * Build the MQTT 5 PUBLISH properties for one deadline-bound reply and their
 * exact encoded length. A reply never carries a Response Topic.
 */
export function replyProperties({ requestId = '', expirySeconds = null } = {}) {
  const properties = {}
  let propertiesBytes = 0
  if (typeof requestId === 'string' && requestId !== '') {
    const correlation = Buffer.from(requestId, 'utf8')
    properties.correlationData = correlation
    propertiesBytes += 1 + 2 + correlation.length
  }
  if (Number.isSafeInteger(expirySeconds) && expirySeconds > 0) {
    properties.messageExpiryInterval = expirySeconds
    propertiesBytes += 1 + 4
  }
  return { properties, propertiesBytes }
}


const TLS_TRUST_CODES = new Set([
  'CERT_HAS_EXPIRED', 'CERT_NOT_YET_VALID', 'CERT_UNTRUSTED', 'DEPTH_ZERO_SELF_SIGNED_CERT',
  'SELF_SIGNED_CERT_IN_CHAIN', 'UNABLE_TO_VERIFY_LEAF_SIGNATURE', 'UNABLE_TO_GET_ISSUER_CERT',
  'UNABLE_TO_GET_ISSUER_CERT_LOCALLY', 'ERR_TLS_CERT_ALTNAME_INVALID', 'ERR_SSL_WRONG_VERSION_NUMBER',
  'ERR_SSL_TLSV1_ALERT_UNKNOWN_CA', 'ERR_SSL_SSLV3_ALERT_BAD_CERTIFICATE',
])

/** MQTT 5 CONNACK reason codes that prove the saved credential is refused. */
const REFUSED_CREDENTIAL_CODES = new Set([4, 5, 134, 135])

/** Classify a transport failure for honest status and retry policy. */
export function classifyTransportFailure(errorCode, reasonCode) {
  // MQTT 5 CONNACK: bad user name/password (4/134) or not authorized (5/135).
  // mqtt@5 reports a refused CONNACK as an `error` event carrying the numeric
  // reason code in `error.code` and never emits a `disconnect` packet, so the
  // numeric code must count wherever it arrives.
  if (REFUSED_CREDENTIAL_CODES.has(Number(reasonCode))) return 'unauthorized'
  if (typeof errorCode === 'number' && REFUSED_CREDENTIAL_CODES.has(errorCode)) return 'unauthorized'
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
  // A trust failure is definitive only before this lane ever succeeded. After a
  // successful connection the broker may still be rotating its leaf, so a trust
  // error during reconnect is retried with bounded backoff instead of stopping
  // the Host's only transport permanently.
  #everConnected = false
  #reconnects = 0
  #attempts = 0
  #published = 0
  #publishErrors = 0
  #lastErrorCode = ''
  #terminalCode = ''
  #handlers = {}
  #disconnectReason = -1
  #transport
  #accessReceive = null
  #lastPublish = { kind: 'none', bytes: 0 }

  /**
   * @param {object} options
   * @param {string} options.url mqtt:// or mqtts:// URL.
   * @param {string} options.hostId
   * @param {string} options.token broker password.
   * @param {string} [options.caFile]
   * @param {string} [options.accessOrigin] configured Access HTTPS origin.
   * @param {Uint8Array} [options.authorizationPrivateKey] Ed25519 seed already
   *   announced in HostReady; used by the Host-authority relay client.
   * @param {object} [options.relayClient] test seam for relay object HTTPS.
   * @param {number} [options.relayObjectTtlMs]
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
    authorizationPrivateKey = null,
    accessOrigin = '',
    relayClient = null,
    relayObjectTtlMs,
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
    if (!relayClient && accessOrigin && authorizationPrivateKey instanceof Uint8Array) {
      relayClient = new RelayObjectClient({
        origin: accessOrigin,
        hostId,
        privateKey: authorizationPrivateKey,
        codec: this.codec,
        now,
        timeoutMs: this.budget.transfer_timeout_ms,
        caFile,
        logger,
      })
    }
    this.#transport = new RelayTransport({
      codec: this.codec,
      relay: relayClient,
      budget: this.budget,
      publish: (topic, bytes, options) => this.#publish(topic, bytes, options),
      now,
      logger: this.logger,
      objectTtlMs: relayObjectTtlMs,
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
      relay_version: RELAY_VERSION,
    })
  }

  /** Access's authenticated receive advertisement for this Hop, or null. */
  accessReceiveAdvertisement() {
    return this.#accessReceive
  }

  /** Effective Host -> Access packet limit after Access advertised its budget. */
  sendPacketLimit() {
    return this.#transport.effectivePacketLimit(this.#accessReceive?.budget?.packet_bytes)
  }

  #verifyControlEnvelopes() {
    const envelopeLimit = this.budget.envelope_bytes
    const samples = [
      ['host_ready', 'whalerein.v1.HostReady', {
        host_id: this.hostId, connected_at_unix_ms: 1,
        receive: {
          profile: this.profile, budget: this.budget,
          connection_generation: 1, relay_version: RELAY_VERSION,
        },
        relay_version: RELAY_VERSION,
      }],
      ['channel_ready', 'whalerein.v1.ChannelReady', {
        host_id: this.hostId, device_id: 'device', protocol_version: PROTOCOL_VERSION,
        capabilities: { capabilities: ['sessions', 'events'], protocol_version: PROTOCOL_VERSION },
        host_nonce: 'nonce',
        receive: {
          profile: this.profile, budget: this.budget,
          connection_generation: 1, relay_version: RELAY_VERSION,
        },
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
        relay_version: RELAY_VERSION,
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
      if (size > envelopeLimit) {
        this.logger?.warn('transport_control_too_large', { code: 'invalid_config', kind, bytes: size, limit: envelopeLimit })
        throw new ConfigError('invalid_config', `control envelope ${kind} exceeds the receive budget`)
      }
    }
    // HostReady is a bootstrap body: it must travel inline in one packet.
    const ready = this.codec.encode('whalerein.v1.HostReady', samples[0][2])
    const result = this.codec.encode('whalerein.v1.HostResult', {
      host_id: this.hostId, status: 'succeeded', result_body: ready,
      completed_at_unix_ms: 1, protocol_version: PROTOCOL_VERSION, relay_version: RELAY_VERSION,
    })
    const packet = publishPacketBytes(this.topics.result, result.length)
    if (packet > this.budget.packet_bytes) {
      this.logger?.warn('transport_control_too_large', {
        code: 'invalid_config', kind: 'host_ready', bytes: packet, limit: this.budget.packet_bytes,
      })
      throw new ConfigError('invalid_config', 'control envelope host_ready exceeds the packet budget')
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

  /** The authorized direct QoS 0 preview topic for one Device. */
  deviceLiveTopic(deviceId) {
    return this.topics.deviceLive(deviceId)
  }

  /** Begin connecting; reconnects continue in the background after failures. */
  start(handlers = {}) {
    if (!this.#stopped) return this
    this.#stopped = false
    this.#terminalCode = ''
    this.#handlers = handlers
    this.#connect()
    return this
  }

  async stop() {
    this.#stopped = true
    this.#connected = false
    this.#generation++
    this.#transport.reset()
    this.#accessReceive = null
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
      properties: { maximumPacketSize: this.budget.packet_bytes },
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
      // Small ordered previews must not wait for a full RTT of Nagle
      // coalescing on a high-latency link. MQTT already delimits packets, so
      // disabling Nagle only removes an artificial per-packet delay.
      try {
        client.stream?.setNoDelay?.(true)
      } catch {
        // A transport without a socket (for example a test double) needs no
        // tuning; the failure cannot affect message semantics.
      }
      this.#onConnected(client, generation, connack)
    })
    client.on('message', (topic, payload, packet) => {
      if (generation !== this.#generation) return
      this.#onMessage(topic, payload, packet?.properties)
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
      this.#accessReceive = null
      const code = classifyTransportFailure(undefined, this.#disconnectReason)
      if (wasConnected) {
        this.logger?.warn('mqtt_disconnected', { code, reasonCode: this.#disconnectReason, reason: this.#disconnectReason < 0 ? 'transport_closed' : 'broker_disconnect', generation, ...this.#lastPublish })
        this.#handlers.onDisconnected?.()
      }
      if (code !== 'unavailable') {
        this.#failDefinitive(code, generation)
        return
      }
      this.#scheduleReconnect()
    })
  }

  #failDefinitive(code, generation) {
    if (generation !== this.#generation || this.#terminalCode !== '') return
    if (code === 'tls_trust' && this.#everConnected && !this.#stopped) {
      this.#lastErrorCode = code
      this.logger?.warn('mqtt_tls_rotation_retry', { code, generation })
      this.#scheduleReconnect()
      return
    }
    this.#terminalCode = code
    this.#lastErrorCode = code
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
    // A smaller broker ceiling lowers the effective limit; it never raises it.
    this.#transport.brokerPacketBytes = Number.isSafeInteger(maximum) && maximum > 0
      ? Math.min(maximum, BROKER_PACKET_LIMIT_BYTES)
      : BROKER_PACKET_LIMIT_BYTES
    this.#accessReceive = null
    this.#connected = true
    this.#everConnected = true
    if (this.#attempts > 1) this.#reconnects++
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
    // Reset the backoff only after the whole ready cycle succeeded. Resetting
    // it at CONNACK would let a broker that accepts the connection but keeps
    // refusing the subscription stay at reconnectMinMs forever.
    this.#backoffMs = this.reconnectMinMs
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
      receive,
      relay_version: RELAY_VERSION,
    }
    if (this.authorizationPublicKey) ready.authorization_public_key = this.authorizationPublicKey
    const body = this.codec.encode('whalerein.v1.HostReady', ready)
    const result = this.codec.encode('whalerein.v1.HostResult', {
      host_id: this.hostId,
      status: 'succeeded',
      result_body: body,
      completed_at_unix_ms: this.now(),
      protocol_version: PROTOCOL_VERSION,
      relay_version: RELAY_VERSION,
    })
    // Readiness is a bootstrap body and must stay one inline packet on the
    // current hop even when the broker ceiling is lower than the local profile.
    const packet = publishPacketBytes(this.topics.result, result.length)
    if (packet > this.#transport.effectivePacketLimit(this.#accessReceive?.budget?.packet_bytes)) {
      throw new PluginError('protocol_version_incompatible', 'HostReady exceeds the hop packet budget')
    }
    await this.#publish(this.topics.result, result)
  }

  #onMessage(topic, payload, properties) {
    if (topic === this.topics.presence) {
      let event
      try { event = JSON.parse(payload.toString('utf8')) } catch { return }
      if (!['connection_closed', 'request_stale'].includes(event?.event)) return
      if (event.event === 'connection_closed' && (typeof event?.device_id !== 'string' || !event.device_id)) return
      if (event.event === 'request_stale' && (typeof event?.request_id !== 'string' || !event.request_id)) return
      try { this.#handlers.onPresence?.(event) } catch { /* presence must not break the MQTT loop */ }
      return
    }
    if (topic !== this.topics.request) return
    const receivedAt = this.now()
    this.#transport.receiveRequest(payload)
      .then((request) => {
        if (request.kind === 'REQUEST_KIND_QUERY' || request.kind === 'REQUEST_KIND_HANDSHAKE') {
          const expiry = properties?.messageExpiryInterval
          if (!Number.isSafeInteger(expiry) || expiry <= 0) return
          request.replyDeadline = Math.min(Number(request.expires_at_unix_ms), receivedAt + expiry * 1000)
          if (request.replyDeadline <= this.now()) return
        }
        return this.#dispatchRequest(request)
      })
      .catch((error) => this.logger?.warn('transport_receive_failed', {
        code: typeof error?.code === 'string' ? error.code : 'invalid_argument',
        bytes: payload?.length ?? 0,
        limit: this.budget.packet_bytes,
        generation: this.#generation,
      }))
  }

  #dispatchRequest(request) {
    // The Access receive advertisement for the Host -> Access hop is control
    // information, never business. It is scoped to this authenticated broker
    // generation and cannot raise a local limit.
    if (request?.kind === 'REQUEST_KIND_ACCESS_READY') {
      this.#adoptAccessReady(request)
      return
    }
    return Promise.resolve()
      .then(() => this.#handlers.onRequest?.(request))
      .catch(() => this.logger?.error('request_handler_failed', { code: 'internal', request: request?.request_id }))
  }

  #adoptAccessReady(request) {
    let advertisement
    try {
      advertisement = this.codec.decode('whalerein.v1.ReceiveAdvertisement', request.body)
      advertisement = validateReceiveAdvertisement(advertisement)
    } catch (error) {
      this.logger?.warn('access_ready_rejected', {
        code: typeof error?.code === 'string' ? error.code : 'invalid_argument',
        generation: this.#generation,
      })
      return
    }
    this.#accessReceive = advertisement
    this.logger?.info('access_receive_advertised', {
      generation: advertisement.connection_generation,
      packet_bytes: advertisement.budget.packet_bytes,
      envelope_bytes: advertisement.budget.envelope_bytes,
    })
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

  #kindFor(topic) {
    if (topic === this.topics.result) return 'result'
    if (typeof topic === 'string' && topic.endsWith('/live')) return 'live'
    return 'event'
  }

  async #publish(topic, payload, { qos = 1, properties } = {}) {
    const client = this.#client
    if (!client || !this.#connected) throw new PluginError('unavailable', 'mqtt not connected')
    if (payload.length > this.budget.envelope_bytes) {
      this.#publishErrors++
      this.logger?.warn('mqtt_payload_too_large', { code: 'payload_too_large', kind: this.#kindFor(topic), bytes: payload.length, limit: this.budget.envelope_bytes, generation: this.#generation })
      throw new PluginError('payload_too_large', 'mqtt payload exceeds the envelope budget')
    }
    const publication = { kind: this.#kindFor(topic), bytes: payload.length }
    const generation = this.#generation
    this.#lastPublish = publication
    this.logger?.debug('mqtt_publish_started', { ...publication, qos, generation })
    let timer
    try {
      await new Promise((resolve, reject) => {
        const options = { qos, ...(properties && Object.keys(properties).length > 0 ? { properties } : {}) }
        if (qos === 0) {
          // Live is fire-and-forget: the packet is released when the client
          // accepts it. There is no PUBACK timer and no acknowledgement error.
          client.publish(topic, Buffer.from(payload), options, (error) => (error ? reject(error) : resolve()))
          return
        }
        timer = setTimeout(() => {
          this.logger?.warn('mqtt_publish_timeout', { code: 'unavailable', generation, ms: this.publishTimeoutMs, ...publication })
          client.end(true)
          reject(new PluginError('unavailable', 'MQTT acknowledgement timed out'))
        }, this.publishTimeoutMs)
        client.publish(topic, Buffer.from(payload), options, (error) => (error ? reject(error) : resolve()))
      })
      this.#published++
    } catch {
      this.#publishErrors++
      this.logger?.warn('mqtt_publish_failed', { code: 'unavailable', qos, ...publication, generation })
      throw new PluginError('unavailable', 'mqtt publish failed')
    } finally { clearTimeout(timer) }
  }

  /**
   * Publish one complete HostResult, inline or as a ready reference.
   *
   * A deadline-bound reply carries Correlation Data = request_id and the
   * positive ceiling of the remaining original deadline as Message Expiry
   * Interval, capped by a shorter upstream value. It never carries a Response
   * Topic. A reply whose deadline elapsed before publication is skipped.
   */
  publishResult(payload, options = {}) {
    const expiresAt = Number.isSafeInteger(options.expiresAt) && options.expiresAt > 0
      ? options.expiresAt
      : this.now() + this.#transport.objectTtlMs
    const expirySeconds = messageExpirySeconds(expiresAt, this.now(), options.upstreamExpirySeconds)
    if (expirySeconds === 0) {
      this.logger?.info('mqtt_reply_skipped', { reason: 'expired' })
      return Promise.resolve({ skipped: true, representation: 'none' })
    }
    const { properties, propertiesBytes } = replyProperties({
      requestId: options.requestId,
      expirySeconds,
    })
    return this.#transport.sendComplete(this.topics.result, payload, {
      kind: 'result',
      receiverLimit: this.#accessReceive?.budget?.packet_bytes,
      ...options,
      expiresAt,
      properties,
      propertiesBytes,
    })
  }

  /** Publish one complete control EventEnvelope, inline or as a ready reference. */
  publishEvent(payload) {
    return this.#transport.sendComplete(this.topics.event, payload, {
      kind: 'event',
      receiverLimit: this.#accessReceive?.budget?.packet_bytes,
      expiresAt: this.now() + this.#transport.objectTtlMs,
    })
  }

  /**
   * Publish one complete live preview at QoS 0 directly on the authorized
   * Device topic.
   *
   * Resolves after the client accepts the packet, or false when the preview
   * cannot be carried. It never waits for an acknowledgement, never stages a
   * reference and never retries. Preview delivery does not depend on Access
   * advertising receive limits; the receiving Device's own ChannelHello budget
   * is applied by the caller's per-watch route.
   */
  publishLive(payload, { deviceId = '' } = {}) {
    if (typeof deviceId !== 'string' || deviceId === '') return Promise.resolve(false)
    return this.#transport.sendLive(this.deviceLiveTopic(deviceId), payload, {})
  }

  /** Wait until the first successful connect+subscribe+Ready cycle. */
  waitForConnected(timeoutMs = 5000) {
    if (this.#connected) return Promise.resolve()
    if (this.#terminalCode) return Promise.reject(new PluginError(this.#terminalCode, 'transport refused the saved credential or trust configuration'))
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
