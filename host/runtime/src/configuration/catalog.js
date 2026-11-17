// The built-in environment authority is shipped data, not source-tree policy.
// The generated catalog.json is synced from contracts/client-environment by
// scripts/sync-client-environment-contract.mjs; never edit it by hand.
import { readFileSync } from 'node:fs'

export const CATALOG_SCHEMA = 'whalerein/v1/client-environment-catalog'
export const CATALOG_VERSION = 1
export const BUILTIN_ENVIRONMENTS = Object.freeze(['local', 'staging', 'production'])
export const BUILTIN_SET = new Set(BUILTIN_ENVIRONMENTS)

let cached = null

/** Read and validate the packaged catalog. Built-ins never come from user files. */
export function loadCatalog() {
  if (cached) return cached
  let raw
  try {
    raw = readFileSync(new URL('./catalog.json', import.meta.url), 'utf8')
  } catch {
    throw new Error('built-in client environment catalog is unavailable')
  }
  let value
  try {
    value = JSON.parse(raw)
  } catch {
    throw new Error('built-in client environment catalog is invalid')
  }
  if (value?.schema !== CATALOG_SCHEMA || value?.version !== CATALOG_VERSION || !isTable(value.environments)) {
    throw new Error('built-in client environment catalog is invalid')
  }
  const environments = {}
  for (const name of BUILTIN_ENVIRONMENTS) {
    const entry = value.environments[name]
    if (!isTable(entry)) throw new Error('built-in client environment catalog is incomplete')
    environments[name] = Object.freeze({
      id: entry.id,
      mode: entry.mode,
      label: entry.label,
      api_origin: entry.api_origin,
      issuer: entry.issuer,
      native_client_id: entry.native_client_id,
      environment: entry.environment,
      tls: Object.freeze({ ca_provider: entry.tls.ca_provider, ca_file: entry.tls.ca_file ?? null }),
    })
  }
  cached = Object.freeze({ schema: value.schema, version: value.version, environments: Object.freeze(environments) })
  return cached
}

export function isBuiltinEnvironment(name) {
  return BUILTIN_SET.has(name)
}

export function builtinDefinition(name) {
  const entry = loadCatalog().environments[name]
  if (!entry) return null
  return { ...entry, tls: { ...entry.tls } }
}

function isTable(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}
