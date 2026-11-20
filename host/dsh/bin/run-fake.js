#!/usr/bin/env node
import { MemoryHostCredentials } from 'whalerein-host/security/credentials.js'
// Standalone FakeDSH host for local integration runs against a real Access and
// MQTT broker. It opens no HTTP port; the only network connection is MQTT 5/TLS.
//
// Required env: WHALEREIN_MQTT_URL, WHALEREIN_HOST_ID, WHALEREIN_HOST_TOKEN
// Optional env: WHALEREIN_CA_FILE, WHALEREIN_STATE_DIR, WHALEREIN_FAKE_SEED,
//   WHALEREIN_FAKE_APPROVAL_ON_FIRST_MESSAGE=1,
//   WHALEREIN_FAKE_LONG_RUNNING=1, WHALEREIN_FAKE_OUTPUT_ON_MESSAGE=0

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { resolveConfig } from '../src/config.js'
import { HostRuntime } from 'whalerein-host/host.js'
import { FakeDshAdapter } from '../src/adapter/fake.js'

function fail(message) {
  process.stderr.write(`${JSON.stringify({ level: 'error', event: 'fake_host_failed', message })}\n`)
  process.exit(1)
}

function configFromEnv(env) {
  const names = [
    'server', 'admin_token', 'access_port', 'mqtt_port', 'mqtt_url', 'host_id', 'host_token',
    'ca_file', 'state_dir', 'adapter', 'access_url', 'log_level', 'allow_insecure_mqtt',
  ]
  const raw = {}
  for (const name of names) {
    const value = env[`WHALEREIN_${name.toUpperCase()}`]
    if (value === undefined || value === '') continue
    raw[name] = name === 'allow_insecure_mqtt' ? value === '1' || value === 'true' : value
  }
  return raw
}

function envFlag(name, fallback) {
  const value = process.env[name]
  if (value === undefined || value === '') return fallback
  return value === '1' || value === 'true'
}

function loadSeed(path) {
  if (!path) return { sessions: [], behavior: {} }
  try {
    const parsed = JSON.parse(readFileSync(path, 'utf8'))
    if (Array.isArray(parsed)) return { sessions: parsed, behavior: {} }
    return {
      sessions: Array.isArray(parsed.sessions) ? parsed.sessions : [],
      behavior: parsed.behavior && typeof parsed.behavior === 'object' ? parsed.behavior : {},
    }
  } catch (error) {
    fail(`seed file unreadable: ${error.code ?? 'read_failed'}`)
    return null
  }
}

async function main() {
  if (process.argv.includes('--help') || process.argv.includes('-h')) {
    process.stdout.write('Usage: WHALEREIN_MQTT_URL=... WHALEREIN_HOST_ID=... WHALEREIN_HOST_TOKEN=... whalerein-dsh-fake\n')
    return
  }
  let config
  try {
    config = resolveConfig(configFromEnv(process.env))
  } catch (error) {
    fail(`configuration invalid: ${error.code ?? 'invalid_config'}`)
    return
  }
  if (config.adapter === 'auto') config.adapter = 'fake'
  const seed = loadSeed(process.env.WHALEREIN_FAKE_SEED)
  const behavior = {
    approvalOnFirstMessage: envFlag('WHALEREIN_FAKE_APPROVAL_ON_FIRST_MESSAGE', false),
    longRunning: envFlag('WHALEREIN_FAKE_LONG_RUNNING', false),
    emitOutputOnMessage: envFlag('WHALEREIN_FAKE_OUTPUT_ON_MESSAGE', true),
    ...seed.behavior,
  }
  const adapter = new FakeDshAdapter({
    sessions: seed.sessions,
    behavior,
    hostId: config.hostId,
    credentialFile: join(config.stateDir, 'fake-credentials.json'),
  })
  // The host creates a logger whose diagnostic salt is persisted in SQLite.
  const host = new HostRuntime({ config, adapter, credentials: new MemoryHostCredentials({credentialFile: join(config.stateDir, 'fake-credentials.json')}) })
  try {
    await host.start()
    await host.access.waitForConnected(15_000)
  } catch (error) {
    process.stderr.write(`${JSON.stringify({
      level: 'warn',
      event: 'fake_host_connect_pending',
      code: typeof error?.code === 'string' ? error.code : 'unavailable',
    })}\n`)
    // The plugin keeps reconnecting; the process stays alive for Access to
    // observe the host coming online later.
  }
  process.stdout.write(`${JSON.stringify({
    event: 'whalerein_dsh_fake_started',
    host_id: config.hostId,
    mqtt_connected: Boolean(host.access?.describe().connected),
    sessions: seed.sessions.map((session) => session.sessionId).filter(Boolean),
    approvals_on_first_message: behavior.approvalOnFirstMessage,
    long_running: behavior.longRunning,
  })}\n`)

  let stopping = false
  const shutdown = async () => {
    if (stopping) return
    stopping = true
    try {
      await host.stop()
    } finally {
      process.exit(0)
    }
  }
  process.on('SIGINT', shutdown)
  process.on('SIGTERM', shutdown)
}

main().catch((error) => {
  fail(`unexpected failure: ${error?.code ?? 'internal'}`)
})
