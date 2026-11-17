// Settings projection with the same fields as Core's ConfigView: source path,
// effective and desired selection, field provenance, offline-vs-live scope and
// redacted errors. It never exposes file bytes, credential URLs or secrets.
import { emptyConfiguration } from './document.js'
import { configError, redactError } from './errors.js'
import { isSnapshot, resolveEnvironment, resolutionContext } from './resolve.js'

function fieldProvenance(document, resolution) {
  const entry = document.environments[resolution.environment]
  const builtin = resolution.definition_source === 'builtin'
  const userTls = Boolean(entry && entry.tls !== null)
  const userLabel = Boolean(entry && entry.label !== null)
  const fileOrDefault = (present) => (present ? 'file' : 'default')
  const provenance = {
    id: builtin ? 'catalog' : 'file',
    mode: builtin ? 'catalog' : 'file',
    label: builtin ? (userLabel ? 'file' : 'catalog') : fileOrDefault(userLabel),
    environment: builtin ? 'catalog' : 'default',
    tls: builtin ? (userTls ? 'file' : 'catalog') : fileOrDefault(userTls),
  }
  for (const field of ['api_origin', 'issuer', 'native_client_id']) {
    provenance[field] = builtin ? 'catalog' : fileOrDefault(Boolean(entry && entry[field] !== null))
  }
  return provenance
}

function errorView(error) {
  if (!error) return null
  const redacted = redactError(error)
  const view = { code: redacted.code, message: redacted.message, environment: error.environment ?? null }
  return view
}

/**
 * Project a snapshot or an already-resolved environment for settings display.
 * Resolution failures are redacted diagnostics, not a reason to hide the
 * selected path and desired default.
 */
export function view(target, options = {}) {
  let snapshot
  let resolution = null
  let override = options.override ?? null

  if (isSnapshot(target)) {
    snapshot = target
    try {
      resolution = resolveEnvironment(snapshot, override)
    } catch (error) {
      resolution = null
      options = { ...options, error }
    }
  } else if (target && typeof target === 'object' && typeof target.environment === 'string') {
    resolution = target
    const context = resolutionContext(target)
    snapshot = context?.snapshot ?? options.snapshot
    override = context?.override ?? override
    if (!snapshot) throw configError('missing_config', 'a configuration snapshot is required for a view')
  } else {
    throw configError('missing_config', 'a configuration snapshot or resolution is required for a view')
  }

  const document = snapshot.document ?? emptyConfiguration()
  const errors = []
  if (options.error) {
    const projected = errorView(options.error)
    if (projected) errors.push(projected)
  }
  return {
    path: snapshot.path,
    source_kind: snapshot.sourceKind ?? 'explicit',
    revision: snapshot.revision,
    selected_environment: resolution ? resolution.environment : null,
    selection_source: resolution ? resolution.selection_source : null,
    definition_source: resolution ? resolution.definition_source : null,
    desired_default: document.environment ?? null,
    effective_override: override,
    definition: resolution ? resolution.definition : null,
    field_provenance: resolution ? fieldProvenance(document, resolution) : {},
    validation: { scope: 'offline', live_readiness: 'not_checked' },
    errors,
  }
}
