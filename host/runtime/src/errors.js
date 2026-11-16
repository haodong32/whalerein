// Stable error vocabulary shared by every module. Values are the proto
// ErrorCode names; nothing outside this module invents new wire codes.

export const ERROR_CODES = Object.freeze([
  'error_code_unspecified',
  'invalid_argument',
  'unauthorized',
  'forbidden',
  'not_found',
  'conflict',
  'expired',
  'unsupported_capability',
  'protocol_version_incompatible',
  'payload_digest_mismatch',
  'approval_expired',
  'approval_version_mismatch',
  'approval_already_resolved',
  'approval_unavailable',
  'snapshot_required',
  'revocations_pending',
  'rate_limited',
  'internal',
  'unavailable',
  'payload_too_large',
  'pairing_fenced',
  'pairing_committed_unavailable',
  'pairing_committed_unconfirmed',
  'channel_replaced',
])

/**
 * Exit code and client action for every wire `ErrorCode`, mirroring
 * proto/v1/error_codes.json and the complete table in proto/v1/WIRE.md.
 * Host-side code never invents an exit code; this table exists so the plugin,
 * Core and CLI cannot drift.
 */
export const ERROR_EXIT_CODES = Object.freeze({
  'error_code_unspecified': 1,
  'invalid_argument': 2,
  'unauthorized': 4,
  'forbidden': 4,
  'not_found': 3,
  'conflict': 8,
  'expired': 7,
  'unsupported_capability': 9,
  'protocol_version_incompatible': 9,
  'payload_digest_mismatch': 8,
  'approval_expired': 8,
  'approval_version_mismatch': 8,
  'approval_already_resolved': 8,
  'approval_unavailable': 8,
  'snapshot_required': 8,
  'revocations_pending': 8,
  'rate_limited': 8,
  'internal': 1,
  'unavailable': 5,
  'payload_too_large': 5,
  'pairing_fenced': 8,
  'pairing_committed_unavailable': 8,
  'pairing_committed_unconfirmed': 8,
  'channel_replaced': 8,
})

export const ERROR_ACTIONS = Object.freeze({
  'error_code_unspecified': 'none',
  'invalid_argument': 'fix_input',
  'unauthorized': 'reauth',
  'forbidden': 'reauth',
  'not_found': 'refresh',
  'conflict': 'refresh',
  'expired': 'retry',
  'unsupported_capability': 'upgrade',
  'protocol_version_incompatible': 'upgrade',
  'payload_digest_mismatch': 'refresh',
  'approval_expired': 'refresh',
  'approval_version_mismatch': 'refresh',
  'approval_already_resolved': 'refresh',
  'approval_unavailable': 'retry',
  'snapshot_required': 'resync',
  'revocations_pending': 'retry',
  'rate_limited': 'retry',
  'internal': 'retry',
  'unavailable': 'retry',
  'payload_too_large': 'fix_input',
  'pairing_fenced': 'restart_pairing',
  'pairing_committed_unavailable': 'restart_pairing',
  'pairing_committed_unconfirmed': 'review_pairing',
  'channel_replaced': 'retry',
})

/** Stable exit code for a wire code; an unknown code degrades to internal. */
export function errorExitCode(code) {
  return ERROR_EXIT_CODES[code] ?? 1
}

/** Stable client action for a wire code; an unknown code is not trusted. */
export function clientAction(code) {
  return ERROR_ACTIONS[code] ?? 'retry'
}

export const ONLINE_STATUSES = Object.freeze([
  'online_status_unspecified',
  'received',
  'host_offline',
  'rejected',
  'host_accepted',
  'succeeded',
  'failed',
  'unknown',
])

export class PluginError extends Error {
  /**
   * @param {string} code stable proto ErrorCode name.
   * @param {string} [detail] sanitized internal diagnostic; never sent on the wire.
   */
  constructor(code, detail = '') {
    super(detail || code)
    this.name = 'PluginError'
    this.code = code
    this.detail = detail
  }
}

/** Error raised by an adapter implementation. `code` is always a wire code. */
export class AdapterError extends Error {
  constructor(code, detail = '') {
    super(detail || code)
    this.name = 'AdapterError'
    this.code = isErrorCode(code) ? code : 'internal'
    this.detail = detail
  }
}

/**
 * A source has evidence that a command definitively failed or was never invoked.
 * A wire error code alone cannot establish this: a timeout may follow a commit.
 * Unmarked errors from execute() therefore leave the command outcome unknown.
 */
export class DefinitiveExecutionError extends AdapterError {
  constructor(code, detail = '') {
    super(code, detail)
    this.name = 'DefinitiveExecutionError'
  }
}

export function isErrorCode(code) {
  return typeof code === 'string' && ERROR_CODES.includes(code)
}

export function assertErrorCode(code) {
  if (!isErrorCode(code)) throw new PluginError('internal', 'unknown error code')
  return code
}

/** Fixed, content-free text for a wire error code. */
export function publicMessage(code) {
  switch (code) {
    case 'error_code_unspecified':
      return ''
    case 'invalid_argument':
      return 'invalid argument'
    case 'unauthorized':
      return 'unauthorized'
    case 'forbidden':
      return 'forbidden'
    case 'not_found':
      return 'not found'
    case 'conflict':
      return 'conflict'
    case 'expired':
      return 'request expired'
    case 'unsupported_capability':
      return 'unsupported capability'
    case 'protocol_version_incompatible':
      return 'protocol version incompatible'
    case 'payload_digest_mismatch':
      return 'payload digest mismatch'
    case 'approval_expired':
      return 'approval expired'
    case 'approval_version_mismatch':
      return 'approval version mismatch'
    case 'approval_already_resolved':
      return 'approval already resolved'
    case 'approval_unavailable':
      return 'approval unavailable'
    case 'snapshot_required':
      return 'snapshot required'
    case 'revocations_pending':
      return 'revocation replay pending'
    case 'rate_limited':
      return 'rate limited'
    case 'payload_too_large':
      return 'logical message exceeds transfer resource budget'
    case 'pairing_fenced':
      return 'pairing intent was invalidated before commit'
    case 'pairing_committed_unavailable':
      return 'pairing committed but its original grant is unavailable'
    case 'pairing_committed_unconfirmed':
      return 'pairing committed but Host confirmation is unknown'
    case 'unavailable':
      return 'unavailable'
    case 'channel_replaced':
      return 'channel replaced'
    default:
      return 'internal error'
  }
}

