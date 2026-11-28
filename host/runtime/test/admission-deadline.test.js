import test from 'node:test'
import assert from 'node:assert/strict'
import {
  MAX_DEADLINE_HORIZON_MS,
  MAX_EXECUTION_WINDOW_MS,
  admitDeadline,
  isAdmissionFresh,
} from '../src/security/deadline.js'
import { PluginError } from '../src/errors.js'

const WALL = 1_000_000
const MONO = 500

test('a deadline strictly before the cap is admitted with the remaining window', () => {
  const admission = admitDeadline({ deadlineAt: WALL + 60_000, wallNow: WALL, monotonicNow: MONO })
  assert.equal(admission.ok, true)
  assert.equal(admission.absoluteDeadline, WALL + 60_000)
  assert.equal(admission.admittedAtWall, WALL)
  assert.equal(admission.admittedAtMonotonic, MONO)
  assert.equal(admission.windowMs, 60_000)
  assert.equal(admission.expiresAtMonotonic, MONO + 60_000)
  assert.equal(admission.resubmittable, false)
  assert.equal(Object.isFrozen(admission), true)
})

test('just before, at and after the absolute deadline differ', () => {
  assert.equal(admitDeadline({ deadlineAt: WALL + 1, wallNow: WALL, monotonicNow: MONO }).ok, true)
  assert.deepEqual(admitDeadline({ deadlineAt: WALL, wallNow: WALL, monotonicNow: MONO }), { ok: false, reason: 'expired' })
  assert.deepEqual(admitDeadline({ deadlineAt: WALL - 1, wallNow: WALL, monotonicNow: MONO }), { ok: false, reason: 'expired' })
})

test('a missing or tampered deadline fails closed', () => {
  for (const deadlineAt of [undefined, null, 0, -1, 1.5, Number.NaN, Number.MAX_SAFE_INTEGER + 2, '1234']) {
    assert.deepEqual(
      admitDeadline({ deadlineAt, wallNow: WALL, monotonicNow: MONO }),
      { ok: false, reason: 'missing_deadline' },
    )
  }
})

test('an unavailable clock fails closed and an implausible horizon is refused', () => {
  assert.deepEqual(admitDeadline({ deadlineAt: WALL + 1000, wallNow: null, monotonicNow: MONO }), { ok: false, reason: 'clock_unavailable' })
  assert.deepEqual(admitDeadline({ deadlineAt: WALL + 1000, wallNow: Number.NaN, monotonicNow: MONO }), { ok: false, reason: 'clock_unavailable' })
  assert.deepEqual(admitDeadline({ deadlineAt: WALL + 1000, wallNow: WALL, monotonicNow: null }), { ok: false, reason: 'clock_unavailable' })
  assert.deepEqual(
    admitDeadline({ deadlineAt: WALL + MAX_DEADLINE_HORIZON_MS + 1, wallNow: WALL, monotonicNow: MONO }),
    { ok: false, reason: 'deadline_too_far' },
  )
  assert.equal(admitDeadline({ deadlineAt: WALL + MAX_DEADLINE_HORIZON_MS, wallNow: WALL, monotonicNow: MONO }).ok, true)
  assert.throws(
    () => admitDeadline({ deadlineAt: WALL + 1000, wallNow: WALL, monotonicNow: MONO, maxHorizonMs: 0 }),
    (error) => error instanceof PluginError && error.code === 'invalid_argument',
  )
  assert.throws(
    () => admitDeadline({ deadlineAt: WALL + 1000, wallNow: WALL, monotonicNow: MONO, maxExecutionMs: -1 }),
    (error) => error instanceof PluginError && error.code === 'invalid_argument',
  )
})

test('the admitted execution window is capped independently of a distant deadline', () => {
  const admission = admitDeadline({ deadlineAt: WALL + MAX_DEADLINE_HORIZON_MS, wallNow: WALL, monotonicNow: MONO })
  assert.equal(admission.windowMs, MAX_EXECUTION_WINDOW_MS)
  assert.equal(admission.expiresAtMonotonic, MONO + MAX_EXECUTION_WINDOW_MS)
  const short = admitDeadline({ deadlineAt: WALL + 500, wallNow: WALL, monotonicNow: MONO })
  assert.equal(short.windowMs, 500)
})

test('admission freshness uses the monotonic cap and never extends', () => {
  const admission = admitDeadline({ deadlineAt: WALL + 60_000, wallNow: WALL, monotonicNow: MONO })
  assert.equal(isAdmissionFresh(admission, { monotonicNow: MONO }), true)
  assert.equal(isAdmissionFresh(admission, { monotonicNow: MONO + 59_999 }), true)
  assert.equal(isAdmissionFresh(admission, { monotonicNow: MONO + 60_000 }), false)
  assert.equal(isAdmissionFresh(admission, { monotonicNow: MONO + 60_001 }), false)
  // A monotonic clock that moved backwards cannot extend admitted work.
  assert.equal(isAdmissionFresh(admission, { monotonicNow: MONO - 1 }), false)
  // A wall-clock adjustment is irrelevant after admission.
  assert.equal(isAdmissionFresh(admission, { monotonicNow: MONO + 1 }), true)
})

test('tampered, cloned, missing or malformed admissions fail closed', () => {
  const admission = admitDeadline({ deadlineAt: WALL + 500, wallNow: WALL, monotonicNow: MONO })
  assert.equal(isAdmissionFresh(admission, { monotonicNow: MONO + 499 }), true)
  assert.equal(isAdmissionFresh({ ...admission }, { monotonicNow: MONO }), false)
  assert.equal(isAdmissionFresh(JSON.parse(JSON.stringify(admission)), { monotonicNow: MONO }), false)
  assert.equal(isAdmissionFresh({ ...admission, ok: true }, { monotonicNow: MONO }), false)
  assert.equal(isAdmissionFresh(admission, {}), false)
  assert.equal(isAdmissionFresh(admission, { monotonicNow: undefined }), false)
  assert.equal(isAdmissionFresh(admission, { monotonicNow: 1.5 }), false)
  assert.equal(isAdmissionFresh(null, { monotonicNow: MONO }), false)
  assert.equal(isAdmissionFresh({ ok: false, reason: 'expired' }, { monotonicNow: MONO }), false)
  // A denial never carries an admission window or resubmission authority.
  const denied = admitDeadline({ deadlineAt: WALL, wallNow: WALL, monotonicNow: MONO })
  assert.equal('expiresAtMonotonic' in denied, false)
  assert.equal('resubmittable' in denied, false)
})