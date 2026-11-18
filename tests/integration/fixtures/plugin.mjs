import { FixtureAdapter } from './adapter.mjs'
import fs from 'node:fs'
import path from 'node:path'
import { codec } from '../../../host/runtime/src/protocol.js'
// Start the real WhaleRein DSH plugin runtime with FakeDSH for the integration
// harness. All settings come from WHALEREIN_* environment variables:
//
//   WHALEREIN_MQTT_URL=mqtt://127.0.0.1:1883
//   WHALEREIN_HOST_ID=host-integration-1
//   WHALEREIN_HOST_TOKEN=<host provisioning token>
//   WHALEREIN_STATE_DIR=.data/integration/plugin-state
//   WHALEREIN_ALLOW_INSECURE_MQTT=1   (loopback integration only)
//
// The plugin entry point is owned by host/dsh; this file only wires it for
// the harness. It exits nonzero when the runtime cannot start.

import { createHost } from '../../../host/dsh/index.js'

const approvalMode = process.env.WHALEREIN_APPROVAL_MODE === 'delegate' ? 'delegate' : 'claim'
const approvalTtlMs = Number(process.env.WHALEREIN_APPROVAL_TTL_MS || 0)
const config = {
  adapter: 'fake',
  mqtt_url: process.env.WHALEREIN_MQTT_URL,
  host_id: process.env.WHALEREIN_HOST_ID,
  host_token: process.env.WHALEREIN_HOST_TOKEN,
  state_dir: process.env.WHALEREIN_STATE_DIR,
  allow_insecure_mqtt: process.env.WHALEREIN_ALLOW_INSECURE_MQTT !== '0',
  ca_file: process.env.WHALEREIN_CA_FILE || '',
  event_batch_ms: 1,
  approval_mode: approvalMode,
  ...(Number.isSafeInteger(approvalTtlMs) && approvalTtlMs > 0 ? { approval_ttl_ms: approvalTtlMs } : {}),
  pending_queue_enabled: process.env.WHALEREIN_PENDING_QUEUE_ENABLED !== 'false',
  log_level: process.env.WHALEREIN_LOG_LEVEL || 'warn',
}

const adapter = new FixtureAdapter()
// FixtureAdapter inherits from DshAdapter, which has no `behavior` object. Keep
// the behavior mirror for parity with FakeDshAdapter and set the property the
// proxy's describe/readPendingQueue/updatePendingQueue actually read.
adapter.behavior = { ...(adapter.behavior ?? {}), pendingQueueEnabled: config.pending_queue_enabled }
adapter.pendingQueueEnabled = config.pending_queue_enabled
adapter.approvalMode = approvalMode

// The FakeDSH authority advertises every capability it was built with. The
// Host-visible capability set follows this Host process's configuration, as
// the real adapter does: delegate mode withholds remote approvals, and the
// pending-queue opt-out removes the four queue capabilities.
const capabilityOverrides = {
  approvals: approvalMode !== 'delegate',
  pending_queue_read: config.pending_queue_enabled !== false,
  pending_queue_edit: config.pending_queue_enabled !== false,
  pending_queue_remove: config.pending_queue_enabled !== false,
  pending_queue_steer: config.pending_queue_enabled !== false,
}
const listSessions = adapter.listSessions.bind(adapter)
adapter.listSessions = async options => {
  const sessions = await listSessions(options)
  for (const session of sessions) {
    if (session?.sourceCapabilities) Object.assign(session.sourceCapabilities, capabilityOverrides)
  }
  return sessions
}

const host = createHost(config, { adapter, credentials: { getCredential: (...args) => adapter.call('getCredential', ...args), setCredential: (...args) => adapter.call('setCredential', ...args) } })
// Keep the process alive while waiting for the first broker connection; the
// wait timer itself is unref'd.
const keepAlive = setInterval(() => {}, 1 << 30)
try {
  await host.start()
  // Isolated fixture fault: fail exactly one terminal success journal write.
  const updateCommand = host.store.setCommandStatus.bind(host.store)
  host.store.setCommandStatus = fields => {
    const marker = path.join(process.env.WHALEREIN_STATE_DIR, 'gx-fail-terminal')
    if (fields.status === 'succeeded' && fs.existsSync(marker)) {
      fs.unlinkSync(marker)
      throw new Error('synthetic GX journal failure')
    }
    return updateCommand(fields)
  }
  // Drop exactly one terminal command result after its journal commit.
  const publishResult = host.access.publishResult.bind(host.access)
  const protocol = codec()
  host.access.publishResult = async bytes => {
    const marker = path.join(process.env.WHALEREIN_STATE_DIR, 'gx-drop-terminal')
    const result = protocol.decode('whalerein.v1.HostResult', bytes)
    // Test-only metadata for exact encrypted response budget assertions; no body is persisted.
    const budgetMarker = path.join(process.env.WHALEREIN_STATE_DIR, 'test-queue-wire-size')
    if (fs.existsSync(budgetMarker) && !result.command_id) {
      fs.writeFileSync(budgetMarker, JSON.stringify({size:bytes.length,status:result.status,bodySize:result.result_body?.length ?? 0}))
    }
    if (result.command_id && result.status === 'succeeded' && fs.existsSync(marker)) {
      fs.unlinkSync(marker)
      return
    }
    // Hold every published command result (acceptance and terminal) while the
    // marker exists. The Host journal still records the outcome, so recovery
    // can prove it without another source invocation.
    const holdResults = path.join(process.env.WHALEREIN_STATE_DIR, 'gx-hold-command-results')
    if (result.command_id && fs.existsSync(holdResults)) return
    // Test-only expiry of one completed read-only request after a valid slice.
    const waitExpiry = path.join(process.env.WHALEREIN_STATE_DIR, 'test-wait-expire-result')
    if (fs.existsSync(waitExpiry) && result.request_id && result.status === 'succeeded' && !result.command_id) {
      fs.unlinkSync(waitExpiry)
      result.status = 'failed'
      result.error_code = 'expired'
      result.error_message = 'expired'
      result.result_body = new Uint8Array()
      return publishResult(protocol.encode('whalerein.v1.HostResult', result))
    }
    return publishResult(bytes)
  }

  // Do not call the host ready before its broker connection and subscription
  // exist; the pairing flow requires broker presence.
  await host.access.waitForConnected(30_000)
  const deadline = Date.now() + 30_000
  while (!host.access.connected || host.security.revocationsPending) {
    if (Date.now() >= deadline) throw new Error('host readiness timed out')
    await new Promise(resolve => setTimeout(resolve, 25))
  }
  clearInterval(keepAlive)
  console.log('plugin ready')
} catch (error) {
  console.error(`plugin failed to start: ${error?.code || error?.message || error}`)
  process.exit(3)
}

let stopping = false
async function shutdown() {
  if (stopping) return
  stopping = true
  try {
    await host.stop()
  } finally {
    process.exit(0)
  }
}
process.on('SIGTERM', shutdown)
process.on('SIGINT', shutdown)
setInterval(() => {}, 1 << 30)
