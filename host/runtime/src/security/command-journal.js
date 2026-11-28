// Host-wide command journal.
//
// One high-entropy command ID is reserved before native handoff. The
// reservation is keyed by the command ID alone, so duplicate suppression
// survives a grant edit, a revocation and a re-enrollment under a fresh grant
// ID. The immutable intent binding covers the authenticated body digest, the
// originating device key and the original absolute deadline: a changed request
// can never reuse a reservation.
//
// A lookup is only answered for the grant and device key that reserved the ID.
// Every other caller, including a fresh grant for the same physical phone and
// an unknown ID, receives the same generic `unverifiable` object with no
// stored status, receipt or timing.

import { PluginError } from '../errors.js'
import { bytesEqual, sha256Bytes } from './digest.js'

const TERMINAL_STATES = new Set(['succeeded', 'failed', 'unknown'])
const UNVERIFIABLE = Object.freeze({ ok: false, reason: 'unverifiable' })
const DEFAULT_RESULT_RETENTION_MS = 7 * 24 * 60 * 60 * 1000

/** The only answer returned to a caller that does not own the reservation. */
export function unverifiableCommand() {
  return UNVERIFIABLE
}

function boundedString(value, name, maxLength = 256) {
  if (typeof value !== 'string' || value === '' || value.length > maxLength) {
    throw new PluginError('invalid_argument', `${name} invalid`)
  }
  return value
}

function keyBytes(value, name = 'device public key') {
  if (!(value instanceof Uint8Array) || value.length !== 32) {
    throw new PluginError('invalid_argument', `${name} must be 32 bytes`)
  }
  return Buffer.from(value)
}

function digestBytes(value) {
  if (!(value instanceof Uint8Array) || value.length !== 32) {
    throw new PluginError('invalid_argument', 'intent digest must be 32 bytes')
  }
  return Buffer.from(value)
}

function safeTime(value, name) {
  if (!Number.isSafeInteger(value) || value < 0) throw new PluginError('invalid_argument', `${name} invalid`)
  return value
}

/**
 * Channel-independent intent binding for one command. The caller supplies the
 * authenticated request-body digest; this binds it to the endpoint key and the
 * original absolute deadline that the device authenticated.
 */
export function commandIntentBinding({ commandId, intentDigest, devicePublicKey, deadlineAt }) {
  const id = boundedString(commandId, 'command id')
  const body = digestBytes(intentDigest)
  const key = keyBytes(devicePublicKey)
  const deadline = safeTime(deadlineAt, 'deadline')
  if (deadline === 0) throw new PluginError('invalid_argument', 'deadline missing')
  const canonical = [
    'whalerein/host-command-intent-v1',
    id,
    body.toString('hex'),
    key.toString('hex'),
    String(deadline),
  ].join('\n')
  return Buffer.from(sha256Bytes(Buffer.from(canonical, 'utf8')))
}

function rowCommand(row) {
  if (!row) return null
  return {
    commandId: row.command_id,
    intentDigest: Buffer.from(row.intent_digest),
    devicePublicKey: Buffer.from(row.device_public_key),
    grantId: row.grant_id,
    deadlineAt: row.deadline_at,
    state: row.state,
    resultPayload: row.result_payload ? Buffer.from(row.result_payload) : null,
    errorCode: row.error_code ?? null,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
  }
}

function payloadsEqual(left, right) {
  if (left === null && right === null) return true
  if (left === null || right === null) return false
  return bytesEqual(left, right)
}

export class CommandJournal {
  #db
  #transaction
  #now

  /**
   * @param {object} options
   * @param {import('../state/store.js').StateStore} options.store open Host store.
   * @param {() => number} [options.now] clock injection (wall clock ms).
   */
  constructor({ store, now = Date.now } = {}) {
    if (!store || typeof store.trustStore !== 'function') throw new PluginError('internal', 'state store unavailable')
    const handle = store.trustStore()
    this.#db = handle.db
    this.#transaction = handle.transaction
    this.#now = now
  }

  /**
   * Reserve the command ID before native handoff.
   * @returns {object} `{admitted:true}` for a fresh reservation,
   *   `{admitted:false, reason:'duplicate', state, resultPayload, errorCode}`
   *   for an exact retry under the same grant, `{admitted:false,
   *   reason:'intent_mismatch'}` for a changed request, or the generic
   *   `{ok:false, reason:'unverifiable'}` for a foreign grant/device.
   */
  reserveCommand({
    commandId,
    intentDigest,
    devicePublicKey,
    grantId,
    deadlineAt,
    now = this.#now(),
  }) {
    const id = boundedString(commandId, 'command id')
    const grant = boundedString(grantId, 'grant id')
    const key = keyBytes(devicePublicKey)
    const deadline = safeTime(deadlineAt, 'deadline')
    const at = safeTime(now, 'clock')
    const binding = commandIntentBinding({ commandId: id, intentDigest, devicePublicKey: key, deadlineAt: deadline })

    return this.#transaction(() => {
      const existing = rowCommand(this.#db.prepare('SELECT * FROM command_journal WHERE command_id = ?').get(id))
      if (existing) {
        if (existing.grantId !== grant || !bytesEqual(existing.devicePublicKey, key)) return UNVERIFIABLE
        if (!bytesEqual(existing.intentDigest, binding)) return { admitted: false, reason: 'intent_mismatch' }
        return {
          admitted: false,
          reason: 'duplicate',
          state: existing.state,
          resultPayload: existing.resultPayload,
          errorCode: existing.errorCode,
        }
      }
      this.#db.prepare(`
        INSERT INTO command_journal (
          command_id, intent_digest, device_public_key, grant_id, deadline_at,
          state, result_payload, error_code, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, 'reserved', NULL, NULL, ?, ?)
      `).run(id, binding, key, grant, deadline, at, at)
      return { admitted: true, commandId: id, state: 'reserved' }
    })
  }

  /**
   * Bind the terminal outcome or crash-to-unknown state to an existing
   * reservation. A different terminal outcome can never overwrite proven
   * evidence.
   */
  bindResult({
    commandId,
    grantId,
    devicePublicKey,
    state,
    resultPayload = null,
    errorCode = null,
    now = this.#now(),
  }) {
    const id = boundedString(commandId, 'command id')
    const grant = boundedString(grantId, 'grant id')
    const key = keyBytes(devicePublicKey)
    if (!TERMINAL_STATES.has(state)) throw new PluginError('invalid_argument', 'terminal state invalid')
    if (resultPayload !== null && !(resultPayload instanceof Uint8Array)) throw new PluginError('invalid_argument', 'result payload invalid')
    if (errorCode !== null && (typeof errorCode !== 'string' || errorCode.length > 128)) throw new PluginError('invalid_argument', 'error code invalid')
    const at = safeTime(now, 'clock')
    const payload = resultPayload === null ? null : Buffer.from(resultPayload)

    return this.#transaction(() => {
      const existing = rowCommand(this.#db.prepare('SELECT * FROM command_journal WHERE command_id = ?').get(id))
      if (!existing) throw new PluginError('not_found', 'command reservation unknown')
      if (existing.grantId !== grant || !bytesEqual(existing.devicePublicKey, key)) {
        throw new PluginError('forbidden', 'command reservation belongs to another grant')
      }
      if (existing.state === state) {
        if (!payloadsEqual(existing.resultPayload, payload) || existing.errorCode !== errorCode) {
          throw new PluginError('conflict', 'command outcome already bound')
        }
        return { updated: false, command: existing }
      }
      if (existing.state !== 'reserved') throw new PluginError('conflict', 'command outcome already bound')
      this.#db.prepare(`
        UPDATE command_journal SET state = ?, result_payload = ?, error_code = ?, updated_at = ?
        WHERE command_id = ? AND state = 'reserved'
      `).run(state, payload, errorCode, at, id)
      return { updated: true, command: rowCommand(this.#db.prepare('SELECT * FROM command_journal WHERE command_id = ?').get(id)) }
    })
  }

  /**
   * Recover the receipt for the exact grant and device key that reserved it.
   * The same generic answer covers an unknown ID, a foreign grant and a
   * re-enrolled phone, so no stored content or status leaks.
   */
  lookupCommand({ commandId, grantId, devicePublicKey } = {}) {
    const id = boundedString(commandId, 'command id')
    const grant = boundedString(grantId, 'grant id')
    const key = keyBytes(devicePublicKey)
    const existing = rowCommand(this.#db.prepare('SELECT * FROM command_journal WHERE command_id = ?').get(id))
    if (!existing) return UNVERIFIABLE
    if (existing.grantId !== grant || !bytesEqual(existing.devicePublicKey, key)) return UNVERIFIABLE
    return {
      ok: true,
      command: {
        commandId: existing.commandId,
        state: existing.state,
        resultPayload: existing.resultPayload,
        errorCode: existing.errorCode,
        deadlineAt: existing.deadlineAt,
        createdAt: existing.createdAt,
        updatedAt: existing.updatedAt,
      },
    }
  }

  /**
   * Startup recovery: a reservation that never reached a terminal outcome is
   * uncertain, never successful. Identity is retained so a resubmission is
   * still suppressed.
   */
  markReservedUnknown({ now = this.#now() } = {}) {
    const at = safeTime(now, 'clock')
    const info = this.#db.prepare(`
      UPDATE command_journal SET state = 'unknown', error_code = 'unavailable', updated_at = ?
      WHERE state = 'reserved'
    `).run(at)
    return Number(info.changes)
  }

  /**
   * Bounded retention: drop terminal receipt bytes but keep the reservation
   * identity and state so deduplication survives.
   */
  pruneResults({ now = this.#now(), retentionMs = DEFAULT_RESULT_RETENTION_MS } = {}) {
    const at = safeTime(now, 'clock')
    const retention = safeTime(retentionMs, 'retention')
    const cutoff = at - retention
    const info = this.#db.prepare(`
      UPDATE command_journal SET result_payload = NULL
      WHERE result_payload IS NOT NULL AND state IN ('succeeded', 'failed', 'unknown') AND updated_at < ?
    `).run(cutoff)
    return Number(info.changes)
  }

  countCommands(state = null) {
    if (state === null) return this.#db.prepare('SELECT COUNT(*) AS n FROM command_journal').get().n
    if (!TERMINAL_STATES.has(state) && state !== 'reserved') throw new PluginError('invalid_argument', 'command state invalid')
    return this.#db.prepare('SELECT COUNT(*) AS n FROM command_journal WHERE state = ?').get(state).n
  }
}