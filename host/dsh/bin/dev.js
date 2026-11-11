#!/usr/bin/env node
// Development entry for the standalone FakeDSH host.
//
// Unlike `run-fake.js`, this file supplies local Compose-stack defaults so a
// developer can run `pnpm dev` from host/dsh without exporting five
// variables first. `pnpm dev` adds Node's --watch flag, so edits under src/
// restart the process (not hot module replacement; in-memory channels are
// recreated, SQLite state survives).
//
// This is NOT the real DSH plugin loader. Use a DSH profile for real-DSH
// debugging; see README.md.

import { existsSync, mkdirSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const pluginRoot = join(here, '..')
const repoRoot = join(pluginRoot, '..', '..')
const dataDir = process.env.WHALEREIN_DATA_DIR ?? join(repoRoot, '.data')
const seedFile = process.env.WHALEREIN_FAKE_SEED ?? join(dataDir, 'dsh-plugin-seed.json')
const stateDir = process.env.WHALEREIN_STATE_DIR ?? join(dataDir, 'dsh-plugin-state')

process.env.WHALEREIN_MQTT_URL ??= 'mqtt://127.0.0.1:1883'
process.env.WHALEREIN_ALLOW_INSECURE_MQTT ??= '1'
process.env.WHALEREIN_HOST_ID ??= 'host-1'
process.env.WHALEREIN_HOST_TOKEN ??= 'dev-host-token'
process.env.WHALEREIN_STATE_DIR = stateDir
process.env.WHALEREIN_FAKE_SEED = seedFile
process.env.WHALEREIN_ADAPTER = 'fake'
process.env.WHALEREIN_LOG_LEVEL ??= 'debug'

if (!existsSync(seedFile)) {
  mkdirSync(dirname(seedFile), { recursive: true })
  writeFileSync(
    seedFile,
    `${JSON.stringify({
      sessions: [
        { sessionId: 'session-1', title: 'Demo session', cwd: '/tmp', running: false },
        { sessionId: 'session-2', title: 'Second', cwd: '/tmp', running: false },
      ],
      behavior: { approvalOnFirstMessage: true, emitOutputOnMessage: true, longRunning: false },
    }, null, 2)}\n`,
  )
}
mkdirSync(stateDir, { recursive: true })

if (process.env.WHALEREIN_SKIP_READY !== '1') {
  const ready = await fetch('http://127.0.0.1:8080/readyz')
    .then((response) => response.ok)
    .catch(() => false)
  if (!ready) {
    process.stderr.write(
      'Access is not ready at http://127.0.0.1:8080; start the stack with:\n' +
        '  docker-compose -f deploy/compose/stack.yaml up -d\n' +
        'or set WHALEREIN_SKIP_READY=1 to start anyway.\n',
    )
    process.exit(1)
  }
}

await import('./run-fake.js')
