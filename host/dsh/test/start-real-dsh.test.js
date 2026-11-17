import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs'
import { spawnSync } from 'node:child_process'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const launcher = fileURLToPath(new URL('../bin/start-real-dsh.sh', import.meta.url))
const hasPython = spawnSync('python3', ['--version']).status === 0
const skip = process.platform === 'win32' ? 'The Bash launcher targets Unix platforms' :
  !hasPython ? 'python3 is unavailable on this platform' : false

function fixture(t) {
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-launcher-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('WHALEREIN_')))
  env.DSH_HOME = join(directory, 'dsh')
  env.WHALEREIN_DATA_DIR = join(directory, 'data')
  const preview = (...args) => {
    const result = spawnSync('bash', [launcher, '--dry-run', ...args], { env, encoding: 'utf8' })
    assert.equal(result.status, 0, result.stderr)
    return result.stdout
  }
  return { directory, env, preview }
}

test('launcher previews a fresh managed profile without filesystem changes', { skip }, (t) => {
  const { directory, preview } = fixture(t)
  assert.match(preview(), /connection_mode: managed/)
  assert.deepEqual(readdirSync(directory), [])
})

test('launcher generates distinct standalone host IDs and honors an explicit ID', { skip }, (t) => {
  const { directory, env, preview } = fixture(t)
  const hostId = () => preview('--standalone').match(/host_id: "([^"]+)"/)[1]
  const first = hostId()
  const second = hostId()
  assert.match(first, /-[abcdefghjkmnpqrstuvwxyz23456789]{4}$/)
  assert.notEqual(first, second)
  env.WHALEREIN_HOST_ID = 'host-explicit'
  assert.equal(hostId(), 'host-explicit')
  assert.deepEqual(readdirSync(directory), [])
})

test('launcher preserves an existing entry and reuses its host ID on an explicit rewrite', { skip }, (t) => {
  const { env, preview } = fixture(t)
  const patch = join(env.DSH_HOME, 'profiles', 'whalerein-debug', 'cordis.patch.yml')
  mkdirSync(dirname(patch), { recursive: true })
  const initial = '- id: whalerein-dsh\n  name: whalerein-dsh\n  config:\n    server: 127.0.0.1\n    host_id: "host-existing"\n'
  writeFileSync(patch, initial)
  assert.match(preview(), /existing entry .* is preserved \(mode: standalone\)/)
  assert.match(preview('--standalone'), /host_id: "host-existing"/)
  assert.equal(readFileSync(patch, 'utf8'), initial)
})
