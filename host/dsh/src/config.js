import { ConfigError, validateTopicId, PROTOCOL_VERSION } from 'whalerein-host/config.js'
export { ConfigError, validateTopicId, topics, PROTOCOL_VERSION } from 'whalerein-host/config.js'
// Plugin configuration. Callers pass the profile document; this module does not
// read process.env. A profile value may already be the result of a !!js
// expression that the profile itself stored.

import { homedir } from 'node:os'
import { join } from 'node:path'

function pick(raw, names, fallback) {
  for (const name of names) {
    const value = raw[name]
    if (value !== undefined && value !== null && value !== '') return value
  }
  return fallback
}

function integer(value, fallback, { min = 0, max = Number.MAX_SAFE_INTEGER, name } = {}) {
  if (value === undefined || value === null || value === '') return fallback
  const parsed = typeof value === 'number' ? value : Number(value)
  if (!Number.isSafeInteger(parsed) || parsed < min || parsed > max) {
    throw new ConfigError('invalid_config', `${name} must be an integer between ${min} and ${max}`)
  }
  return parsed
}

function boolean(value, fallback) {
  if (value === undefined || value === null || value === '') return fallback
  if (typeof value === 'boolean') return value
  if (value === 'true' || value === '1') return true
  if (value === 'false' || value === '0') return false
  throw new ConfigError('invalid_config', 'boolean value expected')
}

function requireString(value, name) {
  if (typeof value !== 'string' || value.trim() === '') {
    throw new ConfigError('invalid_config', `${name} is required`)
  }
  return value
}

/**
 * Validate an identifier that is interpolated into an MQTT topic or client id.
 * Opaque ids still must not contain topic wildcards or separators.
 */
function optionalPort(raw, name) {
  const value = pick(raw, [name], '')
  if (value === undefined || value === null || value === '') return undefined
  return integer(value, undefined, { min: 1, max: 65535, name })
}

/** Host name or IPv4 address. Ports and schemes belong in the advanced fields. */
export function serverHost(value) {
  const text = String(value ?? '').trim()
  if (!/^[A-Za-z0-9.-]{1,253}$/.test(text) || text.startsWith('.') || text.startsWith('-') || text.endsWith('.') || text.endsWith('-') || text.includes('..')) {
    throw new ConfigError('invalid_config', 'server must be a host name or IPv4 address')
  }
  return text
}

function derivedEndpoints(server, insecure, accessPort, mqttPort) {
  const access = accessPort ?? (insecure ? 8080 : 443)
  const mqtt = mqttPort ?? (insecure ? 1883 : 8883)
  const accessScheme = insecure ? 'http' : 'https'
  const mqttScheme = insecure ? 'mqtt' : 'mqtts'
  const hideAccessPort = (!insecure && access === 443) || (insecure && access === 80)
  return {
    accessUrl: hideAccessPort ? `${accessScheme}://${server}` : `${accessScheme}://${server}:${access}`,
    mqttUrl: `${mqttScheme}://${server}:${mqtt}`,
    accessPort: access,
    mqttPort: mqtt,
  }
}

/**
 * Resolve one plugin configuration object. A `server` host derives the Access
 * origin and the MQTT URL. Explicit `mqtt_url` / `access_url` apply only when
 * `server` is empty, so existing deployments keep working.
 *
 * @param {object} raw plugin config supplied by Cordis.
 * @returns {object} resolved config.
 */
export function resolveConfig(raw = {}) {
  const adapter = pick(raw, ['adapter'], 'auto')
  if (!['auto', 'fake', 'real'].includes(adapter)) {
    throw new ConfigError('invalid_config', 'adapter must be auto, fake, or real')
  }
  const hostId = validateTopicId(pick(raw, ['host_id'], ''), 'host_id')
  const hostToken = requireString(pick(raw, ['host_token'], ''), 'host_token')
  const adminToken = pick(raw, ['admin_token'], '') || ''
  const serverValue = String(pick(raw, ['server'], '') ?? '').trim()
  const allowInsecure = boolean(pick(raw, ['allow_insecure_mqtt'], false), false)

  let accessUrl = ''
  let mqttUrl = ''
  let accessPort = null
  let mqttPort = null
  let server = ''
  if (serverValue) {
    server = serverHost(serverValue)
    const derived = derivedEndpoints(
      server,
      allowInsecure,
      optionalPort(raw, 'access_port'),
      optionalPort(raw, 'mqtt_port'),
    )
    accessUrl = derived.accessUrl
    mqttUrl = derived.mqttUrl
    accessPort = derived.accessPort
    mqttPort = derived.mqttPort
  } else {
    mqttUrl = requireString(pick(raw, ['mqtt_url'], ''), 'mqtt_url')
    accessUrl = pick(raw, ['access_url'], '')
    if (accessUrl) {
      try {
        void new URL(accessUrl)
      } catch {
        throw new ConfigError('invalid_config', 'access_url must be a URL')
      }
    }
  }

  let parsedUrl
  try {
    parsedUrl = new URL(mqttUrl)
  } catch {
    throw new ConfigError('invalid_config', 'mqtt_url must be a URL')
  }
  if (parsedUrl.protocol !== 'mqtts:' && !allowInsecure) {
    throw new ConfigError('invalid_config', 'mqtt_url must use mqtts TLS (set allow_insecure_mqtt only for tests)')
  }
  if (!['mqtt:', 'mqtts:'].includes(parsedUrl.protocol)) {
    throw new ConfigError('invalid_config', 'mqtt_url must use the mqtt or mqtts scheme')
  }

  const stateDir = pick(raw, ['state_dir'], join(homedir(), '.whalerein', 'dsh'))
  const protoPath = pick(raw, ['proto_path'], '')
  const approvalMode = pick(raw, ['approval_mode'], 'claim')
  if (!['claim', 'delegate'].includes(approvalMode)) {
    throw new ConfigError('invalid_config', 'approval_mode must be claim or delegate')
  }

  return {
    accessUrl: accessUrl || '',
    server,
    adminToken,
    accessPort,
    mqttPort,
    adapter,
    hostId,
    mqttUrl,
    hostToken,
    caFile: pick(raw, ['ca_file'], '') || '',
    stateDir,
    protoPath: protoPath || '',
    allowInsecureMqtt: allowInsecure,
    protocolVersion: PROTOCOL_VERSION,
    // Events
    eventRecentLimit: integer(raw.event_recent_limit, 500, { min: 1, max: 100_000, name: 'event_recent_limit' }),
    outboxPerDeviceLimit: integer(raw.outbox_per_device_limit, 100, { min: 1, max: 100_000, name: 'outbox_per_device_limit' }),
    outboxTotalLimit: integer(raw.outbox_total_limit, 1000, { min: 1, max: 1_000_000, name: 'outbox_total_limit' }),
    // Snapshots
    snapshotMaxEvents: integer(raw.snapshot_max_events, 5000, { min: 1, max: 200_000, name: 'snapshot_max_events' }),
    // Approvals
    approvalMode,
    approvalTtlMs: integer(raw.approval_ttl_ms, 300_000, { min: 1000, max: 86_400_000, name: 'approval_ttl_ms' }),
    // MQTT reconnect
    reconnectMinMs: integer(raw.reconnect_min_ms, 500, { min: 50, max: 60_000, name: 'reconnect_min_ms' }),
    reconnectMaxMs: integer(raw.reconnect_max_ms, 30_000, { min: 100, max: 600_000, name: 'reconnect_max_ms' }),
    // Lifecycle
    shutdownTimeoutMs: integer(raw.shutdown_timeout_ms, 5000, { min: 100, max: 120_000, name: 'shutdown_timeout_ms' }),
    logLevel: pick(raw, ['log_level'], 'info'),
  }
}

/** MQTT topics. Only these four exist in the wire contract. */
