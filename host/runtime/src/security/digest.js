// Digests and small authentication helpers shared by security, commands, and
// the DSH adapters. These values are never logged.

import { randomBytes, timingSafeEqual } from 'node:crypto'
import { sha256 } from '@noble/hashes/sha2.js'
import { hex } from './noise.js'

export { hex }

/** SHA-256 of raw bytes. */
export function sha256Bytes(bytes) {
  return sha256(bytes)
}

/** SHA-256 of a UTF-8 string. */
export function sha256Text(text) {
  return sha256(new TextEncoder().encode(String(text)))
}

/** `payload_digest` for a control command: SHA-256 of the raw HostRequest body. */
export function commandDigest(body) {
  return sha256Bytes(body)
}

/**
 * Canonical approval digest from WIRE.md section "Approval digest".
 * @param {object} approval approval fields; `approval_id` and camelCase aliases accepted.
 */
export function approvalDigest(approval) {
  const id = approval.approvalId ?? approval.approval_id ?? ''
  const sessionId = approval.sessionId ?? approval.session_id ?? ''
  const toolName = approval.toolName ?? approval.tool_name ?? ''
  const callId = approval.callId ?? approval.call_id ?? ''
  const reason = approval.reason ?? ''
  const version = approval.version ?? 0
  const expiresAt = approval.expiresAtUnixMs ?? approval.expires_at_unix_ms
    ?? approval.expiresAt ?? approval.expires_at ?? 0
  return sha256Text(
    `whalerein/v1/approval|${id}|${sessionId}|${toolName}|${callId}|${reason}|${version}|${expiresAt}`,
  )
}

/** Constant-time byte comparison. Different lengths compare false. */
export function bytesEqual(a, b) {
  if (!(a instanceof Uint8Array) || !(b instanceof Uint8Array)) return false
  if (a.length !== b.length) return false
  return timingSafeEqual(Buffer.from(a), Buffer.from(b))
}

/** Random opaque identifier for adapter-side message ids. */
export function randomId(bytes = 16) {
  return randomBytes(bytes).toString('hex')
}
