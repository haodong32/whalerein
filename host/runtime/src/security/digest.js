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
 * Escape one canonical approval field so the `|` field separator cannot be
 * forged by content. The mapping is injective: `\` becomes `\\` and `|`
 * becomes `\|`, so two different field tuples can never produce the same
 * canonical string. Values without those bytes keep their historical digest.
 */
function escapeApprovalField(value) {
  return String(value).replace(/\\/g, '\\\\').replace(/\|/g, '\\|')
}

/**
 * Canonical approval digest from WIRE.md section "Approval digest".
 * `|` inside any field is escaped, so a reason or tool name cannot shift the
 * field boundary. Rust `core::security::approval_digest` uses the same rule
 * and the shared vectors in `proto/v1/approval_digest_vectors.json`.
 * @param {object} approval approval fields; `approval_id` and camelCase aliases accepted.
 */
const APPROVAL_DIGEST_DOMAIN = 'whalerein/v2/approval'

/** Normalize a review availability to the canonical lowercase token. */
export function approvalReviewAvailability(value) {
  const text = String(value ?? '').trim().toLowerCase()
  if (text === 'complete' || text === 'approval_review_complete' || text === '1') return 'complete'
  if (text === 'incomplete' || text === 'approval_review_incomplete' || text === '2') return 'incomplete'
  return 'unavailable'
}

export function approvalDigest(approval) {
  const id = escapeApprovalField(approval.approvalId ?? approval.approval_id ?? '')
  const sessionId = escapeApprovalField(approval.sessionId ?? approval.session_id ?? '')
  const toolName = escapeApprovalField(approval.toolName ?? approval.tool_name ?? '')
  const callId = escapeApprovalField(approval.callId ?? approval.call_id ?? '')
  const reason = escapeApprovalField(approval.reason ?? '')
  const action = escapeApprovalField(approval.reviewAction ?? approval.review_action ?? '')
  const target = escapeApprovalField(approval.reviewTarget ?? approval.review_target ?? '')
  const impact = escapeApprovalField(approval.reviewImpact ?? approval.review_impact ?? '')
  const availability = escapeApprovalField(approvalReviewAvailability(
    approval.reviewAvailability ?? approval.review_availability,
  ))
  const version = approval.version ?? 0
  const expiresAt = approval.expiresAtUnixMs ?? approval.expires_at_unix_ms
    ?? approval.expiresAt ?? approval.expires_at ?? 0
  return sha256Text(
    `${APPROVAL_DIGEST_DOMAIN}|${id}|${sessionId}|${toolName}|${callId}|${reason}` +
    `|${action}|${target}|${impact}|${availability}|${version}|${expiresAt}`,
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
