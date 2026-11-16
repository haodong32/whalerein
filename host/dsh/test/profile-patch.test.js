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
  const result = spawnSync('python3', [helper, file, 'host-seeded', '/tmp/whalerein-state'], { encoding: 'utf8' })
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
  assert.match(text, /host_id: "host-seeded"/)
})

test('an existing profile list gains exactly one WhaleRein entry and stays idempotent', (t) => {
  if (!hasPython) return t.skip('python3 is unavailable on this platform')
  const initial = '# header\n- id: other\n  name: other\n'
  const first = run(t, initial)
  assert.match(first.text, /- id: other/)
  assert.match(first.text, /^- id: whalerein-dsh$/m)
  assert.equal(first.text.match(/^- id:/gm).length, 2)
  const second = spawnSync('python3', [helper, first.file, 'host-other', '/tmp/other'], { encoding: 'utf8' })
  assert.equal(second.status, 0, second.stderr)
  const text = readFileSync(first.file, 'utf8')
  assert.equal(text.match(/^- id: whalerein-dsh$/gm).length, 1)
  assert.match(text, /host_id: "host-seeded"/)
})

test('a missing patch file is created with the entry', (t) => {
  if (!hasPython) return t.skip('python3 is unavailable on this platform')
  const { text } = run(t)
  assert.match(text, /^- id: whalerein-dsh$/m)
  assert.match(text, /adapter: real/)
})
