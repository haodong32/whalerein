// Shared transport profile vocabulary for Access, Host and Core.
//
// The executable values are generated from proto/v1/transport_profiles.json by
// `./repo proto`; this module owns only the normalization seam and relation
// validation. A profile is validated before readiness or connection use. It
// never changes application protocol version 1 or the relay epoch.

import { AdapterError } from '../errors.js'
import {
  BROKER_PACKET_LIMIT_BYTES,
  DEFAULT_TRANSPORT_PROFILE,
  TRANSPORT_PROFILES,
  TRANSPORT_PROFILES_SCHEMA,
  TRANSPORT_PROFILE_WIRE_NAMES,
} from '../generated/transport-profiles.js'

export {
  BROKER_PACKET_LIMIT_BYTES,
  DEFAULT_TRANSPORT_PROFILE,
  TRANSPORT_PROFILES,
  TRANSPORT_PROFILES_SCHEMA,
  TRANSPORT_PROFILE_WIRE_NAMES,
}

/** The one coordinated MQTT relay epoch. There is no fallback or dual epoch. */
export const RELAY_VERSION = 4

const safe = (value) => Number.isSafeInteger(value) && value >= 0
const positive = (value) => Number.isSafeInteger(value) && value > 0
const MAX_UINT32 = 0xffffffff
const uint32 = (value) => positive(value) && value <= MAX_UINT32
// The protobuf codec decodes 64-bit integers as decimal strings. Normalize a
// wire value before validating its range; never clamp it.
const wireInteger = (value) => {
  if (typeof value === 'number') return Number.isSafeInteger(value) ? value : null
  if (typeof value === 'string' && /^(?:0|[1-9][0-9]*)$/.test(value)) {
    const parsed = Number(value)
    return Number.isSafeInteger(parsed) ? parsed : null
  }
  return null
}
const wireUint32 = (value) => {
  const parsed = wireInteger(value)
  return parsed !== null && parsed > 0 && parsed <= MAX_UINT32 ? parsed : null
}

const PROFILE_BY_WIRE = new Map(
  Object.entries(TRANSPORT_PROFILE_WIRE_NAMES).map(([name, wire]) => [wire, name]),
)

/** Validate the packaged profile relations before readiness. */
export function validateTransportProfiles() {
  for (const [name, profile] of Object.entries(TRANSPORT_PROFILES)) {
    validateProfileValues(name, profile)
  }
}

function validateProfileValues(name, profile) {
  const {
    packetBytes,
    packetOverheadBytes,
    envelopeBytes,
    bulkConcurrency,
    reservedBytes,
    transferTimeoutMs,
  } = profile
  if (!positive(packetBytes) || !safe(packetOverheadBytes)) {
    throw new AdapterError('invalid_argument', `transport profile ${name} packet budget invalid`)
  }
  if (packetBytes > BROKER_PACKET_LIMIT_BYTES) {
    throw new AdapterError(
      'invalid_argument',
      `transport profile ${name} packet budget exceeds the broker ceiling`,
    )
  }
  if (!positive(envelopeBytes) || envelopeBytes < packetBytes) {
    throw new AdapterError('invalid_argument', `transport profile ${name} envelope budget invalid`)
  }
  if (!positive(bulkConcurrency) || !safe(reservedBytes) || reservedBytes < envelopeBytes) {
    throw new AdapterError('invalid_argument', `transport profile ${name} reserved bytes invalid`)
  }
  if (!positive(transferTimeoutMs) || transferTimeoutMs > MAX_UINT32) {
    throw new AdapterError('invalid_argument', `transport profile ${name} transfer timeout invalid`)
  }
}

/** Resolve a wire profile name or a local profile id to its validated profile. */
export function resolveTransportProfile(profile) {
  const name = typeof profile === 'string' && profile.startsWith('TRANSPORT_PROFILE_')
    ? PROFILE_BY_WIRE.get(profile)
    : profile
  if (!name || !Object.hasOwn(TRANSPORT_PROFILES, name)) {
    throw new AdapterError('invalid_argument', 'unknown transport profile')
  }
  validateProfileValues(name, TRANSPORT_PROFILES[name])
  return { name, ...TRANSPORT_PROFILES[name] }
}

/**
 * Validate one transport budget and return a frozen normalized copy.
 *
 * Values are checked, never clamped: a sender takes the minimum of its local
 * limit, the broker limit and every advertised receiver limit.
 */
export function validateTransportBudget(budget) {
  if (!budget || typeof budget !== 'object' || Array.isArray(budget)) {
    throw new AdapterError('invalid_argument', 'transport budget missing')
  }
  const {
    packet_bytes: packetBytes,
    envelope_bytes: envelopeBytes,
    bulk_concurrency: bulkConcurrency,
    reserved_bytes: reservedBytes,
    transfer_timeout_ms: transferTimeoutMs,
  } = budget
  const packet = wireUint32(packetBytes)
  const envelope = wireInteger(envelopeBytes)
  const reserved = wireInteger(reservedBytes)
  const concurrency = wireUint32(bulkConcurrency)
  const transferTimeout = wireUint32(transferTimeoutMs)
  if (packet === null) throw new AdapterError('invalid_argument', 'transport packet budget invalid')
  if (packet > BROKER_PACKET_LIMIT_BYTES) {
    throw new AdapterError('invalid_argument', 'transport packet budget exceeds the broker ceiling')
  }
  if (envelope === null || envelope <= 0 || envelope < packet) {
    throw new AdapterError('invalid_argument', 'transport envelope budget invalid')
  }
  if (reserved === null || reserved < envelope) {
    throw new AdapterError('invalid_argument', 'transport reserved bytes do not cover one envelope')
  }
  if (concurrency === null || transferTimeout === null) {
    throw new AdapterError('invalid_argument', 'transport concurrency or timeout invalid')
  }
  return Object.freeze({
    packet_bytes: packet,
    envelope_bytes: envelope,
    bulk_concurrency: concurrency,
    reserved_bytes: reserved,
    transfer_timeout_ms: transferTimeout,
  })
}

/** Validate a profile by id and return its budget in wire field names. */
export function transportProfileBudget(profile = DEFAULT_TRANSPORT_PROFILE) {
  const resolved = resolveTransportProfile(profile)
  return validateTransportBudget({
    packet_bytes: resolved.packetBytes,
    envelope_bytes: resolved.envelopeBytes,
    bulk_concurrency: resolved.bulkConcurrency,
    reserved_bytes: resolved.reservedBytes,
    transfer_timeout_ms: resolved.transferTimeoutMs,
  })
}

/**
 * Validate an event receive advertisement and return its normalized budget.
 *
 * An advertisement is the only exchange of receive limits. It applies to the
 * authenticated connection generation that carried it, and it must name the
 * one relay epoch. A peer cannot raise a local limit: senders take the minimum
 * of local and advertised values, so nothing is clamped here.
 */
export function validateReceiveAdvertisement(advertisement) {
  if (!advertisement || typeof advertisement !== 'object' || Array.isArray(advertisement)) {
    throw new AdapterError('invalid_argument', 'receive advertisement missing')
  }
  const profile = resolveTransportProfile(advertisement.profile)
  const generation = wireInteger(advertisement.connection_generation)
  if (generation === null || generation < 1) {
    throw new AdapterError('invalid_argument', 'connection generation invalid')
  }
  const relay = wireInteger(advertisement.relay_version)
  if (relay !== RELAY_VERSION) {
    throw new AdapterError(
      'protocol_version_incompatible',
      'receive advertisement relay epoch is incompatible',
    )
  }
  const budget = validateTransportBudget(advertisement.budget)
  return Object.freeze({
    profile: profile.wireName,
    budget,
    connection_generation: generation,
    relay_version: RELAY_VERSION,
  })
}

/** Validate request metadata bound by Access at submission. */
export function validateRequestTransportMetadata(metadata) {
  if (!metadata || typeof metadata !== 'object' || Array.isArray(metadata)) {
    throw new AdapterError('invalid_argument', 'request transport metadata missing')
  }
  const resultMaxBytes = wireInteger(metadata.result_max_bytes)
  // Proto3 omits zero-valued scalars. Access always captures both durable
  // generations, so an absent field means the zero revision, not a missing
  // stamp; Access owns the fail-closed admission check.
  const grantGeneration = wireInteger(metadata.grant_generation ?? 0)
  const revocationGeneration = wireInteger(metadata.revocation_generation ?? 0)
  if (resultMaxBytes === null || resultMaxBytes <= 0) {
    throw new AdapterError('invalid_argument', 'validated result bound missing')
  }
  if (grantGeneration === null || grantGeneration < 0) {
    throw new AdapterError('invalid_argument', 'grant generation missing')
  }
  if (revocationGeneration === null || revocationGeneration < 0) {
    throw new AdapterError('invalid_argument', 'revocation generation missing')
  }
  return {
    result_max_bytes: resultMaxBytes,
    grant_generation: grantGeneration,
    revocation_generation: revocationGeneration,
  }
}
