import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync, readFileSync, readdirSync, realpathSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { serviceConfiguration, customConfiguration, migrateServiceConfig } from '../src/service-config.js'
import { resolveManagerConfig, resolveConnectionMode } from '../src/config.js'
import { createManagedHost, apply } from '../index.js'
import { HostProfileRegistry } from 'whalerein-host/registry.js'
import { settingsStatus, WhaleReinSettingsService } from '../src/settings.js'
import { Context } from '@deepseek-ai/cordis'

const custom = { label: 'My service', api_origin: 'https://app.example', issuer: 'https://auth.example/oidc', native_client_id: 'whalerein-cli' }
function directory(t) {
  const path = mkdtempSync(join(tmpdir(), 'whalerein-service-'))
  t.after(() => rmSync(path, { recursive: true, force: true }))
  return path
}
const invalid = (fn) => assert.throws(fn, { code: 'invalid_config' })

test('service config accepts exactly three connection types and explicit Cloud environments', () => {
  assert.equal(serviceConfiguration().service, 'whalerein_cloud')
  for (const environment of ['production', 'staging', 'local']) {
    const config = serviceConfiguration({ service: 'whalerein_cloud', cloud_environment: environment })
    assert.equal(config.environment, environment)
    assert.equal(resolveConnectionMode({ service: 'whalerein_cloud', cloud_environment: environment }), 'managed')
  }
  assert.equal(resolveConnectionMode({ service: 'custom', custom }), 'managed')
  assert.equal(resolveConnectionMode({ service: 'standalone' }), 'standalone')
  invalid(() => serviceConfiguration({ service: 'managed' }))
  invalid(() => serviceConfiguration({ service: 'whalerein_cloud', cloud_environment: 'typo' }))
  invalid(() => serviceConfiguration({ service: 'custom' }))
  for (const [key, value] of Object.entries({ environment: 'local', config_file: '/tmp/old', profile_id: 'old', connection_mode: 'managed', developer_tools_enabled: true })) {
    invalid(() => serviceConfiguration({ service: 'whalerein_cloud', [key]: value }))
  }
})

test('custom service config rejects unknown fields, insecure URLs and ambiguous trust', () => {
  const resolved = customConfiguration(custom)
  assert.deepEqual(resolved.tls, { ca_provider: 'system', ca_file: null })
  assert.equal(customConfiguration({ ...custom, label: '' }).label, 'Custom service')
  assert.equal(customConfiguration({ ...custom, tls: { ca_provider: 'file', ca_file: '/tmp/test-ca.pem' } }).tls.ca_file, '/tmp/test-ca.pem')
  for (const value of [null, [], '', { ...custom, mqtt_url: 'mqtts://broker' },
    { ...custom, label: 'bad\nlabel' }, { ...custom, label: 'x'.repeat(65) },
    { ...custom, api_origin: 'http://app.example' }, { ...custom, api_origin: 'invalid' },
    { ...custom, api_origin: ' https://app.example' }, { ...custom, api_origin: 'https://app.example/path' },
    { ...custom, api_origin: 'https://user:pass@app.example' }, { ...custom, api_origin: 'https://app.example?q=x' },
    { ...custom, issuer: 'https://app.example/oidc' }, { ...custom, issuer: 'http://auth.example' },
    { ...custom, native_client_id: '' }, { ...custom, native_client_id: 'bad id' },
    { ...custom, tls: [] }, { ...custom, tls: { unknown: true } },
    { ...custom, tls: { ca_provider: 'mkcert' } }, { ...custom, tls: { ca_provider: 'file' } },
    { ...custom, tls: { ca_provider: 'file', ca_file: 'relative.pem' } },
    { ...custom, tls: { ca_provider: 'system', ca_file: '/tmp/unused' } }]) {
    invalid(() => customConfiguration(value))
  }
})

test('profile config ignores WHALEREIN_CONFIG and never creates a shared file', async (t) => {
  const state = directory(t)
  const shared = join(state, 'shared.toml')
  writeFileSync(shared, 'invalid TOML')
  const calls = []
  const host = await createManagedHost({ service: 'whalerein_cloud', cloud_environment: 'staging', state_root: state }, {
    envVars: { WHALEREIN_CONFIG: shared },
    credentials: {},
    managerFactory: async (options) => {
      calls.push(options)
      return { snapshot: () => ({ state: 'idle', environment: options.environment }) }
    },
  })
  assert.equal(calls[0].environment, 'staging')
  assert.equal(calls[0].configFile, undefined)
  assert.equal(calls[0].profileConfiguration, true)
  assert.equal(readFileSync(shared, 'utf8'), 'invalid TOML')
  assert.deepEqual(readdirSync(state), ['shared.toml'])
  assert.equal(settingsStatus(host).configuration, null)
  await host.stop()
})

test('profile service restarts override registry selection and reuse isolated identities', async (t) => {
  const state = directory(t)
  const credentials = { async getCredential() { return null }, async setCredential() {} }
  async function open(environment) {
    const host = await createManagedHost({ service: 'whalerein_cloud', cloud_environment: environment, state_root: state }, { credentials, source: {} })
    const snapshot = host.manager.snapshot()
    assert.equal(snapshot.environment, environment)
    assert.equal(snapshot.state, 'idle')
    await assert.rejects(host.manager.applyEnvironment('production'), { code: 'unsupported_capability' })
    await assert.rejects(host.manager.updateConfiguration({}), { code: 'unsupported_capability' })
    await host.stop()
    return snapshot.profileId
  }
  const production = await open('production')
  const staging = await open('staging')
  assert.notEqual(production, staging)
  assert.equal(await open('production'), production)
  assert.equal(await open('staging'), staging)
})

test('custom profile selection isolates changed authority and reuses it on return', async (t) => {
  const state = directory(t)
  const credentials = { async getCredential() { return null }, async setCredential() {} }
  async function open(definition) {
    const host = await createManagedHost({ service: 'custom', custom: definition, state_root: state }, { credentials, source: {} })
    try {
      assert.equal(host.manager.snapshot().configError, null)
      return host.manager.snapshot().profileId
    } finally { await host.stop() }
  }
  const first = await open(custom)
  const second = await open({ ...custom, api_origin: 'https://other.example' })
  assert.notEqual(first, second)
  assert.equal(await open({ ...custom, label: 'Renamed' }), first)
})

test('legacy migration writes the full selection once and stops if the profile write fails', (t) => {
  const state = directory(t)
  const commits = []
  const ctx = { fiber: {}, emit: (...args) => commits.push(args) }
  const next = migrateServiceConfig({ state_root: state, environment: 'staging', connection_mode: 'managed' }, ctx)
  assert.equal(next.service, 'whalerein_cloud')
  assert.equal(next.cloud_environment, 'staging')
  assert.equal('environment' in next, false)
  assert.equal('connection_mode' in next, false)
  assert.equal(commits.length, 1)
  assert.equal(migrateServiceConfig(next, ctx), next)
  assert.equal(commits.length, 1)
  assert.throws(() => migrateServiceConfig({ state_root: state }, { fiber: {}, emit() { throw new Error('write failed') } }), /write failed/)
  assert.equal(migrateServiceConfig({ state_root: state, host_token: 'not-logged' }).service, 'standalone')
})

test('legacy migration imports an explicit TOML once and preserves its custom authority', (t) => {
  const state = directory(t)
  const file = join(state, 'legacy.toml')
  const content = 'version = 1\nenvironment = "company"\n[environments.company]\nmode = "account"\napi_origin = "https://app.example"\nissuer = "https://auth.example/oidc"\nnative_client_id = "whalerein-cli"\n'
  writeFileSync(file, content)
  const next = migrateServiceConfig({ state_root: state, config_file: file })
  assert.equal(next.service, 'custom')
  assert.equal(next.custom.api_origin, custom.api_origin)
  assert.equal('config_file' in next, false)
  assert.equal(readFileSync(file, 'utf8'), content)
  writeFileSync(file, 'invalid')
  assert.equal(resolveManagerConfig(next).service, 'custom')
  invalid(() => migrateServiceConfig({ state_root: state, config_file: file }))
})

test('legacy registry selection is imported without changing identity and bad selected IDs fail closed', (t) => {
  const state = directory(t)
  const registry = new HostProfileRegistry({ stateRoot: state })
  registry.ensure()
  const next = migrateServiceConfig({ state_root: state })
  assert.equal(next.cloud_environment, 'production')
  invalid(() => migrateServiceConfig({ state_root: state, profile_id: 'missing' }))
})

test('DSH snapshot exposes no internal environment list or second configuration writer', async () => {
  const ctx = new Context()
  const host = { managed: true, profileConfigured: true, manager: { snapshot: () => ({ state: 'idle', environment: 'staging', profiles: [{ id: 'internal', environment: 'local' }], stagingEnabled: true, developerToolsEnabled: true, capabilities: ['switching', 'custom-service', 'staging', 'removal'] }) } }
  const service = new WhaleReinSettingsService(ctx, host)
  const snapshot = service.snapshot()
  assert.deepEqual(snapshot.profiles, [])
  assert.equal(snapshot.stagingEnabled, false)
  assert.equal(snapshot.developerToolsEnabled, false)
  assert.equal(snapshot.configuration, null)
  assert.deepEqual(snapshot.capabilities, ['removal'])
  for (const method of ['switchProfile', 'enableStaging', 'addCustomService', 'deleteProfile', 'saveDefaultEnvironment', 'applyEnvironment']) assert.equal(service[method], undefined)
  await ctx.fiber.dispose()
})

test('schemastery defaults do not turn an absent custom block into a required service', async () => {
  const { Config, materializeConfig } = await import('../index.js')
  const raw = materializeConfig(Config['~standard'].validate({ service: 'whalerein_cloud' }).value)
  assert.equal(serviceConfiguration(raw).service, 'whalerein_cloud')
  assert.equal(Config.dict.cloud_environment.meta.hidden, true)
})

test('Standalone holds installation activation until its runtime has stopped', async (t) => {
  const { ActivationOwner } = await import('whalerein-host/activation.js')
  const state = directory(t)
  const host = apply({ effect() {} }, { service: 'standalone', adapter: 'fake', state_dir: state, host_id: 'test-host', mqtt_url: 'mqtts://127.0.0.1:1', host_token: 'not-logged' })
  const other = new ActivationOwner({ stateRoot: state })
  try {
    assert.throws(() => other.acquire(), { code: 'conflict' })
    await host.stop()
    assert.equal(other.acquire().owned, true)
  } finally { other.release(); await host.stop() }
})

test('missing scoped trust fails before creating a manager or any identity state', async (t) => {
  const state = directory(t)
  let calls = 0
  const options = { homeDir: state, credentials: {}, managerFactory: async () => { calls += 1 } }
  await assert.rejects(createManagedHost({ service: 'whalerein_cloud', cloud_environment: 'local', state_root: state }, options), { code: 'invalid_config' })
  await assert.rejects(createManagedHost({ service: 'custom', custom: { ...custom, tls: { ca_provider: 'file', ca_file: join(state, 'missing.pem') } }, state_root: state }, options), { code: 'invalid_config' })
  assert.equal(calls, 0)
  assert.deepEqual(readdirSync(state), [])
})

test('browser connection provenance cannot replace the native source adapter', async () => {
  const ctx = new Context()
  const calls = []
  const service = new WhaleReinSettingsService(ctx, { managed: true, manager: {
    snapshot: () => ({ state: 'idle', environment: 'production' }),
    startConnect: (...args) => calls.push(args),
  } })
  await service.startConnect({ source: 'settings' })
  assert.deepEqual(calls, [[]])
  await ctx.fiber.dispose()
})

test('legacy custom migration imports its explicit definition even when the registry has an older descriptor', (t) => {
  const state = directory(t)
  const registry = new HostProfileRegistry({ stateRoot: state })
  registry.ensure()
  const record = registry.addProfile({ label: 'Old custom', environment: 'company', service: { id: 'old-company', environment: 'custom', label: 'Old custom', apiOrigin: 'https://app.example' } })
  registry.prepareActivation(record.id, registry.read().revision)
  registry.commitActivation(record.id, registry.read().revision)
  const file = join(state, 'legacy.toml')
  writeFileSync(file, 'version = 1\nenvironment = "company"\n[environments.company]\nmode = "account"\napi_origin = "https://app.example"\nissuer = "https://auth.example/oidc"\nnative_client_id = "whalerein-cli"\n')
  const next = migrateServiceConfig({ state_root: state, config_file: file })
  assert.equal(next.service, 'custom')
  assert.equal(next.cloud_environment, 'production')
  assert.equal(next.custom.issuer, custom.issuer)
  invalid(() => migrateServiceConfig({ state_root: state }))
  assert.equal(migrateServiceConfig({ state_root: state, connection_mode: 'standalone', config_file: '/missing/unused.toml' }).service, 'standalone')
})

test('legacy file trust is imported relative to the resolved configuration file', (t) => {
  const state = directory(t)
  const file = join(state, 'legacy.toml')
  writeFileSync(file, 'version = 1\nenvironment = "company"\n[environments.company]\nmode = "account"\napi_origin = "https://app.example"\nissuer = "https://auth.example/oidc"\nnative_client_id = "client"\n[environments.company.tls]\nca_provider = "file"\nca_file = "root-ca.pem"\n')
  const next = migrateServiceConfig({ state_root: state, config_file: file })
  assert.equal(next.custom.tls.ca_file, join(realpathSync(state), 'root-ca.pem'))
})

test('Standalone generated identity follows state_root when the legacy state_dir is empty', async (t) => {
  const { withHostId } = await import('../index.js')
  const first = directory(t)
  const second = directory(t)
  const one = withHostId(null, { state_root: first, state_dir: '' }).host_id
  const two = withHostId(null, { state_root: second, state_dir: '' }).host_id
  assert.equal(readFileSync(join(first, 'host-id'), 'utf8').trim(), one)
  assert.equal(readFileSync(join(second, 'host-id'), 'utf8').trim(), two)
  assert.equal(withHostId(null, { state_root: first }).host_id, one)
})
