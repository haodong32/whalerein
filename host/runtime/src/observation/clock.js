// One measurement clock for the Node Host and DSH adapter.
//
// The capture accepts only the Linux kernel CLOCK_MONOTONIC timeline. Node's
// `process.hrtime.bigint()` is that clock, but it is only admitted after a
// startup verification: a non-Linux platform or a source that is not a
// monotonic bigint reading is rejected instead of being coerced into one
// shared timeline. Instrumentation never manufactures a timestamp for an
// absent boundary; the caller is expected to treat a rejected clock as
// "capture unavailable".
import { strict as assert } from 'node:assert'

export const CLOCK_SOURCE = 'process.hrtime.bigint'
export const CLOCK_EPOCH = 'linux-monotonic'

export class ObservationClockError extends Error {
  constructor(reason) {
    super(`observation clock unavailable: ${reason}`)
    this.name = 'ObservationClockError'
    this.code = 'clock_unverified'
    this.reason = reason
  }
}

function defaultHrtime() {
  return process.hrtime.bigint()
}

/**
 * Verify the pinned Node runtime exposes a Linux monotonic reading.
 *
 * The check is deliberately narrow: Linux, a callable bigint source, and two
 * ordered readings. It never maps a different epoch onto the shared one.
 */
export function verifyNodeMonotonicClock({ platform = process.platform, hrtime = defaultHrtime } = {}) {
  if (platform !== 'linux') {
    return { ok: false, reason: 'platform_not_linux', source: CLOCK_SOURCE, epoch: CLOCK_EPOCH }
  }
  try {
    const before = hrtime()
    const after = hrtime()
    if (typeof before !== 'bigint' || typeof after !== 'bigint') {
      return { ok: false, reason: 'not_nanoseconds', source: CLOCK_SOURCE, epoch: CLOCK_EPOCH }
    }
    if (before < 0n || after < before) {
      return { ok: false, reason: 'not_monotonic', source: CLOCK_SOURCE, epoch: CLOCK_EPOCH }
    }
    return { ok: true, reason: null, source: CLOCK_SOURCE, epoch: CLOCK_EPOCH, reading_ns: before.toString() }
  } catch {
    return { ok: false, reason: 'source_unavailable', source: CLOCK_SOURCE, epoch: CLOCK_EPOCH }
  }
}

/**
 * Build a clock facade. `nowNs()` throws ObservationClockError until a proof
 * admitted the Linux monotonic epoch, so an unverified capture cannot silently
 * produce a timestamp.
 */
export function createMonotonicClock({ platform = process.platform, hrtime = defaultHrtime, proof } = {}) {
  const verified = proof ?? verifyNodeMonotonicClock({ platform, hrtime })
  return {
    source: verified.source,
    epoch: verified.epoch,
    verified: verified.ok === true,
    reason: verified.reason ?? null,
    nowNs() {
      if (verified.ok !== true) throw new ObservationClockError(verified.reason ?? 'unverified')
      const value = hrtime()
      if (typeof value !== 'bigint' || value < 0n) throw new ObservationClockError('reading_invalid')
      return value
    },
  }
}

/**
 * Parse a manifest `clock` block and confirm it names the epoch this runtime
 * can produce. A contract that asks for any other clock is rejected.
 */
export function manifestClockProof(manifest) {
  assert(manifest && typeof manifest === 'object', 'observation manifest missing')
  const clock = manifest.clock ?? {}
  const ok = clock.source === 'CLOCK_MONOTONIC' && clock.unit === 'ns' && clock.epoch === 'kernel-boot'
  return { ok, reason: ok ? null : 'manifest_clock_mismatch' }
}