// The DSH profile is the only configuration source. This module adapts its
// values to the shared Host resolver. Only the one-time legacy importer reads TOML.
import { createHash } from 'node:crypto'
import { homedir } from 'node:os'
import { dirname, isAbsolute, join, resolve } from 'node:path'
import { ConfigError } from 'whalerein-host/config.js'
import { emptyConfiguration, open, resolveEnvironment } from 'whalerein-host/configuration'
import { HostProfileRegistry } from 'whalerein-host/registry.js'

export const SERVICES = Object.freeze(['whalerein_cloud', 'custom', 'standalone'])
const LEGACY_KEYS = ['environment', 'config_file', 'connection_mode', 'profile_id', 'developer_tools_enabled']
const CUSTOM_KEYS = new Set(['label', 'api_origin', 'issuer', 'native_client_id', 'tls'])
const fail = (field) => { throw new ConfigError('invalid_config', `${field} is invalid`) }
const present = (value) => value !== undefined && value !== null && value !== ''

function httpsUrl(value, field, originOnly = false) {
  if (typeof value !== 'string' || value !== value.trim()) fail(field)
  let url
  try { url = new URL(value) } catch { fail(field) }
  if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash
    || (originOnly && url.pathname !== '/')) fail(field)
  return originOnly ? url.origin : value
}

export function customConfiguration(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) fail('custom')
  for (const key of Object.keys(value)) if (!CUSTOM_KEYS.has(key)) fail('custom')
  const label = present(value.label) ? value.label : 'Custom service'
  if (typeof label !== 'string' || label.trim() !== label || label.length > 64 || /[\x00-\x1f\x7f]/.test(label)) fail('custom.label')
  const api_origin = httpsUrl(value.api_origin, 'custom.api_origin', true)
  const issuer = httpsUrl(value.issuer, 'custom.issuer')
  if (new URL(issuer).origin === api_origin) fail('custom.issuer')
  const native_client_id = value.native_client_id
  if (typeof native_client_id !== 'string' || !native_client_id.trim() || native_client_id.length > 256 || /[\s\x00-\x1f\x7f]/.test(native_client_id)) fail('custom.native_client_id')
  const tls = value.tls ?? {}
  if (!tls || typeof tls !== 'object' || Array.isArray(tls) || Object.keys(tls).some((k) => !['ca_provider', 'ca_file'].includes(k))) fail('custom.tls')
  const ca_provider = present(tls.ca_provider) ? tls.ca_provider : 'system'
  if (!['system', 'file'].includes(ca_provider)) fail('custom.tls.ca_provider')
  if (ca_provider === 'file' && (typeof tls.ca_file !== 'string' || !isAbsolute(tls.ca_file) || tls.ca_file.includes('\0'))) fail('custom.tls.ca_file')
  if (ca_provider !== 'file' && present(tls.ca_file)) fail('custom.tls.ca_file')
  return { label, api_origin, issuer, native_client_id, tls: { ca_provider, ca_file: ca_provider === 'file' ? tls.ca_file : null } }
}

export function serviceConfiguration(raw = {}) {
  const service = present(raw.service) ? raw.service : (['server', 'mqtt_url', 'host_token'].some((k) => present(raw[k])) ? 'standalone' : 'whalerein_cloud')
  if (!SERVICES.includes(service)) fail('service')
  const legacy = LEGACY_KEYS.filter((k) => present(raw[k]) && raw[k] !== 'auto' && raw[k] !== false)
  if (raw.service && legacy.length) fail(legacy[0])
  const cloudEnvironment = present(raw.cloud_environment) ? raw.cloud_environment : raw.environment || 'production'
  if (!['production', 'staging', 'local'].includes(cloudEnvironment)) fail('cloud_environment')
  if (present(raw.custom) && (typeof raw.custom !== 'object' || Array.isArray(raw.custom)
    || Object.keys(raw.custom).some((key) => !CUSTOM_KEYS.has(key)))) fail('custom')
  const custom = service === 'custom' ? customConfiguration(raw.custom) : null
  const environment = service === 'custom'
    ? `dsh-${createHash('sha256').update(JSON.stringify([custom.api_origin, custom.issuer, custom.native_client_id])).digest('hex').slice(0, 24)}`
    : cloudEnvironment
  return { service, cloudEnvironment, environment, custom }
}

export function profileSnapshot(config) {
  const document = emptyConfiguration()
  document.environment = config.environment
  if (config.service === 'custom') {
    document.environments[config.environment] = { id: config.environment, environment: null, mode: 'account', ...config.custom }
  }
  const snapshot = Object.freeze({
    path: join(config.stateRoot, 'profile-configuration'),
    sourceKind: 'profile', document,
  })
  resolveEnvironment(snapshot, config.environment)
  return snapshot
}

/** One-time legacy import. Persistence must succeed before runtime startup. */
export function migrateServiceConfig(raw, ctx) {
  if (present(raw.service)) { serviceConfiguration(raw); return raw }
  const next = { ...raw }
  if (present(raw.cloud_environment) && present(raw.environment) && raw.cloud_environment !== raw.environment) fail('cloud_environment')
  let environment = raw.cloud_environment || raw.environment || 'production'
  const stateRoot = raw.state_root || raw.state_dir || join(homedir(), '.whalerein', 'dsh')
  // Preserve the persisted identity selection; it is no longer a runtime
  // override once the choice has been written into the DSH profile.
  const registry = new HostProfileRegistry({ stateRoot })
  const saved = registry.read()
  const active = saved?.profiles.find((p) => p.id === (raw.profile_id || saved.active))
  if (raw.profile_id && !active) fail('profile_id')
  if (!raw.cloud_environment && !raw.environment && active && active.mode !== 'standalone') environment = active.environment
  next.service = raw.connection_mode === 'standalone' || (!raw.connection_mode || raw.connection_mode === 'auto')
    && (active?.mode === 'standalone' || ['server', 'mqtt_url', 'host_token'].some((k) => present(raw[k])))
    ? 'standalone' : 'whalerein_cloud'
  next.cloud_environment = ['production', 'staging', 'local'].includes(environment) ? environment : 'production'
  if (next.service !== 'standalone' && active?.mode === 'managed'
    && !['production', 'staging', 'local'].includes(environment) && !present(raw.config_file)) fail('custom')
  if (present(raw.config_file) && next.service !== 'standalone') {
    let resolved
    let snapshot
    try {
      snapshot = open({ configFile: raw.config_file, inspectionOnly: true })
      resolved = resolveEnvironment(snapshot, raw.environment || (active?.mode === 'managed' ? active.environment : null))
    } catch { fail('config_file') }
    const definition = resolved.definition
    if (definition.mode !== 'account') fail('config_file')
    const builtin = ['production', 'staging', 'local'].includes(resolved.environment)
    const expectedProvider = resolved.environment === 'local' ? 'mkcert' : 'system'
    if (builtin && definition.tls.ca_provider === expectedProvider) {
      next.service = 'whalerein_cloud'
      next.cloud_environment = resolved.environment
    } else {
      next.service = 'custom'
      next.custom = {
        label: definition.label || 'Custom service', api_origin: definition.api_origin,
        issuer: definition.issuer, native_client_id: definition.native_client_id,
        tls: definition.tls,
      }
      if (next.custom.tls.ca_provider === 'file' && !isAbsolute(next.custom.tls.ca_file)) {
        next.custom.tls = { ...next.custom.tls, ca_file: next.custom.tls.ca_file.startsWith('~/')
          ? join(homedir(), next.custom.tls.ca_file.slice(2))
          : resolve(dirname(snapshot.path), next.custom.tls.ca_file) }
      }
    }
  }
  for (const key of LEGACY_KEYS) delete next[key]
  serviceConfiguration(next)
  if (ctx?.fiber && typeof ctx.emit === 'function') ctx.emit('internal/commit', ctx.fiber, next)
  return next
}
