// Noise XX pairing, secure channels, and revocation replay.
//
// Pairing:  HELLO consumes msg1 and returns msg2. FINISH consumes msg3 and
//           returns the device public key without persisting it. AUTHORIZE is
//           the only path that persists a peer key; Access sends it only after
//           its grants transaction committed.
// Channels: one Noise XX handshake over two HostRequest calls, bounded
//           concurrent channels per device, and frame AAD as defined in WIRE.md.

import {
  NoiseXX,
  NoiseError,
  channelPrologue,
  eventAad,
  fingerprint,
  formatFingerprint,
  pairingPrologue,
  requestAad,
} from './noise.js'
import { randomBytes } from 'node:crypto'
import { bytesEqual, sha256Bytes } from './digest.js'
import { codec as defaultCodec, resolveCodec } from '../protocol.js'
import { PluginError } from '../errors.js'
import { sanitizeDisplayName } from '../display-name.js'
import { sanitizeOs } from '../os.js'
import { DeviceTrustStore } from './grants.js'
import {
  RELAY_VERSION,
  resolveTransportProfile,
  transportProfileBudget,
  validateBrokerConnectionRef,
  validateReceiveAdvertisement,
} from '../contracts/transport.js'

export const HOST_CAPABILITIES = Object.freeze([
  'sessions',
  'events',
  'messages',
  'approvals',
  'interrupt',
  'sync',
  'snapshot',
])

/**
 * Rights granted by the existing owner-approved pairing flow. A sponsored
 * enrollment carries only what its sponsor selected, and never a grantable
 * ceiling or add-device authority.
 */
export const DEFAULT_ENROLLMENT_RIGHTS = Object.freeze(['read', 'write', 'approve'])

const TOKEN_HASH = /^[0-9a-fA-F]{64}$/
const PENDING_TTL_MS = 120_000
const CHANNEL_IDLE_MS = 120_000
const MAX_DEVICE_CHANNELS = 64
const MAX_HOST_CHANNELS = 512

/** Host receive limits for the authenticated broker connection generation. */
export function defaultReceiveAdvertisement(generation = 1) {
  const profile = resolveTransportProfile('default')
  return {
    profile: profile.wireName,
    budget: transportProfileBudget(profile.name),
    connection_generation: generation,
    relay_version: RELAY_VERSION,
  }
}

function protocolMismatch(value, expected) {
  // WIRE treats 0/absent as an old peer that must upgrade. A missing field is
  // never proof that the peer speaks the current contract.
  return value === undefined || value === null || Number(value) !== expected
}

function normalizeFingerprint(value) {
  return typeof value === 'string' ? value.replace(/\s+/g, '').toLowerCase() : ''
}

function requiredString(value, name) {
  if (typeof value !== 'string' || value === '') {
    throw new PluginError('invalid_argument', `${name} missing`)
  }
  return value
}

function requestGeneration(value) {
  if (value === undefined || value === null || value === 0 || value === '0') return 0
  if (typeof value === 'number') return Number.isSafeInteger(value) && value >= 0 ? value : null
  if (typeof value === 'string' && /^(?:0|[1-9][0-9]*)$/.test(value)) {
    const parsed = Number(value)
    return Number.isSafeInteger(parsed) ? parsed : null
  }
  return null
}

export class SecurityManager {
  #channels = new Map()
  #pendingHandshakes = new Map()
  #pendingPairings = new Map()
  #completedPairings = new Map()
  #revocationsPending = false
  #channelReceiveGeneration = 0
  // Bounded memory of Broker connection instances that were closed by revoke,
  // replacement or lifecycle. A late reply bound to one of them is dropped.
  #closedConnections = new Map()
  #closedConnectionsLimit = 512

  /**
   * @param {object} options
   * @param {string} options.hostId
   * @param {{publicKey: Uint8Array, privateKey: Uint8Array, fingerprint: string}} options.identity
   * @param {import('../state/store.js').StateStore} options.store
   * @param {object} [options.logger]
   * @param {() => number} [options.now]
   * @param {number} [options.pendingTtlMs]
   * @param {number} [options.protocolVersion]
   * @param {import('../protocol.js').ProtoCodec} [options.codec]
   * @param {number} [options.channelIdleMs]
   * @param {number} [options.maxDeviceChannels]
   * @param {number} [options.maxHostChannels]
   * @param {(channels: Array<{deviceId: string, channelId: string}>, reason: string) => void} [options.onClosedChannels]
   * @param {(channels: Array<{deviceId: string, channelId: string}>, deviceIds: string[]) => void} [options.onRevokedChannels]
   * @param {import('./grants.js').DeviceTrustStore} [options.trust] Host-owned
   *   grant store. Defaults to one over the same open state store so tests and
   *   embeddings that construct the manager directly share the authority.
   * @param {(context: {claim: object, deviceId: string, devicePublicKey: Uint8Array}) => ({approver: object, useRights?: string[]})} [options.resolveEnrollmentApprover]
   *   Host-local owner/sponsor selection for a COMMIT. Defaults to the owner
   *   flow, which is the only path the current wire exposes.
   * @param {() => object} [options.receiveAdvertisement] Host receive limits for
   *   the authenticated broker connection generation. Re-evaluated per channel
   *   handshake so a reconnect can never reuse an older generation.
   */
  constructor({
    hostId,
    identity,
    store,
    logger,
    now = Date.now,
    pendingTtlMs = PENDING_TTL_MS,
    protocolVersion = 1,
    maxPending = 32,
    codec = defaultCodec,
    channelIdleMs = CHANNEL_IDLE_MS,
    maxDeviceChannels = MAX_DEVICE_CHANNELS,
    maxHostChannels = MAX_HOST_CHANNELS,
    onClosedChannels = () => {},
    onRevokedDevices = () => {},
    onRevokedChannels = () => {},
    trust = null,
    resolveEnrollmentApprover = null,
    receiveAdvertisement = () => defaultReceiveAdvertisement(1),
  }) {
    this.hostId = hostId
    this.identity = identity
    this.store = store
    this.logger = logger
    this.now = now
    this.pendingTtlMs = pendingTtlMs
    this.protocolVersion = protocolVersion
    this.maxPending = maxPending
    this.codec = resolveCodec(codec)
    this.channelIdleMs = channelIdleMs
    this.maxDeviceChannels = maxDeviceChannels
    this.maxHostChannels = maxHostChannels
    this.onClosedChannels = onClosedChannels
    this.onRevokedDevices = onRevokedDevices
    this.onRevokedChannels = onRevokedChannels
    this.receiveAdvertisement = receiveAdvertisement
    this.hostFingerprint = identity.fingerprint ?? fingerprint(identity.publicKey)
    this.hostFingerprintDisplay = formatFingerprint(identity.publicKey)
    this.trust = trust ?? new DeviceTrustStore({ store, now })
    this.resolveEnrollmentApprover = resolveEnrollmentApprover
  }

  get revocationsPending() {
    return this.#revocationsPending
  }

  setRevocationsPending(pending) {
    this.#revocationsPending = Boolean(pending)
    if (pending) this.logger?.info('revocations_pending', {})
  }

  // --- channel inventory and lifetime -------------------------------------

  listChannels() {
    const now = this.now()
    return [...this.#channels.values()]
      .filter(channel => !this.#isIdle(channel, now))
      .map(({ channelId, deviceId, createdAt }) => ({ channelId, deviceId, createdAt }))
  }

  getChannel(channelId) {
    const channel = this.#channels.get(channelId)
    return channel && !this.#isIdle(channel, this.now()) ? channel : null
  }

  /** The immutable receive limits bound to one authenticated channel. */
  getChannelReceive(channelId) {
    return this.getChannel(channelId)?.receive ?? null
  }

  /** The Broker connection instance that owns one authenticated channel. */
  getChannelConnectionRef(channelId) {
    return this.getChannel(channelId)?.connectionRef ?? null
  }

  /**
   * True when a reply still belongs to the connection scope that admitted it.
   * A channel removed by revoke, replace or lifecycle close can never publish a
   * late result.
   */
  canReplyForRequest(request) {
    if (!request || typeof request !== 'object') return false
    if (request.kind !== 'REQUEST_KIND_QUERY' && request.kind !== 'REQUEST_KIND_HANDSHAKE') return true
    const ref = typeof request.connection_ref === 'string' ? request.connection_ref : ''
    if (request.kind === 'REQUEST_KIND_HANDSHAKE') {
      const pending = this.#pendingHandshakes.get(request.channel_id)
      if (pending && pending.deviceId === request.device_id) {
        return ref === '' || pending.connectionRef === ref
      }
      const channel = this.#resolveChannel(request.channel_id, request.device_id)
      return channel ? ref === '' || channel.connectionRef === ref : true
    }
    // The exact connection instance that owned this query is already closed by
    // revoke, replacement or lifecycle. No late result may be published.
    if (ref !== '' && this.#closedConnections.has(ref)) return false
    if (request.kind === 'REQUEST_KIND_QUERY') {
      const channel = this.#resolveChannel(request.channel_id, request.device_id)
      if (!channel) return true
      return ref === '' || channel.connectionRef === ref
    }
    return true
  }

  /**
   * Close only the channels and in-progress handshakes owned by one exact
   * Broker connection instance. A delayed close for an older reference never
   * touches a newer connection's channels.
   */
  closeConnection(deviceId, connectionRef) {
    let ref
    try { ref = validateBrokerConnectionRef(connectionRef) } catch { return [] }
    this.#markConnectionClosed(ref)
    const closed = this.#removeChannels(channel => channel.deviceId === deviceId && channel.connectionRef === ref)
    for (const [key, pending] of this.#pendingHandshakes) {
      if (pending.deviceId === deviceId && pending.connectionRef === ref) this.#pendingHandshakes.delete(key)
    }
    if (closed.length > 0) {
      this.logger?.info('connection_channels_cleared', { count: closed.length, device: deviceId })
      this.onClosedChannels(closed, 'connection_closed')
    }
    return closed
  }

  #hostReceive() {
    try { return validateReceiveAdvertisement(this.receiveAdvertisement()) }
    catch { throw new PluginError('invalid_argument', 'host receive advertisement invalid') }
  }

  /**
   * A Host channel advertisement is scoped to the Noise channel that carries
   * it, not to the Host's Access-broker connection. A fresh channel handshake
   * always mints a new value so a delayed advertisement cannot look current.
   */
  #channelReceive(hostReceive) {
    this.#channelReceiveGeneration = this.#channelReceiveGeneration >= Number.MAX_SAFE_INTEGER
      ? 1
      : this.#channelReceiveGeneration + 1
    return validateReceiveAdvertisement({
      profile: hostReceive.profile,
      budget: hostReceive.budget,
      connection_generation: this.#channelReceiveGeneration,
      relay_version: hostReceive.relay_version,
    })
  }

  #isIdle(channel, now) {
    return channel.inFlight === 0 && channel.lastUsedAt + this.channelIdleMs <= now
  }

  #markConnectionClosed(connectionRef) {
    if (typeof connectionRef !== 'string' || connectionRef === '') return
    if (this.#closedConnections.has(connectionRef)) this.#closedConnections.delete(connectionRef)
    this.#closedConnections.set(connectionRef, this.now())
    while (this.#closedConnections.size > this.#closedConnectionsLimit) {
      this.#closedConnections.delete(this.#closedConnections.keys().next().value)
    }
  }

  #removeChannels(matches) {
    const closed = []
    for (const [channelId, channel] of this.#channels) {
      if (!matches(channel)) continue
      this.#channels.delete(channelId)
      closed.push({ deviceId: channel.deviceId, channelId })
    }
    return closed
  }

  pruneIdleChannels() {
    const now = this.now()
    for (const [channelId, channel] of this.#channels) {
      if (this.#isIdle(channel, now)) this.closeChannel(channelId)
    }
  }

  closeChannel(channelId) {
    const channel = this.#channels.get(channelId)
    if (!channel) return
    this.#channels.delete(channelId)
    this.onClosedChannels([{ deviceId: channel.deviceId, channelId }], 'channel_replaced')
  }

  /**
   * Drop every live and in-progress channel. A reconnect always requires a
   * fresh Noise handshake with a new channel_id; peer keys and applied
   * revocations are untouched.
   */
  clearChannels() {
    const closed = this.#removeChannels(() => true)
    this.#pendingHandshakes.clear()
    if (closed.length > 0) {
      this.logger?.info('channels_cleared', { count: closed.length })
      this.onClosedChannels(closed, 'unavailable')
    }
  }

  /**
   * Drop the live channels and any in-progress handshake for one device.
   * @param {string} deviceId
   * @returns {Array<{deviceId: string, channelId: string}>} closed channels.
   */
  #clearDeviceChannels(deviceId) {
    for (const pending of this.#pendingHandshakes.values()) {
      if (pending.deviceId === deviceId) this.#markConnectionClosed(pending.connectionRef)
    }
    const closed = this.#removeChannels(channel => channel.deviceId === deviceId)
    for (const [key, pending] of this.#pendingHandshakes) {
      if (pending.deviceId === deviceId) this.#pendingHandshakes.delete(key)
    }
    this.store.deleteOutboxForDevice(deviceId)
    if (closed.length > 0) this.logger?.info('device_channels_cleared', { count: closed.length, device: deviceId })
    return closed
  }

  grantGeneration(deviceId) { return this.store.getPeerKey(deviceId)?.grantGeneration ?? 0 }

  hasPeer(deviceId) {
    return this.store.getPeerKey(deviceId) !== null
  }

  #pruneLocked() {
    const now = this.now()
    for (const [key, pending] of this.#pendingPairings) {
      if (pending.expiresAt <= now) this.#pendingPairings.delete(key)
    }
    for (const [key, completed] of this.#completedPairings) {
      if (completed.expiresAt <= now) this.#completedPairings.delete(key)
    }
    for (const [key, pending] of this.#pendingHandshakes) {
      if (pending.expiresAt <= now) this.#pendingHandshakes.delete(key)
    }
  }

  // --- pairing ------------------------------------------------------------

  /**
   * Handle one Access-forwarded PairingClaim. The returned object is the
   * PairingClaimResult body; it is never persisted as-is.
   */
  handlePairingClaim({ request, claim }) {
    this.#pruneLocked()
    const phase = claim?.phase
    const claimId = requiredString(claim?.claim_id, 'claim_id')
    if (claim?.expires_at_unix_ms && claim.expires_at_unix_ms <= this.now()) {
      throw new PluginError('expired', 'pairing claim expired')
    }
    if (phase === 'PAIRING_PHASE_HELLO' || phase === 'PAIRING_PHASE_FINISH') {
      if (this.#revocationsPending) throw new PluginError('revocations_pending', 'revocation replay pending')
      if (!TOKEN_HASH.test(claim.token_hash ?? '')) throw new PluginError('invalid_argument', 'token_hash invalid')
      if (!(claim.noise_message instanceof Uint8Array) || claim.noise_message.length === 0) {
        throw new PluginError('invalid_argument', 'noise_message missing')
      }
    }
    if (phase === 'PAIRING_PHASE_HELLO') return this.#pairingHello({ request, claim, claimId })
    if (phase === 'PAIRING_PHASE_FINISH') return this.#pairingFinish({ request, claim, claimId })
    if (phase === 'PAIRING_PHASE_AUTHORIZE' || phase === 'PAIRING_PHASE_COMMIT') return this.#pairingAuthorize({ claim, claimId })
    throw new PluginError('invalid_argument', 'pairing phase invalid')
  }

  #pairingHello({ request, claim, claimId }) {
    const pending = this.#pendingPairings.get(claimId)
    const approvedKey = claim.device_public_key
    if (approvedKey?.length && approvedKey.length !== 32) throw new PluginError('invalid_argument', 'approved device key invalid')
    const helloHash = sha256Bytes(claim.noise_message)
    if (pending && approvedKey?.length && pending.deviceId === claim.device_id &&
        (!request.device_id || request.device_id === pending.deviceId) &&
        pending.tokenHash === claim.token_hash && bytesEqual(pending.helloHash, helloHash) &&
        bytesEqual(pending.approvedKey, approvedKey)) {
      return pending.helloResult
    }
    if (pending || this.#completedPairings.has(claimId) || this.store.getPendingPeer(claimId)) {
      throw new PluginError('conflict', 'claim already in progress')
    }
    if (this.#pendingPairings.size >= this.maxPending) throw new PluginError('rate_limited', 'too many pairing claims')
    const noise = new NoiseXX({
      initiator: false,
      staticKeypair: this.identity,
      prologue: pairingPrologue({ hostId: this.hostId, tokenHash: claim.token_hash, claimId }),
    })
    let hello
    try {
      const payload = noise.readMessage(claim.noise_message)
      hello = this.codec.decode('whalerein.v1.PairingHello', payload)
    } catch (error) {
      if (error instanceof PluginError) throw error
      if (error instanceof NoiseError) throw new PluginError('unauthorized', 'pairing message rejected')
      throw new PluginError('invalid_argument', 'pairing payload invalid')
    }
    const deviceId = requiredString(hello.device_id, 'device_id')
    if (hello.host_id !== this.hostId) throw new PluginError('forbidden', 'host mismatch')
    if (protocolMismatch(hello.protocol_version, this.protocolVersion)) {
      throw new PluginError('protocol_version_incompatible', 'pairing version mismatch')
    }
    if (claim.device_id && claim.device_id !== deviceId) throw new PluginError('forbidden', 'device mismatch')
    if (request.device_id && request.device_id !== deviceId) throw new PluginError('forbidden', 'device mismatch')
    const reply = this.codec.encode('whalerein.v1.PairingHelloReply', {
      host_id: this.hostId,
      device_id: deviceId,
      protocol_version: this.protocolVersion,
      capabilities: {
        capabilities: [...HOST_CAPABILITIES],
        protocol_version: this.protocolVersion,
      },
      host_nonce: randomNonce(),
    })
    const helloResult = {
      noise_message: noise.writeMessage(reply),
      host_public_key: this.identity.publicKey,
      host_fingerprint: this.hostFingerprint,
      device_id: deviceId,
    }
    this.#pendingPairings.set(claimId, {
      claimId,
      approvedKey,
      helloHash,
      helloResult,
      tokenHash: claim.token_hash,
      deviceId,
      displayName: sanitizeDisplayName(hello.display_name),
      os: sanitizeOs(hello.os),
      noise,
      createdAt: this.now(),
      expiresAt: this.now() + this.pendingTtlMs,
    })
    this.logger?.info('pairing_hello', { device: deviceId, phase: 'hello' })
    return helloResult
  }

  #pairingFinish({ request, claim, claimId }) {
    const staged=this.#completedPairings.get(claimId) ?? this.store.getPendingPeer(claimId)
    if (staged) {
      if (staged.deviceId!==claim.device_id || staged.tokenHash!==claim.token_hash ||
          !bytesEqual(staged.finishHash,sha256Bytes(claim.noise_message)) || !bytesEqual(staged.authHash,claim.device_auth_hash) ||
          (claim.device_public_key?.length && !bytesEqual(staged.publicKey, claim.device_public_key))) {
        throw new PluginError('unauthorized','pairing retry binding mismatch')
      }
      return {device_id:staged.deviceId,device_public_key:staged.publicKey,device_auth_hash:staged.authHash,
        host_public_key:this.identity.publicKey,host_fingerprint:this.hostFingerprint}
    }
    const pending = this.#pendingPairings.get(claimId)
    if (!pending) throw new PluginError('invalid_argument', 'pairing claim unknown')
    if (claim.device_id && claim.device_id !== pending.deviceId) throw new PluginError('forbidden', 'device mismatch')
    if (request.device_id && request.device_id !== pending.deviceId) throw new PluginError('forbidden', 'device mismatch')
    let finish
    try {
      const payload = pending.noise.readMessage(claim.noise_message)
      finish = this.codec.decode('whalerein.v1.PairingFinish', payload)
    } catch (error) {
      this.#pendingPairings.delete(claimId)
      if (error instanceof PluginError) throw error
      throw new PluginError('unauthorized', 'pairing message rejected')
    }
    if (finish.device_id !== pending.deviceId) throw new PluginError('forbidden', 'device mismatch')
    if (finish.host_id && finish.host_id !== this.hostId) throw new PluginError('forbidden', 'host mismatch')
    if (finish.host_fingerprint && normalizeFingerprint(finish.host_fingerprint) !== normalizeFingerprint(this.hostFingerprint)) {
      throw new PluginError('unauthorized', 'fingerprint mismatch')
    }
    if (!(finish.device_auth_hash instanceof Uint8Array) || finish.device_auth_hash.length !== 32 ||
        !bytesEqual(finish.device_auth_hash, claim.device_auth_hash)) {
      this.#pendingPairings.delete(claimId)
      throw new PluginError('unauthorized', 'pairing credential binding mismatch')
    }
    const devicePublicKey = pending.noise.remoteStaticKey
    if (!devicePublicKey || devicePublicKey.length !== 32) throw new PluginError('unauthorized', 'device key missing')
    this.#pendingPairings.delete(claimId)
    if (pending.approvedKey?.length || claim.device_public_key?.length) {
      if (!bytesEqual(pending.approvedKey, devicePublicKey) || !bytesEqual(claim.device_public_key, devicePublicKey)) {
        throw new PluginError('unauthorized', 'device key does not match the approved scope')
      }
    }
    this.#completedPairings.set(claimId, {
      deviceId: pending.deviceId,
      displayName: pending.displayName ?? '',
      os: pending.os ?? '',
      publicKey: new Uint8Array(devicePublicKey),
      finishHash: sha256Bytes(claim.noise_message),
      authHash: finish.device_auth_hash,
      tokenHash: claim.token_hash,
      expiresAt: this.now() + this.pendingTtlMs,
    })
    try { this.store.stagePeer({ claimId, ...this.#completedPairings.get(claimId) }) }
    catch (error) { this.#completedPairings.delete(claimId); throw error }
    this.logger?.info('pairing_finish', { device: pending.deviceId, phase: 'finish' })
    return {
      device_id: pending.deviceId,
      device_public_key: new Uint8Array(devicePublicKey),
      device_auth_hash: finish.device_auth_hash,
      host_public_key: this.identity.publicKey,
      host_fingerprint: this.hostFingerprint,
    }
  }

  #pairingAuthorize({ claim, claimId }) {
    const generation=Number(claim.grant_generation??0)
    if(!Number.isSafeInteger(generation)||generation<0)throw new PluginError('invalid_argument','invalid grant generation')
    if(claim.phase==='PAIRING_PHASE_COMMIT') {
      const peer=this.store.getPeerKey(claim.device_id)
      // A strictly older Access grant incarnation is superseded. A competing
      // claim in the same incarnation is decided by the Host grant writer,
      // which reports a conflict without replacing the active grant.
      if(this.store.isGrantRevoked(claim.device_id,generation)||(peer&&generation<peer.grantGeneration)) {
        this.store.deletePendingPeer(claimId)
        this.#completedPairings.delete(claimId)
        throw new PluginError('unauthorized','pairing grant superseded or revoked')
      }
    }
    const completed = this.#completedPairings.get(claimId) ?? this.store.getPendingPeer(claimId)
    if (!completed) {
      // An exact COMMIT retry arrives after the staged row was consumed. It is
      // authorized only by the durable decision AND the same still-active Host
      // grant for the exact candidate key. Access activation alone can never
      // recreate session authority.
      const retry = (claim.phase === 'PAIRING_PHASE_COMMIT' || claim.phase === 'PAIRING_PHASE_AUTHORIZE')
        ? this.#activeDecision(claimId, claim)
        : null
      if (retry) {
        // Repair the peer mirror from the durable decision. The grant row stays
        // the authority; this only restores the pinned identity after an
        // interrupted COMMIT response.
        const existingPeer = this.store.getPeerKey(retry.decision.deviceId)
        this.store.upsertPeerKey({
          deviceId: retry.decision.deviceId,
          publicKey: retry.decision.candidatePublicKey,
          fingerprint: fingerprint(retry.decision.candidatePublicKey),
          claimId,
          grantGeneration: Math.max(existingPeer?.grantGeneration ?? 0, generation),
          grantId: retry.grant.grantId,
          grantRevision: retry.grant.revision,
          pairedAt: existingPeer?.pairedAt ?? this.now(),
          displayName: existingPeer?.displayName || retry.decision.deviceLabel || '',
          os: existingPeer?.os ?? '',
        })
        this.store.deletePendingPeer(claimId)
        // The exact COMMIT retry acknowledges the same Host-owned grant. It is
        // the durable grant row (not the Access request) that supplies the ID.
        return { authorized: true, device_id: claim.device_id, grant_id: retry.grant.grantId }
      }
      throw new PluginError('unauthorized', 'pairing not finished')
    }
    const deviceId = requiredString(claim.device_id, 'device_id')
    if (deviceId !== completed.deviceId) throw new PluginError('forbidden', 'device mismatch')
    if (!(claim.device_public_key instanceof Uint8Array) || !bytesEqual(claim.device_public_key, completed.publicKey)) {
      throw new PluginError('unauthorized', 'device key mismatch')
    }
    const existing = this.store.getPeerKey(deviceId)
    if (existing && !bytesEqual(existing.publicKey, completed.publicKey)) throw new PluginError('unauthorized', 'device identity cannot be rebound')
    // AUTHORIZE validates and stages only. COMMIT follows Access's transaction.
    if (claim.phase === 'PAIRING_PHASE_AUTHORIZE') {
      this.store.stagePeer({claimId,...completed,expiresAt:claim.expires_at_unix_ms || completed.expiresAt})
      return { authorized: true, device_id: deviceId }
    }

    // COMMIT is the Host grant writer. One authenticated candidate commits
    // exactly one grant; the grant row (not the Access row) is the authority.
    const enrollment = this.#enrollmentTerms({ claim, claimId, completed, deviceId })
    const grant = this.#commitHostGrant({ claim, claimId, completed, deviceId, enrollment })
    this.store.upsertPeerKey({
      deviceId,
      publicKey: completed.publicKey,
      fingerprint: fingerprint(completed.publicKey),
      claimId,
      grantGeneration: generation,
      grantId: grant.grantId,
      grantRevision: grant.revision,
      pairedAt: this.now(),
      displayName: completed.displayName ?? '',
      os: completed.os ?? '',
    })
    if (existing && existing.grantGeneration < generation) {
      const closed = this.#clearDeviceChannels(deviceId)
      this.onRevokedChannels(closed, [deviceId])
    }
    this.#completedPairings.delete(claimId)
    this.store.deletePendingPeer(claimId)
    this.logger?.info('pairing_authorized', { device: deviceId })
    return {
      authorized: true,
      device_id: deviceId,
      device_public_key: completed.publicKey,
      host_public_key: this.identity.publicKey,
      host_fingerprint: this.hostFingerprint,
      // The COMMIT acknowledgement names the exact Host-owned grant row this
      // writer just committed. Access persists it for the Device and for a
      // retry after a lost response; it is never derived from a label.
      grant_id: grant.grantId,
    }
  }

  /** The original approved decision while its grant is still active. */
  #activeDecision(claimId, claim) {
    const decision = this.trust.getCandidate(claimId)
    if (!decision || decision.state !== 'approved' || !decision.grantId) return null
    if (decision.deviceId !== claim.device_id) return null
    if (!(claim.device_public_key instanceof Uint8Array) || !bytesEqual(decision.candidatePublicKey, claim.device_public_key)) return null
    const grant = this.trust.getGrant(decision.grantId)
    if (!grant || grant.state !== 'active'
        || grant.hostKeyFingerprint !== this.hostFingerprint
        || !bytesEqual(grant.devicePublicKey, decision.candidatePublicKey)) return null
    return { decision, grant }
  }

  /**
   * Rights and approver for one COMMIT. The wire has no sponsor field yet, so
   * the local owner flow is the default and any sponsor selection must come
   * from the embedding (or a future authenticated claim field).
   */
  #enrollmentTerms({ claim, claimId, completed, deviceId }) {
    const resolved = this.resolveEnrollmentApprover
      ? this.resolveEnrollmentApprover({ claim, claimId, deviceId, devicePublicKey: completed.publicKey })
      : null
    if (resolved && resolved.approver) {
      const useRights = resolved.useRights ?? DEFAULT_ENROLLMENT_RIGHTS
      return {
        approver: resolved.approver,
        useRights,
        grantableRights: resolved.grantableRights ?? [],
        addDevice: resolved.addDevice ?? false,
      }
    }
    return {
      approver: { kind: 'owner' },
      useRights: DEFAULT_ENROLLMENT_RIGHTS,
      grantableRights: [],
      addDevice: false,
    }
  }

  /** Stage the exact candidate (if needed) and let the grant writer decide. */
  #commitHostGrant({ claim, claimId, completed, deviceId, enrollment }) {
    const existing = this.trust.getCandidate(claimId)
    const sponsor = enrollment.approver.kind === 'sponsor'
    if (!existing) {
      const expiresAt = Number(claim?.expires_at_unix_ms) > this.now()
        ? Number(claim.expires_at_unix_ms)
        : completed.expiresAt
      this.trust.stageCandidate({
        candidateId: claimId,
        method: sponsor ? 'sponsor' : 'local',
        hostKeyFingerprint: this.hostFingerprint,
        candidatePublicKey: completed.publicKey,
        deviceId,
        deviceLabel: completed.displayName ?? '',
        useRights: enrollment.useRights,
        grantableRights: enrollment.grantableRights,
        addDevice: enrollment.addDevice,
        sponsorGrantId: sponsor ? enrollment.approver.grantId : null,
        sponsorRevision: sponsor ? (enrollment.approver.revision ?? null) : null,
        expiresAt,
        now: this.now(),
      })
    } else if (!bytesEqual(existing.candidatePublicKey, completed.publicKey)) {
      throw new PluginError('unauthorized', 'device key mismatch')
    }
    const outcome = this.trust.commitEnrollment({
      candidateId: claimId,
      candidatePublicKey: completed.publicKey,
      useRights: enrollment.useRights,
      grantableRights: enrollment.grantableRights,
      addDevice: enrollment.addDevice,
      approver: enrollment.approver,
      now: this.now(),
    })
    if (!outcome.grant || outcome.grant.state !== 'active'
        || outcome.grant.hostKeyFingerprint !== this.hostFingerprint
        || !bytesEqual(outcome.grant.devicePublicKey, completed.publicKey)) {
      throw new PluginError('unauthorized', 'pairing grant unavailable')
    }
    return outcome.grant
  }

  // --- secure channels ----------------------------------------------------

  /**
   * One channel handshake step. The first call for a channel_id consumes msg1
   * and returns msg2; the second consumes msg3 and installs the channel.
   */
  handleChannelHandshake(request) {
    this.#pruneLocked()
    if (this.#revocationsPending) throw new PluginError('revocations_pending', 'revocation replay pending')
    const channelId = requiredString(request.channel_id, 'channel_id')
    const deviceId = requiredString(request.device_id, 'device_id')
    let connectionRef
    try { connectionRef = validateBrokerConnectionRef(request.connection_ref) }
    catch { throw new PluginError('invalid_argument', 'connection reference missing') }
    const grantGeneration = requestGeneration(request.grant_generation)
    const enrollmentGeneration = requestGeneration(request.enrollment_generation)
    if (grantGeneration === null || enrollmentGeneration === null) {
      throw new PluginError('invalid_argument', 'request generation invalid')
    }
    const message = request.body
    const peer = this.store.getPeerKey(deviceId)
    if (!peer) throw new PluginError('unauthorized', 'device not paired')
    if (this.store.isRevoked(deviceId)) throw new PluginError('forbidden', 'device revoked')
    if (this.getChannel(channelId)) throw new PluginError('conflict', 'channel identity already in use')
    if (!(message instanceof Uint8Array) || message.length === 0) {
      throw new PluginError('invalid_argument', 'handshake message missing')
    }
    const pending = this.#pendingHandshakes.get(channelId)
    if (!pending) {
      if (this.#pendingHandshakes.size >= this.maxPending) {
        throw new PluginError('rate_limited', 'too many handshakes')
      }
      const noise = new NoiseXX({
        initiator: false,
        staticKeypair: this.identity,
        prologue: channelPrologue({ hostId: this.hostId, deviceId, channelId }),
      })
      let hello
      try {
        const payload = noise.readMessage(message)
        hello = this.codec.decode('whalerein.v1.ChannelHello', payload)
      } catch {
        throw new PluginError('unauthorized', 'handshake message rejected')
      }
      if (hello.device_id !== deviceId || hello.host_id !== this.hostId) {
        throw new PluginError('forbidden', 'channel identity mismatch')
      }
      if (protocolMismatch(hello.protocol_version, this.protocolVersion)) {
        throw new PluginError('protocol_version_incompatible', 'channel version mismatch')
      }
      // Snapshot the one active Host grant for this pinned device key at the
      // start of the exchange. The (Host key, device key) tuple names the
      // candidate grant, and every later check binds this exact grant ID. A
      // device without an active grant never reaches session access, even if
      // Access still admits its transport.
      const grant = this.trust.findActiveGrant({
        hostKeyFingerprint: this.hostFingerprint,
        devicePublicKey: peer.publicKey,
      })
      if (!grant) throw new PluginError('forbidden', 'device has no active grant')
      if (grant.deviceId !== deviceId) throw new PluginError('forbidden', 'grant device mismatch')
      // A new Device names the exact grant it expects. A non-empty mismatch is
      // a stale or foreign grant and fails closed before any pending handshake
      // or channel exists. An empty value is accepted only as the legacy
      // compatibility path (old Clients have no grant field); it never relaxes
      // the Host snapshot/re-check below, so the Host still installs only the
      // one active grant for the authenticated device key.
      const clientGrantId = typeof hello.grant_id === 'string' ? hello.grant_id : ''
      if (clientGrantId !== '' && clientGrantId !== grant.grantId) {
        throw new PluginError('forbidden', 'channel grant mismatch')
      }
      // The Host's ChannelReady advertisement is scoped to this Noise channel,
      // never to the Access-broker generation used by HostReady.
      const hostReceive = this.#channelReceive(this.#hostReceive())
      let deviceReceive = null
      if (hello.receive !== undefined) {
        try {
          deviceReceive = validateReceiveAdvertisement(hello.receive)
        } catch {
          throw new PluginError('invalid_argument', 'receive advertisement invalid')
        }
        // The device's generation belongs to this channel's leg. It is
        // validated only as a positive value and frozen below; comparing it
        // with the Host's broker connection generation would cross legs.
      }
      const ready = this.codec.encode('whalerein.v1.ChannelReady', {
        host_id: this.hostId,
        device_id: deviceId,
        protocol_version: this.protocolVersion,
        capabilities: {
          capabilities: [...HOST_CAPABILITIES],
          protocol_version: this.protocolVersion,
        },
        host_nonce: randomNonce(),
        receive: hostReceive,
        // Echo the one grant this channel is bound to. The Device can reject a
        // channel that does not name the grant it asked for.
        grant_id: grant.grantId,
      })
      const reply = noise.writeMessage(ready)
      this.#pendingHandshakes.set(channelId, {
        deviceId,
        devicePublicKey: Buffer.from(peer.publicKey),
        grantId: grant.grantId,
        grantRevision: grant.revision,
        noise,
        connectionRef,
        grantGeneration,
        enrollmentGeneration,
        receive: deviceReceive,
        receiveGeneration: hostReceive.connection_generation,
        expiresAt: this.now() + this.pendingTtlMs,
      })
      this.logger?.info('channel_hello', { device: deviceId, channel: channelId, phase: 'hello' })
      return reply
    }
    if (pending.deviceId !== deviceId) throw new PluginError('forbidden', 'channel device mismatch')
    if (pending.connectionRef !== connectionRef) throw new PluginError('unauthorized', 'connection replaced during handshake')
    if (pending.grantGeneration !== grantGeneration || pending.enrollmentGeneration !== enrollmentGeneration) {
      throw new PluginError('unauthorized', 'authorization scope changed during handshake')
    }
    this.#pendingHandshakes.delete(channelId)
    let payload
    try {
      payload = pending.noise.readMessage(message)
    } catch {
      throw new PluginError('unauthorized', 'handshake message rejected')
    }
    if (payload.length !== 0) throw new PluginError('invalid_argument', 'handshake payload unexpected')
    const remote = pending.noise.remoteStaticKey
    if (!remote || !bytesEqual(remote, peer.publicKey) || !bytesEqual(remote, pending.devicePublicKey)) {
      throw new PluginError('unauthorized', 'device key mismatch')
    }
    // Install only for the SAME grant that was active at hello. Revoke and
    // edit are synchronous with this recheck, so a revoked or replaced grant
    // can never install, and a re-enrollment gets a different grant ID.
    const grant = this.trust.getGrant(pending.grantId)
    if (!grant || grant.state !== 'active'
        || grant.hostKeyFingerprint !== this.hostFingerprint
        || !bytesEqual(grant.devicePublicKey, remote)) {
      throw new PluginError('unauthorized', 'grant changed during handshake')
    }
    // The pinned peer row must still point at this key and (when recorded) at
    // this same grant. A newer pairing supersedes, and this finish fails.
    if (!bytesEqual(peer.publicKey, remote)) throw new PluginError('unauthorized', 'device key mismatch')
    if (peer.grantId && peer.grantId !== grant.grantId) throw new PluginError('unauthorized', 'grant changed during handshake')
    this.pruneIdleChannels()
    let deviceChannels = 0
    for (const channel of this.#channels.values()) {
      if (channel.deviceId === deviceId) deviceChannels++
    }
    if (deviceChannels >= this.maxDeviceChannels || this.#channels.size >= this.maxHostChannels) {
      throw new PluginError('rate_limited', 'channel capacity reached')
    }
    // Application keys are the secret Noise Split outputs. The transcript
    // hash below is retained only for the App-session proof.
    const cipher = pending.noise.takeChannelCipher()
    const now = this.now()
    const channel = {
      channelId,
      deviceId,
      devicePublicKey: Buffer.from(remote),
      grantId: grant.grantId,
      grantRevision: grant.revision,
      connectionRef,
      grantGeneration,
      enrollmentGeneration,
      cipher,
      handshakeHash: pending.noise.handshakeHash,
      createdAt: now,
      lastUsedAt: now,
      inFlight: 0,
      // Set once at install; a later advertisement never mutates it.
      receive: pending.receive ? Object.freeze({ ...pending.receive, budget: pending.receive.budget }) : null,
      receiveGeneration: pending.receiveGeneration,
    }
    this.#channels.set(channelId, channel)
    this.logger?.info('channel_established', { device: deviceId, channel: channelId })
    return new Uint8Array(0)
  }

  /** Noise transcript hash for the exact live channel, if it exists. */
  channelTranscript(channelId, deviceId) {
    const channel = this.#resolveChannel(channelId, deviceId)
    return channel?.handshakeHash ?? null
  }

  #resolveChannel(channelId, deviceId) {
    if (!channelId || !deviceId) return null
    const channel = this.getChannel(channelId)
    if (!channel || channel.deviceId !== deviceId) return null
    return channel
  }

  /**
   * Decode and authenticate one request frame. A wrong channel, MAC, or AAD
   * returns `{ok:false}` without side effects.
   */
  #openRequest(request) {
    const channel = this.#resolveChannel(request.channel_id, request.device_id)
    if (!channel) return { ok: false, code: 'unauthorized', reason: 'channel_missing' }
    // A query is bound to the Broker connection that established the channel.
    // A delayed request stamped with a replaced connection cannot ride a newer
    // channel, even with a valid frame.
    if (request.connection_ref !== undefined && request.connection_ref !== ''
        && channel.connectionRef !== request.connection_ref) {
      return { ok: false, code: 'unauthorized', reason: 'connection_changed' }
    }
    // Top-level generations are Access-filled for query-lane requests only.
    // A durable command carries no such stamp and must not be compared against
    // the channel's admission scope. An explicit zero remains a valid
    // Standalone generation and is still compared.
    const grantGeneration = request.grant_generation === undefined || request.grant_generation === null
      ? null
      : requestGeneration(request.grant_generation)
    const enrollmentGeneration = request.enrollment_generation === undefined || request.enrollment_generation === null
      ? null
      : requestGeneration(request.enrollment_generation)
    if ((grantGeneration !== null && grantGeneration !== channel.grantGeneration)
        || (enrollmentGeneration !== null && enrollmentGeneration !== channel.enrollmentGeneration)) {
      return { ok: false, code: 'unauthorized', reason: 'authorization_scope_changed' }
    }
    let frame
    try {
      frame = this.codec.decode('whalerein.v1.EncryptedFrame', request.body)
    } catch {
      return { ok: false, code: 'unauthorized', reason: 'frame_invalid' }
    }
    if (frame.channel_id !== channel.channelId) return { ok: false, code: 'unauthorized', reason: 'channel_mismatch' }
    const aad = requestAad({
      requestId: request.request_id,
      hostId: this.hostId,
      deviceId: channel.deviceId,
      kind: request.kind,
      commandId: request.command_id ?? '',
      expiresAt: request.expires_at_unix_ms ?? 0,
    })
    let plaintext
    try {
      plaintext = channel.cipher.open('c2h', aad, frame.nonce, frame.ciphertext)
    } catch (error) {
      const reason = error?.code ?? 'authentication_failed'
      this.logger?.warn('frame_rejected', { code: 'unauthorized', device: channel.deviceId, channel: channel.channelId, reason })
      return { ok: false, code: 'unauthorized', reason }
    }
    return { ok: true, plaintext, channel }
  }

  /** Keep an authenticated request's channel available through its response. */
  async withRequest(request, handle) {
    const opened = this.#openRequest(request)
    if (!opened.ok) return handle(opened)
    const { channel, plaintext } = opened
    channel.inFlight++
    try {
      return await handle({ ok: true, plaintext, channel })
    } finally {
      channel.lastUsedAt = this.now()
      channel.inFlight--
    }
  }

  /** The grant tuple a live channel is bound to, or null. */
  getChannelGrant(channelId, deviceId = null) {
    const channel = deviceId === null ? this.getChannel(channelId) : this.#resolveChannel(channelId, deviceId)
    if (!channel) return null
    return {
      grantId: channel.grantId,
      grantRevision: channel.grantRevision,
      deviceId: channel.deviceId,
      devicePublicKey: Buffer.from(channel.devicePublicKey ?? new Uint8Array(0)),
    }
  }

  /** Encrypt one query/command response for the channel that produced it. */
  sealResult({ request, plaintext }) {
    const channel = this.#resolveChannel(request.channel_id, request.device_id)
    if (!channel) return null
    const aad = requestAad({
      requestId: request.request_id,
      hostId: this.hostId,
      deviceId: channel.deviceId,
      kind: request.kind,
      commandId: request.command_id ?? '',
      expiresAt: request.expires_at_unix_ms ?? 0,
    })
    const sealed = channel.cipher.seal('h2c', aad, plaintext)
    return this.codec.encode('whalerein.v1.EncryptedFrame', {
      channel_id: channel.channelId,
      nonce: sealed.nonce,
      ciphertext: sealed.ciphertext,
    })
  }

  /** Encrypt one event envelope body for a specific device channel. */
  sealEvent({ deviceId, channelId, plaintext, createdAt }) {
    const channel = this.#resolveChannel(channelId, deviceId)
    if (!channel) return null
    const aad = eventAad({ hostId: this.hostId, deviceId, channelId, createdAt })
    const sealed = channel.cipher.seal('h2c', aad, plaintext)
    return { channelId, nonce: sealed.nonce, ciphertext: sealed.ciphertext }
  }

  // --- revocation replay --------------------------------------------------

  /**
   * Drop one paired device after this host revoked its own grant.
   * Access has already committed the revocation, so no notice is expected.
   * @param {string} deviceId
   */
  forgetPairedDevice(deviceId, generation = this.store.getPeerKey(deviceId)?.grantGeneration ?? 0) {
    if (typeof deviceId !== 'string' || deviceId === '') return null
    return this.handleRevocation({
      notice: {
        replay_id: '',
        device_ids: [deviceId],
        grant_generations: {[deviceId]: generation},
        final: false,
        revoked_at_unix_ms: this.now(),
      },
    })
  }

  /**
   * Apply one RevocationNotice batch. The plugin never calls Access to delete
   * an ACL; it only applies the authoritative list it was given.
   */
  handleRevocation({ notice }) {
    const replayId = notice.replay_id ?? ''
    const deviceIds = Array.isArray(notice.device_ids) ? notice.device_ids : []
    const applied = this.store.applyRevocations(deviceIds, {
      replayId,
      generations: notice.grant_generations??{},
      revokedAt: notice.revoked_at_unix_ms ?? this.now(),
      now: this.now(),
    })
    // The same authoritative revocation also fences the Host-owned grants for
    // the applied devices, so a delayed transport activation cannot restore
    // session authority and a fresh enrollment starts from a new ceremony.
    const grantIds = []
    if (applied.length > 0) {
      const active = this.trust.listGrants({ hostKeyFingerprint: this.hostFingerprint, state: 'active', limit: 1024 })
      for (const deviceId of applied) {
        for (const grant of active) if (grant.deviceId === deviceId) grantIds.push(grant.grantId)
      }
      if (grantIds.length > 0) this.trust.revokeGrants({ grantIds, now: this.now() })
      for (const deviceId of applied) {
        this.#dropPairingsForDevice(deviceId)
        this.store.deletePendingPeersForDevice(deviceId)
      }
    }
    // Close observations while the channel keys still exist so each Device can
    // receive an authenticated terminal WatchClosed with the revocation reason.
    if (applied.length > 0) this.onRevokedDevices(applied)
    const closed = []
    for (const deviceId of applied) {
      closed.push(...this.#clearDeviceChannels(deviceId))
      this.logger?.info('revocation_applied', { device: deviceId, replay: replayId, source: replayId ? 'access_notice' : 'host_settings', revokedAt: notice.revoked_at_unix_ms, final: notice.final === true })
    }
    if (notice.final === true) this.setRevocationsPending(false)
    this.onRevokedChannels(closed, applied)
    return {
      ack: { replay_id: replayId, device_ids: deviceIds },
      applied,
      closed,
      grants: grantIds,
      final: notice.final === true,
    }
  }

  // --- Host-owned grant revocation ---------------------------------------

  /**
   * Revoke exactly the selected Host grants and fence everything bound to
   * them: pending handshakes, established channels, queued ciphertext and the
   * pinned peer row. Independent child grants and other devices stay live.
   */
  revokeGrants({ grantIds, reason = 'revoked' }) {
    if (!Array.isArray(grantIds)) throw new PluginError('invalid_argument', 'grant ids must be an array')
    if (grantIds.length === 0) {
      return { revoked: [], alreadyRevoked: [], unknown: [], fencedCandidates: [], closed: [], devices: [] }
    }
    const result = this.trust.revokeGrants({ grantIds, now: this.now() })
    const closed = []
    const devices = new Set()
    for (const grantId of result.revoked) {
      const grant = this.trust.getGrant(grantId)
      closed.push(...this.#clearGrantChannels(grantId))
      if (!grant) continue
      devices.add(grant.deviceId)
      this.#dropPairingsForDevice(grant.deviceId, grant.devicePublicKey)
      this.store.deletePendingPeersForDevice(grant.deviceId)
      const peer = this.store.getPeerKey(grant.deviceId)
      if (peer && (peer.grantId === grantId || bytesEqual(peer.publicKey, grant.devicePublicKey))) {
        this.store.deletePeerKey(grant.deviceId)
        this.store.deleteOutboxForDevice(grant.deviceId)
      }
      this.logger?.info('grant_revoked', { device: grant.deviceId, grant: grantId, reason })
    }
    if (closed.length > 0) this.onRevokedChannels(closed, [...devices])
    return { ...result, closed, devices: [...devices] }
  }

  /**
   * Close the channels and pending handshakes of one grant after its rights
   * were edited (for example when the read right is removed). Other grants for
   * other devices are untouched.
   */
  closeGrantChannels(grantId, reason = 'rights_changed') {
    const closed = this.#clearGrantChannels(grantId)
    if (closed.length > 0) this.onClosedChannels(closed, reason)
    return closed
  }

  #clearGrantChannels(grantId) {
    const closed = []
    for (const [channelId, channel] of this.#channels) {
      if (channel.grantId !== grantId) continue
      this.#channels.delete(channelId)
      closed.push({ deviceId: channel.deviceId, channelId })
    }
    for (const [channelId, pending] of this.#pendingHandshakes) {
      if (pending.grantId !== grantId) continue
      this.#markConnectionClosed(pending.connectionRef)
      this.#pendingHandshakes.delete(channelId)
    }
    return closed
  }

  #dropPairingsForDevice(deviceId, devicePublicKey = null) {
    for (const [claimId, pending] of this.#pendingPairings) {
      if (pending.deviceId === deviceId) this.#pendingPairings.delete(claimId)
    }
    for (const [claimId, completed] of this.#completedPairings) {
      if (completed.deviceId !== deviceId) continue
      if (devicePublicKey && !bytesEqual(completed.publicKey, devicePublicKey)) continue
      this.#completedPairings.delete(claimId)
    }
  }
}

function randomNonce() {
  return randomBytes(16).toString('hex')
}
