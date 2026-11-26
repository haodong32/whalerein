// Manifest-driven validation for the shared latency observation contract.
//
// The checked-in manifest is the single allowlist: event names, producer roles,
// required/optional typed fields, bounded enums and forbidden keys. A record
// that is not declared here is rejected rather than written, so a new field
// can never leak content by accident.
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

// The integration image symlinks this package under /opt, so a relative
// import.meta.url would resolve outside the mounted workspace. The fixture
// supplies the absolute contract path; local runs keep the checked-in path.
const CONTRACT_ROOT = process.env.WHALEREIN_BENCH_CONTRACT_PATH
  ? process.env.WHALEREIN_BENCH_CONTRACT_PATH.replace(/\/[^/]*$/, '')
  : null
export const OBSERVATION_MANIFEST_PATH = CONTRACT_ROOT
  ? `${CONTRACT_ROOT}/observation.json`
  : fileURLToPath(new URL('../../../../contracts/latency/observation.json', import.meta.url))
export const OBSERVATION_VECTORS_PATH = CONTRACT_ROOT
  ? `${CONTRACT_ROOT}/vectors.json`
  : fileURLToPath(new URL('../../../../contracts/latency/vectors.json', import.meta.url))

export class ObservationError extends Error {
  constructor(reason, detail = '') {
    super(detail === '' ? `latency observation rejected: ${reason}` : `latency observation rejected: ${reason} (${detail})`)
    this.name = 'ObservationError'
    this.code = 'invalid_observation'
    this.reason = reason
  }
}

let cachedManifest = null

export function loadManifest(path = OBSERVATION_MANIFEST_PATH) {
  if (path === OBSERVATION_MANIFEST_PATH && cachedManifest) return cachedManifest
  const manifest = JSON.parse(readFileSync(path, 'utf8'))
  validateManifestShape(manifest)
  if (path === OBSERVATION_MANIFEST_PATH) cachedManifest = manifest
  return manifest
}

export function loadVectors(path = OBSERVATION_VECTORS_PATH) {
  return JSON.parse(readFileSync(path, 'utf8'))
}

function validateManifestShape(manifest) {
  if (!manifest || typeof manifest !== 'object') throw new ObservationError('manifest_missing')
  if (manifest.schema !== 1) throw new ObservationError('manifest_schema')
  if (!manifest.header || typeof manifest.header !== 'object') throw new ObservationError('manifest_header')
  if (!Array.isArray(manifest.header.required)) throw new ObservationError('manifest_header')
  if (!Number.isSafeInteger(manifest.header.v)) throw new ObservationError('manifest_header')
  if (!Array.isArray(manifest.producers) || manifest.producers.length === 0) throw new ObservationError('manifest_producers')
  if (!manifest.events || typeof manifest.events !== 'object') throw new ObservationError('manifest_events')
  return manifest
}

function forbiddenSet(manifest) {
  return new Set(manifest.forbidden_fields ?? [])
}

function isPlainObject(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value)
}

function findForbiddenKey(value, forbidden, where) {
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findForbiddenKey(item, forbidden, where)
      if (found) return found
    }
    return ''
  }
  if (!isPlainObject(value)) return ''
  for (const [key, child] of Object.entries(value)) {
    if (forbidden.has(key)) return `${where}${where ? '.' : ''}${key}`
    const found = findForbiddenKey(child, forbidden, `${where}${where ? '.' : ''}${key}`)
    if (found) return found
  }
  return ''
}

function typeProblem(type, value, manifest) {
  if (typeof type !== 'string' || type === '') return { reason: 'field_type_missing' }
  if (type.startsWith('enum:')) {
    const allowed = type.slice(5).split(',').filter(Boolean)
    return allowed.includes(value) ? null : { reason: 'unknown_enum_value' }
  }
  switch (type) {
    case 'u64':
      return Number.isSafeInteger(value) && value >= 0 ? null : { reason: 'invalid_u64' }
    case 'bool':
      return typeof value === 'boolean' ? null : { reason: 'invalid_bool' }
    case 'str':
      if (typeof value !== 'string') return { reason: 'invalid_string' }
      return value.length <= 512 ? null : { reason: 'oversized_string' }
    case 'producer':
      return manifest.producers.includes(value) ? null : { reason: 'unknown_producer' }
    default:
      return { reason: 'unknown_field_type' }
  }
}

/**
 * Validate one complete record against the manifest.
 * @returns {{ok:true, record:object}|{ok:false, reason:string, field?:string}}
 */
export function validateRecord(record, manifest = loadManifest()) {
  if (!isPlainObject(record)) return { ok: false, reason: 'record_not_object' }
  for (const key of manifest.header.required) {
    if (!Object.hasOwn(record, key)) return { ok: false, reason: 'missing_header_field', field: key }
  }
  if (record.v !== manifest.header.v) return { ok: false, reason: 'schema_version_mismatch', field: 'v' }
  if (typeof record.run !== 'string' || record.run === '') return { ok: false, reason: 'invalid_run', field: 'run' }
  if (typeof record.lifetime !== 'string' || record.lifetime === '') return { ok: false, reason: 'invalid_lifetime', field: 'lifetime' }
  if (!manifest.producers.includes(record.producer)) return { ok: false, reason: 'unknown_producer', field: 'producer' }
  if (!Number.isSafeInteger(record.seq) || record.seq < 0) return { ok: false, reason: 'invalid_seq', field: 'seq' }
  if (!Number.isInteger(record.ts) || record.ts < 0) return { ok: false, reason: 'invalid_timestamp', field: 'ts' }
  const declaration = manifest.events[record.event]
  if (!declaration) return { ok: false, reason: 'unknown_event', field: 'event' }
  if (!declaration.producers.includes(record.producer)) return { ok: false, reason: 'wrong_producer', field: 'producer' }
  if (!isPlainObject(record.data)) return { ok: false, reason: 'missing_data', field: 'data' }

  const forbidden = forbiddenSet(manifest)
  const leaked = findForbiddenKey(record, forbidden, '')
  if (leaked !== '') return { ok: false, reason: 'forbidden_field', field: leaked }

  const allowed = new Set([...(declaration.required ?? []), ...(declaration.optional ?? [])])
  for (const key of Object.keys(record.data)) {
    if (!allowed.has(key)) return { ok: false, reason: 'unknown_data_field', field: key }
  }
  for (const key of declaration.required ?? []) {
    if (!Object.hasOwn(record.data, key)) return { ok: false, reason: 'missing_required_field', field: key }
  }
  for (const [key, value] of Object.entries(record.data)) {
    const problem = typeProblem(declaration.fields?.[key], value, manifest)
    if (problem) return { ok: false, reason: problem.reason, field: key }
  }
  return { ok: true, record }
}

export function assertRecord(record, manifest = loadManifest()) {
  const result = validateRecord(record, manifest)
  if (!result.ok) throw new ObservationError(result.reason, result.field ?? '')
  return result.record
}