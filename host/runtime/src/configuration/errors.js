// Stable configuration error vocabulary shared with Rust Core. The codes are
// contract values, not wire `ErrorCode`s: Core and Host must classify a shared
// fixture identically even though they present errors through different APIs.
export const CONFIG_ERROR_CODES = Object.freeze([
  'invalid_syntax',
  'duplicate_key',
  'unsupported_version',
  'unknown_field',
  'invalid_identifier',
  'invalid_url',
  'invalid_tls',
  'invalid_mode',
  'invalid_mode_fields',
  'authority_override',
  'unknown_environment',
  'missing_config',
  'unreadable_config',
  'revision_conflict',
  'import_conflict',
  'import_unsupported',
  'import_storage_unavailable',
])

/**
 * Typed configuration error. `details` carries structured, non-secret context
 * (environment name, revision, caller draft); it never carries file bytes,
 * credential-bearing URLs or token values.
 */
export class ConfigurationError extends Error {
  constructor(code, message = '', details = {}) {
    super(message || code)
    this.name = 'ConfigurationError'
    this.code = code
    Object.assign(this, details)
  }

  toJSON() {
    const value = { code: this.code, message: this.message }
    for (const key of ['environment', 'path', 'currentRevision', 'expectedRevision', 'operation']) {
      if (this[key] !== undefined) value[key] = this[key]
    }
    return value
  }
}

export function configError(code, message = '', details = {}) {
  return new ConfigurationError(code, message, details)
}

/** Keep diagnostics free of URL userinfo/query material and raw secrets. */
export function redactMessage(message) {
  const value = typeof message === 'string' ? message : ''
  return value
    .replace(/\/\/[^\s/@]+:[^\s/@]+@/g, '//')
    .replace(/\?[^\s]*/g, '')
    .slice(0, 400)
}

/** Redacted error projection used by settings views. */
export function redactError(error) {
  if (!error) return null
  if (error instanceof ConfigurationError) {
    return { code: error.code, message: redactMessage(error.message) }
  }
  return { code: 'unreadable_config', message: 'configuration operation failed' }
}
