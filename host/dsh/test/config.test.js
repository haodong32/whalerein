import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  resolveConfig, resolveConnectionMode, resolveManagerConfig, hasStandaloneConfig,
  validateTopicId, topics, ConfigError,
} from '../src/config.js'

const base = {
  mqtt_url: 'mqtts://broker.example:8883',
  host_id: 'host-1',
  host_token: 'secret-token',
}

test('when configuration values are supplied or omitted, explicit values and safe defaults are resolved', () => {
  const config = resolveConfig({ ...base, state_dir: '/tmp/state', access_url: 'https://access.example' }, {})
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
  assert.throws(() => resolveConfig({}, {}), (error) => error instanceof ConfigError && error.code === 'invalid_config')
  assert.throws(() => resolveConfig({ ...base, adapter: 'cloud' }, {}), ConfigError)
  assert.throws(() => resolveConfig({ ...base, host_id: 'bad/id' }, {}), ConfigError)
  assert.throws(() => resolveConfig({ ...base, host_id: 'bad id' }, {}), ConfigError)
  assert.throws(() => resolveConfig({ ...base, mqtt_url: 'https://broker' }, {}), ConfigError)
  assert.throws(() => resolveConfig({ ...base, mqtt_url: 'mqtt://broker' }, {}), (error) => error.code === 'invalid_config')
  assert.equal(resolveConfig({ ...base, mqtt_url: 'mqtt://broker', allow_insecure_mqtt: true }, {}).allowInsecureMqtt, true)
  assert.throws(() => resolveConfig({ ...base, mqtt_url: 'not a url' }, {}), ConfigError)
  assert.throws(() => resolveConfig({ ...base, access_url: 'not a url' }, {}), ConfigError)
  assert.throws(() => resolveConfig({ ...base, approval_mode: 'auto' }, {}), ConfigError)
  assert.throws(() => resolveConfig({ ...base, allow_insecure_mqtt: 'maybe' }, {}), ConfigError)
  assert.throws(() => validateTopicId('', 'host_id'), ConfigError)
  assert.throws(() => validateTopicId('a'.repeat(129), 'host_id'), ConfigError)
  assert.equal(validateTopicId('a.b:c_d-1', 'id'), 'a.b:c_d-1')
})

test('when a server host is set, derived endpoints override legacy URLs', () => {
  const secure = resolveConfig({
    server: 'access.example',
    host_id: 'host-1',
    host_token: 'secret-token',
    admin_token: 'admin',
    mqtt_url: 'mqtts://legacy.example:8883',
    access_url: 'https://legacy.example',
  }, {})
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
  }, {})
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
  }, {})
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
  assert.throws(() => resolveConfig({ server: 'https://access.example', host_id: 'host-1', host_token: 'secret-token' }, {}), /server must be a host name or IPv4 address/)
  assert.throws(() => resolveConfig({ server: 'access.example:443', host_id: 'host-1', host_token: 'secret-token' }, {}), ConfigError)
  assert.throws(() => resolveConfig({ server: 'access.example', host_id: 'host-1', host_token: 'secret-token', access_port: '0' }, {}), ConfigError)
  assert.throws(() => resolveConfig({ server: '::1', host_id: 'host-1', host_token: 'secret-token' }, {}), ConfigError)
  const legacy = resolveConfig(base, {})
  assert.equal(legacy.server, '')
  assert.equal(legacy.accessPort, null)
  assert.equal(legacy.mqttPort, null)
  assert.equal(legacy.adminToken, '')
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


test('auto mode preserves existing manual profiles and selects managed only for a fresh profile', () => {
  assert.equal(resolveConnectionMode({}), 'managed')
  assert.equal(resolveConnectionMode({ host_id: 'host-1' }), 'managed')
  assert.equal(hasStandaloneConfig({}), false)
  assert.equal(resolveConnectionMode({ server: 'access.example' }), 'standalone')
  assert.equal(resolveConnectionMode({ mqtt_url: 'mqtts://broker.example:8883' }), 'standalone')
  assert.equal(resolveConnectionMode({ host_token: 'token' }), 'standalone')
  assert.equal(resolveConnectionMode({ connection_mode: 'managed', server: 'access.example' }), 'managed')
  assert.equal(resolveConnectionMode({ connection_mode: 'standalone' }), 'standalone')
  assert.throws(() => resolveConnectionMode({ connection_mode: 'cloud' }), (error) => error.code === 'invalid_config')
})

test('managed options validate a state root, opaque selected profile and developer tools without connection secrets', () => {
  const defaults = resolveManagerConfig({})
  assert.equal(defaults.connectionMode, 'managed')
  assert.equal(defaults.profileId, '')
  assert.equal(defaults.developerToolsEnabled, false)
  assert.ok(defaults.stateRoot.endsWith('/.whalerein/dsh'))
  const configured = resolveManagerConfig({
    state_root: '/var/lib/whalerein',
    profile_id: 'profile-1',
    developer_tools_enabled: true,
    adapter: 'fake',
    log_level: 'debug',
  })
  assert.equal(configured.stateRoot, '/var/lib/whalerein')
  assert.equal(configured.stateDir, '/var/lib/whalerein')
  assert.equal(configured.profileId, 'profile-1')
  assert.equal(configured.developerToolsEnabled, true)
  assert.equal(configured.adapter, 'fake')
  assert.equal(configured.logLevel, 'debug')
  assert.throws(() => resolveManagerConfig({ state_root: 'bad\0path' }), ConfigError)
  assert.equal(resolveManagerConfig({ state_root: '' }).stateRoot, resolveManagerConfig({}).stateRoot)
  assert.throws(() => resolveManagerConfig({ profile_id: 'bad id' }), ConfigError)
  assert.throws(() => resolveManagerConfig({ profile_id: 'a'.repeat(129) }), ConfigError)
  assert.throws(() => resolveManagerConfig({ adapter: 'cloud' }), ConfigError)
  assert.throws(() => resolveManagerConfig({ developer_tools_enabled: 'maybe' }), ConfigError)
  assert.equal(resolveManagerConfig({ state_root: '  /tmp/state  ' }).stateRoot, '/tmp/state')
  assert.equal(resolveManagerConfig({ state_dir: '/tmp/legacy-state' }).stateRoot, '/tmp/legacy-state')
})

test('standalone configuration carries the additive manager options without changing existing values', () => {
  const config = resolveConfig({
    ...base,
    state_root: '/tmp/root',
    profile_id: 'profile-1',
    developer_tools_enabled: true,
  })
  assert.equal(config.connectionMode, 'standalone')
  assert.equal(config.stateRoot, '/tmp/root')
  assert.equal(config.profileId, 'profile-1')
  assert.equal(config.developerToolsEnabled, true)
  assert.equal(resolveConfig(base).connectionMode, 'standalone')
  assert.equal(resolveConfig(base).developerToolsEnabled, false)
})

test('the managed config accepts one pinned issuer key and issuer origin', () => {
  const publicKey = Buffer.alloc(32, 7).toString('base64url')
  const resolved = resolveManagerConfig({ enrollment_issuer_public_key: publicKey, enrollment_issuer: 'https://issuer.example' })
  assert.equal(resolved.issuerPublicKey, publicKey)
  assert.equal(resolved.expectedIssuer, 'https://issuer.example')
  assert.throws(() => resolveManagerConfig({ enrollment_issuer_public_key: 'not-a-key' }), (error) => error.code === 'invalid_config')
  assert.throws(() => resolveManagerConfig({ enrollment_issuer: 'issuer' }), (error) => error.code === 'invalid_config')
})

test('the managed configuration accepts a config file and a resolved per-instance override', () => {
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-dsh-config-'))
  try {
    const configFile = join(directory, 'config.toml')
    writeFileSync(configFile, 'version = 1\nenvironment = "company"\n\n[environments.company]\nlabel = "Company"\nmode = "account"\napi_origin = "https://cloud.example.com"\nissuer = "https://auth.example.com/oidc"\nclient_id = "whalerein-cli"\n'.replace('client_id', 'native_client_id'))
    const resolved = resolveManagerConfig({ config_file: configFile, environment: 'company', state_root: '/tmp/state' }, {})
    assert.equal(resolved.configFile, configFile)
    assert.equal(resolved.environment, 'company')
    assert.equal(resolved.stateRoot, '/tmp/state')
    assert.equal(resolved.runtimeConfig.caFile, undefined)
    assert.throws(() => resolveManagerConfig({ environment: 'Invalid Name' }), (error) => error.code === 'invalid_config')
    assert.throws(() => resolveManagerConfig({ config_file: configFile, environment: 'unknown' }, {}), (error) => error.code === 'invalid_config')
    assert.throws(() => resolveManagerConfig({ config_file: '/missing/config.toml', environment: 'production' }, {}), (error) => error.code === 'invalid_config')
    assert.throws(() => resolveManagerConfig({ config_file: 'bad\0path' }, {}), (error) => error.code === 'invalid_config')
    assert.equal(resolveManagerConfig({ environment: '' }).environment, '')
    assert.equal(resolveManagerConfig({ config_file: '' }).configFile, undefined)
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
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
  assert.equal(settings.includes('configurationView'), true, 'settings only projects the Host configuration view')
  const config = readFileSync(new URL('src/config.js', root), 'utf8')
  assert.equal(config.includes('app.whalerein'), false)
  assert.equal(config.includes('staging.whalerein'), false)
})
