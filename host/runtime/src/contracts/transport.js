// Shared transport profile vocabulary for Access, Host and Core.
//
// The executable values are generated from proto/v1/transport_profiles.json by
// `./repo proto`; this module owns only the normalization seam and relation
// validation. A profile is validated before readiness or connection use. It
// never changes application protocol version 1.

import { AdapterError } from '../errors.js'
import {
  DEFAULT_TRANSPORT_PROFILE,
  TRANSPORT_PROFILES,
  TRANSPORT_PROFILES_SCHEMA,
  TRANSPORT_PROFILE_WIRE_NAMES,
} from '../generated/transport-profiles.js'

export {
  DEFAULT_TRANSPORT_PROFILE,
  TRANSPORT_PROFILES,
  TRANSPORT_PROFILES_SCHEMA,
  TRANSPORT_PROFILE_WIRE_NAMES,
}

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
    fragmentPayloadBytes,
    fragmentCount,
    envelopeBytes,
    bulkConcurrency,
    reservedBytes,
    fragmentIdleTimeoutMs,
    transferTimeoutMs,
  } = profile
  if (
    !positive(packetBytes) ||
    !safe(packetOverheadBytes) ||
    !positive(fragmentPayloadBytes) ||
    !positive(fragmentCount)
  ) {
    throw new AdapterError('invalid_argument', `transport profile ${name} byte budget invalid`)
  }
  if (fragmentPayloadBytes + packetOverheadBytes > packetBytes) {
    throw new AdapterError(
      'invalid_argument',
      `transport profile ${name} fragment payload exceeds the packet budget`,
    )
  }
  if (!positive(envelopeBytes) || !safe(reservedBytes) || !positive(bulkConcurrency)) {
    throw new AdapterError('invalid_argument', `transport profile ${name} envelope budget invalid`)
  }
  if (fragmentCount < Math.ceil(envelopeBytes / fragmentPayloadBytes)) {
    throw new AdapterError(
      'invalid_argument',
      `transport profile ${name} fragment count does not cover the envelope`,
    )
  }
  if (reservedBytes < fragmentPayloadBytes * bulkConcurrency) {
    throw new AdapterError(
      'invalid_argument',
      `transport profile ${name} reserved bytes do not cover in-flight fragments`,
    )
  }
  if (reservedBytes < envelopeBytes) {
    throw new AdapterError(
      'invalid_argument',
      `transport profile ${name} reserved bytes do not cover one envelope`,
    )
  }
  if (!positive(fragmentIdleTimeoutMs) || !positive(transferTimeoutMs)) {
    throw new AdapterError('invalid_argument', `transport profile ${name} timeout invalid`)
  }
  if (fragmentIdleTimeoutMs >= transferTimeoutMs) {
    throw new AdapterError(
      'invalid_argument',
      `transport profile ${name} idle timeout must be below the transfer timeout`,
    )
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
 * `packetOverheadBytes` is the overhead of the profile label that names the
 * budget's base vocabulary. It is not itself on the wire, so a local budget
 * may omit it; an advertisement always passes the named profile's overhead.
 * Values are checked, never clamped: a sender takes the minimum instead.
 */
export function validateTransportBudget(budget, packetOverheadBytes = 0) {
  if (!budget || typeof budget !== 'object' || Array.isArray(budget)) {
    throw new AdapterError('invalid_argument', 'transport budget missing')
  }
  if (!safe(packetOverheadBytes)) {
    throw new AdapterError('invalid_argument', 'packet overhead invalid')
  }
  const {
    packet_bytes: packetBytes,
    fragment_payload_bytes: fragmentPayloadBytes,
    fragment_count: fragmentCount,
    envelope_bytes: envelopeBytes,
    bulk_concurrency: bulkConcurrency,
    reserved_bytes: reservedBytes,
    fragment_idle_timeout_ms: fragmentIdleTimeoutMs,
    transfer_timeout_ms: transferTimeoutMs,
  } = budget
  const packet = wireUint32(packetBytes)
  const fragmentPayload = wireUint32(fragmentPayloadBytes)
  const fragments = wireUint32(fragmentCount)
  const envelope = wireInteger(envelopeBytes)
  const reserved = wireInteger(reservedBytes)
  const concurrency = wireUint32(bulkConcurrency)
  const idleTimeout = wireUint32(fragmentIdleTimeoutMs)
  const transferTimeout = wireUint32(transferTimeoutMs)
  if (packet === null || fragmentPayload === null || fragments === null) {
    throw new AdapterError('invalid_argument', 'transport byte budget invalid')
  }
  if (packet <= fragmentPayload || fragmentPayload + packetOverheadBytes > packet) {
    throw new AdapterError('invalid_argument', 'fragment payload does not fit the packet budget')
  }
  if (envelope === null || envelope <= 0 || reserved === null || reserved < 0) {
    throw new AdapterError('invalid_argument', 'transport envelope budget invalid')
  }
  if (concurrency === null || idleTimeout === null || transferTimeout === null) {
    throw new AdapterError('invalid_argument', 'transport concurrency or timeout invalid')
  }
  if (fragments < Math.ceil(envelope / fragmentPayload)) {
    throw new AdapterError('invalid_argument', 'fragment count does not cover the envelope')
  }
  const inFlightBytes = fragmentPayload * concurrency
  if (!Number.isSafeInteger(inFlightBytes) || reserved < inFlightBytes) {
    throw new AdapterError('invalid_argument', 'reserved bytes do not cover in-flight fragments')
  }
  if (reserved < envelope) {
    throw new AdapterError('invalid_argument', 'reserved bytes do not cover one envelope')
  }
  if (idleTimeout >= transferTimeout) {
    throw new AdapterError('invalid_argument', 'fragment idle timeout must be below the transfer timeout')
  }
  return Object.freeze({
    packet_bytes: packet,
    fragment_payload_bytes: fragmentPayload,
    fragment_count: fragments,
    envelope_bytes: envelope,
    bulk_concurrency: concurrency,
    reserved_bytes: reserved,
    fragment_idle_timeout_ms: idleTimeout,
    transfer_timeout_ms: transferTimeout,
  })
}

/** Validate a profile by id and return its budget in wire field names. */
export function transportProfileBudget(profile = DEFAULT_TRANSPORT_PROFILE) {
  const resolved = resolveTransportProfile(profile)
  return validateTransportBudget({
    packet_bytes: resolved.packetBytes,
    fragment_payload_bytes: resolved.fragmentPayloadBytes,
    fragment_count: resolved.fragmentCount,
    envelope_bytes: resolved.envelopeBytes,
    bulk_concurrency: resolved.bulkConcurrency,
    reserved_bytes: resolved.reservedBytes,
    fragment_idle_timeout_ms: resolved.fragmentIdleTimeoutMs,
    transfer_timeout_ms: resolved.transferTimeoutMs,
  }, resolved.packetOverheadBytes)
}

/**
 * Validate an event receive advertisement and return its normalized budget.
 *
 * The named profile selects the base vocabulary, including its packet
 * overhead, but the budget may carry relation-valid operator overrides and
 * directional limits. A peer cannot raise a local limit: senders take the
 * minimum of local and advertised values, so nothing is clamped here.
 * `connection_generation` is scoped to the leg that carried the advertisement
 * and is validated only as a positive value; generations from different legs
 * are never comparable.
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
  const budget = validateTransportBudget(advertisement.budget, profile.packetOverheadBytes)
  return Object.freeze({
    profile: profile.wireName,
    budget,
    connection_generation: generation,
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
