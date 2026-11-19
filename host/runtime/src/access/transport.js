// Logical MQTT envelopes. Security authenticates the reconstructed envelope;
// Access only sees opaque ciphertext. Never deliver partial logical messages.
import { createHash, randomBytes } from 'node:crypto'
import { PluginError } from '../errors.js'
import { codec as defaultCodec } from '../protocol.js'

export const TRANSPORT_VERSION = 2
export const PACKET_BYTES = 64 * 1024
export const CHUNK_BYTES = 60 * 1024
export const MESSAGE_BYTES = 16 * 1024 * 1024
export const DEFAULT_PROFILE = 'TRANSPORT_PROFILE_DEFAULT'
const PREFIX = Buffer.from('WRMQTT2')
const digest = (value) => createHash('sha256').update(value).digest()

export function encodeFrame(frame, codec = defaultCodec()) {
  return Buffer.concat([PREFIX, codec.encode('whalerein.v1.TransportFrame', frame)])
}

export function decodeFrame(bytes, codec = defaultCodec()) {
  if (!Buffer.from(bytes).subarray(0, PREFIX.length).equals(PREFIX)) return null
  if (bytes.length > PACKET_BYTES) throw new PluginError('invalid_argument', 'transport packet exceeds limit')
  return codec.decode('whalerein.v1.TransportFrame', bytes.subarray(PREFIX.length))
}

export class LogicalTransport {
  #incoming = new Map()
  #completed = new Map()
  #pending = new Map()
  #active = 0
  #waiting = []
  #waitingBytes = 0
  #generation = 0
  constructor({
    publish, codec = defaultCodec(), now = Date.now, ackTimeoutMs = 1000, ttlMs = 30_000, onDeliveryError,
    fragmentPayloadBytes = CHUNK_BYTES, profile = DEFAULT_PROFILE, envelopeBytes = MESSAGE_BYTES, logger,
  }) {
    this.publish = publish
    this.onDeliveryError = onDeliveryError
    this.codec = codec
    this.now = now
    this.ackTimeoutMs = ackTimeoutMs
    this.ttlMs = ttlMs
    // The local receive profile caps what this peer may receive; the negotiated
    // width can only lower it. No frame or metadata field can raise it.
    this.fragmentPayloadBytes = Number.isSafeInteger(fragmentPayloadBytes) && fragmentPayloadBytes > 0
      ? fragmentPayloadBytes : CHUNK_BYTES
    this.envelopeBytes = Number.isSafeInteger(envelopeBytes) && envelopeBytes > 0 ? envelopeBytes : MESSAGE_BYTES
    this.profile = profile
    this.logger = logger
    // Sending uses the smaller of the framed packet budget and the configured
    // width so a compact profile cannot exceed its own fragment geometry.
    this.chunkBytes = Math.min(CHUNK_BYTES, this.fragmentPayloadBytes)
  }

  #reject(code, fields) {
    this.logger?.warn('transport_frame_rejected', { code, ...fields })
    return new PluginError(code, 'transport bounds rejected')
  }

  reset() {
    this.#generation++
    for (const waiter of this.#waiting) { clearTimeout(waiter.timer); waiter.reject(new PluginError('unavailable','transport interrupted')) }
    this.#waiting=[]; this.#waitingBytes=0
    this.#incoming.clear()
    this.#completed.clear()
    for (const pending of this.#pending.values()) pending.reject(new PluginError('unavailable', 'transport interrupted'))
    this.#pending.clear()
  }

  #expire() {
    const now = this.now()
    for (const map of [this.#incoming, this.#completed]) {
      for (const [key, value] of map) if (value.until <= now) map.delete(key)
    }
  }

  async receive(topic, bytes, ackTopic, deliver) {
    const frame = decodeFrame(bytes, this.codec)
    if (!frame) return false
    if (frame.version !== TRANSPORT_VERSION || !/^[a-f0-9]{32}$/.test(frame.transfer_id ?? '') || !frame.topic || frame.topic.length > 512 || frame.digest?.length !== 32) throw this.#reject('invalid_argument', { scope: 'header' })
    const index = Number(frame.index ?? 0)
    const key = `${frame.topic}:${frame.transfer_id}`
    if (frame.ack) {
      const valid = (pending) => pending && pending.ackTopic === topic && Buffer.from(frame.digest).equals(pending.digest)
      const pending = this.#pending.get(`${key}:${index}`)
      if (valid(pending)) {
        if (frame.rejected) pending.reject(new PluginError('unavailable', 'transport rejected'))
        else pending.resolve()
      }
      const completion = this.#pending.get(`${key}:complete`)
      if (valid(completion) && (frame.complete || frame.rejected)) {
        for (const [id, waiter] of this.#pending) {
          if (id.startsWith(`${key}:`) && valid(waiter)) {
            if (frame.rejected) waiter.reject(new PluginError('unavailable', 'transport rejected'))
            else waiter.resolve()
          }
        }
      }
      return true
    }
    // A declared width must be positive, consistent for the transfer, and no
    // larger than this peer's negotiated receive budget.
    const width = frame.fragment_payload_bytes === undefined ? CHUNK_BYTES : Number(frame.fragment_payload_bytes)
    if (!Number.isSafeInteger(width) || width < 1 || width > this.fragmentPayloadBytes) {
      throw this.#reject('payload_too_large', { scope: 'width', bytes: width, limit: this.fragmentPayloadBytes })
    }
    if (frame.profile !== undefined && !/^TRANSPORT_PROFILE_(DEFAULT|COMPACT|ROOMY)$/.test(String(frame.profile))) {
      throw this.#reject('protocol_version_incompatible', { scope: 'profile' })
    }
    if (frame.topic !== topic || !ackTopic) throw new PluginError('forbidden', 'transport topic mismatch')
    const total = Number(frame.total_bytes ?? 0)
    const count = Number(frame.count ?? 0)
    if (!Number.isSafeInteger(total) || total < 1 || total > this.envelopeBytes) {
      throw this.#reject('payload_too_large', { scope: 'envelope', bytes: total, limit: this.envelopeBytes })
    }
    if (count !== Math.ceil(total / width) || index >= count || frame.payload?.length !== Math.min(width, total - index * width)) {
      throw this.#reject('invalid_argument', { scope: 'geometry', bytes: total, limit: width })
    }
    this.#expire()
    const ack = { version: TRANSPORT_VERSION, transfer_id: frame.transfer_id, topic, index, digest: frame.digest, ack: true }
    const done = this.#completed.get(key)
    if (done) {
      if (done.total !== total || !done.digest.equals(frame.digest)) throw new PluginError('invalid_argument', 'transport changed after completion')
      await this.publish(ackTopic, encodeFrame({...ack, complete:true}, this.codec))
      return true
    }
    let state = this.#incoming.get(key)
    if (!state) {
      const reserved = [...this.#incoming.values()].reduce((sum, item) => sum + item.total, 0)
      if (this.#completed.size >= 16384 || (total > this.chunkBytes && (this.#incoming.size >= 4 || reserved + total > 32 * 1024 * 1024))) {
        throw this.#reject('rate_limited', { scope: 'reassembly', bytes: total, limit: 32 * 1024 * 1024, count: this.#incoming.size })
      }
      state = { total, count, width, digest: Buffer.from(frame.digest), parts: new Map(), until: this.now() + this.ttlMs }
      this.#incoming.set(key, state)
    }
    if (state.total !== total || state.count !== count || state.width !== width || !state.digest.equals(frame.digest)) throw this.#reject('invalid_argument', { scope: 'reassembly' })
    const existing = state.parts.get(index)
    if (existing && !existing.equals(frame.payload)) throw new PluginError('invalid_argument', 'conflicting transport duplicate')
    state.parts.set(index, Buffer.from(frame.payload))
    if (state.parts.size === count) {
      const logical = Buffer.concat(Array.from({ length: count }, (_, index) => state.parts.get(index)))
      this.#incoming.delete(key)
      if (!digest(logical).equals(state.digest)) {
        await this.publish(ackTopic, encodeFrame({ ...ack, rejected: true }, this.codec))
        throw new PluginError('invalid_argument', 'transport integrity failed')
      }
      // Reserve completion before async dispatch, preventing duplicate execution.
      this.#completed.set(key, { total, digest: state.digest, until: this.now() + this.ttlMs })
      ack.complete = true
      // Transport completion is independent of business execution duration.
      Promise.resolve().then(() => deliver(logical)).catch(() => this.onDeliveryError?.())
    }
    await this.publish(ackTopic, encodeFrame(ack, this.codec))
    return true
  }

  #acquireBulk(size) {
    if(this.#active<2){this.#active++;return Promise.resolve()}
    if(this.#waiting.length>=4||this.#waitingBytes+size>32*1024*1024)throw new PluginError('rate_limited','transport send queue budget exhausted')
    return new Promise((resolve,reject)=>{
      const waiter={size,resolve,reject,timer:null}
      waiter.timer=setTimeout(()=>{
        const index=this.#waiting.indexOf(waiter)
        if(index>=0){this.#waiting.splice(index,1);this.#waitingBytes-=size}
        reject(new PluginError('unavailable','transport admission timeout'))
      },this.ttlMs)
      this.#waitingBytes+=size;this.#waiting.push(waiter)
    })
  }
  #releaseBulk() {
    this.#active--
    const waiter=this.#waiting.shift()
    if(waiter){clearTimeout(waiter.timer);this.#waitingBytes-=waiter.size;this.#active++;waiter.resolve()}
  }

  async send(topic, bytes, ackTopic, capable) {
    if (bytes.length === 0) throw new PluginError('invalid_argument', 'empty logical envelope')
    if (bytes.length > this.envelopeBytes) {
      this.logger?.warn('transport_frame_rejected', { code: 'payload_too_large', scope: 'envelope', bytes: bytes.length, limit: this.envelopeBytes })
      throw new PluginError('payload_too_large', 'logical message exceeds resource budget')
    }
    if (bytes.length <= this.chunkBytes && !capable) return this.publish(topic, bytes)
    if (!capable) throw new PluginError('protocol_version_incompatible', 'peer does not support logical transport')
    const bulk = bytes.length > this.chunkBytes
    const generation = this.#generation
    const deadline = this.now() + this.ttlMs
    if (bulk) await this.#acquireBulk(bytes.length)
    const transferId = randomBytes(16).toString('hex')
    const hash = digest(bytes)
    const count = Math.ceil(bytes.length / this.chunkBytes)
    const completionKey=`${topic}:${transferId}:complete`
    const completed=new Promise((resolve,reject)=>this.#pending.set(completionKey,{resolve,reject,digest:hash,ackTopic}))
    completed.catch(()=>{})
    const sendPart = async (index) => {
      if (generation !== this.#generation || this.now() >= deadline) throw new PluginError('unavailable', 'transport interrupted')
      const frame = encodeFrame({
        version: TRANSPORT_VERSION, transfer_id: transferId, topic, index, count, total_bytes: bytes.length,
        digest: hash, fragment_payload_bytes: this.chunkBytes, profile: this.profile,
        payload: bytes.subarray(index * this.chunkBytes, Math.min(bytes.length, (index + 1) * this.chunkBytes)),
      }, this.codec)
      const key = `${topic}:${transferId}:${index}`
      try {
        for (let attempt = 0; attempt < 3; attempt++) {
          let timer
          const received = new Promise((resolve, reject) => {
            this.#pending.set(key, { resolve, reject, digest: hash, ackTopic })
            timer = setTimeout(() => reject(new PluginError('unavailable', 'transport receipt timeout')), this.ackTimeoutMs)
          })
          // Attach a handler before publication: disconnect can reject immediately.
          received.catch(() => {})
          try {
            await Promise.all([this.publish(topic, frame), received])
            return
          } catch (error) {
            if (generation !== this.#generation || attempt === 2 || error.code !== 'unavailable') throw error
          } finally { clearTimeout(timer) }
        }
      } finally { this.#pending.delete(key) }
    }
    let completionTimer
    try {
      for (let start = 0; start < count; start += 4) await Promise.all(Array.from({ length: Math.min(4, count - start) }, (_, offset) => sendPart(start + offset)))
      // Completion needs one receipt per part; a full envelope has many parts.
      // Scale the bounded wait with the part count instead of a fixed triple.
      const completionMs = Math.max(this.ackTimeoutMs * 3, Math.min(30_000, count * 50))
      await Promise.race([completed,new Promise((_,reject)=>{completionTimer=setTimeout(()=>reject(new PluginError('unavailable','transport completion timeout')),completionMs)})])
    } finally {
      clearTimeout(completionTimer)
      this.#pending.delete(completionKey)
      if (bulk) this.#releaseBulk()
    }
  }
}
