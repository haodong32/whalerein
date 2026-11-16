import { PluginError } from 'whalerein-host/errors.js'

export const EVENT_KINDS = Object.freeze([
  'output',
  'approval_requested',
  'approval_resolved',
  'session_state',
  'notice',
])

export const APPROVAL_STATES = Object.freeze(['pending', 'approved', 'rejected', 'expired', 'unavailable'])
export const APPROVAL_DECISIONS = Object.freeze(['unspecified', 'approve', 'reject'])

/** Event kinds that are never batched and never dropped. */
export const CONTROL_EVENT_KINDS = Object.freeze(['approval_requested', 'approval_resolved', 'session_state', 'notice'])
const CONTROL_EVENT_KIND_SET = new Set(CONTROL_EVENT_KINDS)
const EVENT_KIND_SET = new Set(EVENT_KINDS)

export function isControlEventKind(kind) {
  return CONTROL_EVENT_KIND_SET.has(kind)
}

export function isEventKind(kind) {
  return EVENT_KIND_SET.has(kind)
}

/**
 * Normalize one adapter event. The adapter owns DSH decoding; this function
 * only enforces the plugin-owned shape and keeps content out of logs.
 */
export function normalizeEvent({
  sessionId,
  type = '',
  kind = 'output',
  data = null,
  createdAt = 0,
  dshSeq = null,
  ephemeral = false,
  approval = null,
} = {}) {
  if (typeof sessionId !== 'string' || sessionId === '') {
    throw new PluginError('invalid_argument', 'event session missing')
  }
  if (!EVENT_KINDS.includes(kind)) throw new PluginError('internal', 'event kind invalid')
  return {
    sessionId,
    type: typeof type === 'string' ? type : '',
    kind,
    data,
    createdAt: Number.isFinite(createdAt) && createdAt > 0 ? createdAt : 0,
    dshSeq: Number.isSafeInteger(dshSeq) && dshSeq >= 0 ? dshSeq : null,
    ephemeral: Boolean(ephemeral),
    approval: approval ?? null,
  }
}

export const APPROVAL_REASON_MAX = 1024
export const APPROVAL_FIELD_MAX = 256

/**
 * Bound one native approval string before it enters the approval record and
 * its digest: no control bytes, at most `max` Unicode code points. Tool
 * arguments and other native request data are never accepted here, so they
 * cannot reach the wire.
 */
export function boundedApprovalText(value, max = APPROVAL_FIELD_MAX) {
  if (typeof value !== 'string' || value === '') return ''
  const clean = value.replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g, ' ')
  const points = Array.from(clean)
  return points.length <= max ? clean : points.slice(0, max).join('')
}

export function normalizeApproval(approval) {
  if (!approval) return null
  return {
    approvalId: approval.approvalId,
    sessionId: approval.sessionId,
    toolName: boundedApprovalText(approval.toolName),
    callId: boundedApprovalText(approval.callId),
    reason: boundedApprovalText(approval.reason, APPROVAL_REASON_MAX),
    digest: approval.digest instanceof Uint8Array ? approval.digest : new Uint8Array(approval.digest ?? 0),
    version: Number(approval.version ?? 0),
    expiresAt: Number(approval.expiresAt ?? 0),
    state: APPROVAL_STATES.includes(approval.state) ? approval.state : 'unavailable',
    decision: APPROVAL_DECISIONS.includes(approval.decision) ? approval.decision : 'unspecified',
  }
}

export function normalizeSummary(summary) {
  return {
    sessionId: summary.sessionId,
    title: summary.title ?? '',
    cwd: summary.cwd ?? '',
    createdAt: Number(summary.createdAt ?? 0),
    updatedAt: Number(summary.updatedAt ?? 0),
    running: Boolean(summary.running),
    agentAvailable: Boolean(summary.agentAvailable),
    blank: Boolean(summary.blank),
    parentSessionId: summary.parentSessionId ?? '',
    origin: summary.origin ?? '',
    cursor: Number(summary.cursor ?? 0),
    queuedPrompts: Number(summary.queuedPrompts ?? 0),
  }
}

/** Map the plugin-owned session summary to the wire (snake_case) shape. */
