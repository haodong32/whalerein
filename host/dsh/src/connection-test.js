// Read-only connectivity and credential checks. The probe principal has a
// random client ID, no topic permissions, and no host provisioning/presence.
import { readFile } from 'node:fs/promises'
import http from 'node:http'
import https from 'node:https'
import net from 'node:net'
import { randomBytes } from 'node:crypto'
import mqtt from 'mqtt'
import { resolveConfig } from './config.js'

const TIMEOUT_MS = 5000
const FIELDS = new Set(['server', 'host_id', 'host_token', 'access_port', 'mqtt_port', 'ca_file', 'log_level'])

function failure(error) {
  const code = error?.code
  if ([4, 5, 0x86, 0x87].includes(code)) return 'credentials'
  if (code === 'ETIMEDOUT' || code === 'ABORT_ERR') return 'timeout'
  if (code === 'ENOTFOUND' || code === 'EAI_AGAIN') return 'dns'
  if (code === 'ECONNREFUSED') return 'refused'
  if (/CERT|TLS|SSL|SELF_SIGNED|VERIFY/.test(String(code))) return 'tls'
  return 'network'
}

export function probeAccess(url, ca, timeout = TIMEOUT_MS) {
  return new Promise((resolve) => {
    let request
    let timer
    let settled = false
    const finish = (result) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      request?.destroy()
      resolve(result)
    }
    try {
      const target = new URL('/readyz', url)
      if (!['http:', 'https:'].includes(target.protocol) || target.username || target.password) {
        finish({ ok: false, code: 'invalid_config' })
        return
      }
      request = (target.protocol === 'https:' ? https : http).get(target, {
        ca, rejectUnauthorized: true, agent: false,
      }, (response) => {
        response.destroy()
        finish(response.statusCode === 200 ? { ok: true } : { ok: false, code: 'not_ready' })
      })
      request.on('error', (error) => finish({ ok: false, code: failure(error) }))
      timer = setTimeout(() => finish({ ok: false, code: 'timeout' }), timeout)
    } catch {
      finish({ ok: false, code: 'invalid_config' })
    }
  })
}

export function probeMqtt(url, ca, timeout = TIMEOUT_MS, credentials, connect = mqtt.connect) {
  return new Promise((resolve) => {
    let client
    let timer
    let settled = false
    const finish = (result) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      if (!client) { resolve(result); return }
      // A bounded forced close cannot leave a probe retrying in the background.
      const cleanup = setTimeout(() => { client.stream?.destroy(); resolve(result) }, 250)
      client.end(true, {}, () => { clearTimeout(cleanup); resolve(result) })
    }
    try {
      const target = new URL(url)
      if (!['mqtt:', 'mqtts:'].includes(target.protocol) || target.username || target.password
        || !credentials?.hostId || !credentials?.token) {
        finish({ ok: false, code: 'invalid_config' })
        return
      }
      const host = target.hostname.replace(/^\[|\]$/g, '')
      client = connect(url, {
        protocolVersion: 5, clientId: `whalerein-probe-${randomBytes(16).toString('hex')}`,
        username: `probe:${credentials.hostId}`, password: credentials.token,
        clean: true, reconnectPeriod: 0, resubscribe: false, connectTimeout: timeout + 1000,
        keepalive: 5, ca, rejectUnauthorized: true,
        servername: net.isIP(host) ? undefined : host,
        properties: { sessionExpiryInterval: 0, requestProblemInformation: false },
      })
      // No subscriptions, publications, HostReady, or Will are ever supplied.
      client.once('connect', () => finish({ ok: true, authenticated: true }))
      client.on('error', (error) => finish({ ok: false, code: failure(error) }))
      client.once('close', () => finish({ ok: false, code: 'network' }))
      timer = setTimeout(() => finish({ ok: false, code: 'timeout' }), timeout)
    } catch {
      finish({ ok: false, code: 'invalid_config' })
    }
  })
}

export async function testConnection(host, draft) {
  if (!draft || typeof draft !== 'object' || Array.isArray(draft)
    || Object.entries(draft).some(([key, value]) => !FIELDS.has(key) || typeof value !== 'string' || value.length > 4096)) {
    return { configuration: { ok: false, code: 'invalid_config' } }
  }
  const base = host?.config ?? {}
  let config
  let ca
  try {
    config = resolveConfig({
      mqtt_url: base.mqttUrl, access_url: base.accessUrl,
      host_id: base.hostId, host_token: base.hostToken,
      allow_insecure_mqtt: base.allowInsecureMqtt ?? false,
      server: base.server, ca_file: base.caFile,
      access_port: base.accessPort, mqtt_port: base.mqttPort,
      ...draft,
      host_id: draft.host_id?.trim() ? draft.host_id : base.hostId,
      host_token: draft.host_token || base.hostToken,
    })
    // Access is an HTTP(S) origin, never a credential-bearing or redirected URL.
    const access = new URL(config.accessUrl)
    if (!['http:', 'https:'].includes(access.protocol) || access.username || access.password
      || (!config.allowInsecureMqtt && access.protocol !== 'https:')) throw new Error('invalid origin')
    if (config.caFile) ca = await readFile(config.caFile)
  } catch {
    return { configuration: { ok: false, code: 'invalid_config' } }
  }
  const [access, mqtt] = await Promise.all([
    probeAccess(config.accessUrl, ca), probeMqtt(config.mqttUrl, ca, TIMEOUT_MS, { hostId: config.hostId, token: config.hostToken }),
  ])
  const matchesRunning = config.mqttUrl === base.mqttUrl && config.accessUrl === base.accessUrl
    && config.hostId === base.hostId && config.hostToken === base.hostToken && config.caFile === base.caFile
  return {
    configuration: { ok: true }, access, mqtt,
    running: !host?.started || !matchesRunning ? 'not_checked' : host.access?.connected ? 'connected' : 'disconnected',
  }
}
