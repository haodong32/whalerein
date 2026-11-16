// Time contract shared by the Host runtime.
//
// Every `*_unix_ms` field is milliseconds since the Unix epoch, UTC. Expiry
// comparisons are exact: clock skew never extends a request lifetime. The only
// accepted skew window is the pairing signature window, two minutes in the past
// through one minute in the future.

import { PluginError } from '../errors.js'

export const UNIX_MS_UNIT = 'unix-epoch-milliseconds'
export const PAIRING_SIGNATURE_PAST_MS = 120_000
export const PAIRING_SIGNATURE_FUTURE_MS = 60_000
export const CLOCK_SKEW_TOLERANCE_MS = 60_000

/** Require an integer Unix-millisecond value. */
export function assertUnixMs(value, name) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new PluginError('invalid_argument', `${name} is not a safe Unix-millisecond value`)
  }
  return value
}

/** Ordering of a peer timestamp against the local clock. */
export function classifyTimestamp(atUnixMs, nowUnixMs) {
  if (atUnixMs < nowUnixMs) return { order: 'past', byMs: nowUnixMs - atUnixMs }
  if (atUnixMs > nowUnixMs) return { order: 'future', byMs: atUnixMs - nowUnixMs }
  return { order: 'now', byMs: 0 }
}

/** A request is expired at or before the local clock; no grace is applied. */
export function isExpired(expiresAtUnixMs, nowUnixMs) {
  return expiresAtUnixMs <= nowUnixMs
}

/** Diagnostic label for a peer clock beyond the tolerance. */
export function clockSkewLabel(atUnixMs, nowUnixMs) {
  const { byMs } = classifyTimestamp(atUnixMs, nowUnixMs)
  return byMs > CLOCK_SKEW_TOLERANCE_MS ? 'clock_skew' : null
}

/**
 * Validate the bounded pairing signature window.
 * Returns `{ok: true}` or `{ok: false, reason, message}`.
 */
export function validatePairingSignatureTime(signedAtUnixMs, nowUnixMs) {
  if (nowUnixMs - signedAtUnixMs > PAIRING_SIGNATURE_PAST_MS) {
    return {
      ok: false,
      reason: 'too_old',
      message: `signed_at_unix_ms ${signedAtUnixMs} is more than ${PAIRING_SIGNATURE_PAST_MS} ms before the local clock; check the signer clock`,
    }
  }
  if (signedAtUnixMs - nowUnixMs > PAIRING_SIGNATURE_FUTURE_MS) {
    return {
      ok: false,
      reason: 'too_far_ahead',
      message: `signed_at_unix_ms ${signedAtUnixMs} is more than ${PAIRING_SIGNATURE_FUTURE_MS} ms ahead of the local clock; check the signer clock`,
    }
  }
  return { ok: true }
}
