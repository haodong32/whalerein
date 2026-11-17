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
  assert.match(preview(), /service: whalerein_cloud/)
  assert.deepEqual(readdirSync(directory), [])
})


test('launcher persists internal selection through the profile helper and rejects invalid arguments before writes', { skip }, (t) => {
  const { directory, preview } = fixture(t)
  assert.match(preview('--cloud-environment', 'staging'), /cloud_environment: staging/)
  for (const args of [['--cloud-environment', 'invalid'], ['--port', '0'], ['--port', '65536'], ['--port', 'not-a-port'], ['--profile', '../other'], ['--managed']]) {
    const { env } = fixture(t)
    const result = spawnSync('bash', [launcher, '--dry-run', ...args], { env, encoding: 'utf8' })
    assert.notEqual(result.status, 0)
  }
  assert.deepEqual(readdirSync(directory), [])
})
