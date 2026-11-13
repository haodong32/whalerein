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
  'channel_replaced',
])

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
    case 'unavailable':
      return 'unavailable'
    case 'channel_replaced':
      return 'channel replaced'
    default:
      return 'internal error'
  }
}

