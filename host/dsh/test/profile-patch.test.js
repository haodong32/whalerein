import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { spawnSync } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = dirname(dirname(fileURLToPath(import.meta.url)))
const helper = join(root, 'bin', 'profile-patch.py')
const hasPython = spawnSync('python3', ['--version'], { encoding: 'utf8' }).status === 0

function run(t, initial) {
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-profile-patch-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const file = join(directory, 'cordis.patch.yml')
  if (initial !== undefined) writeFileSync(file, initial)
  const result = spawnSync('python3', [helper, file, '/tmp/whalerein-state'], { encoding: 'utf8' })
  assert.equal(result.status, 0, result.stderr)
  return { file, text: readFileSync(file, 'utf8') }
}

test('a default comment-only empty profile patch becomes one valid entry, not an item after []', (t) => {
  if (!hasPython) return t.skip('python3 is unavailable on this platform')
  const header = '# Your patch layer for this dsh profile:\n# a top-level YAML array of entries.\n[]\n'
  const { text } = run(t, header)
  assert.doesNotMatch(text, /^\[\]$/m)
  assert.match(text, /^# a top-level YAML array of entries\.$/m)
  assert.match(text, /^- id: whalerein-dsh$/m)
  assert.equal(text.match(/^- id:/gm).length, 1)
  assert.match(text, /state_dir: "\/tmp\/whalerein-state"/)
})

test('an existing profile list gains exactly one WhaleRein entry and stays idempotent', (t) => {
  if (!hasPython) return t.skip('python3 is unavailable on this platform')
  const initial = '# header\n- id: other\n  name: other\n'
  const first = run(t, initial)
  assert.match(first.text, /- id: other/)
  assert.match(first.text, /^- id: whalerein-dsh$/m)
  assert.equal(first.text.match(/^- id:/gm).length, 2)
  const second = spawnSync('python3', [helper, first.file, '/tmp/other'], { encoding: 'utf8' })
  assert.equal(second.status, 0, second.stderr)
  const text = readFileSync(first.file, 'utf8')
  assert.equal(text.match(/^- id: whalerein-dsh$/gm).length, 1)
  assert.match(text, /state_dir: "\/tmp\/whalerein-state"/)
})

test('a missing patch file is created with the entry', (t) => {
  if (!hasPython) return t.skip('python3 is unavailable on this platform')
  const { text } = run(t)
  assert.match(text, /^- id: whalerein-dsh$/m)
  assert.match(text, /adapter: real/)
  assert.doesNotMatch(text, /host_id|host_token|server:|mqtt_url|access_url|ca_file/)
})

test('the launcher never injects a Cloud-generated environment descriptor', (t) => {
  if (!hasPython) return t.skip('python3 is unavailable on this platform')
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-local-profile-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const file = join(directory, 'cordis.patch.yml')
  const result = spawnSync('python3', [helper, file, ''], {
    encoding: 'utf8',
    env: { ...process.env, WHALEREIN_LOCAL_ENVIRONMENT: '/generated/environment.json', WHALEREIN_UPSTREAM_ROOT: '/cloud' },
  })
  assert.equal(result.status, 0, result.stderr)
  const text = readFileSync(file, 'utf8')
  assert.match(text, /connection_mode: managed/)
  assert.match(text, /adapter: real/)
  assert.doesNotMatch(text, /local_environment_file|WHALEREIN_LOCAL_ENVIRONMENT|environment: local|host_token|server:|ca_file:/)
})

function invoke(args) {
  return spawnSync('python3', [helper, ...args], { encoding: 'utf8' })
}

test('managed mode seeds the cloud entry without standalone secrets', (t) => {
  if (!hasPython) return t.skip('python3 is unavailable on this platform')
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-profile-patch-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const file = join(directory, 'cordis.patch.yml')
  writeFileSync(file, '# header\n[]\n')
  const result = invoke([file, '--mode', 'managed', '--state-root', '/tmp/root',
    '--developer-tools', '--log-level', 'info'])
  assert.equal(result.status, 0, result.stderr)
  const text = readFileSync(file, 'utf8')
  assert.match(text, /connection_mode: managed/)
  assert.match(text, /state_root: "\/tmp\/root"/)
  assert.match(text, /developer_tools_enabled: true/)
  assert.doesNotMatch(text, /host_token|server:|state_dir:/)
})

test('an existing entry is preserved unless --force rewrites only that item', (t) => {
  if (!hasPython) return t.skip('python3 is unavailable on this platform')
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-profile-patch-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const file = join(directory, 'cordis.patch.yml')
  writeFileSync(file, [
    '- id: whalerein-dsh',
    '  name: whalerein-dsh',
    '  config:',
    '    server: 127.0.0.1',
    '    host_id: "host-keep"',
    '    host_token: !!js process.env.WHALEREIN_HOST_TOKEN',
    '- id: locale',
    '  name: locale',
    '  config: {}',
    '',
  ].join('\n'))
  const preserved = invoke([file])
  assert.equal(preserved.status, 0, preserved.stderr)
  assert.match(readFileSync(file, 'utf8'), /host_token/)

  const rewritten = invoke([file, '--mode', 'managed', '--force', '--no-developer-tools'])
  assert.equal(rewritten.status, 0, rewritten.stderr)
  const text = readFileSync(file, 'utf8')
  assert.match(text, /connection_mode: managed/)
  assert.match(text, /developer_tools_enabled: false/)
  assert.match(text, /- id: locale/)
  assert.equal(text.match(/^- id: whalerein-dsh$/gm).length, 1)
})

test('--print leaves the file untouched and shows the replacement', (t) => {
  if (!hasPython) return t.skip('python3 is unavailable on this platform')
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-profile-patch-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const file = join(directory, 'cordis.patch.yml')
  const initial = '- id: whalerein-dsh\n  name: whalerein-dsh\n  config:\n    server: 127.0.0.1\n'
  writeFileSync(file, initial)
  const result = invoke([file, '--mode', 'managed', '--force', '--print'])
  assert.equal(result.status, 0, result.stderr)
  assert.match(result.stdout, /connection_mode: managed/)
  assert.equal(readFileSync(file, 'utf8'), initial)
})
