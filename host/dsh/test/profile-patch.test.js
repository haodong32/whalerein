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
  assert.match(text, /state_root: "\/tmp\/whalerein-state"/)
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
  assert.match(text, /state_root: "\/tmp\/whalerein-state"/)
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
  assert.match(text, /service: whalerein_cloud/)
  assert.match(text, /adapter: real/)
  assert.doesNotMatch(text, /local_environment_file|WHALEREIN_LOCAL_ENVIRONMENT|environment: local|host_token|server:|ca_file:/)
})


test('explicit Cloud overrides preserve custom settings, sibling entries and private values', (t) => {
  if (!hasPython) return t.skip('python3 is unavailable on this platform')
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-profile-update-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const file = join(directory, 'cordis.patch.yml')
  const initial = '- id: whalerein-dsh\n  name: whalerein-dsh\n  config:\n    service: custom\n    custom:\n      label: Keep me\n    host_token: private-value\n    state_dir: /existing\n- id: other\n  name: other\n  config: {}\n'
  writeFileSync(file, initial)
  const args = [helper, file, '/unused', '--cloud-environment', 'staging']
  const preview = spawnSync('python3', [...args, '--print'], { encoding: 'utf8' })
  assert.equal(preview.status, 0, preview.stderr)
  assert.match(preview.stdout, /cloud_environment: staging/)
  assert.doesNotMatch(preview.stdout, /private-value|host_token/)
  assert.equal(readFileSync(file, 'utf8'), initial)
  const result = spawnSync('python3', args, { encoding: 'utf8' })
  assert.equal(result.status, 0, result.stderr)
  const saved = readFileSync(file, 'utf8')
  assert.match(saved, /    service: custom/)
  assert.match(saved, /    custom:\n      label: Keep me/)
  assert.match(saved, /    state_dir: \/existing/)
  assert.match(saved, /- id: other\n  name: other\n  config: {}/)
})

test('profile updates reject ambiguous config blocks and duplicate connection fields without writes', (t) => {
  if (!hasPython) return t.skip('python3 is unavailable on this platform')
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-profile-invalid-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const file = join(directory, 'cordis.patch.yml')
  for (const initial of ['- id: whalerein-dsh\n  config: {}\n', '- id: whalerein-dsh\n  config:\n    cloud_environment: local\n    cloud_environment: staging\n']) {
    writeFileSync(file, initial)
    const result = spawnSync('python3', [helper, file, '--cloud-environment', 'production'], { encoding: 'utf8' })
    assert.notEqual(result.status, 0)
    assert.equal(readFileSync(file, 'utf8'), initial)
  }
})

test('profile helper never appends another identity for reordered or duplicate target entries', (t) => {
  if (!hasPython) return t.skip('python3 is unavailable on this platform')
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-profile-shape-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const file = join(directory, 'cordis.patch.yml')
  for (const [initial, expectedStatus] of [
    ['- name: whalerein-dsh\n  id: whalerein-dsh\n  config:\n    service: custom\n', 0],
    ['- id: whalerein-dsh # keep\n  config:\n    service: custom\n', 0],
    ['- id: whalerein-dsh\n  config: {}\n- id: whalerein-dsh\n  config: {}\n', 2],
  ]) {
    writeFileSync(file, initial)
    const result = spawnSync('python3', [helper, file], { encoding: 'utf8' })
    assert.equal(result.status, expectedStatus)
    assert.equal(readFileSync(file, 'utf8'), initial)
    const override = spawnSync('python3', [helper, file, '--cloud-environment', 'staging'], { encoding: 'utf8' })
    assert.notEqual(override.status, 0)
    assert.equal(readFileSync(file, 'utf8'), initial)
  }
})
