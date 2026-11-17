import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createConnectionManager } from '../src/manager.js'
import { emptyConfiguration } from '../src/configuration/document.js'
import { MemoryHostCredentials } from '../src/security/credentials.js'

function fixture(t) {
  const stateRoot = mkdtempSync(join(tmpdir(), 'whalerein-profile-source-'))
  t.after(() => rmSync(stateRoot, { recursive: true, force: true }))
  const configSnapshot = { path: join(stateRoot, 'profile'), sourceKind: 'profile', document: emptyConfiguration() }
  return { stateRoot, configSnapshot, credentials: new MemoryHostCredentials(), profileConfiguration: true }
}

async function manager(t, options) {
  const value = createConnectionManager(options)
  t.after(() => value.dispose())
  await value.ready
  return value
}

test('profile-owned configuration never follows a file and refuses every alternate selector', async (t) => {
  const options = fixture(t)
  const configFile = join(options.stateRoot, 'unrelated.toml')
  writeFileSync(configFile, 'invalid TOML')
  const value = await manager(t, { ...options, configFile, environment: 'staging' })
  assert.equal(value.snapshot().environment, 'staging')
  assert.equal(value.configurationView().selected_environment, 'staging')
  assert.equal(value.configurationView().source_kind, 'profile')
  assert.equal(value.environmentTrust().provider, 'system')
  assert.equal(readFileSync(configFile, 'utf8'), 'invalid TOML')
  for (const [name, argument] of [
    ['switchProfile', 'production'], ['enableStaging'], ['addCustomService', {}],
    ['deleteProfile', 'unused'], ['updateConfiguration', {}], ['applyEnvironment', 'production'],
  ]) await assert.rejects(value[name](argument), { code: 'unsupported_capability' })
})

test('profile-owned configuration requires a supplied snapshot before creating state', (t) => {
  const options = fixture(t)
  assert.throws(() => createConnectionManager({ ...options, configSnapshot: null }), { code: 'invalid_argument' })
})

test('invalid profile resolution cannot initialize or reconnect the old authority', async (t) => {
  const options = fixture(t)
  let calls = 0
  const value = await manager(t, { ...options, environment: 'missing', source: {}, fetch: async () => { calls += 1 } })
  assert.equal(value.snapshot().state, 'action_required')
  assert.equal(value.snapshot().connected, false)
  await assert.rejects(value.startConnect(), { code: 'invalid_argument' })
  await assert.rejects(value.reconnect(), { code: 'invalid_argument' })
  assert.equal(calls, 0)
})

test('profile authority matching reuses identical definitions and isolates changed authority', async (t) => {
  const options = fixture(t)
  function configuration(name, api = 'https://app.example') {
    const document = emptyConfiguration()
    document.environments[name] = {
      id: name, mode: 'account', label: 'Company', api_origin: api,
      issuer: 'https://auth.example/oidc', native_client_id: 'company-client', environment: null,
      tls: { ca_provider: 'system', ca_file: null },
    }
    return { ...options.configSnapshot, document }
  }
  const first = await manager(t, { ...options, environment: 'company', configSnapshot: configuration('company') })
  const identity = first.snapshot().profileId
  await first.dispose()
  const reopened = await manager(t, { ...options, environment: 'company', configSnapshot: configuration('company') })
  assert.equal(reopened.snapshot().profileId, identity)
  assert.equal(reopened.environmentTrust().environment, 'company')
  await reopened.dispose()
  const other = await manager(t, { ...options, environment: 'other-company', configSnapshot: configuration('other-company', 'https://other.example') })
  assert.notEqual(other.snapshot().profileId, identity)
})
