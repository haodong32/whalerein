// Authenticated execution deadline admission.
//
// The device authenticates the original absolute deadline inside the command.
// The Host caps the remaining window with its monotonic clock so a wall-clock
// adjustment cannot extend admitted work. An admission is process-local: it is
// branded, non-serializable and never authorizes a resubmission after a
// restart, timeout or lost connection.

import { PluginError } from '../errors.js'

const ADMISSION_BRAND = Symbol('whalerein.host.deadline-admission')

/** A trusted Host clock limits how far in the future a deadline may be. */
export const MAX_DEADLINE_HORIZON_MS = 10 * 60 * 1000
/** The remaining native execution window is capped independently. */
export const MAX_EXECUTION_WINDOW_MS = 5 * 60 * 1000

function safeTime(value) {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null
}

/**
 * Decide admission from an authenticated original absolute deadline.
 * Fails closed when the deadline is missing, malformed, already elapsed or
 * implausibly distant, and when either clock is unavailable.
 *
 * @param {object} options
 * @param {number} options.deadlineAt authenticated absolute wall-clock deadline (ms).
 * @param {number} [options.wallNow] Host wall clock at admission.
 * @param {number} options.monotonicNow Host monotonic clock in milliseconds.
 * @param {number} [options.maxHorizonMs] trusted future horizon.
 * @param {number} [options.maxExecutionMs] maximum admitted execution window.
 */
export function admitDeadline({
  deadlineAt,
  wallNow = Date.now(),
  monotonicNow,
  maxHorizonMs = MAX_DEADLINE_HORIZON_MS,
  maxExecutionMs = MAX_EXECUTION_WINDOW_MS,
} = {}) {
  const deadline = safeTime(deadlineAt)
  if (deadline === null || deadline === 0) return { ok: false, reason: 'missing_deadline' }
  const wall = safeTime(wallNow)
  const monotonic = safeTime(monotonicNow)
  if (wall === null || monotonic === null) return { ok: false, reason: 'clock_unavailable' }
  const horizon = safeTime(maxHorizonMs)
  const windowCap = safeTime(maxExecutionMs)
  if (horizon === null || windowCap === null || horizon <= 0 || windowCap <= 0) {
    throw new PluginError('invalid_argument', 'invalid deadline bound')
  }
  const remaining = deadline - wall
  // At or after the authenticated deadline no new execution is admitted.
  if (remaining <= 0) return { ok: false, reason: 'expired' }
  if (remaining > horizon) return { ok: false, reason: 'deadline_too_far' }
  const windowMs = Math.min(remaining, windowCap)
  const admission = {
    ok: true,
    absoluteDeadline: deadline,
    admittedAtWall: wall,
    admittedAtMonotonic: monotonic,
    expiresAtMonotonic: monotonic + windowMs,
    windowMs,
    // Recovery or polling never authorizes repeating the native work.
    resubmittable: false,
  }
  Object.defineProperty(admission, ADMISSION_BRAND, { value: true, enumerable: false })
  return Object.freeze(admission)
}

/**
 * True only for an unmodified admission produced by this module in this
 * process and still inside its monotonic window. At the exact monotonic
 * boundary the admission is no longer fresh. Any clone, tamper, rollback or
 * missing clock fails closed.
 *
 * @param {object} admission result of {@link admitDeadline}.
 * @param {{monotonicNow: number}} clock current Host monotonic clock.
 */
export function isAdmissionFresh(admission, { monotonicNow } = {}) {
  if (!admission || typeof admission !== 'object' || admission.ok !== true) return false
  if (admission[ADMISSION_BRAND] !== true) return false
  if (admission.resubmittable !== false) return false
  const monotonic = safeTime(monotonicNow)
  if (monotonic === null) return false
  if (typeof admission.expiresAtMonotonic !== 'number' || !Number.isSafeInteger(admission.expiresAtMonotonic)) return false
  if (typeof admission.admittedAtMonotonic !== 'number' || !Number.isSafeInteger(admission.admittedAtMonotonic)) return false
  // A monotonic clock that moved backwards is not trusted to extend work.
  if (monotonic < admission.admittedAtMonotonic) return false
  return monotonic < admission.expiresAtMonotonic
}