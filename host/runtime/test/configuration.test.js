import assert from 'node:assert/strict'
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import {
  INITIAL_CONFIGURATION,
  parseConfiguration,
  open,
  resolveEnvironment,
  update,
  view,
  configError,
} from '../src/configuration/index.js'

const here = dirname(fileURLToPath(import.meta.url))
const runtime = join(here, '..')
const contract = join(runtime, '..', '..', 'contracts', 'client-environment')
const corpus = JSON.parse(readFileSync(join(contract, 'cases.json'), 'utf8'))

function temp(t) {
  const root = mkdtempSync(join(tmpdir(), 'whalerein-config-'))
  t.after(() => rmSync(root, { recursive: true, force: true }))
  return root
}

function openFixture(relative, options = {}) {
  return open({ configFile: join(contract, relative), inspectionOnly: true, ...options })
}

test('config-env/AC-05/valid-corpus normalized documents match the shared contract', () => {
  for (const entry of corpus.cases.filter((item) => item.kind === 'valid')) {
    const snapshot = openFixture(entry.file)
    assert.deepEqual(snapshot.document, entry.document, entry.id)
  }
})

test('config-env/AC-05/invalid-corpus returns the exact stable code without rewriting', () => {
  for (const entry of corpus.cases.filter((item) => item.kind === 'invalid')) {
    const path = join(contract, entry.file)
    const before = readFileSync(path)
    assert.throws(() => open({ configFile: path, inspectionOnly: true }), { code: entry.error }, entry.id)
    assert.deepEqual(readFileSync(path), before, entry.id)
  }
})

test('config-env/AC-05/resolution-corpus matches the shared contract exactly', () => {
  for (const entry of corpus.cases.filter((item) => item.kind === 'resolve')) {
    const snapshot = openFixture(entry.file)
    assert.throws(() => resolveEnvironment(snapshot, entry.override ?? null), { code: entry.error }, entry.id)
  }
  for (const entry of corpus.resolution) {
    const snapshot = openFixture(entry.file)
    if (entry.error) {
      assert.throws(() => resolveEnvironment(snapshot, entry.override ?? null), { code: entry.error }, entry.id)
      continue
    }
    const resolution = resolveEnvironment(snapshot, entry.override ?? null)
    assert.deepEqual(
      {
        environment: resolution.environment,
        selection_source: resolution.selection_source,
        definition_source: resolution.definition_source,
        definition: resolution.definition,
      },
      entry.expect,
      entry.id,
    )
  }
})

test('config-env/AC-05/duplicates are distinct from ordinary syntax errors', () => {
  assert.throws(() => parseConfiguration('version = 1\nversion = 1\n'), { code: 'duplicate_key' })
  assert.throws(() => parseConfiguration('version = "one"\n'), { code: 'unsupported_version' })
  assert.throws(() => parseConfiguration('version = 1\nbroken = [\n'), { code: 'invalid_syntax' })
})

test('config-env/AC-03/explicit-path wins over WHALEREIN_CONFIG and the default', (t) => {
  const root = temp(t)
  const explicit = join(root, 'explicit.toml')
  const fromEnv = join(root, 'from-env.toml')
  writeFileSync(explicit, 'version = 1\nenvironment = "local"\n')
  writeFileSync(fromEnv, 'version = 1\nenvironment = "staging"\n')
  const snapshot = open({ configFile: explicit, envVars: { WHALEREIN_CONFIG: fromEnv }, homeDir: root })
  assert.equal(snapshot.path, realpathSync(explicit))
  assert.equal(snapshot.document.environment, 'local')
})

test('config-env/AC-03/config-env-var selects WHALEREIN_CONFIG unless env is false', (t) => {
  const root = temp(t)
  const fromEnv = join(root, 'env.toml')
  writeFileSync(fromEnv, 'version = 1\n')
  assert.equal(open({ envVars: { WHALEREIN_CONFIG: fromEnv }, homeDir: root }).path, realpathSync(fromEnv))
  const defaultPath = join(root, '.whalerein', 'config.toml')
  mkdirSync(dirname(defaultPath), { recursive: true })
  writeFileSync(defaultPath, 'version = 1\n')
  const snapshot = open({ env: false, envVars: { WHALEREIN_CONFIG: fromEnv }, homeDir: root, inspectionOnly: true })
  assert.equal(snapshot.path, realpathSync(defaultPath))
  assert.equal(snapshot.sourceKind, 'default')
})

test('config-env/AC-03/fresh-default initializes atomically with the shared bytes and 0600', (t) => {
  const root = temp(t)
  const snapshot = open({ homeDir: root, allowDefaultInit: true })
  const path = join(root, '.whalerein', 'config.toml')
  assert.equal(snapshot.path, realpathSync(path))
  assert.equal(snapshot.initialized, true)
  assert.equal(readFileSync(path, 'utf8'), INITIAL_CONFIGURATION)
  assert.equal(statSync(path).mode & 0o777, 0o600)
  assert.equal(snapshot.document.environment, 'production')
})

test('config-env/AC-03/inspection-only never initializes a missing default', (t) => {
  const root = temp(t)
  assert.throws(() => open({ homeDir: root, inspectionOnly: true }), { code: 'missing_config' })
  assert.throws(() => open({ homeDir: root, allowDefaultInit: true, inspectionOnly: true }), { code: 'missing_config' })
  assert.equal(existsSync(join(root, '.whalerein')), false)
})

test('config-env/AC-03/legacy state blocks default initialization', (t) => {
  const root = temp(t)
  assert.throws(() => open({ homeDir: root, allowDefaultInit: true, legacyState: true }), { code: 'missing_config' })
  assert.equal(existsSync(join(root, '.whalerein', 'config.toml')), false)
  assert.throws(() => open({ homeDir: root, allowDefaultInit: true, legacyState: () => { throw new Error('probe') } }), { code: 'missing_config' })
})

test('config-env/AC-03/explicit missing files fail without replacement candidates', (t) => {
  const root = temp(t)
  const explicit = join(root, 'absent.toml')
  assert.throws(() => open({ configFile: explicit, allowDefaultInit: true }), { code: 'missing_config' })
  assert.equal(existsSync(explicit), false)
  const envPath = join(root, 'absent-env.toml')
  assert.throws(() => open({ envVars: { WHALEREIN_CONFIG: envPath }, homeDir: root, allowDefaultInit: true }), { code: 'missing_config' })
  assert.equal(existsSync(envPath), false)
})

test('config-env/AC-03/unreadable files are typed errors', { skip: process.platform === 'win32' ? 'POSIX mode bits required' : false }, (t) => {
  const root = temp(t)
  const path = join(root, 'locked.toml')
  writeFileSync(path, 'version = 1\n')
  chmodSync(path, 0o000)
  assert.throws(() => open({ configFile: path }), { code: 'unreadable_config' })
  assert.throws(() => open({ configFile: root }), { code: 'unreadable_config' })
})

test('config-env/AC-03/future-version files are left untouched', (t) => {
  const root = temp(t)
  const path = join(root, 'future.toml')
  writeFileSync(path, '# keep me\nversion = 2\n')
  const before = readFileSync(path)
  assert.throws(() => open({ configFile: path }), { code: 'unsupported_version' })
  assert.deepEqual(readFileSync(path), before)
})

test('config-env/AC-02/built-in authority override rejects with an actionable code', () => {
  for (const field of ['mode = "standalone"', 'api_origin = "https://evil.example.com"', 'issuer = "https://evil.example.com/oidc"', 'native_client_id = "other"']) {
    assert.throws(
      () => parseConfiguration(`version = 1\n\n[environments.production]\n${field}\n`),
      { code: 'authority_override' },
      field,
    )
  }
  assert.throws(() => parseConfiguration('version = 1\n\n[environments.local]\nmode = "standalone"\n'), { code: 'authority_override' })
})

test('config-env/AC-02/local trust specialization only allows label and tls', () => {
  const document = parseConfiguration('version = 1\n\n[environments.local]\nlabel = "Dev"\n')
  assert.equal(document.environments.local.label, 'Dev')
  assert.equal(document.environments.local.mode, null)
  assert.throws(() => parseConfiguration('version = 1\n\n[environments.local]\napi_origin = "https://x.example.com"\n'), { code: 'authority_override' })
})

test('config-env/AC-04/custom account and standalone round-trip, no hostname inference', () => {
  const account = openFixture('fixtures/valid/custom-account.toml')
  assert.deepEqual(account.document.environments.company.tls, null)
  const resolution = resolveEnvironment(account)
  assert.equal(resolution.definition.mode, 'account')
  const standalone = openFixture('fixtures/valid/standalone.toml')
  assert.equal(resolveEnvironment(standalone).definition.mode, 'standalone')
  assert.throws(
    () => parseConfiguration('version = 1\n\n[environments.lab]\nmode = "standalone"\napi_origin = "https://app.example.com"\n'),
    { code: 'invalid_mode_fields' },
  )
})

test('config-env/AC-04/arbitrary names follow the shared identifier rule', () => {
  for (const name of ['a', '0', 'a-b-c', 'x'.repeat(64)]) {
    assert.equal(parseConfiguration(`version = 1\n\n[environments.${name}]\nmode = "standalone"\n`).environments[name].id, name)
  }
  for (const name of ['Company', '-a', 'a_', 'x'.repeat(65)]) {
    assert.throws(() => parseConfiguration(`version = 1\n\n[environments."${name}"]\nmode = "standalone"\n`), { code: 'invalid_identifier' }, name)
  }
})

test('config-env/AC-20/managed route keys are unknown fields', () => {
  for (const key of ['mqtt_url = "mqtts://broker.example.com"', 'access_url = "https://access.example.com"', 'region = "eu"', 'broker = "x"']) {
    assert.throws(
      () => parseConfiguration(`version = 1\n\n[environments.company]\nmode = "standalone"\n${key}\n`),
      { code: 'unknown_field' },
      key,
    )
  }
})

test('config-env/AC-06/file default, invocation override and production fallback', () => {
  const saved = openFixture('fixtures/valid/custom-account.toml')
  assert.equal(resolveEnvironment(saved).selection_source, 'file')
  const override = resolveEnvironment(saved, 'local')
  assert.equal(override.selection_source, 'invocation')
  assert.equal(override.environment, 'local')
  const minimal = openFixture('fixtures/valid/minimal.toml')
  assert.equal(resolveEnvironment(minimal).selection_source, 'default')
  assert.throws(() => resolveEnvironment(saved, 'ghost'), { code: 'unknown_environment' })
  assert.throws(() => resolveEnvironment(saved, 'Company'), { code: 'invalid_identifier' })
  const ghost = openFixture('fixtures/invalid/unknown-environment-selection.toml')
  assert.throws(() => resolveEnvironment(ghost), { code: 'unknown_environment' })
})

test('config-env/AC-16/view reports source, desired default, override and field provenance', (t) => {
  const root = temp(t)
  const path = join(root, 'config.toml')
  writeFileSync(path, '# note\nversion = 1\nenvironment = "production" # saved\n\n[environments.production]\nlabel = "Work"\n')
  const snapshot = open({ configFile: path })
  const inherited = view(snapshot)
  assert.equal(inherited.path, snapshot.path)
  assert.equal(inherited.source_kind, 'explicit')
  assert.equal(inherited.desired_default, 'production')
  assert.equal(inherited.effective_override, null)
  assert.equal(inherited.selected_environment, 'production')
  assert.equal(inherited.field_provenance.label, 'file')
  assert.equal(inherited.field_provenance.api_origin, 'catalog')
  assert.deepEqual(inherited.validation, { scope: 'offline', live_readiness: 'not_checked' })
  assert.deepEqual(inherited.errors, [])

  const overridden = view(snapshot, { override: 'local' })
  assert.equal(overridden.selected_environment, 'local')
  assert.equal(overridden.selection_source, 'invocation')
  assert.equal(overridden.desired_default, 'production')
  assert.equal(overridden.effective_override, 'local')
  assert.equal(overridden.field_provenance.label, 'catalog')
  assert.equal(overridden.definition.tls.ca_provider, 'mkcert')

  const unknown = view(snapshot, { override: 'ghost' })
  assert.equal(unknown.selected_environment, null)
  assert.equal(unknown.errors[0].code, 'unknown_environment')
  assert.equal(unknown.desired_default, 'production')
})

test('config-env/AC-15/view redacts credential-bearing error text', () => {
  const snapshot = openFixture('fixtures/valid/minimal.toml')
  const projected = view(snapshot, { error: configError('invalid_url', 'https://user:secret@app.example.com/path?token=sentinel') })
  assert.deepEqual(projected.errors, [{ code: 'invalid_url', message: 'https://app.example.com/path', environment: null }])
  assert.equal(JSON.stringify(projected).includes('secret'), false)
  const unknown = view(snapshot, { error: new Error('https://user:secret@x?token=1') })
  assert.deepEqual(unknown.errors, [{ code: 'unreadable_config', message: 'configuration operation failed', environment: null }])
})

test('config-env/AC-16/view accepts a resolution and rejects a missing snapshot', () => {
  const snapshot = openFixture('fixtures/valid/custom-account.toml')
  const resolution = resolveEnvironment(snapshot)
  const projected = view(resolution)
  assert.equal(projected.path, snapshot.path)
  assert.equal(projected.definition_source, 'file')
  assert.throws(() => view({ environment: 'production' }), { code: 'missing_config' })
  assert.throws(() => view('nope'), { code: 'missing_config' })
})

test('config-env/AC-03/custom-account document shape serializes inactive fields as null', () => {
  const document = openFixture('fixtures/valid/custom-account.toml').document
  assert.deepEqual(document.environments.company, {
    id: 'company',
    mode: 'account',
    label: 'Company Cloud',
    api_origin: 'https://cloud.example.com',
    issuer: 'https://auth.example.com/oidc',
    native_client_id: 'whalerein-cli',
    environment: null,
    tls: null,
  })
})

test('config-env/AC-01/host-device keeps independent files and state per installation', (t) => {
  const first = temp(t)
  const second = temp(t)
  const a = join(first, 'config.toml')
  const b = join(second, 'config.toml')
  writeFileSync(a, 'version = 1\nenvironment = "local"\n')
  writeFileSync(b, 'version = 1\nenvironment = "production"\n')
  const snapshotA = open({ configFile: a })
  const snapshotB = open({ configFile: b })
  update(snapshotA, { type: 'set_default_environment', environment: 'staging' }, snapshotA.revision)
  assert.equal(readFileSync(b, 'utf8'), 'version = 1\nenvironment = "production"\n')
  assert.equal(resolveEnvironment(open({ configFile: b })).environment, 'production')
  assert.equal(resolveEnvironment(open({ configFile: a })).environment, 'staging')
})

test('config-env/AC-15/literal secret fields are unknown and never exposed', () => {
  assert.throws(
    () => parseConfiguration('version = 1\n\n[environments.company]\nmode = "standalone"\ntoken = "secret-sentinel"\n'),
    { code: 'unknown_field' },
  )
  const snapshot = openFixture('fixtures/valid/minimal.toml')
  assert.equal(JSON.stringify(view(snapshot)).includes('secret-sentinel'), false)
})

test('config-env/AC-04/Core-style URL and TLS rules are shared', () => {
  const loopback = parseConfiguration('version = 1\n\n[environments.lab]\nmode = "account"\napi_origin = "http://127.0.0.1:8080"\nissuer = "http://localhost:9090/oidc"\nnative_client_id = "whalerein-cli"\n')
  assert.equal(loopback.environments.lab.api_origin, 'http://127.0.0.1:8080')
  assert.throws(
    () => parseConfiguration('version = 1\n\n[environments.lab]\nmode = "account"\napi_origin = "http://cloud.example.com"\nissuer = "https://auth.example.com/oidc"\nnative_client_id = "whalerein-cli"\n'),
    { code: 'invalid_url' },
  )
  assert.throws(
    () => parseConfiguration('version = 1\n\n[environments.company]\nmode = "standalone"\ntls = "system"\n'),
    { code: 'invalid_tls' },
  )
  const localTls = parseConfiguration('version = 1\n\n[environments.local.tls]\n')
  assert.deepEqual(localTls.environments.local.tls, { ca_provider: 'mkcert', ca_file: null })
})

test('config-env/AC-16/view custom entries report default provenance for unset fields', () => {
  const snapshot = openFixture('fixtures/valid/custom-account.toml')
  const projected = view(snapshot)
  assert.equal(projected.field_provenance.id, 'file')
  assert.equal(projected.field_provenance.mode, 'file')
  assert.equal(projected.field_provenance.label, 'file')
  assert.equal(projected.field_provenance.api_origin, 'file')
  assert.equal(projected.field_provenance.environment, 'default')
  assert.equal(projected.field_provenance.tls, 'default')
})

test('config-env/AC-05/user entries reject the reserved environment token', () => {
  assert.throws(
    () => parseConfiguration('version = 1\n\n[environments.company]\nmode = "standalone"\nenvironment = "production"\n'),
    { code: 'unknown_field' },
  )
  assert.throws(
    () => parseConfiguration('version = 1\n\n[environments.production]\nenvironment = "local"\n'),
    { code: 'authority_override' },
  )
})

test('content revision hashes buffers and other byte inputs identically', async () => {
  const { revisionOf } = await import('../src/configuration/revision.js')
  const bytes = Buffer.from('revision-input')
  assert.equal(revisionOf(bytes), revisionOf(new Uint8Array(bytes)))
  assert.equal(revisionOf(new Uint8Array(bytes)), revisionOf('revision-input'))
  assert.match(revisionOf(bytes), /^sha256:[a-f0-9]{64}$/)
})
