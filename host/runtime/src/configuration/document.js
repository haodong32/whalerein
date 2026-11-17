// Strict version-1 document parsing/normalization.
//
// Parsing is delegated to smol-toml (a maintained TOML 1.0 parser) so the Host
// never reimplements the format. toml-eslint-parser supplies the concrete
// integer/source metadata that JavaScript numbers cannot express (`1` vs `1.0`)
// and is also the range source for comment-preserving edits. Both libraries are
// maintained packages; see package.json.
import { parse } from 'smol-toml'
import { parseForESLint } from 'toml-eslint-parser'
import { BUILTIN_SET, isBuiltinEnvironment } from './catalog.js'
import { configError } from './errors.js'

export const IDENTIFIER_PATTERN = /^[a-z0-9][a-z0-9-]{0,63}$/
export const ENVIRONMENT_FIELDS = Object.freeze(['label', 'mode', 'api_origin', 'issuer', 'native_client_id', 'environment', 'tls'])
export const BUILTIN_FIELDS = Object.freeze(['label', 'tls'])
const ACCOUNT_FIELDS = Object.freeze(['api_origin', 'issuer', 'native_client_id'])
const CUSTOM_FIELDS = Object.freeze(['label', 'mode', 'api_origin', 'issuer', 'native_client_id', 'tls'])
const TOP_LEVEL_FIELDS = new Set(['version', 'environment', 'environments'])
const TLS_FIELDS = new Set(['ca_provider', 'ca_file'])
const CA_PROVIDERS = new Set(['system', 'mkcert', 'file'])

function isTable(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

/**
 * smol-toml reports semantic duplicate declarations as parse errors. The
 * contract distinguishes those from ordinary syntax errors. The pinned
 * smol-toml 1.9.x message is stable; keep the check narrow so unrelated
 * diagnostics are not reclassified.
 */
function classifyParseError(error) {
  const message = String(error?.message ?? '')
  return /already defined|trying to redefine/i.test(message) ? 'duplicate_key' : 'invalid_syntax'
}

/** Parse TOML text with the maintained strict parser, mapping failures. */
export function parseToml(text) {
  try {
    return parse(text)
  } catch (error) {
    throw configError(classifyParseError(error), 'configuration is not valid TOML')
  }
}

/** Return the AST value kind of a top-level key, or null when absent. */
function topLevelValueKind(text, key) {
  let ast
  try {
    ast = parseForESLint(text).ast
  } catch {
    // smol-toml already accepted the document; a range parser limitation must
    // not silently change a document. Classify it as unparseable input.
    throw configError('invalid_syntax', 'configuration could not be inspected')
  }
  const top = ast.body[0]
  if (!top || !Array.isArray(top.body)) return null
  for (const statement of top.body) {
    if (statement.type !== 'TOMLKeyValue') continue
    const names = statement.key?.keys
    if (!names || names.length !== 1) continue
    const part = names[0]
    let name = part.name
    if (typeof name !== 'string') {
      // A quoted top-level key is legal TOML; decode it with the same parser
      // rather than treating the quote characters as part of the name.
      const raw = text.slice(part.range[0], part.range[1])
      try {
        name = Object.keys(parse(`${raw} = 0`))[0]
      } catch {
        name = raw
      }
    }
    if (name === key) return statement.value?.kind ?? null
  }
  return null
}

function validateIdentifier(name, operation) {
  if (typeof name !== 'string' || !IDENTIFIER_PATTERN.test(name)) {
    throw configError('invalid_identifier', `${operation} must match ${IDENTIFIER_PATTERN}`)
  }
  return name
}

// Mirrors Core's URL rule: https, or loopback http for local development.
// Returns the canonical origin used for issuer distinctness.
function validateUrl(value, name, field) {
  if (typeof value !== 'string' || value.length === 0 || value.length > 2048) {
    throw configError('invalid_url', `${field} for ${name} is not a usable URL`)
  }
  let url
  try {
    url = new URL(value)
  } catch {
    throw configError('invalid_url', `${field} for ${name} is not an absolute URL`)
  }
  const host = url.hostname
  const loopback = host === 'localhost' || host === '127.0.0.1' || host === '::1' || host === '[::1]'
  if (!(url.protocol === 'https:' || (url.protocol === 'http:' && loopback))) {
    throw configError('invalid_url', `${field} for ${name} must use https (http is only accepted for loopback development)`)
  }
  if (url.username !== '' || url.password !== '' || url.search !== '' || url.hash !== '') {
    throw configError('invalid_url', `${field} for ${name} must not contain credentials, query or fragment`, { environment: name })
  }
  return `${url.protocol}//${url.host}`
}
function validateNativeClientId(value, name) {
  if (typeof value !== 'string' || value.length === 0 || value.length > 128 || /\s|[\x00-\x1f\x7f]/.test(value)) {
    throw configError('invalid_mode_fields', `native_client_id for ${name} is invalid`, { environment: name })
  }
  return value
}

/**
 * Validate and normalize a TLS table. Shared with the writer so a patch cannot
 * create a document the reader would reject.
 */
export function normalizeTls(name, tls) {
  if (!isTable(tls)) throw configError('invalid_tls', `tls for ${name} must be a table`, { environment: name })
  for (const key of Object.keys(tls)) {
    if (!TLS_FIELDS.has(key)) throw configError('unknown_field', `tls for ${name} contains an unknown field`, { environment: name })
  }
  const provider = tls.ca_provider === undefined ? (isBuiltinEnvironment(name) && name === 'local' ? 'mkcert' : 'system') : tls.ca_provider
  if (typeof provider !== 'string' || !CA_PROVIDERS.has(provider)) {
    throw configError('invalid_tls', `ca_provider for ${name} must be system, mkcert or file`, { environment: name })
  }
  if (provider === 'mkcert' && name !== 'local') {
    throw configError('invalid_tls', 'mkcert trust is only available for the built-in local environment', { environment: name })
  }
  const file = tls.ca_file
  if (provider === 'file') {
    if (typeof file !== 'string' || file.length === 0 || file.includes('\0')) {
      throw configError('invalid_tls', `ca_file is required when ca_provider is file for ${name}`, { environment: name })
    }
  } else if (file !== undefined) {
    throw configError('invalid_tls', `ca_file is only valid when ca_provider is file for ${name}`, { environment: name })
  }
  return { ca_provider: provider, ca_file: provider === 'file' ? file : null }
}

function normalizeEntry(name, entry) {
  const builtin = isBuiltinEnvironment(name)
  const allowed = builtin ? new Set(BUILTIN_FIELDS) : new Set(CUSTOM_FIELDS)
  for (const key of Object.keys(entry)) {
    if (!ENVIRONMENT_FIELDS.includes(key)) {
      throw configError('unknown_field', `environment ${name} contains an unknown field`, { environment: name })
    }
    if (!builtin && !CUSTOM_FIELDS.includes(key)) {
      throw configError('unknown_field', `environment ${name} contains an unknown field`, { environment: name })
    }
    if (builtin && !allowed.has(key)) {
      throw configError('authority_override', `built-in environment ${name} only allows label and tls; use a new environment name for custom authority`, { environment: name })
    }
  }

  let label = null
  if (entry.label !== undefined) {
    if (typeof entry.label !== 'string') throw configError('unknown_field', `label for ${name} must be a string`, { environment: name })
    label = entry.label
  }

  const tls = entry.tls === undefined ? null : normalizeTls(name, entry.tls)

  if (builtin) {
    return {
      id: name,
      mode: null,
      label,
      api_origin: null,
      issuer: null,
      native_client_id: null,
      environment: null,
      tls,
    }
  }

  const mode = entry.mode
  if (mode === undefined || typeof mode !== 'string' || (mode !== 'account' && mode !== 'standalone')) {
    throw configError('invalid_mode', `environment ${name} must declare mode account or standalone`, { environment: name })
  }

  if (mode === 'standalone') {
    for (const field of ACCOUNT_FIELDS) {
      if (entry[field] !== undefined) {
        throw configError('invalid_mode_fields', `standalone environment ${name} cannot set ${field}`, { environment: name })
      }
    }
  } else {
    for (const field of ACCOUNT_FIELDS) {
      if (typeof entry[field] !== 'string' || entry[field].length === 0) {
        throw configError('invalid_mode_fields', `account environment ${name} requires a non-empty ${field}`, { environment: name })
      }
    }
    validateNativeClientId(entry.native_client_id, name)
  }

  let apiOrigin = null
  let issuer = null
  let nativeClientId = null
  if (mode === 'account') {
    const api = validateUrl(entry.api_origin, name, 'api_origin')
    const idp = validateUrl(entry.issuer, name, 'issuer')
    if (api === idp) throw configError('invalid_url', `issuer for ${name} must use a different origin than api_origin`, { environment: name })
    apiOrigin = entry.api_origin
    issuer = entry.issuer
    nativeClientId = entry.native_client_id
  }
  return {
    id: name,
    mode,
    label,
    api_origin: apiOrigin,
    issuer,
    native_client_id: nativeClientId,
    environment: null,
    tls,
  }
}

/**
 * Read a version-1 document from text using the normative validation order.
 * Returns the normalized JSON shape shared with Rust Core.
 */
export function parseConfiguration(text) {
  if (typeof text !== 'string') throw configError('invalid_syntax', 'configuration source must be text')
  const parsed = parseToml(text)

  // 3. Version: missing, non-integer, non-1 or future -> unsupported_version.
  const version = parsed.version
  if (!Object.hasOwn(parsed, 'version') || typeof version !== 'number' || !Number.isInteger(version) || version !== 1) {
    throw configError('unsupported_version', 'configuration version must be the integer 1')
  }
  if (topLevelValueKind(text, 'version') !== 'integer') {
    throw configError('unsupported_version', 'configuration version must be the integer 1')
  }

  // 4. Top-level shape and saved selection identifier.
  for (const key of Object.keys(parsed)) {
    if (!TOP_LEVEL_FIELDS.has(key)) throw configError('unknown_field', `unknown top-level field ${key}`)
  }
  let environment = null
  if (parsed.environment !== undefined) {
    environment = validateIdentifier(parsed.environment, 'environment selection')
  }

  // 5. Environments table shape.
  const environments = {}
  if (parsed.environments !== undefined) {
    if (!isTable(parsed.environments)) throw configError('unknown_field', 'environments must be a table')
    for (const name of Object.keys(parsed.environments).sort()) {
      validateIdentifier(name, `environment name ${name}`)
      const entry = parsed.environments[name]
      if (!isTable(entry)) throw configError('unknown_field', `environment ${name} must be a table`, { environment: name })
      environments[name] = normalizeEntry(name, entry)
    }
  }

  return { version: 1, environment, environments }
}

export function emptyConfiguration() {
  return { version: 1, environment: null, environments: {} }
}

/** True when the name is a reserved built-in and may only be specialized. */
export function isBuiltin(name) {
  return BUILTIN_SET.has(name)
}
