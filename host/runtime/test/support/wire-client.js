// Device-side wire helper for the local harness and integration tests. It
// implements the client half of pairing, the secure channel, encrypted
// queries/commands, result decryption, and per-device event delivery.

import { randomBytes } from 'node:crypto'
import {
  NoiseXX,
  channelPrologue,
  deriveChannelKeys,
  eventAad,
  fingerprint,
  generateKeypair,
  open,
  pairingPrologue,
  requestAad,
  seal,
} from '../../src/security/noise.js'
import { randomId, sha256Bytes } from '../../src/security/digest.js'

/** In-memory Access temporary ciphertext directory for the local harness. */
export class MemoryRelayObjects {
  #objects = new Map()
  calls = []
  async reserve({ ciphertext, ...scope }) {
    this.calls.push('reserve')
    const objectId = randomId(16)
    this.#objects.set(objectId, { ciphertext: Buffer.from(ciphertext), scope })
    return {
      object_id: objectId,
      owner_node_id: 'harness-node',
      upload_path: `/v1/relay/objects/${objectId}`,
      max_bytes: ciphertext.length,
      expires_at_unix_ms: scope.expiresAt ?? Date.now() + 60_000,
      upload_timeout_ms: 5000,
    }
  }
  async upload({ reservation, ciphertext }) {
    this.calls.push('upload')
    const object = this.#objects.get(reservation.object_id)
    if (!object) throw Object.assign(new Error('missing'), { code: 'not_found' })
    object.ciphertext = Buffer.from(ciphertext)
  }
  async finish({ objectId }) {
    this.calls.push('finish')
    const object = this.#objects.get(objectId)
    return { object_id: objectId, purpose: object?.scope?.purpose ?? 'PAYLOAD_PURPOSE_EVENT' }
  }
  async fetch({ reference }) {
    this.calls.push('fetch')
    const object = this.#objects.get(reference.object_id)
    if (!object) throw Object.assign(new Error('missing'), { code: 'not_found' })
    return object.ciphertext
  }
  async delete({ objectId }) {
    this.calls.push('delete')
    this.#objects.delete(objectId)
    return { cancelled: true }
  }
}

const PROTOCOL_VERSION = 1

export class WireError extends Error {
  constructor(status, code) {
    super(`${status}:${code}`)
    this.name = 'WireError'
    this.status = status
    this.code = code
  }
}

function timeout(ms, message) {
  return new Promise((_, reject) => {
    const timer = setTimeout(() => reject(new Error(message)), ms)
    timer.unref?.()
  })
}

export class FakeAccessSimulator {
  /**
   * @param {object} options
   * @param {import('./fake-mqtt.js').FakeBroker} options.broker
   * @param {import('../../src/protocol.js').ProtoCodec} options.codec
   * @param {string} options.hostId
   */
  constructor({ broker, codec, hostId, now = Date.now, relay = null }) {
    this.broker = broker
    this.relay = relay
    this.codec = codec
    this.hostId = hostId
    this.now = now
    this.results = new Map()
    this.events = []
    this.revocationQueue = []
    this.revocationResults = []
    this.hostReady = false
    this.connectionRef = '0123456789abcdef:access-connection'
    this.requestTopic = `whalerein/v1/h/${hostId}/request`
    this.resultTopic = `whalerein/v1/h/${hostId}/result`
    this.eventTopic = `whalerein/v1/h/${hostId}/event`
    this.liveEvents = []
    this.#attach(broker.connectFn()('mqtts://access.test', { clientId: 'access' }))
  }

  #attach(client) {
    this.client = client
    this.ready = new Promise((resolve) => {
      client.on('connect', () => {
        client.subscribe(this.resultTopic, { qos: 1 }, () => {
          client.subscribe(this.eventTopic, { qos: 1 }, () => resolve())
        })
      })
    })
    client.on('message', (topic, payload) => this.#onMessage(topic, payload))
  }

  /** Reconnect the simulated Access after a broker-only outage. */
  async reconnect() {
    try { this.client.end?.(true) } catch { /* already offline */ }
    this.hostReady = false
    this.#attach(this.broker.connectFn()('mqtts://access.test', { clientId: 'access' }))
    await this.ready
  }

  #onMessage(topic, payload) {
    this.#deliver(topic, Buffer.from(payload))
  }

  async #deliver(topic, payload) {
    if (topic === this.resultTopic) {
      const result = this.codec.decode('whalerein.v1.HostResult', payload)
      if (result.result_reference && this.relay) {
        result.result_body = await this.relay.fetch({ reference: result.result_reference })
        delete result.result_reference
      }
      if (!result.request_id) {
        this.hostReady = true
        this.#replayRevocations()
        return
      }
      const list = this.results.get(result.request_id) ?? []
      list.push(result)
      this.results.set(result.request_id, list)
      return
    }
    if (topic === this.eventTopic) {
      const envelope = this.codec.decode('whalerein.v1.EventEnvelope', payload)
      if (envelope.payload_reference && this.relay) {
        envelope.ciphertext = await this.relay.fetch({ reference: envelope.payload_reference })
        delete envelope.payload_reference
      }
      this.events.push(envelope)
      const deviceTopic = `whalerein/v1/h/${this.hostId}/d/${envelope.device_id}/event`
      // The resolved ciphertext models an on-demand HTTPS read; it is delivered
      // directly so raw MQTT packets stay within the broker limit.
      this.broker.deliverDirect(deviceTopic, this.codec.encode('whalerein.v1.EventEnvelope', envelope))
      return
    }
  }

  async send(request) {
    await this.ready
    const properties = request.kind === 'REQUEST_KIND_QUERY' || request.kind === 'REQUEST_KIND_HANDSHAKE'
      ? { correlationData: Buffer.from(request.request_id), responseTopic: this.resultTopic,
          messageExpiryInterval: Math.max(1, Math.ceil((Number(request.expires_at_unix_ms) - Date.now()) / 1000)) }
      : undefined
    const bytes = this.codec.encode('whalerein.v1.HostRequest', { relay_version: 5, ...request })
    // The simulated Device -> Access hop may deliver an oversized complete
    // request out of band; real Access would resolve it from HTTPS.
    if (this.broker.maxPacketBytes !== Infinity && bytes.length > this.broker.maxPacketBytes) {
      this.broker.deliverDirect(this.requestTopic, bytes, properties)
      return
    }
    await new Promise((resolve, reject) => this.client.publish(this.requestTopic, bytes, { qos: 1, properties }, error => error ? reject(error) : resolve()))
  }

  /** Queue revocation batches to replay on the next HostReady. */
  queueRevocations(notices) {
    this.revocationQueue.push(...notices)
  }

  async #replayRevocations() {
    const notices = this.revocationQueue.splice(0)
    if (notices.length === 0) notices.push({ replay_id: `replay-${randomId(4)}`, device_ids: [], final: true })
    for (const notice of notices) {
      const request = {
        request_id: randomId(),
        host_id: this.hostId,
        device_id: '',
        kind: 'REQUEST_KIND_REVOCATION',
        protocol_version: PROTOCOL_VERSION,
        body: this.codec.encode('whalerein.v1.RevocationNotice', notice),
        expires_at_unix_ms: this.now() + 60_000,
      }
      await this.send(request)
      const terminal = await this.waitTerminal(request.request_id, 5000)
      this.revocationResults.push(terminal)
    }
  }

  async waitForHostReady(timeoutMs = 5000) {
    if (this.hostReady) return
    await Promise.race([this.ready, timeout(timeoutMs, 'access not ready')])
    const start = Date.now()
    while (!this.hostReady) {
      if (Date.now() - start > timeoutMs) throw new Error('host ready timeout')
      await new Promise((resolve) => setTimeout(resolve, 5))
    }
  }

  resultsFor(requestId) {
    return this.results.get(requestId) ?? []
  }

  async waitTerminal(requestId, timeoutMs = 5000) {
    const start = Date.now()
    for (;;) {
      const list = this.results.get(requestId) ?? []
      const terminal = list.find((result) => ['succeeded', 'failed', 'rejected', 'unknown', 'host_offline'].includes(result.status))
      if (terminal) return terminal
      if (Date.now() - start > timeoutMs) throw new Error(`result timeout ${requestId}`)
      await new Promise((resolve) => setTimeout(resolve, 2))
    }
  }

  async waitResults(requestId, timeoutMs = 5000) {
    const terminal = await this.waitTerminal(requestId, timeoutMs)
    return this.results.get(requestId) ?? [terminal]
  }

  async pairingClaim({ phase, claimId, tokenHash, noiseMessage, deviceId, deviceAuthHash }) {
    const body = this.codec.encode('whalerein.v1.PairingClaim', {
      claim_id: claimId,
      token_hash: tokenHash,
      phase,
      noise_message: noiseMessage,
      device_auth_hash: deviceAuthHash,
      device_id: deviceId,
      expires_at_unix_ms: this.now() + 60_000,
    })
    const request = this.#request('REQUEST_KIND_PAIRING_CLAIM', deviceId, body)
    await this.send(request)
    const terminal = await this.waitTerminal(request.request_id)
    if (terminal.status !== 'succeeded') throw new WireError(terminal.status, terminal.error_code)
    return this.codec.decode('whalerein.v1.PairingClaimResult', terminal.result_body)
  }

  async pairingAuthorize({ claimId, deviceId, devicePublicKey }) {
    let terminal
    for (const phase of ['PAIRING_PHASE_AUTHORIZE', 'PAIRING_PHASE_COMMIT']) {
    const body = this.codec.encode('whalerein.v1.PairingClaim', {
      claim_id: claimId,
      phase,
      device_id: deviceId,
      device_public_key: devicePublicKey,
      expires_at_unix_ms: this.now() + 60_000,
    })
    const request = this.#request('REQUEST_KIND_PAIRING_AUTHORIZE', deviceId, body)
    await this.send(request)
    terminal = await this.waitTerminal(request.request_id)
    if (terminal.status !== 'succeeded') throw new WireError(terminal.status, terminal.error_code)
    }
    return this.codec.decode('whalerein.v1.PairingClaimResult', terminal.result_body)
  }

  async handshake({ channelId, deviceId, message, connectionRef = this.connectionRef }) {
    const request = this.#request('REQUEST_KIND_HANDSHAKE', deviceId, message, { channelId, connectionRef })
    await this.send(request)
    const terminal = await this.waitTerminal(request.request_id)
    if (terminal.status !== 'succeeded') throw new WireError(terminal.status, terminal.error_code)
    return terminal.result_body
  }

  #request(kind, deviceId, body, { channelId = '', connectionRef = null } = {}) {
    return {
      request_id: randomId(),
      host_id: this.hostId,
      device_id: deviceId,
      kind,
      protocol_version: PROTOCOL_VERSION,
      channel_id: channelId,
      body,
      expires_at_unix_ms: this.now() + 60_000,
      ...(connectionRef ? { connection_ref: connectionRef, grant_generation: 1, enrollment_generation: 1 } : {}),
    }
  }
}

export class DeviceClient {
  #events = []
  #eventWaiters = []

  /**
   * @param {object} options
   * @param {FakeAccessSimulator} options.access
   * @param {import('./fake-mqtt.js').FakeBroker} options.broker
   * @param {import('../../src/protocol.js').ProtoCodec} options.codec
   * @param {string} options.hostId
   * @param {string} options.deviceId
   * @param {{privateKey: Uint8Array, publicKey: Uint8Array}} [options.identity]
   * @param {() => number} [options.now]
   */
  constructor({ access, broker, codec, hostId, deviceId, identity = null, now = Date.now }) {
    this.access = access
    this.codec = codec
    this.hostId = hostId
    this.deviceId = deviceId
    this.identity = identity ?? generateKeypair()
    this.now = now
    this.channel = null
    this.pairing = null
    this.eventTopic = `whalerein/v1/h/${hostId}/d/${deviceId}/event`
    this.liveTopic = `whalerein/v1/h/${hostId}/d/${deviceId}/live`
    this.client = broker.connectFn()('mqtts://device.test', { clientId: `whalerein-device-${deviceId}` })
    this.ready = new Promise((resolve) => {
      this.client.on('connect', () => {
        this.client.subscribe(this.eventTopic, { qos: 1 }, () => {
          this.client.subscribe(this.liveTopic, { qos: 0 }, () => resolve())
        })
      })
    })
    this.client.on('message', (topic, payload) => this.#onEvent(topic, payload))
  }

  async close() {
    await new Promise((resolve) => this.client.end(false, {}, resolve))
  }

  #onEvent(topic, payload) {
    this.#deliverEvent(topic, Buffer.from(payload))
  }

  #deliverEvent(topic, payload) {
    if ((topic !== this.eventTopic && topic !== this.liveTopic) || !this.channel) return
    let envelope
    try {
      envelope = this.codec.decode('whalerein.v1.EventEnvelope', payload)
    } catch {
      return
    }
    if (envelope.device_id !== this.deviceId || envelope.channel_id !== this.channel.channelId) return
    const aad = eventAad({
      hostId: this.hostId,
      deviceId: this.deviceId,
      channelId: this.channel.channelId,
      createdAt: envelope.created_at_unix_ms,
    })
    const plaintext = open(this.channel.h2c, aad, envelope.nonce, envelope.ciphertext)
    if (!plaintext) return
    const event = this.codec.decode('whalerein.v1.EventPayload', plaintext)
    this.#events.push(event)
    for (const waiter of this.#eventWaiters.splice(0)) waiter(event)
  }

  async beginPairing({ claimId = randomId(), tokenHash, displayName = 'test device', os = 'macos' }) {
    const noise = new NoiseXX({
      initiator: true,
      staticKeypair: this.identity,
      prologue: pairingPrologue({ hostId: this.hostId, tokenHash, claimId }),
    })
    const hello = this.codec.encode('whalerein.v1.PairingHello', {
      device_id: this.deviceId,
      host_id: this.hostId,
      protocol_version: PROTOCOL_VERSION,
      capabilities: { capabilities: ['sessions', 'events', 'messages', 'approvals', 'interrupt', 'sync', 'snapshot'], protocol_version: PROTOCOL_VERSION },
      device_nonce: randomId(8),
      display_name: displayName,
      os,
    })
    const msg1 = noise.writeMessage(hello)
    const result = await this.access.pairingClaim({
      phase: 'PAIRING_PHASE_HELLO',
      claimId,
      tokenHash,
      noiseMessage: msg1,
      deviceId: this.deviceId,
    })
    noise.readMessage(result.noise_message)
    const hostPublicKey = noise.remoteStaticKey
    const hostFingerprint = fingerprint(hostPublicKey)
    if (!same(result.host_public_key, hostPublicKey)) throw new Error('pairing host key mismatch')
    if (result.host_fingerprint !== hostFingerprint) throw new Error('pairing fingerprint mismatch')
    this.pairing = { claimId, tokenHash, noise, hostPublicKey, hostFingerprint }
    return { hostPublicKey, hostFingerprint }
  }

  async finishPairing({ hostFingerprint = this.pairing?.hostFingerprint, authorize = true } = {}) {
    if (!this.pairing) throw new Error('pairing not started')
    const finish = this.codec.encode('whalerein.v1.PairingFinish', {
      device_id: this.deviceId,
      host_id: this.hostId,
      host_fingerprint: hostFingerprint,
      device_auth_hash: new Uint8Array(32).fill(7),
    })
    const msg3 = this.pairing.noise.writeMessage(finish)
    const result = await this.access.pairingClaim({
      phase: 'PAIRING_PHASE_FINISH',
      deviceAuthHash: new Uint8Array(32).fill(7),
      claimId: this.pairing.claimId,
      tokenHash: this.pairing.tokenHash,
      noiseMessage: msg3,
      deviceId: this.deviceId,
    })
    if (!same(result.device_public_key, this.identity.publicKey)) throw new Error('pairing device key mismatch')
    const completed = { ...this.pairing, devicePublicKey: result.device_public_key }
    this.pairing = completed
    if (authorize) {
      const authorized = await this.access.pairingAuthorize({
        claimId: completed.claimId,
        deviceId: this.deviceId,
        devicePublicKey: this.identity.publicKey,
      })
      if (!authorized.authorized) throw new Error('pairing not authorized')
    }
    return result
  }

  async connectChannel({ channelId = randomBytes(8).toString('hex') } = {}) {
    const prologue = channelPrologue({ hostId: this.hostId, deviceId: this.deviceId, channelId })
    const noise = new NoiseXX({ initiator: true, staticKeypair: this.identity, prologue })
    const hello = this.codec.encode('whalerein.v1.ChannelHello', {
      device_id: this.deviceId,
      host_id: this.hostId,
      protocol_version: PROTOCOL_VERSION,
      capabilities: { capabilities: ['sessions', 'events', 'messages', 'approvals', 'interrupt', 'sync', 'snapshot'], protocol_version: PROTOCOL_VERSION },
      client_nonce: randomId(8),
    })
    const msg1 = noise.writeMessage(hello)
    const msg2 = await this.access.handshake({ channelId, deviceId: this.deviceId, message: msg1 })
    const readyPayload = noise.readMessage(msg2)
    const ready = this.codec.decode('whalerein.v1.ChannelReady', readyPayload)
    if (ready.device_id !== this.deviceId || ready.host_id !== this.hostId) throw new Error('channel identity mismatch')
    const msg3 = noise.writeMessage(new Uint8Array(0))
    await this.access.handshake({ channelId, deviceId: this.deviceId, message: msg3 })
    const derived = deriveChannelKeys(noise.handshakeHash, prologue)
    this.channel = { channelId, c2h: derived.c2h, h2c: derived.h2c }
    this.#events = []
    return this.channel
  }

  async query(payload) {
    return this.#encrypted('REQUEST_KIND_QUERY', payload, {})
  }

  async command(payload, { commandId = randomId() } = {}) {
    return this.#encrypted('REQUEST_KIND_COMMAND', payload, { commandId })
  }

  async #encrypted(kind, payload, { commandId = '' }) {
    if (!this.channel) throw new Error('channel not established')
    const requestId = randomId()
    const expiresAt = this.now() + 60_000
    const plaintext = this.codec.encode('whalerein.v1.RequestPayload', payload)
    const aad = requestAad({
      requestId,
      hostId: this.hostId,
      deviceId: this.deviceId,
      kind,
      commandId,
      expiresAt,
    })
    const sealed = seal(this.channel.c2h, aad, plaintext)
    const frame = this.codec.encode('whalerein.v1.EncryptedFrame', {
      channel_id: this.channel.channelId,
      nonce: sealed.nonce,
      ciphertext: sealed.ciphertext,
    })
    const request = {
      request_id: requestId,
      host_id: this.hostId,
      device_id: this.deviceId,
      kind,
      command_id: commandId,
      payload_digest: commandId ? sha256Bytes(frame) : new Uint8Array(0),
      expires_at_unix_ms: expiresAt,
      trace_id: randomId(8),
      protocol_version: PROTOCOL_VERSION,
      channel_id: this.channel.channelId,
      body: frame,
      connection_ref: this.access.connectionRef,
      grant_generation: 1,
      enrollment_generation: 1,
    }
    const outcome = await this.send(request)
    return { ...outcome, request, aad }
  }

  async send(request) {
    await this.access.send(request)
    const results = await this.access.waitResults(request.request_id)
    const terminal = results[results.length - 1]
    let response = null
    if (terminal.result_body instanceof Uint8Array && terminal.result_body.length > 0) {
      const aad = requestAad({
        requestId: request.request_id,
        hostId: request.host_id,
        deviceId: request.device_id,
        kind: request.kind,
        commandId: request.command_id ?? '',
        expiresAt: request.expires_at_unix_ms,
      })
      const frame = this.codec.decode('whalerein.v1.EncryptedFrame', terminal.result_body)
      const plaintext = open(this.channel.h2c, aad, frame.nonce, frame.ciphertext)
      if (plaintext) response = this.codec.decode('whalerein.v1.ResponsePayload', plaintext)
    }
    return {
      request,
      results,
      status: terminal.status,
      errorCode: terminal.error_code,
      response,
    }
  }

  async events({ timeoutMs = 5000 } = {}) {
    if (this.#events.length > 0) return this.#events.splice(0)
    return [await Promise.race([
      new Promise((resolve) => this.#eventWaiters.push(resolve)),
      timeout(timeoutMs, 'event timeout'),
    ])]
  }

  async nextEvent({ timeoutMs = 5000 } = {}) {
    const [event] = await this.events({ timeoutMs })
    return event
  }

  get bufferedEvents() {
    return [...this.#events]
  }
}

function same(a, b) {
  if (!(a instanceof Uint8Array) || !(b instanceof Uint8Array) || a.length !== b.length) return false
  let diff = 0
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i]
  return diff === 0
}
