// Shared approval review redaction. The DSH adapter projects the facts and the
// Host serialization boundary re-checks them; both use these exact rules.

/** Maximum length of one review field, in Unicode code points. */
export const REVIEW_FIELD_MAX = 256

/** Fixed placeholder for anything that looks like a credential. */
export const REVIEW_REDACTED = '<redacted>'

const SECRET_ASSIGNMENT = /((?:api[_-]?key|apikey|access[_-]?key|secret[_-]?key|client[_-]?secret|token|secret|password|passwd|pwd|authorization|private[_-]?key|credential|session[_-]?key)\s*[:=]\s*)("[^"]*"|'[^']*'|[^\s&;,]+)/gi

const MASK_RULES = Object.freeze([
  // PEM private-key blocks before the generic base64 rule.
  [new RegExp(
    '-----BEGIN ' +
    '[A-Z0-9 ]*' +
    'PRIVATE KEY-----' + '[\\s\\S]*?-----END [A-Z0-9 ]*' + 'PRIVATE KEY-----',
    'gi',
  ), REVIEW_REDACTED],
  // HTTP authorization values.
  [/\b(Bearer|Basic|Token)\s+[A-Za-z0-9._~+/=-]{4,}/gi, `$1 ${REVIEW_REDACTED}`],
  // `token=...`, `api_key: ...`, `Authorization: ...` and similar assignments.
  [SECRET_ASSIGNMENT, `$1${REVIEW_REDACTED}`],
  // Well-known credential prefixes.
  [/\b(sk|rk|pk)-[A-Za-z0-9_-]{8,}/g, REVIEW_REDACTED],
  [/\bsk_[A-Za-z0-9_-]{8,}/g, REVIEW_REDACTED],
  [/\bgh[pousr]_[A-Za-z0-9]{20,}\b/g, REVIEW_REDACTED],
  [/\bxox[abprs]-[A-Za-z0-9-]{10,}\b/g, REVIEW_REDACTED],
  [/\bAKIA[0-9A-Z]{16}\b/g, REVIEW_REDACTED],
  [/\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b/g, REVIEW_REDACTED],
  // Credentials embedded in a URL authority.
  [/([a-z][a-z0-9+.-]*:\/\/)[^\s/@]+@/gi, `$1${REVIEW_REDACTED}@`],
  // Long hexadecimal tokens (keys, digests, session tokens).
  [/\b[0-9a-fA-F]{32,}\b/g, REVIEW_REDACTED],
  // Long base64/base64url-looking tokens. `/` is excluded so a long path is
  // still identifiable; a secret embedded as a single path segment is masked.
  [/\b[A-Za-z0-9+_-]{48,}={0,2}\b/g, REVIEW_REDACTED],
])

/**
 * Mask credential-looking substrings in one already-stringified value.
 * The replacement is a fixed placeholder, so no part of the secret survives.
 */
export function maskReviewValue(value) {
  if (typeof value !== 'string' || value === '') return ''
  let text = value
  for (const [pattern, replacement] of MASK_RULES) {
    // Reset lastIndex for global expressions reused across calls.
    pattern.lastIndex = 0
    text = text.replace(pattern, replacement)
  }
  return text
}

/**
 * Strip control characters, mask credentials and bound one review value.
 * Order matters: controls are removed first so they cannot split a secret
 * match, and masking happens before truncation so a secret is never half
 * visible at the boundary.
 */
export function boundReviewValue(value) {
  if (typeof value !== 'string' || value === '') return ''
  const cleaned = value.replace(/[\u0000-\u001F\u007F-\u009F]/g, ' ').replace(/\s+/g, ' ').trim()
  const masked = maskReviewValue(cleaned)
  const points = Array.from(masked)
  return (points.length <= REVIEW_FIELD_MAX ? masked : points.slice(0, REVIEW_FIELD_MAX).join('')).trim()
}
