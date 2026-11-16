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
import { LogicalTransport } from '../../src/access/transport.js'

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
  constructor({ broker, codec, hostId, now = Date.now }) {
    this.broker = broker
    this.codec = codec
    this.hostId = hostId
    this.now = now
    this.results = new Map()
    this.events = []
    this.revocationQueue = []
    this.revocationResults = []
    this.hostReady = false
    this.requestTopic = `whalerein/v1/h/${hostId}/request`
    this.resultTopic = `whalerein/v1/h/${hostId}/result`
    this.eventTopic = `whalerein/v1/h/${hostId}/event`
    this.#attach(broker.connectFn()('mqtts://access.test', { clientId: 'access' }))
  }

  #attach(client) {
    this.client = client
    this.transport = new LogicalTransport({codec:this.codec, publish:(topic,bytes)=>new Promise((resolve,reject)=>client.publish(topic,bytes,{qos:1},error=>error?reject(error):resolve()))})
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
    this.transport.receive(topic,payload,this.requestTopic,bytes=>this.#deliver(topic,bytes))
      .then(consumed=>{if(!consumed)return this.#deliver(topic,payload)})
      .catch(()=>{})
  }

  async #deliver(topic, payload) {
    if (topic === this.resultTopic) {
      const result = this.codec.decode('whalerein.v1.HostResult', payload)
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
      this.events.push(envelope)
      const deviceTopic = `whalerein/v1/h/${this.hostId}/d/${envelope.device_id}/event`
      const ackTopic=`whalerein/v1/h/${this.hostId}/d/${envelope.device_id}/ack`
      await new Promise(resolve=>this.client.subscribe(ackTopic,{qos:1},resolve))
      await this.transport.send(deviceTopic,payload,ackTopic,true)
    }
  }

  async send(request) {
    await this.ready
    await this.transport.send(this.requestTopic,this.codec.encode('whalerein.v1.HostRequest',{...request,relay_transport_version:2}),this.resultTopic,true)
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
      relay_transport_version: 2,
      client_transport_version: 2,
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

  async handshake({ channelId, deviceId, message }) {
    const request = this.#request('REQUEST_KIND_HANDSHAKE', deviceId, message, { channelId })
    await this.send(request)
    const terminal = await this.waitTerminal(request.request_id)
    if (terminal.status !== 'succeeded') throw new WireError(terminal.status, terminal.error_code)
    return terminal.result_body
  }

  #request(kind, deviceId, body, { channelId = '' } = {}) {
    return {
      request_id: randomId(),
      host_id: this.hostId,
      device_id: deviceId,
      kind,
      protocol_version: PROTOCOL_VERSION,
      relay_transport_version: 2,
      client_transport_version: 2,
      channel_id: channelId,
      body,
      expires_at_unix_ms: this.now() + 60_000,
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
    this.client = broker.connectFn()('mqtts://device.test', { clientId: `whalerein-device-${deviceId}` })
    this.transport = new LogicalTransport({codec,publish:(topic,bytes)=>new Promise((resolve,reject)=>this.client.publish(topic,bytes,{qos:1},error=>error?reject(error):resolve()))})
    this.ready = new Promise((resolve) => {
      this.client.on('connect', () => {
        this.client.subscribe(this.eventTopic, { qos: 1 }, () => resolve())
      })
    })
    this.client.on('message', (topic, payload) => this.#onEvent(topic, payload))
  }

  async close() {
    await new Promise((resolve) => this.client.end(false, {}, resolve))
  }

  #onEvent(topic, payload) {
    this.transport.receive(topic,payload,`whalerein/v1/h/${this.hostId}/d/${this.deviceId}/ack`,bytes=>this.#deliverEvent(topic,bytes))
      .then(consumed=>{if(!consumed)this.#deliverEvent(topic,payload)})
      .catch(()=>{})
  }

  #deliverEvent(topic, payload) {
    if (topic !== this.eventTopic || !this.channel) return
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
      relay_transport_version: 2,
      client_transport_version: 2,
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
      relay_transport_version: 2,
      client_transport_version: 2,
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
      relay_transport_version: 2,
      client_transport_version: 2,
      channel_id: this.channel.channelId,
      body: frame,
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
