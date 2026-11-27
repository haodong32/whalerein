// Resolved Host configuration for tests; connector parsing is tested in DSH.
export function testConfig(raw = {}, extra = {}) {
  return {
    hostId: raw.host_id ?? 'host-1',
    hostToken: raw.host_token ?? 'token',
    mqttUrl: raw.mqtt_url ?? 'mqtts://broker.test',
    stateDir: raw.state_dir,
    outboxPerDeviceLimit: raw.outbox_per_device_limit ?? 100,
    outboxTotalLimit: raw.outbox_total_limit ?? 1000,
    shutdownTimeoutMs: raw.shutdown_timeout_ms ?? 5000,
    reconnectMinMs: raw.reconnect_min_ms ?? 500,
    reconnectMaxMs: raw.reconnect_max_ms ?? 30_000,
    logLevel: 'info',
    ...extra,
  }
}
