// Bounded wire identifiers. Values are checked before logging or forwarding so
// a peer cannot amplify logs or database rows, and no identifier may contain the
// '|' AAD separator. Keep this table identical to the Rust validator in
// proto/v1/bindings/rust/src/compat.rs and the table in proto/v1/WIRE.md.

import { PluginError } from '../errors.js'

export const ID_LIMITS = Object.freeze({
  request_id: Object.freeze({ max: 128, ascii: true, allowEmpty: false }),
  command_id: Object.freeze({ max: 128, ascii: true, allowEmpty: true }),
  channel_id: Object.freeze({ max: 128, ascii: true, allowEmpty: false }),
  claim_id: Object.freeze({ max: 128, ascii: true, allowEmpty: false }),
  approval_id: Object.freeze({ max: 128, ascii: true, allowEmpty: false }),
  delivery_id: Object.freeze({ max: 128, ascii: true, allowEmpty: false }),
  message_id: Object.freeze({ max: 128, ascii: true, allowEmpty: false }),
  block_id: Object.freeze({ max: 128, ascii: true, allowEmpty: false }),
  batch_id: Object.freeze({ max: 128, ascii: true, allowEmpty: false }),
  item_id: Object.freeze({ max: 128, ascii: true, allowEmpty: false }),
  host_id: Object.freeze({ max: 200, ascii: true, allowEmpty: false }),
  device_id: Object.freeze({ max: 200, ascii: true, allowEmpty: false }),
  trace_id: Object.freeze({ max: 64, ascii: true, allowEmpty: false }),
  watch_id: Object.freeze({ max: 64, ascii: true, allowEmpty: false }),
  wait_id: Object.freeze({ max: 64, ascii: true, allowEmpty: false }),
  session_id: Object.freeze({ max: 512, ascii: false, allowEmpty: false }),
  cursor: Object.freeze({ max: 4096, ascii: false, allowEmpty: true }),
})

/**
 * Validate one identifier. Returns `{ok: true}` or `{ok: false, reason}`.
 */
export function validateId(kind, value) {
  const limit = ID_LIMITS[kind]
  if (!limit) throw new PluginError('internal', `unknown identifier kind ${String(kind).slice(0, 32)}`)
  if (typeof value !== 'string') return { ok: false, reason: 'not a string' }
  if (value.length === 0) return limit.allowEmpty ? { ok: true } : { ok: false, reason: 'empty' }
  if (Buffer.byteLength(value, 'utf8') > limit.max) return { ok: false, reason: 'too long' }
  for (const character of value) {
    if (character === '|') return { ok: false, reason: 'AAD separator' }
    const code = character.codePointAt(0)
    if (code < 0x20 || code === 0x7f) return { ok: false, reason: 'control character' }
    if (limit.ascii && (code < 0x21 || code > 0x7e)) return { ok: false, reason: 'not printable ASCII' }
  }
  return { ok: true }
}

/** Throw `invalid_argument` for an identifier outside the wire contract. */
export function assertId(kind, value) {
  const result = validateId(kind, value)
  if (!result.ok) {
    throw new PluginError('invalid_argument', `${kind} is invalid: ${result.reason}`)
  }
  return value
}

/**
 * Validate the routing identity of one inbound `HostRequest`.
 *
 * `host_id` and `device_id` are optional on a legacy envelope, but a present
 * value must be valid. `request_id`, `channel_id` and `command_id` follow the
 * schema contract.
 */
export function assertRequestIdentity(request) {
  assertId('request_id', request?.request_id)
  if (request?.host_id) assertId('host_id', request.host_id)
  if (request?.device_id) assertId('device_id', request.device_id)
  if (request?.channel_id) assertId('channel_id', request.channel_id)
  if (request?.command_id) assertId('command_id', request.command_id)
  if (request?.trace_id) assertId('trace_id', request.trace_id)
  return request
}
