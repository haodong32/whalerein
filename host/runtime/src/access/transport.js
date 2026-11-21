// Complete-message relay adapter. One application message travels in exactly
// one MQTT PUBLISH: inline when the full encoded packet fits the effective hop
// limit, otherwise a packet-sized PayloadReference to complete HTTPS
// ciphertext under the same message identity. There are no fragments,
// reassembly maps, receipt acknowledgements or device ACK publishes.

import { PluginError } from '../errors.js'
import { codec as defaultCodec, resolveCodec } from '../protocol.js'
import { sha256Bytes } from '../security/digest.js'
import {
  BROKER_PACKET_LIMIT_BYTES,
  RELAY_VERSION,
  transportProfileBudget,
  validateReceiveAdvertisement,
  validateTransportBudget,
} from '../contracts/transport.js'
import {
  effectivePacketBytes,
  maxPlaintextBytes,
  publishPacketBytes,
  varintBytes,
} from './packet.js'
import { validatePayloadReference } from './relay-client.js'

export {
  BROKER_PACKET_LIMIT_BYTES,
  RELAY_VERSION,
  effectivePacketBytes,
  maxPlaintextBytes,
  publishPacketBytes,
  varintBytes,
}

// Compatibility exports for the packaged default profile. Live code takes the
// value from the resolved transport budget instead of these constants.
export const PACKET_BYTES = 64 * 1024
export const MESSAGE_BYTES = 16 * 1024 * 1024

const EMPTY = new Uint8Array(0)
const DEFAULT_OBJECT_TTL_MS = 5 * 60 * 1000

const DESCRIPTORS = Object.freeze({
  result: Object.freeze({
    type: 'whalerein.v1.HostResult',
    inline: 'result_body',
    reference: 'result_reference',
    purpose: 'PAYLOAD_PURPOSE_RESULT',
  }),
  event: Object.freeze({
    type: 'whalerein.v1.EventEnvelope',
    inline: 'ciphertext',
    reference: 'payload_reference',
    purpose: 'PAYLOAD_PURPOSE_EVENT',
  }),
  request: Object.freeze({
    type: 'whalerein.v1.HostRequest',
    purpose: 'PAYLOAD_PURPOSE_REQUEST',
  }),
})

function descriptorFor(kind, topic = '') {
  if (kind && DESCRIPTORS[kind]) return DESCRIPTORS[kind]
  if (typeof topic === 'string' && topic.endsWith('/result')) return DESCRIPTORS.result
  if (typeof topic === 'string' && topic.endsWith('/event')) return DESCRIPTORS.event
  return DESCRIPTORS.request
}

function present(value) {
  return value instanceof Uint8Array && value.length > 0
}

/** Reject any envelope that does not name the one relay epoch. */
export function assertRelayEpoch(envelope) {
  const epoch = envelope?.relay_version
  if (Number(epoch) !== RELAY_VERSION) {
    throw new PluginError('protocol_version_incompatible', 'relay epoch is not 3')
  }
  return envelope
}

const INLINE_ONLY_REQUESTS = new Set([
  'REQUEST_KIND_HANDSHAKE',
  'REQUEST_KIND_PAIRING_CLAIM',
  'REQUEST_KIND_PAIRING_AUTHORIZE',
  'REQUEST_KIND_REVOCATION',
  'REQUEST_KIND_HOST_CLIENTS',
  'REQUEST_KIND_ACCESS_READY',
])

/**
 * A request carries exactly one complete body representation. Bootstrap and
 * internal notices stay inline; QUERY and COMMAND may use a reference.
 */
export function assertRequestRepresentation(request) {
  const inline = present(request?.body)
  const referenced = request?.body_reference != null
  if (inline && referenced) {
    throw new PluginError('invalid_argument', 'request carries both body representations')
  }
  if (INLINE_ONLY_REQUESTS.has(request?.kind)) {
    if (referenced) throw new PluginError('invalid_argument', 'bootstrap request must stay inline')
    if (!inline) throw new PluginError('invalid_argument', 'bootstrap request body missing')
    return request
  }
  if (request?.kind === 'REQUEST_KIND_QUERY' || request?.kind === 'REQUEST_KIND_COMMAND') {
    if (!inline && !referenced) throw new PluginError('invalid_argument', 'request body missing')
    return request
  }
  // Unknown kinds are left to the business router's explicit version error,
  // but a request may never carry two representations.
  return request
}

// A result without a body is legitimate only for a status-only control update
// or a terminal outcome whose content could not be admitted.
const BODYLESS_RESULT_STATUSES = new Set([
  'host_accepted',
  'host_offline',
  'rejected',
  'succeeded',
  'failed',
  'unknown',
])

/** A HostResult carries at most one representation; both is always invalid. */
export function assertResultRepresentation(result) {
  const inline = present(result?.result_body)
  const referenced = result?.result_reference != null
  if (inline && referenced) {
    throw new PluginError('invalid_argument', 'result carries both body representations')
  }
  if (!inline && !referenced && !BODYLESS_RESULT_STATUSES.has(result?.status)) {
    throw new PluginError('invalid_argument', 'result body representation missing')
  }
  return result
}

/** An EventEnvelope carries the nonce plus exactly one body representation. */
export function assertEventRepresentation(event) {
  const inline = present(event?.ciphertext)
  const referenced = event?.payload_reference != null
  if (inline && referenced) {
    throw new PluginError('invalid_argument', 'event carries both body representations')
  }
  if (!inline && !referenced) {
    throw new PluginError('invalid_argument', 'event body representation missing')
  }
  return event
}

/**
 * The one complete-message relay adapter.
 *
 * `publish(topic, bytes)` submits one MQTT PUBLISH. Publication is ordered per
 * topic: a later message for the same topic never overtakes an earlier one.
 */
export class RelayTransport {
  #chains = new Map()
  #liveChains = new Map()
  #generation = 0

  constructor({
    publish,
    relay = null,
    budget = null,
    brokerPacketBytes = BROKER_PACKET_LIMIT_BYTES,
    codec = defaultCodec,
    now = Date.now,
    logger,
    objectTtlMs = DEFAULT_OBJECT_TTL_MS,
  }) {
    if (typeof publish !== 'function') throw new PluginError('invalid_argument', 'publish is required')
    this.publish = publish
    this.relay = relay
    this.budget = budget ? validateTransportBudget(budget) : transportProfileBudget('default')
    this.brokerPacketBytes = Number.isSafeInteger(brokerPacketBytes) && brokerPacketBytes > 0
      ? Math.min(brokerPacketBytes, BROKER_PACKET_LIMIT_BYTES)
      : BROKER_PACKET_LIMIT_BYTES
    this.codec = resolveCodec(codec)
    this.now = now
    this.logger = logger
    this.objectTtlMs = Number.isSafeInteger(objectTtlMs) && objectTtlMs > 0 ? objectTtlMs : DEFAULT_OBJECT_TTL_MS
  }

  get envelopeBytes() {
    return this.budget.envelope_bytes
  }

  /** Effective packet limit for the Host -> Access hop in one direction. */
  effectivePacketLimit(receiverLimit) {
    return effectivePacketBytes(this.budget.packet_bytes, this.brokerPacketBytes, receiverLimit)
  }

  reset() {
    this.#generation++
    this.#chains.clear()
    this.#liveChains.clear()
  }

  #chain(topic, run, live = false) {
    // Live previews use their own ordered chain. A committed event that is
    // staging a large HTTPS reference must never stall a small preview behind
    // it: previews are droppable and the committed message remains
    // authoritative, while both lanes keep their own sequence order.
    const chains = live ? this.#liveChains : this.#chains
    const previous = chains.get(topic) ?? Promise.resolve()
    const next = previous.catch(() => {}).then(run)
    chains.set(topic, next)
    const cleanup = () => {
      if (chains.get(topic) === next) chains.delete(topic)
    }
    next.then(cleanup, cleanup)
    return next
  }

  /**
   * Publish one complete serialized envelope.
   *
   * options.kind selects the envelope contract (`result`, `event` or
   * `request`). options.receiverLimit is the authenticated Access
   * advertisement for this hop. options.expiresAt, sessionId, partId and
   * deviceId/channelId describe the relay-object scope for a reference.
   */
  sendComplete(topic, envelopeBytes, options = {}) {
    if (!(envelopeBytes instanceof Uint8Array) || envelopeBytes.length === 0) {
      return Promise.reject(new PluginError('invalid_argument', 'empty relay envelope'))
    }
    const bytes = Buffer.from(envelopeBytes)
    return this.#chain(topic, () => this.#sendComplete(topic, bytes, options), options.live === true)
  }

  async #sendComplete(topic, envelopeBytes, options) {
    const generation = this.#generation
    const descriptor = descriptorFor(options.kind, topic)
    if (descriptor.inline === undefined) {
      throw new PluginError('invalid_argument', 'unsupported relay envelope kind')
    }
    if (envelopeBytes.length > this.budget.envelope_bytes) {
      this.logger?.warn('relay_envelope_rejected', {
        code: 'payload_too_large', scope: 'envelope', bytes: envelopeBytes.length, limit: this.budget.envelope_bytes,
      })
      throw new PluginError('payload_too_large', 'complete message exceeds the envelope budget')
    }
    const envelope = this.codec.decode(descriptor.type, envelopeBytes)
    assertRelayEpoch(envelope)
    if (descriptor.type === 'whalerein.v1.HostResult') assertResultRepresentation(envelope)
    else assertEventRepresentation(envelope)

    const limit = this.effectivePacketLimit(options.receiverLimit)
    const encoded = publishPacketBytes(topic, envelopeBytes.length)
    const referenced = envelope[descriptor.reference] != null
    if (options.inlineOnly === true && encoded > limit) {
      throw new PluginError('payload_too_large', 'inline message exceeds the hop packet budget')
    }
    if (referenced) {
      if (encoded > limit) {
        throw new PluginError('payload_too_large', 'reference packet exceeds the hop packet budget')
      }
      await this.publish(topic, envelopeBytes)
      return { representation: 'reference', packetBytes: encoded }
    }
    if (encoded <= limit) {
      await this.publish(topic, envelopeBytes)
      return { representation: 'inline', packetBytes: encoded }
    }

    // The full packet is oversized and the message is inline: stage the exact
    // ciphertext before publishing its reference.
    const body = envelope[descriptor.inline]
    if (!(body instanceof Uint8Array) || body.length === 0) {
      throw new PluginError('invalid_argument', 'oversized envelope carries no inline body')
    }
    if (!this.relay) {
      throw new PluginError('payload_too_large', 'no relay object route is configured for an oversized message')
    }
    const scope = this.#scopeFor(options, envelope)
    const digest = sha256Bytes(body)
    const reservation = await this.relay.reserve({
      deviceId: scope.deviceId,
      sessionId: scope.sessionId,
      channelId: scope.channelId,
      purpose: descriptor.purpose,
      partId: scope.partId,
      ciphertext: body,
      expiresAt: scope.expiresAt,
    })
    let finished = false
    try {
      const reference = {
        object_id: reservation.object_id,
        owner_node_id: reservation.owner_node_id,
        ciphertext_bytes: body.length,
        sha256: digest,
        expires_at_unix_ms: reservation.expires_at_unix_ms,
        purpose: descriptor.purpose,
      }
      if (typeof reference.owner_node_id !== 'string' || reference.owner_node_id.length === 0) {
        throw new PluginError('protocol_version_incompatible', 'relay reservation is missing its owner')
      }
      const candidate = { ...envelope, [descriptor.inline]: EMPTY, [descriptor.reference]: reference }
      const candidateBytes = this.codec.encode(descriptor.type, candidate)
      const referencePacket = publishPacketBytes(topic, candidateBytes.length)
      if (referencePacket > limit) {
        throw new PluginError('payload_too_large', 'reference packet exceeds the hop packet budget')
      }
      if (generation !== this.#generation) throw new PluginError('unavailable', 'relay interrupted')
      await this.relay.upload({ reservation, ciphertext: body })
      await this.relay.finish({ objectId: reservation.object_id, ciphertext: body })
      finished = true
      if (generation !== this.#generation) throw new PluginError('unavailable', 'relay interrupted')
      // Verification is complete before any reference is published.
      await this.publish(topic, candidateBytes)
      this.logger?.debug('relay_reference_published', {
        purpose: descriptor.purpose, bytes: body.length, envelope: candidateBytes.length,
      })
      return { representation: 'reference', packetBytes: referencePacket, objectId: reservation.object_id }
    } finally {
      if (!finished) {
        try { await this.relay.delete({ objectId: reservation.object_id }) } catch { /* bounded cancel */ }
      }
    }
  }

  #scopeFor(options, envelope) {
    const deviceId = options.deviceId ?? envelope.device_id
    const channelId = options.channelId ?? envelope.channel_id
    if (typeof deviceId !== 'string' || deviceId === '') {
      throw new PluginError('invalid_argument', 'relay object device scope missing')
    }
    if (typeof channelId !== 'string' || channelId === '') {
      throw new PluginError('invalid_argument', 'relay object channel scope missing')
    }
    const expiresAt = Number.isSafeInteger(options.expiresAt) && options.expiresAt > 0
      ? options.expiresAt
      : this.now() + this.objectTtlMs
    if (expiresAt <= this.now()) throw new PluginError('expired', 'relay object lifetime already ended')
    return {
      deviceId,
      channelId,
      sessionId: typeof options.sessionId === 'string' ? options.sessionId : '',
      partId: typeof options.partId === 'string' ? options.partId : '',
      expiresAt,
    }
  }

  /**
   * Resolve one complete received request or event body. An inline body is
   * returned unchanged; a reference is fetched over Host-authority HTTPS and
   * verified by exact length and SHA-256 before it reaches business handling.
   * A partial, truncated or mismatched fetch is a terminal failure.
   */
  async receiveComplete(topic, bytes, options = {}) {
    if (!(bytes instanceof Uint8Array) || bytes.length === 0) {
      throw new PluginError('invalid_argument', 'empty relay envelope')
    }
    if (bytes.length > this.budget.envelope_bytes) {
      throw new PluginError('payload_too_large', 'complete message exceeds the envelope budget')
    }
    const descriptor = descriptorFor(options.kind, topic)
    let envelope
    try {
      envelope = this.codec.decode(descriptor.type, bytes)
    } catch (error) {
      if (error instanceof PluginError) throw error
      throw new PluginError('invalid_argument', 'relay envelope is not decodable')
    }
    assertRelayEpoch(envelope)
    if (descriptor.type === 'whalerein.v1.HostRequest') {
      assertRequestRepresentation(envelope)
      if (envelope.body_reference != null) {
        if (!this.relay) throw new PluginError('unavailable', 'no relay object route is configured')
        const reference = validatePayloadReference(envelope.body_reference, {
          envelopeBytes: this.budget.envelope_bytes,
          now: this.now,
        })
        const body = await this.relay.fetch({
          reference,
          envelopeBytes: this.budget.envelope_bytes,
          purpose: descriptor.purpose,
        })
        envelope.body = body
        delete envelope.body_reference
        assertRequestRepresentation(envelope)
      }
      return envelope
    }
    if (descriptor.type === 'whalerein.v1.EventEnvelope') {
      assertEventRepresentation(envelope)
      if (envelope.payload_reference != null) {
        if (!this.relay) throw new PluginError('unavailable', 'no relay object route is configured')
        const reference = validatePayloadReference(envelope.payload_reference, {
          envelopeBytes: this.budget.envelope_bytes,
          now: this.now,
        })
        envelope.ciphertext = await this.relay.fetch({
          reference,
          envelopeBytes: this.budget.envelope_bytes,
          purpose: descriptor.purpose,
        })
        delete envelope.payload_reference
        assertEventRepresentation(envelope)
      }
      return envelope
    }
    return envelope
  }
}

// Re-export the receive-advertisement validator beside the adapter so callers
// do not import a second transport seam.
export { validateReceiveAdvertisement }
