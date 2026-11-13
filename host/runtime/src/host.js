// Plugin lifecycle and the single HostRequest router. It owns start/stop/
// reload, wires the security/command/event/access modules, and accesses native Harness APIs only through the injected SourceAdapter.

import { join } from 'node:path'
import { createLogger, createLogSalt } from './log.js'
import { isErrorCode, PluginError, publicMessage } from './errors.js'
import { PROTOCOL_VERSION } from './config.js'
import { codec as defaultCodec, resolveCodec } from './protocol.js'
import { lockInstance } from './state/instance-lock.js'
import { StateStore } from './state/store.js'
import { SecurityManager } from './security/manager.js'
import { authorizationPublicKey } from './security/host-auth.js'
import { CommandService } from './commands/service.js'
import { questionResponseBudget } from './commands/question-pager.js'
import { WatchedEvents } from './events/watches.js'
import { MqttAccess } from './access/mqtt.js'
import { MESSAGE_BYTES } from './access/transport.js'
import { loadOrCreateHostIdentity } from './security/identity.js'

const EMPTY = new Uint8Array(0)

export class HostRuntime {
  #started = false
  #starting = null
  #stopping = false
  #inFlight = new Set()
  #instanceLock = null
  #ownsStore = false
  #logger = null
  #store = null
  #adapter = null
  #security = null
  #events = null
  #commands = null
  presenceEvents = 0
  #access = null
  #codec = null
  #config

  /**
   * @param {object} options
   * @param {object} options.config resolved config from config.js.
   * @param {object} [options.adapter] adapter instance (tests/harness).
   * @param {object} [options.store] StateStore instance (tests).
   * @param {object} [options.access] MqttAccess-compatible instance (tests).
   * @param {object} [options.logger]
   * @param {() => number} [options.now]
   * @param {import('./protocol.js').ProtoCodec} [options.codec]
   */
  constructor({ config, adapter, credentials, store, access, logger, now = Date.now, codec = defaultCodec }) {
    this.#config = config
    this.credentials = credentials
    this.injectedAdapter = adapter ?? null
    this.injectedStore = store ?? null
    this.injectedAccess = access ?? null
    this.injectedLogger = logger ?? null
    this.now = now
    this.#codec = resolveCodec(codec)
  }

  get config() {
    return this.#config
  }

  get started() {
    return this.#started
  }

  get security() {
    return this.#security
  }

  get store() {
    return this.#store
  }

  get events() {
    return this.#events
  }

  get adapter() {
    return this.#adapter
  }

  get access() {
    return this.#access
  }

  async start() {
    if (this.#started) return this
    if (this.#starting) return this.#starting
    this.#starting = this.#start()
    try {
      return await this.#starting
    } finally {
      this.#starting = null
    }
  }

  async #start() {
    this.#stopping = false
    try {
      if (!this.injectedStore && !this.#instanceLock) this.#instanceLock = lockInstance(this.#config.stateDir)
      if (this.#store) {
        // Reload keeps the already-open store.
        this.#ownsStore = !this.injectedStore
      } else if (this.injectedStore) {
        this.#store = this.injectedStore
        this.#ownsStore = false
      } else {
        this.#store = new StateStore({ path: join(this.#config.stateDir, 'state.sqlite'), now: this.now })
        this.#ownsStore = true
      }
      const salt = this.#store.getMeta('log_salt') ?? createLogSalt()
      if (this.#store.getMeta('log_salt') === null) this.#store.setMeta('log_salt', salt)
      this.#logger = this.injectedLogger ?? createLogger({ salt, level: this.#config.logLevel, now: this.now })

      this.#adapter = this.injectedAdapter
      if (!this.#adapter || !this.credentials) throw new PluginError('invalid_argument', 'source and host credentials are required')
      await this.#adapter.open({ metadata: { get: key => this.#store.getMeta(`source:${key}`), set: (key, value) => this.#store.setMeta(`source:${key}`, value) } })
      const identity = await loadOrCreateHostIdentity({ credentials: this.credentials, store: this.#store, logger: this.#logger })
      this.#security = new SecurityManager({
        hostId: this.#config.hostId,
        identity,
        store: this.#store,
        logger: this.#logger,
        now: this.now,
        protocolVersion: PROTOCOL_VERSION,
        codec: this.#codec,
        onRevokedChannels: channels => {
          this.#events?.onChannelsCleared(channels)
          for (const { deviceId, channelId } of channels) this.#commands?.cancelChannel(deviceId, channelId, 'revoked')
        },
      })
      this.#access = this.injectedAccess ?? new MqttAccess({
        url: this.#config.mqttUrl,
        hostId: this.#config.hostId,
        token: this.#config.hostToken,
        authorizationPublicKey: authorizationPublicKey(identity.privateKey),
        caFile: this.#config.caFile,
        reconnectMinMs: this.#config.reconnectMinMs,
        reconnectMaxMs: this.#config.reconnectMaxMs,
        logger: this.#logger,
        now: this.now,
        codec: this.#codec,
      })
      this.#events = new WatchedEvents({
        hostId: this.#config.hostId,
        store: this.#store,
        security: this.#security,
        access: this.#access,
        adapter: this.#adapter,
        config: this.#config,
        logger: this.#logger,
        now: this.now,
        codec: this.#codec,
      })
      this.#commands = new CommandService({
        hostId: this.#config.hostId,
        store: this.#store,
        adapter: this.#adapter,
        events: this.#events,
        logger: this.#logger,
        now: this.now,
        codec: this.#codec,
        onAccepted: (request) => this.#reply(request, { status: 'host_accepted' }),
      })
      // Crash recovery happens before any request is accepted.
      this.#commands.recover()
      this.#events.start()
      this.#access.start({
        onRequest: (request) => this.#track(this.#handleRequest(request)),
        onConnected: () => {
          this.#clearChannels()
          this.#security.setRevocationsPending(true)
        },
        onReady: () => {
          this.#events.flushOutbox().catch(() => {})
        },
        onDisconnected: () => {
          this.#clearChannels()
          this.#logger?.warn('host_offline', { code: 'unavailable' })
        },
        onPresence: (event) => this.#handleDeviceOffline(event),
      })
      this.#started = true
      this.#logger.info('plugin_started', { status: 'started' })
      return this
    } catch (error) {
      this.#logger?.error('plugin_start_failed', { code: codeOf(error) })
      await this.#teardown({ closeStore: this.#ownsStore, force: true })
      throw error instanceof PluginError ? error : new PluginError(codeOf(error), 'plugin start failed')
    }
  }

  /** Normalized presence entry point (also used by the integration fixture). */
  notifyPresence(event) { this.#handleDeviceOffline(event) }


  #handleDeviceOffline(event) {
    if (event?.event === 'request_stale') {
      const requestId = event?.request_id
      if (typeof requestId !== 'string' || !requestId) return
      this.#commands?.cancelRequest(requestId, 'client_poll_expired')
      this.presenceEvents += 1
      this.#logger?.info('request_stale', { request: requestId, reason: String(event?.reason ?? '').slice(0, 32) })
      return
    }
    const deviceId = event?.device_id
    if (typeof deviceId !== 'string' || !deviceId) return
    const closed = this.#security?.clearDeviceChannels(deviceId) ?? []
    this.#events?.onChannelsCleared(closed)
    this.#commands?.cancelDevice(deviceId, 'device_offline')
    this.presenceEvents += 1
    this.#logger?.info('device_offline', { device: deviceId, reason: String(event?.reason ?? '').slice(0, 32), closed: closed.length })
  }

  #clearChannels() {
    const closed = this.#security?.clearChannels() ?? []
    this.#events?.onChannelsCleared(closed)
    for (const { deviceId, channelId } of closed) this.#commands?.cancelChannel(deviceId, channelId)
    return closed
  }

  #track(promise) {
    this.#inFlight.add(promise)
    promise.then(
      () => this.#inFlight.delete(promise),
      () => this.#inFlight.delete(promise),
    )
    return promise
  }

  /** Stop network, subscriptions, and timers; leave state consistent. */
  async stop({ keepStore = false } = {}) {
    const starting = this.#starting
    if (!this.#started && !this.#adapter && !starting) return
    this.#stopping = true
    if (starting) {
      try {
        await starting
      } catch {
        // a failed start already tore itself down
      }
    }
    this.#logger?.info('plugin_stopping', {})
    await this.#teardown({ closeStore: this.#ownsStore && !keepStore, force: false })
    this.#logger?.info('plugin_stopped', { status: 'stopped' })
    this.#started = false
  }

  async #teardown({ closeStore, force }) {
    this.#commands?.closeRequests()
    this.#commands?.closeWaits()
    this.#commands?.closeQuestionAnswers()
    this.#commands?.closeModelQueries()
    try {
      await this.#access?.stop()
    } catch {
      // already stopped
    }
    // Wait briefly for in-flight requests so terminal results are not lost.
    if (!force && this.#inFlight.size > 0) {
      await Promise.race([
        Promise.allSettled([...this.#inFlight]),
        new Promise((resolve) => {
          const timer = setTimeout(resolve, this.#config.shutdownTimeoutMs)
          timer.unref?.()
        }),
      ])
    }
    try {
      await this.#events?.stop()
    } catch {
      // contained
    }
    try {
      await this.#adapter?.close()
    } catch {
      // contained
    }
    // Any command still accepted was not confirmed: report unknown, never success.
    try {
      this.#commands?.recover()
    } catch {
      // storage may already be closed only if teardown was forced twice
    }
    if (closeStore) {
      try {
        this.#store?.close()
      } catch {
        // contained
      }
      this.#ownsStore = false
    }
    this.#instanceLock?.close()
    this.#instanceLock = null
    this.#access = null
    this.#events = null
    this.#commands = null
    this.#security = null
    this.#adapter = null
    if (closeStore) this.#store = null
    this.#inFlight.clear()
  }

  async reload(nextConfig = null) {
    const keepStore = this.#ownsStore && this.#store !== null
    this.#logger?.info('plugin_reloading', {})
    await this.stop({ keepStore })
    if (nextConfig) this.#config = { ...this.#config, ...nextConfig }
    return this.start()
  }

  // --- request routing ----------------------------------------------------

  async #handleRequest(request) {
    try {
      const kind = request.kind
      this.#logger?.info('host_request_received', { trace: request.trace_id, request: request.request_id, kind, bytes: request.body?.length ?? 0, layer: 'host' })
      if (kind === 'REQUEST_KIND_REVOCATION') return await this.#handleRevocation(request)
      if (!request.request_id) throw new PluginError('invalid_argument', 'request_id missing')
      if (request.host_id && request.host_id !== this.#config.hostId) {
        return await this.#reply(request, { status: 'rejected', errorCode: 'forbidden' })
      }
      const needsVersion = kind === 'REQUEST_KIND_PAIRING_CLAIM' || kind === 'REQUEST_KIND_PAIRING_AUTHORIZE'
        || kind === 'REQUEST_KIND_HANDSHAKE' || kind === 'REQUEST_KIND_QUERY' || kind === 'REQUEST_KIND_COMMAND'
      if ((kind === 'REQUEST_KIND_PAIRING_CLAIM' || kind === 'REQUEST_KIND_PAIRING_AUTHORIZE') && request.relay_transport_version !== 2) {
        return await this.#reply(request, { status: 'rejected', errorCode: 'protocol_version_incompatible' })
      }
      if (needsVersion && Number(request.protocol_version) !== PROTOCOL_VERSION) {
        return await this.#reply(request, { status: 'rejected', errorCode: 'protocol_version_incompatible' })
      }
      if (request.expires_at_unix_ms && request.expires_at_unix_ms <= this.now()) {
        return await this.#reply(request, { status: 'rejected', errorCode: 'expired' })
      }
      if (kind === 'REQUEST_KIND_PAIRING_CLAIM' || kind === 'REQUEST_KIND_PAIRING_AUTHORIZE') {
        return await this.#handlePairing(request)
      }
      if (kind === 'REQUEST_KIND_HANDSHAKE') return await this.#handleHandshake(request)
      if (kind === 'REQUEST_KIND_QUERY' || kind === 'REQUEST_KIND_COMMAND') return await this.#handleEncrypted(request)
      return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument' })
    } catch (error) {
      const code = codeOf(error)
      this.#logger?.error('request_failed', { code, request: request?.request_id, trace: request?.trace_id })
      if (request?.request_id) {
        try {
          await this.#reply(request, { status: 'failed', errorCode: code })
        } catch {
          // access down; Access times the request out on its own
        }
      }
    }
  }

  async #handlePairing(request) {
    let claim
    try {
      claim = this.#codec.decode('whalerein.v1.PairingClaim', request.body)
    } catch {
      return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument' })
    }
    const authorize = claim?.phase === 'PAIRING_PHASE_AUTHORIZE' || claim?.phase === 'PAIRING_PHASE_COMMIT'
    if ((request.kind === 'REQUEST_KIND_PAIRING_CLAIM' && authorize)
      || (request.kind === 'REQUEST_KIND_PAIRING_AUTHORIZE' && !authorize)) {
      return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument' })
    }
    try {
      const result = this.#security.handlePairingClaim({ request, claim })
      const body = this.#codec.encode('whalerein.v1.PairingClaimResult', result)
      return await this.#reply(request, { status: 'succeeded', resultBody: body })
    } catch (error) {
      const code = codeOf(error)
      this.#logger?.warn('pairing_rejected', { code, phase: claim?.phase })
      return await this.#reply(request, { status: 'rejected', errorCode: code })
    }
  }

  async #handleHandshake(request) {
    try {
      const outcome = this.#security.handleChannelHandshake({ request, message: request.body })
      if (outcome.established && outcome.replacedChannelId) {
        this.#events.onChannelReplaced(request.device_id, outcome.replacedChannelId)
        this.#commands.cancelChannel(request.device_id, outcome.replacedChannelId, 'channel_replaced')
      }
      return await this.#reply(request, { status: 'succeeded', resultBody: outcome.noiseMessage })
    } catch (error) {
      const code = codeOf(error)
      this.#logger?.warn('handshake_rejected', { code, device: request.device_id, channel: request.channel_id })
      return await this.#reply(request, { status: 'rejected', errorCode: code })
    }
  }

  async #handleRevocation(request) {
    let notice
    try {
      notice = this.#codec.decode('whalerein.v1.RevocationNotice', request.body)
    } catch {
      return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument' })
    }
    try {
      const outcome = this.#security.handleRevocation({ notice })
      const body = this.#codec.encode('whalerein.v1.RevocationAck', outcome.ack)
      return await this.#reply(request, { status: 'succeeded', resultBody: body })
    } catch (error) {
      this.#logger?.error('revocation_failed', { code: codeOf(error) })
      return await this.#reply(request, { status: 'failed', errorCode: codeOf(error) })
    }
  }

  async #handleEncrypted(request) {
    const opened = this.#security.openRequest({ request, body: request.body })
    if (!opened.ok) {
      this.#logger?.warn('request_frame_rejected', { trace: request.trace_id, request: request.request_id, device: request.device_id, channel: request.channel_id, code: opened.code, reason: opened.reason, layer: 'host', scope: 'channel' })
      return await this.#reply(request, { status: 'rejected', errorCode: opened.code })
    }
    let payload
    try {
      payload = this.#codec.decode('whalerein.v1.RequestPayload', opened.plaintext)
    } catch {
      return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument' })
    }
    let completedAt
    const questionBudget = payload.list_questions ? () => {
      completedAt = this.now()
      return questionResponseBudget(this.#codec, request, this.#config.hostId, completedAt, PROTOCOL_VERSION)
    } : undefined
    const result = await this.#commands.handle({ request, payload, questionResponseBudget: questionBudget })
    const responseBody = this.#encryptResult(request, result)
    if (result.sourceFailure && (!(responseBody instanceof Uint8Array) || responseBody.length === 0)) {
      return await this.#reply(request, { status: 'failed', errorCode: 'unavailable' })
    }
    if (result.status === 'succeeded' && (!(responseBody instanceof Uint8Array) || responseBody.length === 0)) {
      // A query cannot succeed without an authenticated response; for a
      // completed command the effect may already exist, so retain uncertainty.
      return this.#reply(request, { status:request.kind === 'REQUEST_KIND_QUERY' ? 'failed' : 'unknown', errorCode:'internal', completedAt })
    }
    return await this.#reply(request, {
      status: result.status,
      resultBody: responseBody ?? undefined,
      errorCode: result.errorCode,
      enforceResponseBudget: Boolean(payload.list_models || payload.read_session_model || payload.read_pending_queue || payload.list_questions || payload.get_question || payload.wait_session || payload.read_wait_state),
      completedAt,
    })
  }

  #encryptResult(request, result) {
    try {
      if (result.resultPayload instanceof Uint8Array) {
        return this.#security.sealResult({ request, plaintext: result.resultPayload })
      }
      if (result.response) {
        const encoded = this.#codec.encode('whalerein.v1.ResponsePayload', result.response)
        return this.#security.sealResult({ request, plaintext: encoded })
      }
    } catch (error) {
      // Keep the failure attributable without logging payloads or sizes.
      this.#logger?.warn('host_result_encrypt_failed', { code: error?.code ?? 'internal', kind: request.kind })
      return null
    }
    return null
  }

  async #reply(request, { status, resultBody, errorCode = 'error_code_unspecified', enforceResponseBudget = false, completedAt }) {
    const error = isErrorCode(errorCode) ? errorCode : 'internal'
    const result = {
      request_id: request.request_id ?? '',
      host_id: this.#config.hostId,
      device_id: request.device_id ?? '',
      status,
      command_id: request.command_id ?? '',
      payload_digest: request.payload_digest instanceof Uint8Array ? request.payload_digest : EMPTY,
      result_body: resultBody instanceof Uint8Array ? resultBody : EMPTY,
      error_code: error,
      trace_id: request.trace_id ?? '',
      error_message: error === 'error_code_unspecified' ? '' : publicMessage(error),
      completed_at_unix_ms: completedAt ?? this.now(),
      protocol_version: PROTOCOL_VERSION,
      channel_id: request.channel_id ?? '',
    }
    let encoded = this.#codec.encode('whalerein.v1.HostResult', result)
    if (enforceResponseBudget && encoded.length > MESSAGE_BYTES) {
      result.status = 'failed'
      result.result_body = EMPTY
      result.error_code = 'payload_too_large'
      result.error_message = publicMessage('payload_too_large')
      encoded = this.#codec.encode('whalerein.v1.HostResult', result)
    }
    await this.#access.publishResult(encoded)
    this.#logger?.info('host_result', { status: result.status, code: result.error_code, request: request.request_id, trace: request.trace_id, kind: request.kind, bytes: encoded.length, layer: 'host' })
    return result
  }

  // --- diagnostics --------------------------------------------------------

  /** Secret-free runtime summary for `doctor`-style local diagnostics. */
  diagnostics() {
    const channelIds = this.#security?.listChannels() ?? []
    const conversation = this.#events?.describe()
    let unknownCommands = 0
    let acceptedCommands = 0
    try {
      unknownCommands = this.#store?.countCommands('unknown') ?? 0
      acceptedCommands = this.#store?.countCommands('accepted') ?? 0
    } catch {
      unknownCommands = 0
    }
    return {
      source: this.#adapter?.describe() ?? { mode: 'none', available: false },
      access: this.#access?.describe() ?? { connected: false },
      revocationsPending: this.#security?.revocationsPending ?? false,
      cursors: {
        sessions: conversation?.sessions ?? null,
        snapshotRequired: null,
      },
      commands: { unknown: unknownCommands, accepted: acceptedCommands },
      pendingCiphertext: {
        count: this.#store?.countOutbox() ?? 0,
        limit: this.#config.outboxTotalLimit,
      },
      activeChannels: channelIds.length,
    }
  }
}

function codeOf(error) {
  if (error instanceof PluginError && isErrorCode(error.code)) return error.code
  if (error && typeof error.code === 'string' && isErrorCode(error.code)) return error.code
  return 'internal'
}
