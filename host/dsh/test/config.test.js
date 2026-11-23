import test from 'node:test'
import assert from 'node:assert/strict'
import {
  resolveConfig, resolveConnectionMode, resolveManagerConfig,
  validateTopicId, topics, ConfigError,
} from '../src/config.js'

const base = {
  mqtt_url: 'mqtts://broker.example:8883',
  host_id: 'host-1',
  host_token: 'secret-token',
}

test('when configuration values are supplied or omitted, explicit values and safe defaults are resolved', () => {
  const config = resolveConfig({ ...base, state_dir: '/tmp/state', access_url: 'https://access.example' })
  assert.equal(config.hostId, 'host-1')
  assert.equal(config.mqttUrl, base.mqtt_url)
  assert.equal(config.hostToken, 'secret-token')
  assert.equal(config.stateDir, '/tmp/state')
  assert.equal(config.accessUrl, 'https://access.example')
  assert.equal(config.adapter, 'auto')
  assert.equal(config.eventRecentLimit, 500)
  assert.equal(config.outboxPerDeviceLimit, 100)
  assert.equal(config.outboxTotalLimit, 1000)
  assert.equal(config.approvalMode, 'claim')
  assert.equal(config.pendingQueueEnabled, true)
  assert.equal(config.enableOpenQuestionBroker, true)
  assert.equal(config.protocolVersion, 1)
})

test('when resolving configuration, values come only from the supplied profile document', () => {
  assert.throws(() => resolveConfig({}), ConfigError)
  const config = resolveConfig({
    mqtt_url: 'mqtts://env.example:8883',
    host_id: 'env-host',
    host_token: 'env-token',
    state_dir: '/env/state',
    adapter: 'fake',
    ca_file: '/env/ca.pem',
    log_level: 'debug',
  })
  assert.equal(config.hostId, 'env-host')
  assert.equal(config.mqttUrl, 'mqtts://env.example:8883')
  assert.equal(config.adapter, 'fake')
  assert.equal(config.caFile, '/env/ca.pem')
  assert.equal(config.logLevel, 'debug')
})

test('when mqtt:// is configured, it is accepted only if the plaintext flag is enabled', () => {
  const saved = {
    mqtt_url: 'mqtt://127.0.0.1:1883',
    host_id: 'host-real',
    host_token: 'dev-host-token',
    allow_insecure_mqtt: false,
  }
  assert.throws(() => resolveConfig(saved), /mqtt_url must use mqtts TLS/)
  const config = resolveConfig({ ...saved, allow_insecure_mqtt: true })
  assert.equal(config.allowInsecureMqtt, true)
  assert.equal(config.mqttUrl, 'mqtt://127.0.0.1:1883')
})

test('when configuration lacks required fields or has invalid adapter, topic ID, URL, approval mode or plaintext flag, it rejects invalid_config', () => {
  assert.throws(() => resolveConfig({}), (error) => error instanceof ConfigError && error.code === 'invalid_config')
  assert.throws(() => resolveConfig({ ...base, adapter: 'cloud' }), ConfigError)
  assert.throws(() => resolveConfig({ ...base, host_id: 'bad/id' }), ConfigError)
  assert.throws(() => resolveConfig({ ...base, host_id: 'bad id' }), ConfigError)
  assert.throws(() => resolveConfig({ ...base, mqtt_url: 'https://broker' }), ConfigError)
  assert.throws(() => resolveConfig({ ...base, mqtt_url: 'mqtt://broker' }), (error) => error.code === 'invalid_config')
  assert.equal(resolveConfig({ ...base, mqtt_url: 'mqtt://broker', allow_insecure_mqtt: true }).allowInsecureMqtt, true)
  assert.throws(() => resolveConfig({ ...base, mqtt_url: 'not a url' }), ConfigError)
  assert.throws(() => resolveConfig({ ...base, access_url: 'not a url' }), ConfigError)
  assert.throws(() => resolveConfig({ ...base, approval_mode: 'auto' }), ConfigError)
  assert.throws(() => resolveConfig({ ...base, allow_insecure_mqtt: 'maybe' }), ConfigError)
  assert.throws(() => validateTopicId('', 'host_id'), ConfigError)
  assert.throws(() => validateTopicId('a'.repeat(129), 'host_id'), ConfigError)
  assert.equal(validateTopicId('a.b:c_d-1', 'id'), 'a.b:c_d-1')
})

test('when a server host is set, derived endpoints override explicit URLs', () => {
  const secure = resolveConfig({
    server: 'access.example',
    host_id: 'host-1',
    host_token: 'secret-token',
    admin_token: 'admin',
    mqtt_url: 'mqtts://explicit.example:8883',
    access_url: 'https://explicit.example',
  })
  assert.equal(secure.server, 'access.example')
  assert.equal(secure.adminToken, 'admin')
  assert.equal(secure.accessUrl, 'https://access.example')
  assert.equal(secure.mqttUrl, 'mqtts://access.example:8883')
  assert.equal(secure.accessPort, 443)
  assert.equal(secure.mqttPort, 8883)

  const insecure = resolveConfig({
    server: ' 10.0.0.8 ',
    host_id: 'host-1',
    host_token: 'secret-token',
    allow_insecure_mqtt: true,
  })
  assert.equal(insecure.server, '10.0.0.8')
  assert.equal(insecure.accessUrl, 'http://10.0.0.8:8080')
  assert.equal(insecure.mqttUrl, 'mqtt://10.0.0.8:1883')
  assert.equal(insecure.adminToken, '')

  const ports = resolveConfig({
    server: 'access.example',
    host_id: 'host-1',
    host_token: 'secret-token',
    access_port: '80',
    mqtt_port: '8884',
    allow_insecure_mqtt: true,
  })
  assert.equal(ports.accessUrl, 'http://access.example')
  assert.equal(ports.mqttUrl, 'mqtt://access.example:8884')

  const secureSaved = resolveConfig({
    server: '127.0.0.1',
    host_id: 'host-1',
    host_token: 'secret-token',
    allow_insecure_mqtt: false,
  })
  assert.equal(secureSaved.accessUrl, 'https://127.0.0.1')
  assert.equal(secureSaved.mqttUrl, 'mqtts://127.0.0.1:8883')
})

test('when server values contain a scheme, port, or incomplete host, configuration rejects them', () => {
  assert.throws(() => resolveConfig({ server: 'https://access.example', host_id: 'host-1', host_token: 'secret-token' }), /server must be a host name or IPv4 address/)
  assert.throws(() => resolveConfig({ server: 'access.example:443', host_id: 'host-1', host_token: 'secret-token' }), ConfigError)
  assert.throws(() => resolveConfig({ server: 'access.example', host_id: 'host-1', host_token: 'secret-token', access_port: '0' }), ConfigError)
  assert.throws(() => resolveConfig({ server: '::1', host_id: 'host-1', host_token: 'secret-token' }), ConfigError)
  const explicit = resolveConfig(base)
  assert.equal(explicit.server, '')
  assert.equal(explicit.accessPort, null)
  assert.equal(explicit.mqttPort, null)
  assert.equal(explicit.adminToken, '')
})

test('when resolving topic templates, only the four wire topics are produced', () => {
  const t = topics('host-1')
  assert.equal(t.request, 'whalerein/v1/h/host-1/request')
  assert.equal(t.result, 'whalerein/v1/h/host-1/result')
  assert.equal(t.event, 'whalerein/v1/h/host-1/event')
  assert.equal(t.deviceEvent('device-1'), 'whalerein/v1/h/host-1/d/device-1/event')
})


test('production capability switches default open and accept an explicit opt-out', () => {
  const config = resolveConfig(base)
  assert.equal(config.pendingQueueEnabled, true)
  assert.equal(config.enableOpenQuestionBroker, true)
  const off = resolveConfig({ ...base, pending_queue_enabled: false, enable_open_question_broker: false })
  assert.equal(off.pendingQueueEnabled, false)
  assert.equal(off.enableOpenQuestionBroker, false)
  const on = resolveConfig({ ...base, pending_queue_enabled: 'true', enable_open_question_broker: '1' })
  assert.equal(on.pendingQueueEnabled, true)
  assert.equal(on.enableOpenQuestionBroker, true)
  assert.throws(() => resolveConfig({ ...base, pending_queue_enabled: 'maybe' }), ConfigError)
  assert.throws(() => resolveConfig({ ...base, enable_open_question_broker: 'maybe' }), ConfigError)
})

test('every documented profile key materializes through the shipped Config schema', async () => {
  const { Config, materializeConfig } = await import('../index.js')
  const documented = [
    'server', 'admin_token', 'access_port', 'mqtt_port', 'mqtt_url', 'host_id',
    'host_token', 'ca_file', 'state_dir', 'adapter', 'access_url', 'proto_path',
    'allow_insecure_mqtt', 'pending_queue_enabled', 'enable_open_question_broker',
    'approval_mode', 'approval_ttl_ms', 'event_batch_ms', 'outbox_per_device_limit',
    'outbox_total_limit', 'snapshot_max_events', 'reconnect_min_ms',
    'reconnect_max_ms', 'shutdown_timeout_ms', 'log_level',
  ]
  for (const key of documented) {
    assert.ok(Object.hasOwn(Config.dict, key), `Config schema is missing documented key ${key}`)
  }
  const raw = materializeConfig({
    approval_mode: 'delegate',
    approval_ttl_ms: 1000,
    event_batch_ms: 3,
    outbox_per_device_limit: 7,
    outbox_total_limit: 8,
    snapshot_max_events: 9,
    reconnect_min_ms: 151,
    reconnect_max_ms: 252,
    shutdown_timeout_ms: 153,
    proto_path: '/tmp/descriptor.json',
    ignored_key: 'drop',
  })
  assert.equal('ignored_key' in raw, false)
  const config = resolveConfig({ ...base, ...raw })
  assert.equal(config.approvalMode, 'delegate')
  assert.equal(config.approvalTtlMs, 1000)
  assert.equal(config.eventBatchMs, 3)
  assert.equal(config.outboxPerDeviceLimit, 7)
  assert.equal(config.outboxTotalLimit, 8)
  assert.equal(config.snapshotMaxEvents, 9)
  assert.equal(config.reconnectMinMs, 151)
  assert.equal(config.reconnectMaxMs, 252)
  assert.equal(config.shutdownTimeoutMs, 153)
  assert.equal(config.protoPath, '/tmp/descriptor.json')
  assert.equal(resolveConfig(base).eventBatchMs, 20)
  assert.throws(() => resolveConfig({ ...base, event_batch_ms: 0 }), ConfigError)
})


test('service alone selects the connection mode; manual fields never change the default', () => {
  assert.equal(resolveConnectionMode({}), 'managed')
  for (const raw of [{ server: 'access.example' }, { mqtt_url: 'mqtts://broker.example' }, { host_token: 'token' }]) {
    assert.equal(resolveConnectionMode(raw), 'managed')
  }
  assert.equal(resolveConnectionMode({ service: 'standalone' }), 'standalone')
  assert.throws(() => resolveConnectionMode({ connection_mode: 'standalone' }), ConfigError)
})

test('managed options validate only the profile-owned connection and runtime parameters', () => {
  const defaults = resolveManagerConfig({})
  assert.equal(defaults.service, 'whalerein_cloud')
  assert.equal(defaults.cloudEnvironment, 'production')
  assert.ok(defaults.stateRoot.endsWith('/.whalerein/dsh'))
  const configured = resolveManagerConfig({ state_root: '/var/lib/whalerein', adapter: 'fake', log_level: 'debug' })
  assert.equal(configured.stateRoot, '/var/lib/whalerein')
  assert.equal(configured.adapter, 'fake')
  assert.equal(configured.logLevel, 'debug')
  assert.throws(() => resolveManagerConfig({ state_root: 'bad\0path' }), ConfigError)
  assert.throws(() => resolveManagerConfig({ adapter: 'cloud' }), ConfigError)
  assert.equal(resolveManagerConfig({ state_root: '' }).stateRoot, defaults.stateRoot)
  assert.equal(resolveManagerConfig({ state_root: '  /tmp/state  ' }).stateRoot, '/tmp/state')
})

test('Standalone resolves its parameters without a separate profile selector', () => {
  const config = resolveConfig({ ...base, service: 'standalone', state_root: '/tmp/root' })
  assert.equal(config.connectionMode, 'standalone')
  assert.equal(config.stateRoot, '/tmp/root')
  assert.equal('profileId' in config, false)
  assert.equal('developerToolsEnabled' in config, false)
})

test('the managed config accepts one pinned issuer key and issuer origin', () => {
  const publicKey = Buffer.alloc(32, 7).toString('base64url')
  const resolved = resolveManagerConfig({ enrollment_issuer_public_key: publicKey, enrollment_issuer: 'https://issuer.example' })
  assert.equal(resolved.issuerPublicKey, publicKey)
  assert.equal(resolved.expectedIssuer, 'https://issuer.example')
  assert.throws(() => resolveManagerConfig({ enrollment_issuer_public_key: 'not-a-key' }), (error) => error.code === 'invalid_config')
  assert.throws(() => resolveManagerConfig({ enrollment_issuer: 'issuer' }), (error) => error.code === 'invalid_config')
})

test('the DSH adapter keeps no catalog, descriptor loader or TOML writer', async () => {
  const { readFileSync } = await import('node:fs')
  const root = new URL('../', import.meta.url)
  const files = ['src/config.js', 'src/settings.js', 'index.js', 'bin/profile-patch.py']
  for (const file of files) {
    const text = readFileSync(new URL(file, root), 'utf8')
    assert.equal(text.includes('WHALEREIN_LOCAL_ENVIRONMENT'), false, `${file} must not read the Cloud descriptor variable`)
    assert.equal(text.includes('local_environment_file'), false, `${file} must not carry the Cloud descriptor reference`)
    assert.equal(text.includes('official-services.json'), false, `${file} must not own an endpoint catalog`)
  }
  const settings = readFileSync(new URL('src/settings.js', root), 'utf8')
  assert.equal(settings.includes("writeFileSync"), false)
  assert.equal(settings.includes('configurationView'), false, 'DSH does not expose a second configuration view')
  const config = readFileSync(new URL('src/config.js', root), 'utf8')
  assert.equal(config.includes('app.whalerein'), false)
  assert.equal(config.includes('staging.whalerein'), false)
})

test('the Standalone runtime resolver refuses another explicitly selected service', () => {
  for (const service of ['whalerein_cloud', 'custom', 'invalid']) {
    assert.throws(() => resolveConfig({ ...base, service }), { code: 'invalid_config' })
  }
  assert.equal(resolveConfig({ ...base, service: 'standalone' }).connectionMode, 'standalone')
})

test('a managed profile accepts a bounded display label without affecting identity', () => {
  const config = resolveManagerConfig({ service: 'whalerein_cloud', state_root: '/tmp/whalerein-label', host_label: 'Work Mac' })
  assert.equal(config.instanceLabel, 'Work Mac')
  assert.throws(
    () => resolveManagerConfig({ service: 'whalerein_cloud', state_root: '/tmp/whalerein-label', host_label: 'x'.repeat(65) }),
    /host_label/
  )
})
