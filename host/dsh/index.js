// WhaleRein for DSH plugin entry point.
//
// The plugin is a protocol endpoint: it opens exactly one outbound MQTT 5/TLS
// connection, may call Access to mint a pairing URI, and never listens or links Rust.

import os from 'node:os'
import { mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs'
import z from '@deepseek-ai/schemastery'
import { validateTopicId } from 'whalerein-host/config.js'
import { ConfigError, resolveConfig } from './src/config.js'
import { generateHostId } from 'whalerein-host/host-id.js'
import { HostRuntime } from 'whalerein-host/host.js'
import { MemoryHostCredentials } from 'whalerein-host/security/credentials.js'
import { DshHostCredentials } from './src/credentials.js'
import { createAdapter, FakeDshAdapter } from './src/adapter/index.js'
import { join } from 'node:path'
import { createLogger, createLogSalt } from 'whalerein-host/log.js'
import { createSettingsService } from './src/settings.js'

export const name = 'whalerein-dsh'
// Cordis applies the plugin only once the real DSH services exist, so `auto`
// selects the real adapter instead of racing service registration.
export const inject = ['sessions', 'sessionController', 'credentials']

/** Config schema shown in the DSH settings form. The profile document is the
 * only input. Empty strings are missing and fall back to the schema default.
 * Secret tokens have no default, so an absent token stays unset. */
export const Config = z.object({
  server: z.string().default('').volatile(),
  admin_token: z.string().role('secret').volatile(),
  access_port: z.string().default('').volatile(),
  mqtt_port: z.string().default('').volatile(),
  mqtt_url: z.string().default('').volatile(),
  host_id: z.string().default('').volatile(),
  host_token: z.string().role('secret').volatile(),
  ca_file: z.string().default('').volatile(),
  state_dir: z.string().default('').volatile(),
  adapter: z.string().default('auto').volatile(),
  access_url: z.string().default('').volatile(),
  log_level: z.string().default('info').volatile(),
  allow_insecure_mqtt: z.boolean().default(false).volatile(),
  pending_queue_enabled: z.boolean().default(true).volatile(),
  enable_open_question_broker: z.boolean().default(true).volatile(),
  approval_mode: z.string().default('claim').volatile(),
  approval_ttl_ms: z.number().default(300000).volatile(),
  event_batch_ms: z.number().default(20).volatile(),
  outbox_per_device_limit: z.number().default(100).volatile(),
  outbox_total_limit: z.number().default(1000).volatile(),
  snapshot_max_events: z.number().default(5000).volatile(),
  reconnect_min_ms: z.number().default(500).volatile(),
  reconnect_max_ms: z.number().default(30000).volatile(),
  shutdown_timeout_ms: z.number().default(5000).volatile(),
  proto_path: z.string().default('').volatile(),
})

function configValue(raw, key) {
  const value = raw?.[key]
  return value && typeof value.get === 'function' ? value.get() : value
}

export function materializeConfig(raw) {
  const keys = [
    'server',
    'admin_token',
    'access_port',
    'mqtt_port',
    'mqtt_url',
    'host_id',
    'host_token',
    'ca_file',
    'state_dir',
    'adapter',
    'access_url',
    'log_level',
    'allow_insecure_mqtt',
    'pending_queue_enabled',
    'enable_open_question_broker',
    'approval_mode',
    'approval_ttl_ms',
    'event_batch_ms',
    'outbox_per_device_limit',
    'outbox_total_limit',
    'snapshot_max_events',
    'reconnect_min_ms',
    'reconnect_max_ms',
    'shutdown_timeout_ms',
    'proto_path',
  ]
  const out = {}
  for (const key of keys) {
    const value = configValue(raw, key)
    if (value !== undefined) out[key] = value
  }
  return out
}

function fallbackLog(code, reason) {
  try {
    process.stderr.write(`${JSON.stringify({
      level: 'error',
      event: 'plugin_config_failed',
      code,
      ...(reason ? { reason } : {}),
    })}\n`)
  } catch {
    // nothing else to do without a logger
  }
}

// A generated id must survive a failed profile write. The profile user layer is
// the primary store; this bounded file in the existing state directory is the
// fallback that keeps MQTT identity stable when that write is rejected.
function hostIdFile(stateDir) {
  return join(stateDir, 'host-id')
}

function readPersistedHostId(stateDir) {
  try {
    const value = readFileSync(hostIdFile(stateDir), 'utf8').trim()
    if (!value) return ''
    validateTopicId(value, 'host_id')
    return value
  } catch {
    return ''
  }
}

function persistHostId(stateDir, hostId) {
  try {
    mkdirSync(stateDir, { recursive: true })
    const file = hostIdFile(stateDir)
    const temporary = `${file}.tmp-${process.pid}`
    writeFileSync(temporary, `${hostId}\n`, { mode: 0o600 })
    renameSync(temporary, file)
    return true
  } catch {
    return false
  }
}

/**
 * Resolve config and create a host. The caller owns start/stop; this is the
 * entry used by local harnesses and tests.
 * @param {object} rawConfig plugin config.
 * @param {object} [options] host options (ctx, adapter, store, access, logger, now, codec).
 */
export function createHost(rawConfig, options = {}) {
  const {
    logger: providedLogger,
    logSalt,
    now,
    ...hostOptions
  } = options
  const config = resolveConfig(materializeConfig(rawConfig))
  const logger = providedLogger ?? createLogger({
    salt: logSalt ?? createLogSalt(),
    level: config.logLevel,
    now,
  })
  return createRuntime(config, { ...hostOptions, logger, now })
}

function createRuntime(config, options) {
  const adapter = options.adapter ?? createAdapter({ mode: config.adapter, ctx: options.ctx, config, now: options.now, logger: options.logger })
  const credentials = options.credentials ?? (adapter instanceof FakeDshAdapter
    ? new MemoryHostCredentials({credentialFile: join(config.stateDir, 'fake-credentials.json')})
    : new DshHostCredentials({ctx: options.ctx, hostId: config.hostId}))
  return new HostRuntime({ ...options, config, adapter, credentials })
}

/**
 * Cordis plugin apply. Starts the endpoint in the plugin fiber and stops it on
 * unload/reload, releasing MQTT, timers, and DSH subscriptions.
 */
export function withHostId(ctx, rawConfig) {
  const materialized = materializeConfig(rawConfig)
  if (String(materialized.host_id ?? '').trim()) return materialized
  const stateDir = String(materialized.state_dir ?? '').trim() || join(os.homedir(), '.whalerein', 'dsh')
  const persisted = readPersistedHostId(stateDir)
  const hostId = persisted || generateHostId(os.hostname())
  const next = { ...materialized, host_id: hostId }
  if (!persisted) persistHostId(stateDir, hostId)
  if (ctx && typeof ctx.emit === 'function' && ctx.fiber) {
    try {
      ctx.emit('internal/commit', ctx.fiber, next)
    } catch {
      // The state-directory fallback above still keeps this identity stable on
      // the next start even when the profile user layer rejects the write.
    }
  }
  return next
}

// Name the field a stored-configuration failure belongs to so the settings
// page can explain it instead of looking like an unpaired/offline host.
export function configFailure(error) {
  const code = typeof error?.code === 'string' ? error.code : 'invalid_config'
  const reason = error instanceof ConfigError
    ? error.message.replace(`${error.code}: `, '')
    : undefined
  const names = ['server', 'access_port', 'mqtt_port', 'mqtt_url', 'access_url', 'host_id',
    'host_token', 'adapter', 'approval_mode', 'approval_ttl_ms', 'event_batch_ms',
    'outbox_per_device_limit', 'outbox_total_limit', 'snapshot_max_events',
    'reconnect_min_ms', 'reconnect_max_ms', 'shutdown_timeout_ms', 'ca_file', 'state_dir']
  const field = reason ? names.find(name => reason.startsWith(`${name} `) || reason === name) ?? '' : ''
  return { code, field, reason: reason ?? 'configuration could not be resolved' }
}

export function apply(ctx, rawConfig = {}) {
  let config
  try {
    config = resolveConfig(withHostId(ctx, rawConfig))
  } catch (error) {
    const failure = configFailure(error)
    fallbackLog(failure.code, failure.reason)
    // Keep the settings surface available while the Host is stopped so the
    // page can name the invalid field and still test a corrected draft.
    if (ctx && typeof ctx.get === 'function' && ctx.get('typertGateway')) {
      createSettingsService(ctx, {
        started: false,
        config: { hostId: String(configValue(rawConfig, 'host_id') ?? '') },
        security: null,
        store: null,
        access: null,
        diagnostics: () => null,
        configError: failure,
      })
    }
    return
  }
  const host = createRuntime(config, { ctx })
  if (ctx && typeof ctx.get === 'function' && ctx.get('typertGateway')) {
    createSettingsService(ctx, host)
  }
  const start = () => {
    host.start().catch((error) => {
      fallbackLog(typeof error?.code === 'string' ? error.code : 'internal')
    })
  }
  const dispose = () => host.stop().catch(() => {})
  if (ctx && typeof ctx.effect === 'function') {
    ctx.effect(() => {
      start()
      return dispose
    }, 'whalerein-dsh')
  } else {
    start()
  }
  return host
}

// Public connector entry; Host implementation types are exported by whalerein-host.
export { resolveConfig } from './src/config.js'

export default { name, inject, Config, apply, createHost }
