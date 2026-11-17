// Effective environment selection: invocation override -> saved file default ->
// production. Unknown names never fall back.
import { builtinDefinition, isBuiltinEnvironment } from './catalog.js'
import { IDENTIFIER_PATTERN, emptyConfiguration } from './document.js'
import { configError } from './errors.js'

const resolutionContexts = new WeakMap()

function attachContext(resolution, context) {
  resolutionContexts.set(resolution, context)
  // A non-enumerable property keeps the normative JSON shape exact for corpus
  // comparison while still letting a copied view consumer recover provenance.
  Object.defineProperty(resolution, 'snapshot', { value: context.snapshot, enumerable: false })
  return resolution
}

function validOverride(value) {
  if (typeof value !== 'string' || !IDENTIFIER_PATTERN.test(value)) {
    throw configError('invalid_identifier', `environment override must match ${IDENTIFIER_PATTERN}`)
  }
  return value
}

/**
 * Resolve the selected environment against the catalog and file entries.
 * The returned object is exactly the normalized contract shape.
 */
export function resolveEnvironment(snapshot, invocationOverride = null) {
  if (!snapshot || typeof snapshot !== 'object') throw configError('missing_config', 'configuration snapshot is required')
  const document = snapshot.document ?? emptyConfiguration()

  let environment
  let selectionSource
  if (invocationOverride !== undefined && invocationOverride !== null) {
    environment = validOverride(invocationOverride)
    selectionSource = 'invocation'
  } else if (document.environment !== null) {
    environment = document.environment
    selectionSource = 'file'
  } else {
    environment = 'production'
    selectionSource = 'default'
  }

  let definitionSource
  let definition
  if (isBuiltinEnvironment(environment)) {
    definition = builtinDefinition(environment)
    const entry = document.environments[environment]
    if (entry) {
      if (entry.label !== null) definition.label = entry.label
      if (entry.tls !== null) definition.tls = { ...entry.tls }
    }
    definitionSource = 'builtin'
  } else if (document.environments[environment]) {
    const entry = document.environments[environment]
    definition = {
      ...entry,
      tls: entry.tls === null ? { ca_provider: 'system', ca_file: null } : { ...entry.tls },
    }
    definitionSource = 'file'
  } else {
    throw configError('unknown_environment', `environment ${environment} is not defined`, { environment })
  }

  const resolution = {
    environment,
    selection_source: selectionSource,
    definition_source: definitionSource,
    definition,
  }
  return attachContext(resolution, { snapshot, override: invocationOverride ?? null })
}

export function resolutionContext(resolution) {
  return resolutionContexts.get(resolution) ?? null
}

export function isSnapshot(value) {
  return Boolean(value && typeof value === 'object' && typeof value.path === 'string' && 'revision' in value && 'exists' in value)
}
