import test from 'node:test'
import assert from 'node:assert/strict'
import { resolveConfig, validateTopicId, topics, ConfigError } from '../src/config.js'

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
