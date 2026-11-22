// Exact MQTT 5 PUBLISH accounting for the inline/reference relay.
//
// A complete message is one MQTT PUBLISH. The size that matters is the whole
// encoded packet, not the ciphertext payload: fixed header, remaining-length
// varint, topic length prefix, QoS packet identifier, MQTT 5 properties and
// payload. Callers must never estimate from ciphertext alone.

import { PluginError } from '../errors.js'
import { BROKER_PACKET_LIMIT_BYTES, RELAY_VERSION } from '../contracts/transport.js'

export { BROKER_PACKET_LIMIT_BYTES, RELAY_VERSION }

/** Bytes needed to encode an MQTT variable-byte integer. */
export function varintBytes(value) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new PluginError('invalid_argument', 'MQTT length is not a non-negative safe integer')
  }
  let bytes = 1
  let rest = Math.floor(value / 128)
  while (rest > 0) {
    bytes++
    rest = Math.floor(rest / 128)
  }
  return bytes
}

/**
 * Exact encoded size of one MQTT 5 PUBLISH.
 *
 * `propertiesBytes` is the encoded byte length of the PUBLISH properties
 * payload; the property-length varint itself is counted here.
 */
export function publishPacketBytes(topic, payloadBytes, { qos = 1, propertiesBytes = 0 } = {}) {
  if (typeof topic !== 'string' || topic.length === 0) {
    throw new PluginError('invalid_argument', 'MQTT topic missing')
  }
  if (!Number.isSafeInteger(payloadBytes) || payloadBytes < 0) {
    throw new PluginError('invalid_argument', 'MQTT payload length invalid')
  }
  if (!Number.isSafeInteger(propertiesBytes) || propertiesBytes < 0) {
    throw new PluginError('invalid_argument', 'MQTT properties length invalid')
  }
  const topicBytes = Buffer.byteLength(topic, 'utf8')
  // topic length prefix + packet identifier (QoS 1) + property length + payload
  const remaining = 2 + topicBytes + (qos > 0 ? 2 : 0) + varintBytes(propertiesBytes)
    + propertiesBytes + payloadBytes
  return 1 + varintBytes(remaining) + remaining
}

/**
 * The effective packet limit for one outgoing hop: the smallest of the local
 * send budget, the broker ceiling this Host knows, and the receiver's
 * authenticated advertisement. Nothing raises another peer's limit.
 */
export function effectivePacketBytes(...limits) {
  let effective = Number.POSITIVE_INFINITY
  for (const limit of limits) {
    if (limit === undefined || limit === null) continue
    if (!Number.isSafeInteger(limit) || limit <= 0) {
      throw new PluginError('invalid_argument', 'packet limit is not a positive safe integer')
    }
    effective = Math.min(effective, limit)
  }
  return Number.isFinite(effective) ? effective : BROKER_PACKET_LIMIT_BYTES
}

/**
 * Largest plaintext byte length whose envelope still fits a packet limit.
 *
 * `envelopeSizeFor(plaintextBytes)` returns the complete encoded outer
 * envelope length for a body of exactly that many bytes. The search is exact
 * because the relation is monotonic but not linear (varints and protobuf field
 * headers change size). Returns 0 when even an empty body does not fit.
 */
export function maxPlaintextBytes(envelopeSizeFor, packetLimit) {
  if (envelopeSizeFor(0) > packetLimit) return 0
  let low = 0
  let high = Math.max(0, packetLimit - (envelopeSizeFor(0) - 0))
  while (low < high) {
    const middle = Math.ceil((low + high) / 2)
    if (envelopeSizeFor(middle) <= packetLimit) low = middle
    else high = middle - 1
  }
  return low
}
