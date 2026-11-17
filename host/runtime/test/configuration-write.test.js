import assert from 'node:assert/strict'
import {
  chmodSync,
  closeSync,
  existsSync,
  fsyncSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  symlinkSync,
  unlinkSync,
  writeFileSync,
  writeSync,
} from 'node:fs'
import { spawn } from 'node:child_process'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { open, update } from '../src/configuration/index.js'

const here = dirname(fileURLToPath(import.meta.url))
const moduleUrl = new URL('../src/configuration/index.js', import.meta.url).href
const corpus = join(here, '..', '..', '..', 'contracts', 'client-environment')

const baseIo = { closeSync, fsyncSync, mkdirSync, openSync, renameSync, unlinkSync, writeSync }

function temp(t) {
  const root = mkdtempSync(join(tmpdir(), 'whalerein-config-write-'))
  t.after(() => rmSync(root, { recursive: true, force: true }))
  return root
}

function fixture(root, text) {
  const path = join(root, 'config.toml')
  writeFileSync(path, text)
  return path
}

function runChild(args) {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, args, { stdio: ['ignore', 'pipe', 'pipe'] })
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', (chunk) => { stdout += chunk })
    child.stderr.on('data', (chunk) => { stderr += chunk })
    child.on('close', (code) => resolve({ code, stdout: stdout.trim(), stderr: stderr.trim() }))
  })
}

const WRITER_SCRIPT = `
import { open, update } from ${JSON.stringify(moduleUrl)}
const [path, patch, revision] = process.argv.slice(1)
try {
  update(open({ configFile: path }), JSON.parse(patch), revision)
  console.log('OK')
} catch (error) {
  console.log('ERR:' + error.code)
  process.exitCode = error.code === 'revision_conflict' ? 0 : 1
}
`

const HOLDER_SCRIPT = `
import { DatabaseSync } from 'node:sqlite'
const database = new DatabaseSync(process.argv[1])
database.exec('PRAGMA busy_timeout = 2000')
database.exec('CREATE TABLE IF NOT EXISTS whalerein_config_lock (id INTEGER PRIMARY KEY)')
database.exec('BEGIN IMMEDIATE')
console.log('READY')
setInterval(() => {}, 1000)
`

test('config-env/AC-19/set-default persists and preserves unrelated comments and keys', (t) => {
  const root = temp(t)
  const path = fixture(root, readFileSync(join(corpus, 'fixtures/valid/production-selection.toml'), 'utf8'))
  const snapshot = open({ configFile: path })
  const next = update(snapshot, { type: 'set_default_environment', environment: 'local' }, snapshot.revision)
  const text = readFileSync(path, 'utf8')
  assert.match(text, /# Operator notes must survive settings updates\./)
  assert.match(text, /environment = "local" # saved default/)
  assert.match(text, /label = "WhaleRein Cloud \(work laptop\)"/)
  assert.equal(next.document.environment, 'local')
  assert.equal(snapshot.revision === next.revision, false)
})

test('config-env/AC-19/set-default accepts built-ins and rejects unknown names', (t) => {
  const root = temp(t)
  const path = fixture(root, 'version = 1\nenvironment = "production"\n')
  let snapshot = open({ configFile: path })
  snapshot = update(snapshot, { type: 'set_default_environment', environment: null }, snapshot.revision)
  assert.equal(snapshot.document.environment, null)
  assert.throws(() => update(snapshot, { type: 'set_default_environment', environment: 'ghost' }, snapshot.revision), { code: 'unknown_environment' })
  assert.throws(() => update(snapshot, { type: 'set_default_environment', environment: 'Company' }, snapshot.revision), { code: 'invalid_identifier' })
})

test('config-env/AC-04/upsert creates, specializations update, remove drops only the target', (t) => {
  const root = temp(t)
  const path = fixture(root, '# kept\nversion = 1\nenvironment = "production"\n\n[environments.production]\nlabel = "Work"\n')
  let snapshot = open({ configFile: path })
  snapshot = update(snapshot, {
    type: 'upsert_environment',
    name: 'company',
    definition: {
      mode: 'account',
      label: 'Company Cloud',
      api_origin: 'https://cloud.example.com',
      issuer: 'https://auth.example.com/oidc',
      native_client_id: 'whalerein-cli',
    },
  }, snapshot.revision)
  assert.equal(snapshot.document.environments.company.mode, 'account')
  snapshot = update(snapshot, {
    type: 'upsert_environment',
    name: 'company',
    definition: { mode: 'standalone', label: 'Lab' },
  }, snapshot.revision)
  const standalone = snapshot.document.environments.company
  assert.equal(standalone.mode, 'standalone')
  assert.equal(standalone.label, 'Lab')
  assert.equal(standalone.api_origin, null)
  assert.equal(standalone.issuer, null)
  assert.equal(standalone.native_client_id, null)
  snapshot = update(snapshot, {
    type: 'upsert_environment',
    name: 'company',
    definition: {
      mode: 'account',
      api_origin: 'https://cloud.example.com',
      issuer: 'https://auth.example.com/oidc',
      native_client_id: 'whalerein-cli',
    },
  }, snapshot.revision)
  assert.equal(snapshot.document.environments.company.label, null)
  snapshot = update(snapshot, { type: 'remove_environment', name: 'company' }, snapshot.revision)
  assert.equal(snapshot.document.environments.company, undefined)
  assert.match(readFileSync(path, 'utf8'), /# kept/)
  assert.match(readFileSync(path, 'utf8'), /\[environments\.production\]/)
})

test('config-env/AC-02/specialize builtin writes only label and tls', (t) => {
  const root = temp(t)
  const path = fixture(root, 'version = 1\nenvironment = "local"\n\n[environments.local]\nlabel = "Existing"\n')
  let snapshot = open({ configFile: path })
  snapshot = update(snapshot, {
    type: 'specialize_builtin',
    name: 'local',
    label: 'Dev machine',
    tls: { ca_provider: 'file', ca_file: '/opt/dev/rootCA.pem' },
  }, snapshot.revision)
  const entry = snapshot.document.environments.local
  assert.equal(entry.label, 'Dev machine')
  assert.deepEqual(entry.tls, { ca_provider: 'file', ca_file: '/opt/dev/rootCA.pem' })
  snapshot = update(snapshot, { type: 'specialize_builtin', name: 'local', tls: null }, snapshot.revision)
  assert.equal(snapshot.document.environments.local.tls, null)
  assert.throws(() => update(snapshot, { type: 'specialize_builtin', name: 'company', label: 'x' }, snapshot.revision), { code: 'invalid_argument' })
  assert.throws(() => update(snapshot, { type: 'specialize_builtin', name: 'local', mode: 'standalone' }, snapshot.revision), { code: 'authority_override' })
  assert.throws(() => update(snapshot, { type: 'remove_environment', name: 'local' }, snapshot.revision), { code: 'authority_override' })
})

test('config-env/AC-12/stale edit returns revision_conflict with the current revision and draft', (t) => {
  const root = temp(t)
  const path = fixture(root, 'version = 1\nenvironment = "production"\n')
  const snapshot = open({ configFile: path })
  writeFileSync(path, '# external\nversion = 1\nenvironment = "local"\n')
  const external = open({ configFile: path })
  assert.throws(
    () => update(snapshot, { type: 'set_default_environment', environment: 'staging' }, snapshot.revision),
    (error) => {
      assert.equal(error.code, 'revision_conflict')
      assert.equal(error.currentRevision, external.revision)
      assert.equal(error.expectedRevision, snapshot.revision)
      assert.deepEqual(error.draft, { type: 'set_default_environment', environment: 'staging' })
      return true
    },
  )
  assert.equal(readFileSync(path, 'utf8'), '# external\nversion = 1\nenvironment = "local"\n')
})

test('config-env/AC-12/expected revision mismatch is a conflict before any write', (t) => {
  const root = temp(t)
  const path = fixture(root, 'version = 1\n')
  const snapshot = open({ configFile: path })
  assert.throws(() => update(snapshot, { type: 'set_default_environment', environment: 'production' }, 'sha256:deadbeef'), { code: 'revision_conflict' })
  assert.equal(readFileSync(path, 'utf8'), 'version = 1\n')
})

test('config-env/AC-13/a removed file is a stale revision, not a recreated default', (t) => {
  const root = temp(t)
  const path = fixture(root, 'version = 1\n')
  const snapshot = open({ configFile: path })
  unlinkSync(path)
  assert.throws(
    () => update(snapshot, { type: 'set_default_environment', environment: 'local' }, snapshot.revision),
    (error) => {
      assert.equal(error.code, 'revision_conflict')
      assert.equal(error.currentRevision, null)
      return true
    },
  )
  assert.equal(existsSync(path), false)
})

test('config-env/AC-13/validation failure leaves the old file byte-identical', (t) => {
  const root = temp(t)
  const original = 'version = 1\n\n[environments.company]\nmode = "standalone"\n'
  const path = fixture(root, original)
  const snapshot = open({ configFile: path })
  assert.throws(
    () => update(snapshot, {
      type: 'upsert_environment',
      name: 'company',
      definition: { mode: 'account', api_origin: 'https://cloud.example.com', native_client_id: 'whalerein-cli' },
    }, snapshot.revision),
    { code: 'invalid_mode_fields' },
  )
  assert.equal(readFileSync(path, 'utf8'), original)
})

test('config-env/AC-13/temp write and rename failure keep the old file and clean temp files', (t) => {
  const root = temp(t)
  const original = 'version = 1\n'
  const path = fixture(root, original)
  const snapshot = open({ configFile: path })
  assert.throws(
    () => update(snapshot, { type: 'set_default_environment', environment: 'production' }, snapshot.revision, {
      io: { ...baseIo, writeSync() { throw new Error('ENOSPC') } },
    }),
    { code: 'unreadable_config' },
  )
  assert.equal(readFileSync(path, 'utf8'), original)
  assert.deepEqual(readdirSync(root).filter((name) => name.endsWith('.tmp')), [])
  assert.throws(
    () => update(snapshot, { type: 'set_default_environment', environment: 'production' }, snapshot.revision, {
      io: { ...baseIo, renameSync() { throw new Error('EACCES') } },
    }),
    { code: 'unreadable_config' },
  )
  assert.equal(readFileSync(path, 'utf8'), original)
  assert.deepEqual(readdirSync(root).filter((name) => name.endsWith('.tmp')), [])
})

test('config-env/AC-12/two Node writers race without losing the committed update', { timeout: 30000 }, async (t) => {
  const root = temp(t)
  const path = fixture(root, 'version = 1\nenvironment = "production"\n')
  const snapshot = open({ configFile: path })
  const patch = JSON.stringify({ type: 'set_default_environment', environment: 'staging' })
  const results = await Promise.all([
    runChild(['--input-type=module', '-e', WRITER_SCRIPT, path, patch, snapshot.revision]),
    runChild(['--input-type=module', '-e', WRITER_SCRIPT, path, patch, snapshot.revision]),
  ])
  assert.equal(results.every((result) => result.code === 0), true, JSON.stringify(results))
  assert.deepEqual(results.map((result) => result.stdout).sort(), ['ERR:revision_conflict', 'OK'])
  const reopened = open({ configFile: path })
  assert.equal(reopened.document.environment, 'staging')
})

test('config-env/AC-12/crash releases the SQLite writer lock for an immediate retry', { timeout: 30000 }, async (t) => {
  const root = temp(t)
  const path = fixture(root, 'version = 1\n')
  const snapshot = open({ configFile: path })
  const holder = spawn(process.execPath, ['--input-type=module', '-e', HOLDER_SCRIPT, `${path}.lock.sqlite`], { stdio: ['ignore', 'pipe', 'pipe'] })
  t.after(() => holder.kill('SIGKILL'))
  await new Promise((resolve, reject) => {
    holder.stdout.on('data', (chunk) => { if (String(chunk).includes('READY')) resolve() })
    holder.on('error', reject)
  })
  holder.kill('SIGKILL')
  await new Promise((resolve) => holder.on('close', resolve))
  const next = update(snapshot, { type: 'set_default_environment', environment: 'local' }, snapshot.revision)
  assert.equal(next.document.environment, 'local')
  assert.equal(existsSync(`${path}.lock.sqlite`), true)
})

test('config-env/AC-12/a retargeted symlink is a changed canonical target', { skip: process.platform === 'win32' ? 'symlink procedure requires POSIX' : false }, (t) => {
  const root = temp(t)
  const real = fixture(root, 'version = 1\n')
  const other = join(root, 'other.toml')
  writeFileSync(other, 'version = 1\nenvironment = "local"\n')
  const link = join(root, 'linked.toml')
  symlinkSync(real, link)
  const snapshot = open({ configFile: link })
  unlinkSync(link)
  symlinkSync(other, link)
  assert.throws(() => update(snapshot, { type: 'set_default_environment', environment: 'production' }, snapshot.revision), { code: 'revision_conflict' })
  assert.equal(readFileSync(other, 'utf8'), 'version = 1\nenvironment = "local"\n')
})

test('config-env/AC-13/future-version files remain untouched by a revisioned update', (t) => {
  const root = temp(t)
  const path = fixture(root, 'version = 2\n')
  assert.throws(() => open({ configFile: path }), { code: 'unsupported_version' })
  assert.equal(readFileSync(path, 'utf8'), 'version = 2\n')
})

test('config-env/AC-04/Core-style patch variants and flat custom input work', (t) => {
  const root = temp(t)
  const path = fixture(root, 'version = 1\n')
  let snapshot = open({ configFile: path })
  snapshot = update(snapshot, {
    type: 'UpsertCustomEnvironment',
    environment: 'lab',
    label: 'Lab pairing',
    mode: 'standalone',
    tls: { ca_provider: 'file', ca_file: '/opt/lab/rootCA.pem' },
  }, snapshot.revision)
  assert.equal(snapshot.document.environments.lab.mode, 'standalone')
  assert.equal(snapshot.document.environments.lab.tls.ca_provider, 'file')
  snapshot = update(snapshot, {
    type: 'SpecializeBuiltin',
    environment: 'local',
    label: 'Dev',
    tls: 'mkcert',
  }, snapshot.revision)
  assert.equal(snapshot.document.environments.local.label, 'Dev')
  assert.equal(snapshot.document.environments.local.tls.ca_provider, 'mkcert')
  snapshot = update(snapshot, { type: 'SetDefaultEnvironment', environment: 'lab' }, snapshot.revision)
  assert.equal(snapshot.document.environment, 'lab')
  snapshot = update(snapshot, { type: 'RemoveCustomEnvironment', environment: 'lab' }, snapshot.revision)
  assert.equal(snapshot.document.environments.lab, undefined)
  assert.throws(() => update(snapshot, { type: 'not_a_patch' }, snapshot.revision), { code: 'invalid_argument' })
})

test('config-env/AC-12/an unavailable writer lock is a typed failure that keeps the file', (t) => {
  const root = temp(t)
  const path = fixture(root, 'version = 1\n')
  const snapshot = open({ configFile: path })
  mkdirSync(`${snapshot.path}.lock.sqlite`)
  assert.throws(
    () => update(snapshot, { type: 'set_default_environment', environment: 'production' }, snapshot.revision),
    { code: 'unreadable_config' },
  )
  assert.equal(readFileSync(path, 'utf8'), 'version = 1\n')
})
