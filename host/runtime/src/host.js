// Plugin lifecycle and the single HostRequest router. It owns start/stop/
// reload, wires the security/command/event/access modules, and accesses native Harness APIs only through the injected SourceAdapter.

import { join } from 'node:path'
import { hostname } from 'node:os'
import { createLogger, createLogSalt } from './log.js'
import { DefinitiveExecutionError, isErrorCode, PluginError, publicMessage } from './errors.js'
import { PROTOCOL_VERSION } from './config.js'
import { hostOsFamily } from './os.js'
import { codec as defaultCodec, protocolVersionCompatibility, resolveCodec } from './protocol.js'
import { assertRequestIdentity, validateId } from './contracts/ids.js'
import { isExpired } from './contracts/time.js'
import { lockInstance } from './state/instance-lock.js'
import { StateStore } from './state/store.js'
import { SecurityManager, defaultReceiveAdvertisement } from './security/manager.js'
import { authorizationPublicKey } from './security/host-auth.js'
import { AuthorityStore } from './security/authority-store.js'
import { verifySessionProof } from './security/authority.js'
import { bytesEqual, sha256Bytes } from './security/digest.js'
import { classifyOperation, DeviceTrustStore } from './security/grants.js'
import { CommandJournal } from './security/command-journal.js'
import { admitDeadline, isAdmissionFresh } from './security/deadline.js'
import { HostDeviceTrustService } from './management/device-trust.js'
import { CommandService } from './commands/service.js'
import { questionResponseBudget } from './commands/question-pager.js'
import { WatchedEvents } from './events/watches.js'
import { MqttAccess } from './access/mqtt.js'
import { loadOrCreateHostIdentity } from './security/identity.js'
import { RELAY_VERSION, validateRequestTransportMetadata, validateBrokerConnectionRef } from './contracts/transport.js'

const EMPTY = new Uint8Array(0)

/** Journal state to the wire OnlineStatus vocabulary. */
const JOURNAL_STATE_TO_PROTO = Object.freeze({
  reserved: 'host_accepted',
  succeeded: 'succeeded',
  failed: 'failed',
  unknown: 'unknown',
})

/** Integer milliseconds from the Host monotonic clock when available. */
function monotonicClock() {
  if (typeof performance !== 'undefined' && typeof performance.now === 'function') {
    return Math.floor(performance.now())
  }
  return Date.now()
}

/** The single selected RequestPayload variant, or '' when none is present. */
function requestOperation(payload) {
  if (!payload || typeof payload !== 'object') return ''
  for (const [key, value] of Object.entries(payload)) {
    if (value !== undefined && value !== null) return key
  }
  return ''
}

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
  'REQUEST_KIND_AUTHORITY',
  'REQUEST_KIND_SESSION_PROOF',
  'REQUEST_KIND_SELF_STATUS',
])

/**
 * Bounded self-status frame contract. The values mirror the Access service
 * lane: a grant claim that fits the shared resource-id grammar and a fresh
 * challenge. The body ceiling matches Access's serialized frame ceiling.
 */
const SELF_STATUS_MAX_BODY_BYTES = 512
const SELF_STATUS_GRANT_ID = /^[A-Za-z0-9_.:-]{1,128}$/
const SELF_STATUS_MIN_CHALLENGE_BYTES = 8
const SELF_STATUS_MAX_CHALLENGE_BYTES = 64

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
  #authorities = null
  #trust = null
  #journal = null
  #trustService = null
  #monotonicNow = null
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
   * @param {(url: string, opts: object) => object} [options.accessConnect] test seam for the runtime-owned MqttAccess.
   * @param {object} [options.logger]
   * @param {() => number} [options.now]
   * @param {() => number} [options.monotonicNow] Host monotonic clock in ms.
   * @param {import('./protocol.js').ProtoCodec} [options.codec]
   * @param {object} [options.observation] shared process outlet (trace mode).
   */
  constructor({ config, adapter, credentials, store, access, accessConnect = null, logger, now = Date.now, monotonicNow = monotonicClock, codec = defaultCodec, observation = null }) {
    this.#config = config
    this.credentials = credentials
    this.injectedAdapter = adapter ?? null
    this.injectedStore = store ?? null
    this.injectedAccess = access ?? null
    this.injectedAccessConnect = accessConnect ?? null
    this.injectedLogger = logger ?? null
    this.now = now
    this.#monotonicNow = monotonicNow
    this.observation = observation
    this.#codec = resolveCodec(codec)
    let key = config.authorityPublicKey
    if (typeof key === 'string' && key !== '') {
      for (const encoding of ['base64url', 'base64']) {
        const decoded = Uint8Array.from(Buffer.from(key, encoding))
        if (decoded.length === 32) { key = decoded; break }
      }
    }
    this.#authorities = new AuthorityStore(key)
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

  /** Host-owned device grants (the durable authority). */
  get trust() {
    return this.#trust
  }

  /** Host-wide execution journal. */
  get journal() {
    return this.#journal
  }

  /** Host-local owner/recovery management surface. */
  get deviceTrust() {
    return this.#trustService
  }

  /** Record the proof key a Device demonstrated for one App session. */
  /** Test/diagnostic view: is a permit installed for this exact session? */
  sessionAuthorized(sessionId, deviceId) {
    return this.#authorities.validFor(sessionId, this.#config.hostId, deviceId, this.now())
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
      // One Host-owned grant store and one command journal over the existing
      // state store. Every trust write shares the store's IMMEDIATE transaction.
      this.#trust = new DeviceTrustStore({ store: this.#store, now: this.now })
      this.#journal = new CommandJournal({ store: this.#store, now: this.now })
      this.#security = new SecurityManager({
        hostId: this.#config.hostId,
        identity,
        store: this.#store,
        logger: this.#logger,
        now: this.now,
        protocolVersion: PROTOCOL_VERSION,
        codec: this.#codec,
        trust: this.#trust,
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
      this.#trustService = new HostDeviceTrustService({
        trust: this.#trust,
        security: this.#security,
        store: this.#store,
        hostKeyFingerprint: this.#security.hostFingerprint,
        now: this.now,
        logger: this.#logger,
      })
      this.#access = this.injectedAccess ?? new MqttAccess({
        url: this.#config.mqttUrl,
        hostId: this.#config.hostId,
        token: this.#config.hostToken,
        // Directory display metadata is bounded by the transport. It never
        // feeds the Host id, route, fingerprint or grants.
        displayName: this.#config.displayName || this.#config.instanceLabel || hostname(),
        osFamily: this.#config.osFamily || hostOsFamily(),
        authorizationPublicKey: authorizationPublicKey(identity.privateKey),
        authorizationPrivateKey: identity.privateKey,
        accessOrigin: this.#config.accessUrl ?? '',
        caFile: this.#config.caFile,
        reconnectMinMs: this.#config.reconnectMinMs,
        reconnectMaxMs: this.#config.reconnectMaxMs,
        connect: this.injectedAccessConnect ?? undefined,
        logger: this.#logger,
        now: this.now,
        codec: this.#codec,
        transportProfile: this.#config.transportProfile ?? 'default',
        observation: this.observation,
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
        observation: this.observation,
      })
      this.#commands = new CommandService({
        hostId: this.#config.hostId,
        store: this.#store,
        adapter: this.#nativeGuardedAdapter(this.#adapter),
        events: this.#events,
        logger: this.#logger,
        now: this.now,
        codec: this.#codec,
        security: this.#security,
        attachmentPolicy: this.#config.attachmentPolicy ?? null,
        transportProfile: this.#config.transportProfile ?? 'default',
        observation: this.observation,
        onAccepted: (request) => this.#reply(request, { status: 'host_accepted' }),
      })
      // Crash recovery happens before any request is accepted. A reservation
      // that never reached a terminal outcome is uncertain, never successful.
      const recovered = this.#journal.markReservedUnknown()
      if (recovered > 0) this.#logger.warn('command_journal_recovered', { count: recovered, code: 'unavailable' })
      // Bound retained receipt bytes at every start. Reservation identity and
      // terminal state stay so a late resubmission is still suppressed.
      const pruned = this.#journal.pruneResults()
      if (pruned > 0) this.#logger.info('command_journal_pruned', { count: pruned })
      this.#commands.recover()
      this.#events.start()
      this.#channelPruneTimer = setInterval(() => this.#security.pruneIdleChannels(), 30_000)
      this.#channelPruneTimer.unref?.()
      this.#access.start({
        // The private self-status lane answers on its own reply topic and never
        // enters the ordinary request router or its result topic.
        onSelfStatus: (request) => this.#track(this.#handleSelfStatus(request)),
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
    this.#trust = null
    this.#journal = null
    this.#trustService = null
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
    // Self-status owns a private reply topic and a different refusal policy, so
    // a misplaced envelope never produces an ordinary result-lane reply.
    if (request?.kind === 'REQUEST_KIND_SELF_STATUS') {
      await this.#handleSelfStatus(request)
      return
    }
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
      if (kind === 'REQUEST_KIND_AUTHORITY') return await this.#handleAuthority(request)
      if (kind === 'REQUEST_KIND_SESSION_PROOF') return await this.#handleSessionProof(request)
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
        const gated = this.#config.requireAuthority ||
          (request.proof_expected === true && this.#authorities.hasSession(request.device_session_id))
        if (gated) {
          const session = request.device_session_id
          const routeGeneration = this.#access?.route?.generation
          if (typeof session !== 'string' || session === '' ||
              routeGeneration === undefined || routeGeneration === null ||
              !this.#authorities.validFor(session, this.#config.hostId, request.device_id, this.now(),
                routeGeneration)) {
            return await this.#reply(request, { status: 'rejected', errorCode: 'unauthorized' })
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

  /**
   * Answer one bounded self-status exchange on the private service lane.
   *
   * The only grant facts that ever leave this method are the claimed id, the
   * echoed challenge and the active/revoked state. A missing or foreign grant
   * is a bodyless rejected result, which Access renders exactly like an
   * unknown Host, so existence never leaks. A request that is not this exact
   * exchange is dropped without a reply.
   */
  async #handleSelfStatus(request) {
    if (!request || typeof request !== 'object') return null
    if (request.kind !== 'REQUEST_KIND_SELF_STATUS' || request.host_id !== this.#config.hostId) {
      this.#logger?.warn('self_status_rejected', { code: 'rejected', reason: 'addressing' })
      return null
    }
    const compatibility = protocolVersionCompatibility(Number(request.protocol_version))
    if (!compatibility.compatible || Number(request.relay_version) !== RELAY_VERSION) {
      this.#logger?.warn('self_status_rejected', { code: 'rejected', reason: 'protocol' })
      return null
    }
    if (!validateId('request_id', request.request_id).ok) {
      this.#logger?.warn('self_status_rejected', { code: 'rejected', reason: 'request_id' })
      return null
    }
    const body = request.body
    if (!(body instanceof Uint8Array) || body.length === 0 || body.length > SELF_STATUS_MAX_BODY_BYTES) {
      this.#logger?.warn('self_status_rejected', { code: 'rejected', reason: 'body' })
      return null
    }
    let frame
    try {
      frame = this.#codec.decode('whalerein.v1.SelfStatusRequest', body)
    } catch {
      this.#logger?.warn('self_status_rejected', { code: 'rejected', reason: 'body' })
      return null
    }
    const grantId = frame?.grant_id
    const challenge = frame?.challenge
    if (typeof grantId !== 'string' || !SELF_STATUS_GRANT_ID.test(grantId)) {
      this.#logger?.warn('self_status_rejected', { code: 'rejected', reason: 'grant_id' })
      return null
    }
    if (!(challenge instanceof Uint8Array) || challenge.length < SELF_STATUS_MIN_CHALLENGE_BYTES ||
        challenge.length > SELF_STATUS_MAX_CHALLENGE_BYTES) {
      this.#logger?.warn('self_status_rejected', { code: 'rejected', reason: 'challenge' })
      return null
    }
    const expiresAt = Number(request.expires_at_unix_ms)
    if (Number.isSafeInteger(expiresAt) && expiresAt > 0 && expiresAt <= this.now()) {
      // Access already abandoned this exchange; a late reply only adds noise.
      this.#logger?.info('self_status_skipped', { code: 'expired' })
      return null
    }
    const fingerprint = this.#security?.hostFingerprint
    let grant = null
    try {
      grant = this.#trust?.getGrant(grantId) ?? null
    } catch {
      grant = null
    }
    if (!grant || (typeof fingerprint === 'string' && grant.hostKeyFingerprint !== fingerprint)) {
      // One indistinguishable refusal for an absent and a foreign grant.
      this.#logger?.warn('self_status_rejected', { code: 'rejected', reason: 'grant' })
      return await this.#replySelfStatus(request, { status: 'rejected' })
    }
    let resultBody
    try {
      resultBody = this.#codec.encode('whalerein.v1.SelfStatus', {
        grant_id: grantId,
        // Echo the exact claim challenge; never re-derive or truncate it.
        challenge,
        active: grant.state === 'active',
        revoked_at_unix_ms: Number.isSafeInteger(grant.revokedAt) && grant.revokedAt > 0 ? grant.revokedAt : 0,
      })
    } catch {
      this.#logger?.warn('self_status_rejected', { code: 'internal', reason: 'encode' })
      return await this.#replySelfStatus(request, { status: 'failed', errorCode: 'internal' })
    }
    return await this.#replySelfStatus(request, { status: 'succeeded', resultBody })
  }

  /** Publish exactly one bounded HostResult on the private service reply topic. */
  async #replySelfStatus(request, { status, resultBody = EMPTY, errorCode }) {
    const result = {
      request_id: request.request_id,
      host_id: this.#config.hostId,
      status,
      result_body: resultBody instanceof Uint8Array ? resultBody : EMPTY,
      error_code: errorCode ?? (status === 'succeeded' ? 'error_code_unspecified' : status === 'rejected' ? 'forbidden' : 'internal'),
      completed_at_unix_ms: this.now(),
      protocol_version: PROTOCOL_VERSION,
      relay_version: RELAY_VERSION,
    }
    let encoded
    try {
      encoded = this.#codec.encode('whalerein.v1.HostResult', result)
    } catch {
      this.#logger?.warn('self_status_reply_failed', { code: 'internal' })
      return null
    }
    try {
      const publish = this.#access?.publishSelfStatusReply
      if (typeof publish !== 'function') throw new PluginError('unavailable', 'self-status reply lane unavailable')
      await publish.call(this.#access, encoded)
    } catch (error) {
      // A stable code only: the reply never carries a grant or challenge log.
      this.#logger?.warn('self_status_reply_failed', {
        code: typeof error?.code === 'string' && error.code !== '' ? error.code : 'unavailable',
      })
      return null
    }
    return result
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

  async #handleAuthority(request) {
    if (!request.request_id || request.host_id !== this.#config.hostId || !request.device_id) {
      return await this.#reply(request, { status: 'rejected', errorCode: 'forbidden' })
    }
    if (!this.#authorities.enabled) {
      return await this.#reply(request, { status: 'rejected', errorCode: 'unavailable' })
    }
    let authority
    try {
      authority = this.#codec.decode('whalerein.v1.ScopedAuthority', request.body)
    } catch {
      return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument' })
    }
    const sessionId = authority.session_id
    if (typeof sessionId !== 'string' || sessionId === '' || authority.host_id !== this.#config.hostId ||
        authority.device_id !== request.device_id) {
      return await this.#reply(request, { status: 'rejected', errorCode: 'forbidden' })
    }
    const routeGeneration = Number(this.#access?.route?.generation ?? authority.route_generation)
    const result = this.#authorities.stage(authority, {
      hostId: this.#config.hostId,
      deviceId: request.device_id,
      sessionId,
      servingNodeId: authority.serving_node_id,
      routeGeneration,
      proofPublicKey: authority.session_proof_public_key,
    }, this.now())
    if (!result.ok) {
      this.#logger?.warn('authority_rejected', { code: 'forbidden', reason: result.reason, layer: 'host' })
      return await this.#reply(request, { status: 'rejected', errorCode: 'forbidden' })
    }
    return await this.#reply(request, { status: 'succeeded' })
  }

  async #handleSessionProof(request) {
    if (!request.request_id || request.host_id !== this.#config.hostId || !request.device_id) {
      return await this.#reply(request, { status: 'rejected', errorCode: 'forbidden' })
    }
    let proof
    try {
      proof = this.#codec.decode('whalerein.v1.SessionProof', request.body)
    } catch {
      return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument' })
    }
    if (proof.host_id !== this.#config.hostId || proof.device_id !== request.device_id ||
        proof.channel_id !== request.channel_id ||
        typeof proof.session_id !== 'string' || proof.session_id === '') {
      return await this.#reply(request, { status: 'rejected', errorCode: 'forbidden' })
    }
    const channelHash = this.#security?.channelTranscript?.(request.channel_id, request.device_id)
    if (!(channelHash instanceof Uint8Array) || channelHash.length !== 32 ||
        !(proof.transcript_hash instanceof Uint8Array) || proof.transcript_hash.length !== 32) {
      return await this.#reply(request, { status: 'rejected', errorCode: 'unauthorized' })
    }
    for (let i = 0; i < 32; i += 1) {
      if (channelHash[i] !== proof.transcript_hash[i]) {
        return await this.#reply(request, { status: 'rejected', errorCode: 'unauthorized' })
      }
    }
    const key = this.#authorities.pendingProofKey(proof.session_id)
    if (!verifySessionProof(key, proof, this.now())) {
      return await this.#reply(request, { status: 'rejected', errorCode: 'forbidden' })
    }
    if (!this.#authorities.activate(proof.session_id, key, this.now())) {
      return await this.#reply(request, { status: 'rejected', errorCode: 'forbidden' })
    }
    return await this.#reply(request, { status: 'succeeded' })
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
      for (const deviceId of notice.device_ids ?? []) {
        this.#authorities.revokeDevice(deviceId)
      }
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
    // The channel carries the exact Host grant it was installed for. Every
    // operation maps to a current right on that grant; an unclassified
    // operation, a missing channel grant or a revoked grant is denied.
    const operation = requestOperation(payload)
    const grantId = opened.channel?.grantId
    const isApprovalOperation = operation === 'get_approval' || operation === 'resolve_approval'
    const authorize = () => (isApprovalOperation
      ? this.#trust.authorizeApproval(grantId)
      : this.#trust.authorizeOperation({ grantId, operation }))
    if (typeof grantId !== 'string' || grantId === '' || !classifyOperation(operation).classified) {
      this.#logger?.warn('operation_denied', {
        request: request.request_id, device: request.device_id, operation: String(operation).slice(0, 64), reason: 'unclassified',
      })
      return await this.#reply(request, { status: 'rejected', errorCode: 'forbidden' })
    }
    const permitted = authorize()
    if (!permitted.allowed) {
      this.#logger?.warn('operation_denied', {
        request: request.request_id, device: request.device_id, operation, reason: permitted.reason,
      })
      return await this.#reply(request, { status: 'rejected', errorCode: 'forbidden' })
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
    // Admission for executable work: the authenticated absolute deadline is
    // capped by the Host clock and the journal reserves the command identity
    // before any native handoff.
    let admission = null
    let reservation = null
    const devicePublicKey = opened.channel.devicePublicKey
    if (request.kind === 'REQUEST_KIND_COMMAND') {
      if (typeof request.command_id !== 'string' || request.command_id === '') {
        return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument' })
      }
      const absoluteDeadline = Number(request.expires_at_unix_ms)
      admission = admitDeadline({
        deadlineAt: absoluteDeadline,
        wallNow: this.now(),
        monotonicNow: this.#monotonicNow(),
      })
      if (!admission.ok) {
        const code = admission.reason === 'missing_deadline' ? 'invalid_argument'
          : admission.reason === 'deadline_too_far' ? 'invalid_argument' : 'expired'
        this.#logger?.warn('command_deadline_rejected', {
          request: request.request_id, device: request.device_id, reason: admission.reason,
        })
        return await this.#reply(request, { status: 'rejected', errorCode: code })
      }
      reservation = this.#journal.reserveCommand({
        commandId: request.command_id,
        // The plaintext body digest is authenticated by the frame and is
        // independent of the channel's nonce/ciphertext, so a retry or a fresh
        // channel cannot change the immutable intent.
        intentDigest: sha256Bytes(opened.plaintext),
        devicePublicKey,
        grantId,
        deadlineAt: absoluteDeadline,
      })
      if (reservation.ok === false) {
        // A foreign grant or an unknown historical owner yields the same
        // generic refusal and releases no stored content or status.
        this.#logger?.warn('command_journal_refused', {
          request: request.request_id, device: request.device_id, reason: reservation.reason,
        })
        return await this.#reply(request, { status: 'failed', errorCode: 'forbidden' })
      }
      if (reservation.admitted === false) {
        if (reservation.reason === 'intent_mismatch') {
          return await this.#reply(request, { status: 'rejected', errorCode: 'payload_digest_mismatch' })
        }
        return await this.#replayJournalReceipt(request, reservation)
      }
      // The last boundary before native DSH checks freshness and current
      // rights again; a queued or delayed command executes nothing.
      request.guard = {
        assertNative: () => this.#assertNativeHandoff({ admission, grantId, operation }),
      }
    }
    // A command lookup is a read, never a native action. Only the exact grant
    // and device key that reserved the ID observe its receipt; every other
    // requester gets the same generic not-found answer.
    if (operation === 'get_command_result') {
      return await this.#handleCommandReceiptLookup(request, payload.get_command_result, grantId, devicePublicKey)
    }
    let completedAt
    const questionBudget = payload.list_questions ? () => {
      completedAt = this.now()
      return questionResponseBudget(this.#codec, request, this.#config.hostId, completedAt, PROTOCOL_VERSION)
    } : undefined
    let result
    try {
      result = await this.#commands.handle({ request, payload, questionResponseBudget: questionBudget })
    } catch (error) {
      if (request.kind === 'REQUEST_KIND_COMMAND') {
        this.#bindJournalOutcome(request, { status: 'unknown', errorCode: 'internal' }, grantId, devicePublicKey, false)
      }
      throw error
    }
    // The receipt is bound after the reply attempt. A proven native success
    // whose reply could not be delivered is recorded as unknown, never success;
    // a fresh authenticated lookup is the only recovery for the receipt.
    const isCommand = request.kind === 'REQUEST_KIND_COMMAND'
    // Re-check the current right before releasing protected output; a grant
    // that lost the right while the source call was in flight releases nothing.
    const released = authorize()
    if (!released.allowed) {
      this.#logger?.warn('output_denied', {
        request: request.request_id, device: request.device_id, operation, reason: released.reason,
      })
      const reply = await this.#reply(request, { status: 'failed', errorCode: 'forbidden', completedAt })
      if (isCommand) this.#bindJournalOutcome(request, result, grantId, devicePublicKey, reply !== null)
      return reply
    }
    const responseBody = this.#encryptResult(request, result)
    if (result.sourceFailure && (!(responseBody instanceof Uint8Array) || responseBody.length === 0)) {
      const reply = await this.#reply(request, { status: 'failed', errorCode: 'unavailable' })
      if (isCommand) this.#bindJournalOutcome(request, result, grantId, devicePublicKey, reply !== null)
      return reply
    }
    if (result.status === 'succeeded' && (!(responseBody instanceof Uint8Array) || responseBody.length === 0)) {
      // A query cannot succeed without an authenticated response; for a
      // completed command the effect may already exist, so retain uncertainty.
      const reply = await this.#reply(request, { status:request.kind === 'REQUEST_KIND_QUERY' ? 'failed' : 'unknown', errorCode:'internal', completedAt })
      if (isCommand) this.#bindJournalOutcome(request, { status: 'unknown', errorCode: 'internal' }, grantId, devicePublicKey, reply !== null)
      return reply
    }
    const reply = await this.#reply(request, {
      status: result.status,
      resultBody: responseBody ?? undefined,
      errorCode: result.errorCode,
      // A complete query result is admitted whole: if its inline form exceeds
      // the receiver budget the transport stages the exact ciphertext and
      // publishes a reference, so content never fails for a transport reason.
      receiverLimitBytes: requestTransport?.result_max_bytes,
      completedAt,
    })
    if (isCommand) this.#bindJournalOutcome(request, result, grantId, devicePublicKey, reply !== null)
    return reply
  }

  /** Answer only the exact reserving grant's receipt; otherwise not found. */
  async #handleCommandReceiptLookup(request, lookup, grantId, devicePublicKey) {
    const commandId = lookup?.command_id
    if (typeof commandId !== 'string' || commandId === '') {
      return await this.#reply(request, { status: 'rejected', errorCode: 'invalid_argument' })
    }
    let receipt
    try {
      receipt = this.#journal.lookupCommand({ commandId, grantId, devicePublicKey })
    } catch {
      receipt = { ok: false, reason: 'unverifiable' }
    }
    // Unknown IDs and foreign grants deliberately produce the same body.
    const response = receipt.ok
      ? {
          command_result: {
            found: true,
            status: JOURNAL_STATE_TO_PROTO[receipt.command.state] ?? 'unknown',
            result_payload: receipt.command.resultPayload ?? EMPTY,
            error_code: receipt.command.errorCode ?? 'error_code_unspecified',
          },
        }
      : { command_result: { found: false, status: 'online_status_unspecified' } }
    const body = this.#encryptResult(request, { response })
    if (!(body instanceof Uint8Array) || body.length === 0) {
      return await this.#reply(request, { status: 'failed', errorCode: 'internal' })
    }
    return await this.#reply(request, { status: 'succeeded', resultBody: body })
  }

  /** Replay a stored receipt for an exact duplicate, never re-executing it. */
  async #replayJournalReceipt(request, reservation) {
    const status = reservation.state === 'reserved' ? 'host_accepted'
      : JOURNAL_STATE_TO_PROTO[reservation.state] ?? 'unknown'
    const payload = reservation.resultPayload instanceof Uint8Array ? reservation.resultPayload : null
    const body = payload ? this.#encryptResult(request, { resultPayload: payload }) : null
    if (payload && !(body instanceof Uint8Array)) {
      return await this.#reply(request, { status: 'unknown', errorCode: 'internal' })
    }
    return await this.#reply(request, {
      status,
      resultBody: body ?? undefined,
      errorCode: reservation.errorCode ?? (status === 'unknown' ? 'unavailable' : 'error_code_unspecified'),
    })
  }

  /** Bind the proven native outcome, never overwriting earlier evidence. */
  #bindJournalOutcome(request, result, grantId, devicePublicKey, delivered = true) {
    if (typeof grantId !== 'string' || grantId === '') return
    let resultPayload = null
    try {
      if (result.resultPayload instanceof Uint8Array) resultPayload = result.resultPayload
      else if (result.response) resultPayload = this.#codec.encode('whalerein.v1.ResponsePayload', result.response)
    } catch {
      resultPayload = null
    }
    // A lost reply is not proof that the native action succeeded; bind the
    // conservative unknown state so a lookup never manufactures success.
    const state = result.status === 'succeeded' && delivered ? 'succeeded'
      : result.status === 'failed' || result.status === 'rejected' ? 'failed' : 'unknown'
    try {
      this.#journal.bindResult({
        commandId: request.command_id,
        grantId,
        devicePublicKey,
        state,
        resultPayload,
        errorCode: typeof result.errorCode === 'string' ? result.errorCode : null,
      })
    } catch (error) {
      this.#logger?.warn('command_journal_bind_failed', {
        command: request.command_id, state, code: error?.code ?? 'internal',
      })
    }
  }

  /**
   * Throw before native DSH entry when the admitted window elapsed or the
   * current grant lost the required right.
   */
  #assertNativeHandoff({ admission, grantId, operation }) {
    if (admission && !isAdmissionFresh(admission, { monotonicNow: this.#monotonicNow() })) {
      throw new DefinitiveExecutionError('expired')
    }
    const permitted = operation === 'get_approval' || operation === 'resolve_approval'
      ? this.#trust.authorizeApproval(grantId)
      : this.#trust.authorizeOperation({ grantId, operation })
    if (!permitted.allowed) throw new DefinitiveExecutionError('forbidden')
  }

  /**
   * Wrap only the native handoff calls so the command service keeps using the
   * real source for reads; every execute passes the per-request guard context.
   */
  #nativeGuardedAdapter(adapter) {
    const wrap = (fn) => async (...args) => {
      const context = args[args.length - 1]
      context?.guard?.assertNative?.()
      return await fn.apply(adapter, args)
    }
    return new Proxy(adapter, {
      get: (target, property) => {
        const value = Reflect.get(target, property, target)
        if (typeof value !== 'function') return value
        if (property === 'execute' || property === 'executeHost') return wrap(value)
        return value.bind(target)
      },
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

  // --- local owner and recovery management (backend only) ----------------

  #trustManagement() {
    if (!this.#trustService) throw new PluginError('unavailable', 'device trust unavailable')
    return this.#trustService
  }

  /** Every grant with provenance, rights and timestamps. */
  listDeviceGrants(options = {}) { return this.#trustManagement().listGrants(options) }

  getDeviceGrant(grantId) { return this.#trustManagement().getGrant(grantId) }

  /** Create one owner-approved grant through the shared writer. */
  createDeviceGrant(options = {}) { return this.#trustManagement().createGrant(options) }

  /** Replace a device grant through the same writer. */
  replaceDeviceGrant(options = {}) { return this.#trustManagement().replaceGrant(options) }

  /** Owner edit of rights, ceiling and add-device. */
  editDeviceGrant(options = {}) { return this.#trustManagement().editGrant(options) }

  /** Revoke exactly the selected grant IDs (ordinary retirement / loss). */
  revokeDeviceGrants(options = {}) { return this.#trustManagement().revokeGrants(options) }

  /** Directly approved children and their approval timestamps. */
  listDeviceChildren(sponsorGrantId) { return this.#trustManagement().listChildren(sponsorGrantId) }

  /** Loss review: the selected grant plus its direct children. */
  deviceLossReview(grantId) { return this.#trustManagement().lossReview(grantId) }

  registerRecoveryVerifier(options = {}) { return this.#trustManagement().registerRecoveryVerifier(options) }

  retireRecoveryVerifier(options = {}) { return this.#trustManagement().retireRecoveryVerifier(options) }

  updateRecoveryPolicy(options = {}) { return this.#trustManagement().updateRecoveryPolicy(options) }

  listRecoveryVerifiers() { return this.#trustManagement().listRecoveryVerifiers() }

  issueRecoveryChallenge(options = {}) { return this.#trustManagement().issueRecoveryChallenge(options) }

  commitRecovery(options = {}) { return this.#trustManagement().commitRecovery(options) }

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
