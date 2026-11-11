// WhaleRein for DSH plugin entry point.
//
// The plugin is a protocol endpoint: it opens exactly one outbound MQTT 5/TLS
// connection, may call Access to mint a pairing URI, and never listens or links Rust.

import os from 'node:os'
import z from '@deepseek-ai/schemastery'
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
})

function configValue(raw, key) {
  const value = raw?.[key]
  return value && typeof value.get === 'function' ? value.get() : value
}

function materializeConfig(raw) {
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
function withHostId(ctx, rawConfig) {
  const materialized = materializeConfig(rawConfig)
  if (String(materialized.host_id ?? '').trim()) return materialized
  const next = { ...materialized, host_id: generateHostId(os.hostname()) }
  if (ctx && typeof ctx.emit === 'function' && ctx.fiber) {
    try {
      ctx.emit('internal/commit', ctx.fiber, next)
    } catch {
      // This process still uses the generated id. The next start generates
      // another one only when the profile is still empty.
    }
  }
  return next
}

export function apply(ctx, rawConfig = {}) {
  let config
  try {
    config = resolveConfig(withHostId(ctx, rawConfig))
  } catch (error) {
    const code = typeof error?.code === 'string' ? error.code : 'invalid_config'
    const reason = error instanceof ConfigError
      ? error.message.replace(`${error.code}: `, '')
      : undefined
    fallbackLog(code, reason)
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
