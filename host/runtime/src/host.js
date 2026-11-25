// Plugin lifecycle and the single HostRequest router. It owns start/stop/
// reload, wires the security/command/event/access modules, and accesses native Harness APIs only through the injected SourceAdapter.

import { join } from 'node:path'
import { createLogger, createLogSalt } from './log.js'
import { isErrorCode, PluginError, publicMessage } from './errors.js'
import { PROTOCOL_VERSION } from './config.js'
import { codec as defaultCodec, protocolVersionCompatibility, resolveCodec } from './protocol.js'
import { assertRequestIdentity, validateId } from './contracts/ids.js'
import { isExpired } from './contracts/time.js'
import { lockInstance } from './state/instance-lock.js'
import { StateStore } from './state/store.js'
import { SecurityManager, defaultReceiveAdvertisement } from './security/manager.js'
import { authorizationPublicKey } from './security/host-auth.js'
import { bytesEqual } from './security/digest.js'
import { CommandService } from './commands/service.js'
import { questionResponseBudget } from './commands/question-pager.js'
import { WatchedEvents } from './events/watches.js'
import { MqttAccess } from './access/mqtt.js'
import { loadOrCreateHostIdentity } from './security/identity.js'
import { RELAY_VERSION, validateRequestTransportMetadata, validateBrokerConnectionRef } from './contracts/transport.js'

const EMPTY = new Uint8Array(0)

/** Request kinds this Host understands. An unknown value is a version mismatch. */
const REQUEST_KINDS = new Set([
  'REQUEST_KIND_HANDSHAKE',
  'REQUEST_KIND_QUERY',
  'REQUEST_KIND_COMMAND',
  'REQUEST_KIND_PAIRING_CLAIM',
  'REQUEST_KIND_PAIRING_AUTHORIZE',
  'REQUEST_KIND_REVOCATION',
  'REQUEST_KIND_HOST_CLIENTS',
  'REQUEST_KIND_ACCESS_READY',
])

function sanitizeDetail(value) {
  if (typeof value !== 'string' || value.length === 0) return undefined
  return value.replace(/[\r\n]/g, ' ').slice(0, 200)
}

function requestGeneration(value) {
  if (value === undefined || value === null) return 0
  if (typeof value === 'number') return Number.isSafeInteger(value) && value >= 0 ? value : null
  if (typeof value === 'string' && /^(?:0|[1-9][0-9]*)$/.test(value)) {
    const parsed = Number(value)
    return Number.isSafeInteger(parsed) ? parsed : null
  }
  return null
}

/**
 * The immutable Broker connection scope Access admitted for a post-pairing
 * request. A query or handshake without it fails closed because Host could
 * neither own the observation nor guarantee reply affinity.
 */
function admittedConnectionScope(request) {
  const connectionRef = validateBrokerConnectionRef(request?.connection_ref)
  const grantGeneration = requestGeneration(request?.grant_generation)
  const enrollmentGeneration = requestGeneration(request?.enrollment_generation)
  if (grantGeneration === null || enrollmentGeneration === null) {
    throw new PluginError('invalid_argument', 'request generation invalid')
  }
  return { connectionRef, grantGeneration, enrollmentGeneration }
}

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
  #readyRetryTimer = null
  #readyRetryAttempt = 0
  #channelPruneTimer = null
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
        onClosedChannels: (channels, reason) => {
          this.#events?.onChannelsCleared(channels)
          for (const { deviceId, channelId } of channels) this.#commands?.cancelChannel(deviceId, channelId, reason)
        },
        onRevokedDevices: (deviceIds) => {
          this.#events?.onRevoked?.(deviceIds)
        },
        onRevokedChannels: (channels, deviceIds) => {
          this.#events?.onChannelsCleared(channels)
          for (const deviceId of deviceIds) this.#commands?.revokeDevice(deviceId)
        },
        receiveAdvertisement: () => this.#access?.receiveAdvertisement?.() ?? defaultReceiveAdvertisement(1),
      })
      this.#access = this.injectedAccess ?? new MqttAccess({
        url: this.#config.mqttUrl,
        hostId: this.#config.hostId,
        token: this.#config.hostToken,
        authorizationPublicKey: authorizationPublicKey(identity.privateKey),
        authorizationPrivateKey: identity.privateKey,
        accessOrigin: this.#config.accessUrl ?? '',
        caFile: this.#config.caFile,
        reconnectMinMs: this.#config.reconnectMinMs,
        reconnectMaxMs: this.#config.reconnectMaxMs,
        logger: this.#logger,
        now: this.now,
        codec: this.#codec,
        transportProfile: this.#config.transportProfile ?? 'default',
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
        security: this.#security,
        attachmentPolicy: this.#config.attachmentPolicy ?? null,
        transportProfile: this.#config.transportProfile ?? 'default',
        onAccepted: (request) => this.#reply(request, { status: 'host_accepted' }),
      })
      // Crash recovery happens before any request is accepted.
      this.#commands.recover()
      this.#events.start()
      this.#channelPruneTimer = setInterval(() => this.#security.pruneIdleChannels(), 30_000)
      this.#channelPruneTimer.unref?.()
      this.#access.start({
        onRequest: (request) => this.#track(this.#handleRequest(request)),
        onConnected: () => {
          this.#security.clearChannels()
          this.#readyRetryAttempt = 0
          this.#security.setRevocationsPending(true)
        },
        onReady: () => {
          this.#events.flushOutbox().catch(() => {})
          this.#scheduleRevocationReplay()
        },
        onDisconnected: () => {
          this.#security?.clearChannels()
          this.#logger?.warn('host_offline', { code: 'unavailable' })
        },
        onPresence: (event) => this.#handlePresence(event),
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
  notifyPresence(event) { this.#handlePresence(event) }


  #handlePresence(event) {
    if (event?.event === 'request_stale') return this.#handleRequestStale(event)
    // A connection-scoped lifecycle close replaces the Device Will. Only the
    // observations bound to that exact Broker connection instance end; a
    // delayed close for an old reference can never touch a newer observation.
    if (event?.event === 'connection_closed') {
      return this.#handleConnectionClosed(event)
    }
  }

  #handleConnectionClosed(event) {
    const deviceId = event?.device_id
    if (typeof deviceId !== 'string' || deviceId === '') return
    if (event.host_id !== undefined && event.host_id !== this.#config.hostId) {
      this.#logger?.warn('lifecycle_rejected', { code: 'forbidden', reason: 'host_mismatch' })
      return
    }
    let connectionRef
    try { connectionRef = validateBrokerConnectionRef(event.connection_ref) }
    catch {
      this.#logger?.warn('lifecycle_rejected', { code: 'invalid_argument', reason: 'connection_ref_invalid' })
      return
    }
    if (event.broker_epoch !== undefined && !/^[0-9a-f]{16}$/.test(String(event.broker_epoch))) {
      this.#logger?.warn('lifecycle_rejected', { code: 'invalid_argument', reason: 'broker_epoch_invalid' })
      return
    }
    const closed = this.#security?.closeConnection?.(deviceId, connectionRef) ?? []
    // Even if the channel was already pruned, the exact watch binding still
    // owns the close (for example after a transient channel removal).
    this.#events?.onConnectionClosed?.(deviceId, connectionRef, closed)
    this.presenceEvents += 1
    this.#logger?.info('connection_closed', {
      device: deviceId, channels: closed.length, reason: String(event?.reason ?? '').slice(0, 32),
    })
  }

  // A cancellation notice is only authoritative for its exact existing scope.
  // A foreign Host/device, an unknown peer, a mismatched command/digest or an
  // invalid deadline is rejected without exposing or cancelling anything.
  #handleRequestStale(event) {
    const requestId = event?.request_id
    if (typeof requestId !== 'string' || !requestId) return
    const sessionEnded = event.reason === 'cloud_session_ended'
    if ((sessionEnded || event.host_id !== undefined) && event.host_id !== this.#config.hostId) {
      this.#logger?.warn('presence_rejected', { code: 'forbidden', reason: 'host_mismatch' })
      return
    }
    const active = this.#commands?.requestScope(requestId) ?? null
    const deviceId = sessionEnded ? event.device_id : event.device_id ?? active?.deviceId
    if (typeof deviceId !== 'string' || !deviceId) {
      this.#logger?.warn('presence_rejected', { code: 'invalid_argument', reason: 'device_missing' })
      return
    }
    // An unpaired device cannot own an execution on this Host.
    if (!this.#store?.getPeerKey(deviceId)) {
      this.#logger?.warn('presence_rejected', { code: 'forbidden', reason: 'unknown_device' })
      return
    }
    if (active && active.deviceId !== deviceId) {
      this.#logger?.warn('presence_rejected', { code: 'forbidden', reason: 'device_scope_mismatch' })
      return
    }
    // A deadline alone is context, not a command scope: query/handshake
    // leases can expire without an executable command to fence. The binding
    // becomes mandatory exactly when a command identity or digest is present.
    const hasBinding = event.command_id !== undefined || event.digest !== undefined
    if (hasBinding) {
      const commandId = event.command_id
      const digest = cancellationDigest(event.digest)
      const deadline = Number(event.deadline_unix_ms ?? event.expires_at_unix_ms)
      if (event.host_id !== this.#config.hostId
          || typeof commandId !== 'string' || commandId === '' || commandId.length > 256 || !digest
          || !Number.isSafeInteger(deadline) || deadline <= 0) {
        this.#logger?.warn('presence_rejected', { code: 'invalid_argument', reason: 'binding_invalid' })
        return
      }
      if (active?.commandId && active.commandId !== commandId) {
        this.#logger?.warn('presence_rejected', { code: 'forbidden', reason: 'command_scope_mismatch' })
        return
      }
      if (active?.payloadDigest && !bytesEqual(active.payloadDigest, digest)) {
        this.#logger?.warn('presence_rejected', { code: 'forbidden', reason: 'digest_mismatch' })
        return
      }
      try {
        this.#commands?.fenceCancelledCommand({ deviceId, requestId, commandId, digest, expiresAt: deadline, errorCode: 'unavailable' })
      } catch {
        this.#logger?.warn('presence_fence_failed', { code: 'internal' })
      }
    }
    const reason = sessionEnded ? 'cloud_session_ended' : 'client_poll_expired'
    this.#commands?.cancelRequest(deviceId, requestId, reason)
    this.presenceEvents += 1
    this.#logger?.info('request_stale', { request: requestId, reason: String(event?.reason ?? '').slice(0, 32) })
  }

  // A reconnect can publish HostReady before Access has resubscribed to the
  // result topic, so the one-shot readiness signal and its final revocation
  // replay are lost. Re-announce with bounded backoff until revocations clear.
  #scheduleRevocationReplay() {
    if (this.#readyRetryTimer !== null) clearTimeout(this.#readyRetryTimer)
    if (this.#stopping || !this.#security?.revocationsPending) return
    const delay = Math.min(1000 * 2 ** this.#readyRetryAttempt, 10_000)
    this.#readyRetryTimer = setTimeout(async () => {
      this.#readyRetryTimer = null
      if (this.#stopping || !this.#security?.revocationsPending) return
      if (!this.#access?.connected) return
      this.#logger?.info('host_ready_retry', { attempt: this.#readyRetryAttempt + 1 })
      try {
        await this.#access.publishReady?.()
      } catch {
        // The next scheduled attempt covers a failed re-announcement.
      }
      this.#readyRetryAttempt = Math.min(this.#readyRetryAttempt + 1, 4)
      this.#scheduleRevocationReplay()
    }, delay)
    this.#readyRetryTimer.unref?.()
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
    clearInterval(this.#channelPruneTimer)
    this.#channelPruneTimer = null
    if (this.#readyRetryTimer !== null) {
      clearTimeout(this.#readyRetryTimer)
      this.#readyRetryTimer = null
    }
    this.#readyRetryAttempt = 0
    this.#commands?.closeRequests()
    this.#commands?.closeWaits()
    this.#commands?.closeQuestionAnswers()
    this.#commands?.closeModelQueries()
    this.#commands?.closeAttachments()
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
      try {
        assertRequestIdentity(request)
      } catch (error) {
        // Never echo or log an unbounded identifier; warn with the reason only.
        this.#logger?.warn('request_identity_invalid', { code: codeOf(error), layer: 'host' })
        if (request?.request_id && validateId('request_id', request.request_id).ok) {
          return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument' })
        }
        return
      }
      this.#logger?.info('host_request_received', { trace: request.trace_id, request: request.request_id, kind, bytes: request.body?.length ?? 0, layer: 'host' })
      // One coordinated relay epoch. No v2 frame, unframed body or other epoch
      // ever reaches business handling.
      if (Number(request.relay_version) !== RELAY_VERSION) {
        return await this.#reply(request, {
          status: 'rejected',
          errorCode: 'protocol_version_incompatible',
          errorDetail: `relay epoch is not ${RELAY_VERSION}; upgrade the sender`,
        })
      }
      if (kind === 'REQUEST_KIND_ACCESS_READY') {
        // Control information for the Host -> Access hop; it is recorded by the
        // MQTT adapter and is never business.
        return
      }
      if (kind === 'REQUEST_KIND_UNSPECIFIED') {
        return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument', errorDetail: 'request kind is unspecified' })
      }
      if (typeof kind !== 'string' || !REQUEST_KINDS.has(kind)) {
        return await this.#reply(request, {
          status: 'rejected',
          errorCode: 'protocol_version_incompatible',
          errorDetail: `unsupported request kind ${String(kind).slice(0, 32)}; this Host implements the v${PROTOCOL_VERSION} request kinds and the sender must upgrade`,
        })
      }
      if (kind === 'REQUEST_KIND_REVOCATION') return await this.#handleRevocation(request)
      if (!request.request_id) throw new PluginError('invalid_argument', 'request_id missing')
      if (request.host_id && request.host_id !== this.#config.hostId) {
        return await this.#reply(request, { status: 'rejected', errorCode: 'forbidden' })
      }
      const compatibility = protocolVersionCompatibility(Number(request.protocol_version))
      if (!compatibility.compatible) {
        return await this.#reply(request, { status: 'rejected', errorCode: 'protocol_version_incompatible', errorDetail: compatibility.message })
      }
      if (request.expires_at_unix_ms !== undefined && request.expires_at_unix_ms !== 0 && request.expires_at_unix_ms !== '0') {
        const expires = Number(request.expires_at_unix_ms)
        if (!Number.isSafeInteger(expires) || expires < 0) {
          return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument', errorDetail: 'expires_at_unix_ms is not a safe Unix-millisecond value' })
        }
        if (isExpired(expires, this.now())) {
          return await this.#reply(request, { status: 'rejected', errorCode: 'expired' })
        }
      }
      // Post-pairing handshakes and queries are bound to the Broker connection
      // Access admitted. Host records it on the pending handshake/channel and
      // every observation so a replaced connection can never be revived.
      if (kind === 'REQUEST_KIND_HANDSHAKE' || kind === 'REQUEST_KIND_QUERY') {
        try {
          request.connectionScope = admittedConnectionScope(request)
        } catch {
          this.#logger?.warn('request_transport_rejected', { code: 'invalid_argument', kind, layer: 'host', reason: 'connection_scope' })
          return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument', errorDetail: 'connection scope missing' })
        }
      }
      if (kind === 'REQUEST_KIND_PAIRING_CLAIM' || kind === 'REQUEST_KIND_PAIRING_AUTHORIZE') {
        return await this.#handlePairing(request)
      }
      if (kind === 'REQUEST_KIND_HANDSHAKE') return await this.#handleHandshake(request)
      if (kind === 'REQUEST_KIND_QUERY' || kind === 'REQUEST_KIND_COMMAND') {
        // Request metadata is outside the encrypted body. Validate its shape
        // before forwarding so the result bound and grant stamp cannot be
        // reinterpreted. The values stay untouched for Access authority.
        let requestTransport = null
        if (request.transport !== undefined) {
          try { requestTransport = validateRequestTransportMetadata(request.transport) }
          catch (error) {
            this.#logger?.warn('request_transport_rejected', { code: 'invalid_argument', kind, layer: 'host' })
            return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument', errorDetail: 'request transport metadata invalid' })
          }
        }
        return await this.#security.withRequest(request, opened => this.#handleEncrypted(request, opened, requestTransport))
      }
      if (kind === 'REQUEST_KIND_HOST_CLIENTS') {
        if (request.host_id !== this.#config.hostId || request.device_id) {
          return await this.#reply(request, { status: 'rejected', errorCode: 'forbidden' })
        }
        if (!this.#started || this.#stopping || !this.#access.connected || this.#security.revocationsPending) {
          return await this.#reply(request, { status: 'rejected', errorCode: 'unavailable' })
        }
        const body = this.#codec.encode('whalerein.v1.HostClients', {
          observing_device_ids: this.#events.observingDeviceIds(),
        })
        return await this.#reply(request, { status: 'succeeded', resultBody: body })
      }
      return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument' })
    } catch (error) {
      const code = codeOf(error)
      this.#logger?.error('request_failed', { code, request: request?.request_id, trace: request?.trace_id })
      if (request?.request_id) {
        try {
          await this.#reply(request, { status: 'failed', errorCode: code, errorDetail: error instanceof PluginError ? error.detail : undefined })
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
    // Post-pairing Noise establishment is admitted only after the Host's
    // authorized subscriptions and readiness exist on the current connection.
    if (!this.#started || this.#stopping || !this.#access?.connected || this.#security?.revocationsPending) {
      return await this.#reply(request, { status: 'rejected', errorCode: 'unavailable' })
    }
    try {
      const resultBody = this.#security.handleChannelHandshake(request)
      return await this.#reply(request, { status: 'succeeded', resultBody })
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

  async #handleEncrypted(request, opened, requestTransport = null) {
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
    const enumProblems = this.#codec.unknownEnums('whalerein.v1.RequestPayload', payload)
    if (enumProblems.length > 0) {
      const problem = enumProblems[0]
      return await this.#reply(request, {
        status: 'rejected',
        errorCode: 'protocol_version_incompatible',
        errorDetail: `unknown ${problem.enum} value ${String(problem.value).slice(0, 32)} in ${problem.field}; upgrade the reader`,
      })
    }
    if (!this.#codec.hasOneof('whalerein.v1.RequestPayload', payload, 'payload')) {
      return await this.#reply(request, {
        status: 'failed',
        errorCode: 'invalid_argument',
        errorDetail: 'RequestPayload carries no known payload variant; the sender may use a newer protocol version',
      })
    }
    // The pre-dispatch check above can be outpaced by channel open, queueing or
    // decryption. Re-check the original deadline immediately before any source
    // work; an elapsed query is never dispatched.
    const deadline = Number(request.expires_at_unix_ms ?? 0)
    if (deadline > 0 && isExpired(deadline, this.now())) {
      return await this.#reply(request, { status: 'rejected', errorCode: 'expired' })
    }
    // Local stop fence: once shutdown begins, no fresh read, watch or send may
    // be admitted. A command is journal-fenced so it can never execute later
    // when the same Host resumes; accepted work keeps its recorded outcome.
    if (this.#stopping || !this.#started) {
      if (request.kind === 'REQUEST_KIND_COMMAND') {
        try {
          this.#commands?.fenceCancelledCommand({
            deviceId: request.device_id,
            requestId: request.request_id,
            commandId: request.command_id ?? '',
            digest: request.payload_digest instanceof Uint8Array ? request.payload_digest : EMPTY,
            expiresAt: Number(request.expires_at_unix_ms) || 0,
            errorCode: 'unavailable',
          })
        } catch {
          // the rejection still fences the current attempt
        }
      }
      return await this.#reply(request, { status: 'rejected', errorCode: 'unavailable' })
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
      // A complete query result is admitted whole: if its inline form exceeds
      // the receiver budget the transport stages the exact ciphertext and
      // publishes a reference, so content never fails for a transport reason.
      receiverLimitBytes: requestTransport?.result_max_bytes,
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

  async #reply(request, { status, resultBody, errorCode = 'error_code_unspecified', receiverLimitBytes, completedAt, errorDetail }) {
    // A reply belongs to the connection scope that admitted its request. After
    // revoke, replacement or a connection-scoped lifecycle close, the channel
    // (or pending handshake) is gone and no late result may be published.
    if (!this.#security?.canReplyForRequest?.(request)) {
      this.#logger?.info('host_result_dropped', {
        reason: 'connection_closed', request: request?.request_id, kind: request?.kind,
      })
      return null
    }
    const known = isErrorCode(errorCode)
    if (!known && errorCode !== 'error_code_unspecified') {
      this.#logger?.warn('unknown_error_code', { code: typeof errorCode === 'string' ? errorCode.slice(0, 32) : 'non_string' })
    }
    const error = known ? errorCode : 'internal'
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
      error_message: sanitizeDetail(errorDetail) ?? (error === 'error_code_unspecified' ? '' : publicMessage(error)),
      completed_at_unix_ms: completedAt ?? this.now(),
      protocol_version: PROTOCOL_VERSION,
      channel_id: request.channel_id ?? '',
      relay_version: RELAY_VERSION,
    }
    // A result is published as a complete message. The transport picks the
    // cheap inline representation when it fits the receiver budget and stages
    // the exact ciphertext behind a reference when it does not, so content
    // never fails for a transport reason. `payload_too_large` remains the true
    // hard failure: the envelope cannot be represented even by reference.
    let encoded = this.#codec.encode('whalerein.v1.HostResult', result)
    const replyOptions = {
      envelopeLimit: receiverLimitBytes,
      requestId: request.request_id,
      expiresAt: request.replyDeadline ?? (Number(request.expires_at_unix_ms) || undefined),
      connectionRef: request.connection_ref,
      enrollmentGeneration: Number(request.enrollment_generation) || 0,
    }
    try {
      await this.#access.publishResult(encoded, replyOptions)
    } catch (error) {
      const failureCode = typeof error?.code === 'string' && error.code !== ''
        ? error.code
        : 'internal'
      if (error?.code !== 'payload_too_large') {
        this.#logger?.warn('host_result_publish_failed', {
          code: failureCode, kind: request.kind, bytes: encoded.length,
          reason: typeof error.message === 'string' ? error.message.slice(0, 160) : '',
          layer: 'host',
        })
        result.status = 'failed'
        result.result_body = EMPTY
        result.error_code = isErrorCode(failureCode) ? failureCode : 'internal'
        result.error_message = publicMessage(result.error_code)
        encoded = this.#codec.encode('whalerein.v1.HostResult', result)
        try {
          // The failure envelope is small and inline; it never carries the
          // query object scope that just failed.
          await this.#access.publishResult(encoded, { ...replyOptions, requestId: request.request_id })
        } catch (second) {
          this.#logger?.warn('host_result_failure_unpublishable', {
            code: codeOf(second), kind: request.kind, layer: 'host',
          })
        }
        return result
      }
      this.#logger?.warn('host_result_unrepresentable', {
        code: 'payload_too_large', kind: request.kind, bytes: encoded.length,
        reason: typeof error.message === 'string' ? error.message.slice(0, 160) : '',
        layer: 'host',
      })
      result.status = 'failed'
      result.result_body = EMPTY
      result.error_code = 'payload_too_large'
      result.error_message = publicMessage('payload_too_large')
      encoded = this.#codec.encode('whalerein.v1.HostResult', result)
      await this.#access.publishResult(encoded, replyOptions)
    }
    this.#logger?.info('host_result', { status: result.status, code: result.error_code, request: request.request_id, trace: request.trace_id, kind: request.kind, bytes: encoded.length, layer: 'host' })
    return result
  }

  // --- diagnostics --------------------------------------------------------

  clientDevices() {
    if (!this.#started || this.#stopping || !this.#access.connected || this.#security.revocationsPending) return null
    try {
      const observing = new Set(this.#events.observingDeviceIds())
      return this.#store.listPeerKeys().map(({ deviceId, displayName, os, pairedAt }) => ({
        deviceId, displayName, os, pairedAt, connected: observing.has(deviceId),
      }))
    } catch {
      return null
    }
  }

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
        snapshotRequired: conversation?.snapshotRequired ?? null,
      },
      commands: { unknown: unknownCommands, accepted: acceptedCommands },
      pendingCiphertext: {
        count: this.#store?.countOutbox() ?? 0,
        limit: this.#config.outboxTotalLimit,
        dropped: conversation?.outboxDrops ?? 0,
      },
      activeChannels: channelIds.length,
      attachments: this.#commands?.attachments?.describe?.() ?? null,
    }
  }
}

function cancellationDigest(value) {
  if (typeof value !== 'string' || value.length > 128) return null
  if (/^[a-f0-9]{64}$/i.test(value)) return new Uint8Array(Buffer.from(value, 'hex'))
  try {
    const bytes = Buffer.from(value, 'base64')
    return bytes.length === 32 ? new Uint8Array(bytes) : null
  } catch {
    return null
  }
}

function codeOf(error) {
  if (error instanceof PluginError && isErrorCode(error.code)) return error.code
  if (error && typeof error.code === 'string' && isErrorCode(error.code)) return error.code
  return 'internal'
}
