// Official non-secret service descriptors and custom-service validation.
//
// The descriptors are derived from the packaged canonical client-environment
// catalog. They deliberately contain no credentials, ports, Access origins,
// MQTT URLs or regions: those arrive only in an approved, signed enrollment
// route. There is no second hard-coded endpoint table.

import { BUILTIN_ENVIRONMENTS, builtinDefinition } from './configuration/catalog.js'
import { PluginError } from './errors.js'

export const SERVICE_PROTOCOL_VERSION = 1
export const ENVIRONMENTS = Object.freeze(['production', 'staging', 'local', 'custom', 'standalone'])
export const PROFILE_MODES = Object.freeze(['managed', 'standalone'])

function freezeService(service) {
  return Object.freeze({ ...service })
}

/** Built-in authority descriptors derived from the canonical catalog. */
function descriptor(environment) {
  const definition = builtinDefinition(environment)
  return freezeService({
    id: definition.id,
    environment: definition.environment,
    label: definition.label,
    apiOrigin: definition.api_origin,
    issuer: definition.issuer,
    nativeClientId: definition.native_client_id,
  })
}

export const OFFICIAL_SERVICES = Object.freeze(Object.fromEntries(
  BUILTIN_ENVIRONMENTS.map((environment) => [environment, descriptor(environment)]),
))

export function officialService(environment) {
  const service = OFFICIAL_SERVICES[environment]
  if (!service) throw new PluginError('invalid_argument', 'unknown official environment')
  return service
}

function isLoopbackHostname(hostname) {
  return hostname === 'localhost' || hostname === '127.0.0.1' || hostname === '[::1]' || hostname === '::1'
}

/**
 * Validate an explicitly supplied managed-service origin. There is no
 * hostname inference: callers must state the origin and the environment.
 * HTTPS is required outside explicit loopback development, and credentials,
 * paths, queries and fragments are refused so a route cannot redirect a
 * profile to another authority.
 */
export function validateServiceOrigin(value, { allowLoopback = false } = {}) {
  if (typeof value !== 'string' || value.trim() === '' || value !== value.trim()) {
    throw new PluginError('invalid_argument', 'service API origin is required')
  }
  let url
  try {
    url = new URL(value)
  } catch {
    throw new PluginError('invalid_argument', 'service API origin is not a URL')
  }
  const secure = url.protocol === 'https:'
  const loopback = isLoopbackHostname(url.hostname)
  if (!secure && !(allowLoopback && url.protocol === 'http:' && loopback)) {
    throw new PluginError('invalid_argument', 'service API origin must use HTTPS')
  }
  if (url.username || url.password || url.search || url.hash) {
    throw new PluginError('invalid_argument', 'service API origin must not contain credentials, a query or a fragment')
  }
  if (url.pathname !== '/' && url.pathname !== '') {
    throw new PluginError('invalid_argument', 'service API origin must not contain a path')
  }
  return url.origin
}

/** Build a custom managed-service descriptor from explicit validated input. */
export function customService({ label, apiOrigin, service = '', environment = 'custom' } = {}, options = {}) {
  if (typeof label !== 'string' || label.trim() === '' || label.trim() !== label || label.length > 64) {
    throw new PluginError('invalid_argument', 'custom service label is required')
  }
  if (!['custom', 'production', 'staging'].includes(environment)) {
    throw new PluginError('invalid_argument', 'custom service environment is invalid')
  }
  const origin = validateServiceOrigin(apiOrigin, options)
  return Object.freeze({
    id: typeof service === 'string' && service !== '' ? service : `custom-${origin}`,
    environment,
    label,
    apiOrigin: origin,
  })
}

/** Validate the public `/api/host-service` document before trusting its route. */
export function validateServiceMetadata(metadata, { expectedService = '', expectedEnvironment = '', allowLoopback = false } = {}) {
  if (!metadata || typeof metadata !== 'object' || Array.isArray(metadata)) {
    throw new PluginError('protocol_version_incompatible', 'service metadata is missing')
  }
  if (typeof metadata.service !== 'string' || metadata.service === '') {
    throw new PluginError('protocol_version_incompatible', 'service metadata has no service identity')
  }
  if (!ENVIRONMENTS.includes(metadata.environment) || metadata.environment === 'standalone') {
    throw new PluginError('protocol_version_incompatible', 'service metadata has no managed environment')
  }
  if (expectedService !== '' && metadata.service !== expectedService) {
    throw new PluginError('forbidden', 'service metadata identity mismatch')
  }
  if (expectedEnvironment !== '' && metadata.environment !== expectedEnvironment) {
    throw new PluginError('forbidden', 'service metadata environment mismatch')
  }
  const protocol = metadata.protocol
  if (!protocol || !Number.isSafeInteger(protocol.version) || protocol.version !== SERVICE_PROTOCOL_VERSION) {
    throw new PluginError('protocol_version_incompatible', 'unsupported service protocol version')
  }
  if (!Array.isArray(protocol.capabilities) || !protocol.capabilities.every((item) => typeof item === 'string')) {
    throw new PluginError('protocol_version_incompatible', 'service capabilities are malformed')
  }
  if (!protocol.capabilities.includes('host-enrollment-v1')) {
    throw new PluginError('unsupported_capability', 'service does not support Host enrollment v1')
  }
  const approval = metadata.approval
  if (!approval || typeof approval !== 'object') {
    throw new PluginError('protocol_version_incompatible', 'service approval metadata is missing')
  }
  const approvalOrigin = validateServiceOrigin(approval.origin, { allowLoopback })
  if (typeof approval.path !== 'string' || !approval.path.startsWith('/') || approval.path.length > 256) {
    throw new PluginError('protocol_version_incompatible', 'service approval path is invalid')
  }
  const enrollment = metadata.enrollment
  if (!enrollment || typeof enrollment.createPath !== 'string' || !enrollment.createPath.startsWith('/')) {
    throw new PluginError('protocol_version_incompatible', 'service enrollment metadata is invalid')
  }
  const pollAfterMs = enrollment.pollAfterMs ?? 2000
  if (!Number.isSafeInteger(pollAfterMs) || pollAfterMs < 100 || pollAfterMs > 60_000) {
    throw new PluginError('protocol_version_incompatible', 'service poll interval is invalid')
  }
  return Object.freeze({
    service: metadata.service,
    environment: metadata.environment,
    protocolVersion: protocol.version,
    capabilities: Object.freeze([...protocol.capabilities]),
    approvalOrigin,
    approvalPath: approval.path,
    createPath: enrollment.createPath,
    pollAfterMs,
  })
}
