// The DSH profile is the only configuration source. This module adapts its
// values to the shared Host resolver without reading another configuration source.
import { createHash } from 'node:crypto'
import { isAbsolute, join } from 'node:path'
import { ConfigError } from 'whalerein-host/config.js'
import { emptyConfiguration, resolveEnvironment } from 'whalerein-host/configuration'

export const SERVICES = Object.freeze(['whalerein_cloud', 'custom', 'standalone'])
const UNSUPPORTED_KEYS = ['environment', 'config_file', 'connection_mode', 'profile_id', 'developer_tools_enabled']
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

export function validateProfileKeys(raw = {}) {
  for (const key of UNSUPPORTED_KEYS) if (Object.hasOwn(raw, key)) fail(key)
}

export function serviceConfiguration(raw = {}) {
  validateProfileKeys(raw)
  const service = present(raw.service) ? raw.service : 'whalerein_cloud'
  if (!SERVICES.includes(service)) fail('service')
  const cloudEnvironment = present(raw.cloud_environment) ? raw.cloud_environment : 'production'
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
