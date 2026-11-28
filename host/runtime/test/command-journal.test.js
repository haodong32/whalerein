import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { StateStore } from '../src/state/store.js'
import {
  CommandJournal,
  commandIntentBinding,
  unverifiableCommand,
} from '../src/security/command-journal.js'
import { PluginError } from '../src/errors.js'

const KEY_A = Buffer.alloc(32, 0xb1)
const KEY_B = Buffer.alloc(32, 0xb2)
const BODY = Buffer.alloc(32, 0x11)
const OTHER_BODY = Buffer.alloc(32, 0x22)

function memory(clock = 1000) {
  let now = clock
  const store = new StateStore({ path: ':memory:', now: () => now })
  const journal = new CommandJournal({ store, now: () => now })
  return {
    store,
    journal,
    now: () => now,
    advance: (ms) => { now += ms },
  }
}

function expectCode(fn, code) {
  assert.throws(fn, (error) => error instanceof PluginError && error.code === code)
}

function reserve(journal, overrides = {}) {
  return journal.reserveCommand({
    commandId: 'cmd-1',
    intentDigest: BODY,
    devicePublicKey: KEY_A,
    grantId: 'grant-1',
    deadlineAt: 50_000,
    now: 1000,
    ...overrides,
  })
}

test('a fresh command reservation admits once and an exact retry is a duplicate', () => {
  const { journal } = memory()
  const first = reserve(journal)
  assert.deepEqual(first, { admitted: true, commandId: 'cmd-1', state: 'reserved' })
  const retry = reserve(journal)
  assert.deepEqual(retry, { admitted: false, reason: 'duplicate', state: 'reserved', resultPayload: null, errorCode: null })
  assert.equal(journal.countCommands(), 1)
  assert.equal(journal.countCommands('reserved'), 1)
})

test('a changed body, deadline or grant cannot reuse a reservation', () => {
  const { journal } = memory()
  reserve(journal)
  assert.deepEqual(reserve(journal, { intentDigest: OTHER_BODY }), { admitted: false, reason: 'intent_mismatch' })
  assert.deepEqual(reserve(journal, { deadlineAt: 60_000 }), { admitted: false, reason: 'intent_mismatch' })
  // A foreign grant gets the same generic answer as an unknown ID.
  assert.deepEqual(reserve(journal, { grantId: 'grant-2' }), { ok: false, reason: 'unverifiable' })
  assert.deepEqual(reserve(journal, { devicePublicKey: KEY_B }), { ok: false, reason: 'unverifiable' })
  assert.equal(journal.countCommands(), 1)
})

test('a bound receipt is readable only by the reserving grant and key', () => {
  const { journal } = memory()
  reserve(journal)
  const payload = Buffer.from([9, 8, 7])
  const bound = journal.bindResult({
    commandId: 'cmd-1',
    grantId: 'grant-1',
    devicePublicKey: KEY_A,
    state: 'succeeded',
    resultPayload: payload,
  })
  assert.equal(bound.updated, true)
  assert.equal(bound.command.state, 'succeeded')

  const own = journal.lookupCommand({ commandId: 'cmd-1', grantId: 'grant-1', devicePublicKey: KEY_A })
  assert.equal(own.ok, true)
  assert.equal(own.command.state, 'succeeded')
  assert.deepEqual([...own.command.resultPayload], [9, 8, 7])
  assert.equal(own.command.deadlineAt, 50_000)

  const foreignGrant = journal.lookupCommand({ commandId: 'cmd-1', grantId: 'grant-2', devicePublicKey: KEY_A })
  const foreignKey = journal.lookupCommand({ commandId: 'cmd-1', grantId: 'grant-1', devicePublicKey: KEY_B })
  const unknown = journal.lookupCommand({ commandId: 'missing', grantId: 'grant-1', devicePublicKey: KEY_A })
  for (const answer of [foreignGrant, foreignKey, unknown]) {
    assert.deepEqual(answer, { ok: false, reason: 'unverifiable' })
    assert.equal(Object.keys(answer).length, 2)
  }
  assert.deepEqual(unverifiableCommand(), { ok: false, reason: 'unverifiable' })

  // The same terminal outcome is idempotent; a different one is refused.
  assert.equal(journal.bindResult({
    commandId: 'cmd-1',
    grantId: 'grant-1',
    devicePublicKey: KEY_A,
    state: 'succeeded',
    resultPayload: payload,
  }).updated, false)
  expectCode(() => journal.bindResult({
    commandId: 'cmd-1',
    grantId: 'grant-1',
    devicePublicKey: KEY_A,
    state: 'succeeded',
    resultPayload: Buffer.from([1]),
  }), 'conflict')
  expectCode(() => journal.bindResult({
    commandId: 'cmd-1',
    grantId: 'grant-1',
    devicePublicKey: KEY_A,
    state: 'failed',
    errorCode: 'unavailable',
  }), 'conflict')
})

test('binding a receipt from another grant, key or unknown ID is refused', () => {
  const { journal } = memory()
  reserve(journal)
  expectCode(() => journal.bindResult({
    commandId: 'cmd-1',
    grantId: 'grant-2',
    devicePublicKey: KEY_A,
    state: 'failed',
  }), 'forbidden')
  expectCode(() => journal.bindResult({
    commandId: 'cmd-1',
    grantId: 'grant-1',
    devicePublicKey: KEY_B,
    state: 'failed',
  }), 'forbidden')
  expectCode(() => journal.bindResult({
    commandId: 'missing',
    grantId: 'grant-1',
    devicePublicKey: KEY_A,
    state: 'failed',
  }), 'not_found')
})

test('duplicate suppression survives re-enrollment under a new grant ID', () => {
  const { journal } = memory()
  reserve(journal, { grantId: 'grant-old' })
  journal.bindResult({
    commandId: 'cmd-1',
    grantId: 'grant-old',
    devicePublicKey: KEY_A,
    state: 'succeeded',
    resultPayload: Buffer.from([1, 2]),
  })

  // The same phone re-enrolls and gets a fresh grant for the same key.
  const replay = reserve(journal, { grantId: 'grant-new' })
  assert.deepEqual(replay, { ok: false, reason: 'unverifiable' })
  assert.equal(journal.countCommands(), 1)
  const lookup = journal.lookupCommand({ commandId: 'cmd-1', grantId: 'grant-new', devicePublicKey: KEY_A })
  assert.deepEqual(lookup, { ok: false, reason: 'unverifiable' })
  // The original grant still owns the receipt.
  assert.equal(journal.lookupCommand({ commandId: 'cmd-1', grantId: 'grant-old', devicePublicKey: KEY_A }).command.state, 'succeeded')
})

test('restart turns an unfinished reservation into unknown, never success', () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-journal-restart-'))
  const path = join(dir, 'state.sqlite')
  try {
    let firstStore = new StateStore({ path })
    try {
      const journal = new CommandJournal({ store: firstStore, now: () => 1000 })
      reserve(journal, { commandId: 'crash-command' })
      reserve(journal, { commandId: 'receipt-command' })
      journal.bindResult({
        commandId: 'receipt-command',
        grantId: 'grant-1',
        devicePublicKey: KEY_A,
        state: 'succeeded',
        resultPayload: Buffer.from([5]),
      })
    } finally {
      firstStore.close()
    }

    const secondStore = new StateStore({ path })
    try {
      const journal = new CommandJournal({ store: secondStore, now: () => 2000 })
      assert.equal(journal.markReservedUnknown(), 1)
      const lookup = journal.lookupCommand({ commandId: 'crash-command', grantId: 'grant-1', devicePublicKey: KEY_A })
      assert.equal(lookup.command.state, 'unknown')
      assert.equal(lookup.command.errorCode, 'unavailable')
      assert.equal(lookup.command.resultPayload, null)
      // The identity is retained, so a resubmission is still suppressed.
      const retry = reserve(journal, { commandId: 'crash-command', now: 2000 })
      assert.equal(retry.admitted, false)
      assert.equal(retry.state, 'unknown')
      // A proven receipt is untouched by startup recovery.
      assert.deepEqual(
        [...journal.lookupCommand({ commandId: 'receipt-command', grantId: 'grant-1', devicePublicKey: KEY_A }).command.resultPayload],
        [5],
      )
    } finally {
      secondStore.close()
    }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('receipt bytes are pruned on a bound while reservation identity remains', () => {
  const { journal, advance } = memory()
  reserve(journal)
  journal.bindResult({
    commandId: 'cmd-1',
    grantId: 'grant-1',
    devicePublicKey: KEY_A,
    state: 'succeeded',
    resultPayload: Buffer.from([1]),
  })
  advance(8 * 24 * 60 * 60 * 1000)
  assert.equal(journal.pruneResults({ now: 8 * 24 * 60 * 60 * 1000 + 1000 }), 1)
  const lookup = journal.lookupCommand({ commandId: 'cmd-1', grantId: 'grant-1', devicePublicKey: KEY_A })
  assert.equal(lookup.command.state, 'succeeded')
  assert.equal(lookup.command.resultPayload, null)
  assert.equal(journal.pruneResults({ now: 8 * 24 * 60 * 60 * 1000 + 2000 }), 0)
})

test('intent binding is deterministic and covers the original deadline and key', () => {
  const base = commandIntentBinding({ commandId: 'cmd-1', intentDigest: BODY, devicePublicKey: KEY_A, deadlineAt: 50_000 })
  assert.deepEqual(base, commandIntentBinding({ commandId: 'cmd-1', intentDigest: BODY, devicePublicKey: KEY_A, deadlineAt: 50_000 }))
  assert.notDeepEqual(base, commandIntentBinding({ commandId: 'cmd-1', intentDigest: OTHER_BODY, devicePublicKey: KEY_A, deadlineAt: 50_000 }))
  assert.notDeepEqual(base, commandIntentBinding({ commandId: 'cmd-2', intentDigest: BODY, devicePublicKey: KEY_A, deadlineAt: 50_000 }))
  assert.notDeepEqual(base, commandIntentBinding({ commandId: 'cmd-1', intentDigest: BODY, devicePublicKey: KEY_B, deadlineAt: 50_000 }))
  assert.notDeepEqual(base, commandIntentBinding({ commandId: 'cmd-1', intentDigest: BODY, devicePublicKey: KEY_A, deadlineAt: 50_001 }))
  expectCode(() => commandIntentBinding({ commandId: '', intentDigest: BODY, devicePublicKey: KEY_A, deadlineAt: 1 }), 'invalid_argument')
  expectCode(() => commandIntentBinding({ commandId: 'c', intentDigest: Buffer.alloc(31), devicePublicKey: KEY_A, deadlineAt: 1 }), 'invalid_argument')
  expectCode(() => commandIntentBinding({ commandId: 'c', intentDigest: BODY, devicePublicKey: KEY_A, deadlineAt: 0 }), 'invalid_argument')
})

test('journal inputs are validated instead of guessed', () => {
  const { journal } = memory()
  expectCode(() => reserve(journal, { commandId: '' }), 'invalid_argument')
  expectCode(() => reserve(journal, { grantId: '' }), 'invalid_argument')
  expectCode(() => reserve(journal, { devicePublicKey: Buffer.alloc(31) }), 'invalid_argument')
  expectCode(() => reserve(journal, { intentDigest: Buffer.alloc(31) }), 'invalid_argument')
  expectCode(() => reserve(journal, { deadlineAt: 0 }), 'invalid_argument')
  expectCode(() => reserve(journal, { now: -1 }), 'invalid_argument')
  reserve(journal)
  expectCode(() => journal.bindResult({ commandId: 'cmd-1', grantId: 'grant-1', devicePublicKey: KEY_A, state: 'reserved' }), 'invalid_argument')
  expectCode(() => journal.bindResult({ commandId: 'cmd-1', grantId: 'grant-1', devicePublicKey: KEY_A, state: 'failed', resultPayload: 'no' }), 'invalid_argument')
  expectCode(() => journal.bindResult({ commandId: 'cmd-1', grantId: 'grant-1', devicePublicKey: KEY_A, state: 'failed', errorCode: 5 }), 'invalid_argument')
  expectCode(() => journal.countCommands('bogus'), 'invalid_argument')
  expectCode(() => journal.lookupCommand({ commandId: '', grantId: 'grant-1', devicePublicKey: KEY_A }), 'invalid_argument')
})