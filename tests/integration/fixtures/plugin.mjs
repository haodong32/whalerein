import { FixtureAdapter } from './adapter.mjs'
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

const config = {
  adapter: 'fake',
  mqtt_url: process.env.WHALEREIN_MQTT_URL,
  host_id: process.env.WHALEREIN_HOST_ID,
  host_token: process.env.WHALEREIN_HOST_TOKEN,
  state_dir: process.env.WHALEREIN_STATE_DIR,
  allow_insecure_mqtt: true,
  event_batch_ms: 0,
  log_level: process.env.WHALEREIN_LOG_LEVEL || 'warn',
}

const adapter = new FixtureAdapter()

const host = createHost(config, { adapter, credentials: { getCredential: (...args) => adapter.call('getCredential', ...args), setCredential: (...args) => adapter.call('setCredential', ...args) } })
// Keep the process alive while waiting for the first broker connection; the
// wait timer itself is unref'd.
const keepAlive = setInterval(() => {}, 1 << 30)
try {
  await host.start()
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
