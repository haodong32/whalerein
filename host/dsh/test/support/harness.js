// DSH composition of the source-independent Host harness.
import { startHarness as startHostHarness } from '../../../runtime/test/support/harness.js'
import { waitFor, pairingTokenHash } from '../../../runtime/test/support/harness.js'
import { FakeDshAdapter } from '../../src/adapter/fake.js'
import { resolveConfig } from '../../src/config.js'
import { silentLogger } from 'whalerein-host/log.js'

export { waitFor, pairingTokenHash }

export function startHarness(options = {}) {
  const now = options.now ?? Date.now
  const adapter = new FakeDshAdapter({ now, sessions: options.sessions ?? [], behavior: options.behavior ?? {}, logger: silentLogger })
  const config = resolveConfig({
    adapter: 'fake',
    mqtt_url: 'mqtts://fake-broker.test:8883',
    host_id: options.hostId ?? 'host-1',
    host_token: options.hostToken ?? 'test-host-token',
    approval_ttl_ms: options.approvalTtlMs ?? 300_000,
    outbox_per_device_limit: options.outboxPerDeviceLimit ?? 100,
    outbox_total_limit: options.outboxTotalLimit ?? 1000,
    snapshot_max_events: options.snapshotMaxEvents ?? 5000,
    ...(options.config ?? {}),
  })
  return startHostHarness({ ...options, now, adapter, config })
}
