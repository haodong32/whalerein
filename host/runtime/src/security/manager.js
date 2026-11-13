// Noise XX pairing, secure channels, and revocation replay.
//
// Pairing:  HELLO consumes msg1 and returns msg2. FINISH consumes msg3 and
//           returns the device public key without persisting it. AUTHORIZE is
//           the only path that persists a peer key; Access sends it only after
//           its grants transaction committed.
// Channels: one Noise XX handshake over two HostRequest calls, one live channel
//           per device, a fresh channel_id on every reconnect, and frame AAD
//           exactly as WIRE.md defines it.

import {
  NoiseXX,
  NoiseError,
  channelPrologue,
  deriveChannelKeys,
  eventAad,
  fingerprint,
  formatFingerprint,
  open,
  pairingPrologue,
  requestAad,
  seal,
} from './noise.js'
import { randomBytes } from 'node:crypto'
import { bytesEqual, sha256Bytes } from './digest.js'
import { codec as defaultCodec, resolveCodec } from '../protocol.js'
import { PluginError } from '../errors.js'
import { sanitizeDisplayName } from '../display-name.js'
import { sanitizeOs } from '../os.js'

export const HOST_CAPABILITIES = Object.freeze([
  'sessions',
  'events',
  'messages',
  'approvals',
  'interrupt',
  'sync',
  'snapshot',
])

const TOKEN_HASH = /^[0-9a-fA-F]{64}$/
const PENDING_TTL_MS = 120_000

function protocolMismatch(value, expected) {
  return value !== undefined && value !== null && Number(value) !== expected
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

export class SecurityManager {
  #channels = new Map()
  #deviceChannels = new Map()
  #pendingHandshakes = new Map()
  #pendingPairings = new Map()
  #completedPairings = new Map()
  #revocationsPending = false

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
   * @param {(channels: Array<{deviceId: string, channelId: string}>) => void} [options.onRevokedChannels]
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
    onRevokedChannels = () => {},
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
    this.onRevokedChannels = onRevokedChannels
    this.hostFingerprint = identity.fingerprint ?? fingerprint(identity.publicKey)
    this.hostFingerprintDisplay = formatFingerprint(identity.publicKey)
  }

  get revocationsPending() {
    return this.#revocationsPending
  }

  setRevocationsPending(pending) {
    this.#revocationsPending = Boolean(pending)
    if (pending) this.logger?.info('revocations_pending', {})
  }

  // --- diagnostics --------------------------------------------------------

  listChannels() {
    return [...this.#channels.values()].map((channel) => ({
      channelId: channel.channelId,
      deviceId: channel.deviceId,
      createdAt: channel.createdAt,
    }))
  }

  getChannel(channelId) {
    return this.#channels.get(channelId) ?? null
  }

  /**
   * Drop every live and in-progress channel. A reconnect always requires a
   * fresh Noise handshake with a new channel_id; peer keys and applied
   * revocations are untouched.
   * @returns {Array<{deviceId: string, channelId: string}>} closed channels.
   */
  clearChannels() {
    const closed = [...this.#channels.values()].map((channel) => ({
      deviceId: channel.deviceId,
      channelId: channel.channelId,
    }))
    this.#channels.clear()
    this.#deviceChannels.clear()
    this.#pendingHandshakes.clear()
    if (closed.length > 0) {
      this.logger?.info('channels_cleared', { count: closed.length })
    }
    return closed
  }

  /**
   * Drop the live channel and any in-progress handshake for one device.
   * @param {string} deviceId
   * @returns {Array<{deviceId: string, channelId: string}>} closed channels.
   */
  clearDeviceChannels(deviceId) {
    const closed = []
    for (const [channelId, channel] of this.#channels) {
      if (channel.deviceId !== deviceId) continue
      closed.push({ deviceId, channelId })
      this.#channels.delete(channelId)
      this.#deviceChannels.delete(deviceId)
    }
    for (const [key, pending] of this.#pendingHandshakes) {
      if (pending.deviceId === deviceId) this.#pendingHandshakes.delete(key)
    }
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
    if (this.#pendingPairings.has(claimId) || this.#completedPairings.has(claimId) || this.store.getPendingPeer(claimId)) {
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
    const noiseMessage = noise.writeMessage(reply)
    this.#pendingPairings.set(claimId, {
      claimId,
      tokenHash: claim.token_hash,
      deviceId,
      displayName: sanitizeDisplayName(hello.display_name),
      os: sanitizeOs(hello.os),
      noise,
      createdAt: this.now(),
      expiresAt: this.now() + this.pendingTtlMs,
    })
    this.logger?.info('pairing_hello', { device: deviceId, phase: 'hello' })
    return {
      noise_message: noiseMessage,
      host_public_key: this.identity.publicKey,
      host_fingerprint: this.hostFingerprint,
      device_id: deviceId,
    }
  }

  #pairingFinish({ request, claim, claimId }) {
    const staged=this.#completedPairings.get(claimId) ?? this.store.getPendingPeer(claimId)
    if (staged) {
      if (staged.deviceId!==claim.device_id || staged.tokenHash!==claim.token_hash ||
          !bytesEqual(staged.finishHash,sha256Bytes(claim.noise_message)) || !bytesEqual(staged.authHash,claim.device_auth_hash)) {
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
      if(this.store.isGrantRevoked(claim.device_id,generation)||(peer&&(
        generation<peer.grantGeneration || (generation===peer.grantGeneration&&peer.claimId!==claimId)))) {
        this.store.deletePendingPeer(claimId)
        this.#completedPairings.delete(claimId)
        throw new PluginError('unauthorized','pairing grant superseded or revoked')
      }
    }
    const completed = this.#completedPairings.get(claimId) ?? this.store.getPendingPeer(claimId)
    if (!completed) {
      const existing = this.store.getPeerKey(claim.device_id)
      if (claim.phase === 'PAIRING_PHASE_COMMIT' && existing?.claimId === claimId && bytesEqual(existing.publicKey, claim.device_public_key)) { this.store.deletePendingPeer(claimId); return { authorized: true, device_id: claim.device_id } }
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

    this.store.upsertPeerKey({
      deviceId,
      publicKey: completed.publicKey,
      fingerprint: fingerprint(completed.publicKey),
      claimId,
      grantGeneration: generation,
      pairedAt: this.now(),
      displayName: completed.displayName ?? '',
      os: completed.os ?? '',
    })
    this.#completedPairings.delete(claimId)
    this.store.deletePendingPeer(claimId)
    this.logger?.info('pairing_authorized', { device: deviceId })
    return {
      authorized: true,
      device_id: deviceId,
      device_public_key: completed.publicKey,
      host_public_key: this.identity.publicKey,
      host_fingerprint: this.hostFingerprint,
    }
  }

  // --- secure channels ----------------------------------------------------

  /**
   * One channel handshake step. The first call for a channel_id consumes msg1
   * and returns msg2; the second consumes msg3 and installs the channel.
   */
  handleChannelHandshake({ request, message }) {
    this.#pruneLocked()
    if (this.#revocationsPending) throw new PluginError('revocations_pending', 'revocation replay pending')
    const channelId = requiredString(request.channel_id, 'channel_id')
    const deviceId = requiredString(request.device_id, 'device_id')
    const peer = this.store.getPeerKey(deviceId)
    if (!peer) throw new PluginError('unauthorized', 'device not paired')
    if (this.store.isRevoked(deviceId)) throw new PluginError('forbidden', 'device revoked')
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
      const ready = this.codec.encode('whalerein.v1.ChannelReady', {
        host_id: this.hostId,
        device_id: deviceId,
        protocol_version: this.protocolVersion,
        capabilities: {
          capabilities: [...HOST_CAPABILITIES],
          protocol_version: this.protocolVersion,
        },
        host_nonce: randomNonce(),
      })
      const reply = noise.writeMessage(ready)
      this.#pendingHandshakes.set(channelId, {
        deviceId,
        noise,
        createdAt: this.now(),
        expiresAt: this.now() + this.pendingTtlMs,
      })
      this.logger?.info('channel_hello', { device: deviceId, channel: channelId, phase: 'hello' })
      return { noiseMessage: reply, established: false, replacedChannelId: null }
    }
    if (pending.deviceId !== deviceId) throw new PluginError('forbidden', 'channel device mismatch')
    let payload
    try {
      payload = pending.noise.readMessage(message)
    } catch {
      this.#pendingHandshakes.delete(channelId)
      throw new PluginError('unauthorized', 'handshake message rejected')
    }
    if (payload.length !== 0) throw new PluginError('invalid_argument', 'handshake payload unexpected')
    const remote = pending.noise.remoteStaticKey
    if (!remote || !bytesEqual(remote, peer.publicKey)) {
      this.#pendingHandshakes.delete(channelId)
      throw new PluginError('unauthorized', 'device key mismatch')
    }
    const derived = deriveChannelKeys(
      pending.noise.handshakeHash,
      channelPrologue({ hostId: this.hostId, deviceId, channelId }),
    )
    const replaced = this.#deviceChannels.get(deviceId) ?? null
    if (replaced) {
      this.#channels.delete(replaced)
      this.store.deleteOutboxForChannel(deviceId, replaced)
    }
    const channel = {
      channelId,
      deviceId,
      c2h: derived.c2h,
      h2c: derived.h2c,
      createdAt: this.now(),
    }
    this.#channels.set(channelId, channel)
    this.#deviceChannels.set(deviceId, channelId)
    this.#pendingHandshakes.delete(channelId)
    this.logger?.info('channel_established', { device: deviceId, channel: channelId })
    return { noiseMessage: new Uint8Array(0), established: true, replacedChannelId: replaced }
  }

  #resolveChannel(channelId, deviceId) {
    if (!channelId || !deviceId) return null
    const channel = this.#channels.get(channelId)
    if (!channel || channel.deviceId !== deviceId) return null
    return channel
  }

  /**
   * Decode and authenticate one request frame. A wrong channel, MAC, or AAD
   * returns `{ok:false}` without side effects.
   */
  openRequest({ request, body }) {
    const channel = this.#resolveChannel(request.channel_id, request.device_id)
    if (!channel) return { ok: false, code: 'unauthorized', reason: 'channel_missing' }
    let frame
    try {
      frame = this.codec.decode('whalerein.v1.EncryptedFrame', body)
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
    const plaintext = open(channel.c2h, aad, frame.nonce, frame.ciphertext)
    if (!plaintext) {
      this.logger?.warn('frame_rejected', { code: 'unauthorized', device: channel.deviceId, channel: channel.channelId })
      return { ok: false, code: 'unauthorized', reason: 'authentication_failed' }
    }
    return { ok: true, plaintext, channel }
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
    const sealed = seal(channel.h2c, aad, plaintext)
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
    const sealed = seal(channel.h2c, aad, plaintext)
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
    const closed = []
    for (const deviceId of applied) {
      const channelId = this.#deviceChannels.get(deviceId)
      if (channelId) {
        this.#channels.delete(channelId)
        this.#deviceChannels.delete(deviceId)
        this.store.deleteOutboxForDevice(deviceId)
        closed.push({ deviceId, channelId })
      }
      this.logger?.info('revocation_applied', { device: deviceId, replay: replayId, source: replayId ? 'access_notice' : 'host_settings', revokedAt: notice.revoked_at_unix_ms, final: notice.final === true })
    }
    if (notice.final === true) this.setRevocationsPending(false)
    this.onRevokedChannels(closed)
    return {
      ack: { replay_id: replayId, device_ids: deviceIds },
      applied,
      closed,
      final: notice.final === true,
    }
  }
}

function randomNonce() {
  return randomBytes(16).toString('hex')
}
