import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')
const contract = join(root, 'contracts', 'client-environment')

for (const name of ['catalog.json', 'cases.json']) {
  test(`config-env/AC-17/contract-copy ${name} matches the canonical source`, () => {
    const canonical = readFileSync(join(contract, name))
    const generated = readFileSync(join(root, 'host', 'runtime', 'src', 'configuration', name))
    assert.deepEqual(generated, canonical, `regenerate with scripts/sync-client-environment-contract.mjs`)
  })
}

test('config-env/AC-17/corpus-files referenced by cases.json exist', () => {
  const cases = JSON.parse(readFileSync(join(contract, 'cases.json'), 'utf8'))
  assert.equal(cases.cases.length > 0, true)
  for (const entry of cases.cases) readFileSync(join(contract, entry.file))
  for (const entry of cases.resolution) readFileSync(join(contract, entry.file))
})
