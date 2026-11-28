import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { StateStore } from '../src/state/store.js'
import {
  DeviceTrustStore,
  MAX_PENDING_CANDIDATES,
  classifyOperation,
  generateGrantId,
  normalizeRights,
  rightsSubset,
} from '../src/security/grants.js'
import { PluginError } from '../src/errors.js'

const FP = 'host-fingerprint-1'
const ALL = ['read', 'write', 'approve']
const KEY_A = Buffer.alloc(32, 0xa1)
const KEY_B = Buffer.alloc(32, 0xa2)
const KEY_C = Buffer.alloc(32, 0xa3)
const KEY_D = Buffer.alloc(32, 0xa4)
const KEY_E = Buffer.alloc(32, 0xa5)

let serial = 0
function nextId(prefix = 'candidate') {
  serial += 1
  return `${prefix}-${serial}`
}

function memory(clock = 1000) {
  let now = clock
  const store = new StateStore({ path: ':memory:', now: () => now })
  const trust = new DeviceTrustStore({ store, now: () => now })
  return {
    store,
    trust,
    now: () => now,
    advance: (ms) => { now += ms },
    setClock: (value) => { now = value },
  }
}

function stage(trust, {
  candidateId = nextId(),
  method = 'local',
  key = KEY_A,
  deviceId = 'phone-a',
  deviceLabel = 'Phone A',
  useRights = ALL,
  grantableRights = [],
  addDevice = false,
  sponsorGrantId = null,
  sponsorRevision = null,
  expiresAt = 10_000,
  now = 1000,
} = {}) {
  return trust.stageCandidate({
    candidateId,
    method,
    hostKeyFingerprint: FP,
    candidatePublicKey: key,
    deviceId,
    deviceLabel,
    useRights,
    grantableRights,
    addDevice,
    sponsorGrantId,
    sponsorRevision,
    expiresAt,
    now,
  })
}

function commitOwner(trust, {
  key = KEY_A,
  useRights = ALL,
  grantableRights = [],
  addDevice = false,
  candidateId = nextId(),
  expiresAt = 10_000,
  now = 1000,
} = {}) {
  const staged = stage(trust, { candidateId, key, useRights, grantableRights, addDevice, expiresAt, now })
  const result = trust.commitEnrollment({
    candidateId,
    candidatePublicKey: key,
    useRights,
    grantableRights,
    addDevice,
    approver: { kind: 'owner' },
    now,
  })
  return { ...result, candidateId, staged }
}

function sponsorFor(trust, sponsorGrant, {
  key = KEY_B,
  useRights = ['read'],
  candidateId = nextId(),
  now = 1000,
} = {}) {
  stage(trust, {
    candidateId,
    method: 'sponsor',
    key,
    deviceId: 'phone-b',
    useRights,
    sponsorGrantId: sponsorGrant.grantId,
    sponsorRevision: sponsorGrant.revision,
    now,
  })
  return trust.commitEnrollment({
    candidateId,
    candidatePublicKey: key,
    useRights,
    approver: { kind: 'sponsor', grantId: sponsorGrant.grantId, revision: sponsorGrant.revision },
    now,
  })
}

function expectCode(fn, code) {
  assert.throws(fn, (error) => error instanceof PluginError && error.code === code)
}

test('rights helpers normalize, sort, reject unknown entries and answer subsets', () => {
  assert.deepEqual(normalizeRights(['approve', 'read']), ['read', 'approve'])
  assert.equal(rightsSubset(['read'], ALL), true)
  assert.equal(rightsSubset(['write'], ['read']), false)
  expectCode(() => normalizeRights('read'), 'invalid_argument')
  expectCode(() => normalizeRights(['admin']), 'invalid_argument')
  expectCode(() => normalizeRights(['read', 'read']), 'invalid_argument')
  assert.equal(classifyOperation('send_message').right, 'write')
  assert.equal(classifyOperation('resolve_approval').right, 'approve')
  assert.equal(classifyOperation('ping').classified, true)
  assert.equal(classifyOperation('ping').right, null)
  assert.equal(classifyOperation('unknown_operation').classified, false)
  assert.equal(classifyOperation(undefined).classified, false)
})

test('grant identities are high-entropy and independent from any label', () => {
  const first = generateGrantId()
  const second = generateGrantId()
  assert.match(first, /^grant_[A-Za-z0-9_-]{43}$/)
  assert.notEqual(first, second)
  assert.equal(first.includes('Phone'), false)
})

test('owner approval commits one grant and an exact retry returns the original decision', () => {
  const { trust } = memory()
  const result = commitOwner(trust, {
    useRights: ['read', 'write'],
    grantableRights: ['read'],
    addDevice: true,
  })
  assert.equal(result.retried, false)
  assert.equal(result.decision.outcome, 'approved')
  assert.equal(result.grant.grantId, result.decision.grantId)
  assert.equal(result.grant.state, 'active')
  assert.equal(result.grant.revision, 1)
  assert.deepEqual(result.grant.useRights, ['read', 'write'])
  assert.deepEqual(result.grant.grantableRights, ['read'])
  assert.equal(result.grant.addDevice, true)
  assert.equal(result.grant.approverKind, 'owner')
  assert.deepEqual([...result.grant.devicePublicKey], [...KEY_A])
  assert.equal(trust.listGrants({ state: 'active' }).length, 1)

  const retried = trust.commitEnrollment({
    candidateId: result.candidateId,
    candidatePublicKey: KEY_A,
    useRights: ['read', 'write'],
    grantableRights: ['read'],
    addDevice: true,
    approver: { kind: 'owner' },
  })
  assert.equal(retried.retried, true)
  assert.equal(retried.decision.grantId, result.grant.grantId)
  assert.equal(trust.listGrants({ state: 'active' }).length, 1)
  assert.equal(trust.findActiveGrant({ hostKeyFingerprint: FP, devicePublicKey: KEY_A }).grantId, result.grant.grantId)
  assert.equal(trust.isGrantActive(result.grant.grantId), true)
})

test('a competing approval for the same key has exactly one winner and never merges rights', () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-grants-race-'))
  const path = join(dir, 'state.sqlite')
  const firstStore = new StateStore({ path })
  const secondStore = new StateStore({ path })
  try {
    const first = new DeviceTrustStore({ store: firstStore, now: () => 1000 })
    const second = new DeviceTrustStore({ store: secondStore, now: () => 1000 })
    const firstCandidate = stage(first, { key: KEY_A, useRights: ['read'], candidateId: 'race-first' })
    stage(second, { key: KEY_A, useRights: ['write'], candidateId: 'race-second' })
    assert.equal(firstCandidate.created, true)

    const winner = first.commitEnrollment({
      candidateId: 'race-first',
      candidatePublicKey: KEY_A,
      useRights: ['read'],
      approver: { kind: 'owner' },
    })
    expectCode(() => second.commitEnrollment({
      candidateId: 'race-second',
      candidatePublicKey: KEY_A,
      useRights: ['write'],
      approver: { kind: 'owner' },
    }), 'conflict')

    const active = first.findActiveGrant({ hostKeyFingerprint: FP, devicePublicKey: KEY_A })
    assert.equal(active.grantId, winner.grant.grantId)
    assert.deepEqual(active.useRights, ['read'])
    assert.equal(first.listGrants({ state: 'active' }).length, 1)

    // The loser stays closed across a restart, even after the winner leaves.
    firstStore.close()
    secondStore.close()
    const reopened = new StateStore({ path })
    try {
      const trust = new DeviceTrustStore({ store: reopened, now: () => 1000 })
      expectCode(() => trust.commitEnrollment({
        candidateId: 'race-second',
        candidatePublicKey: KEY_A,
        useRights: ['write'],
        approver: { kind: 'owner' },
      }), 'conflict')
      assert.deepEqual(trust.findActiveGrant({ hostKeyFingerprint: FP, devicePublicKey: KEY_A }).useRights, ['read'])
    } finally {
      reopened.close()
    }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('a sponsor can issue only inside its current ceiling, needs add-device and delegates no ceiling', () => {
  const { trust } = memory()
  const owner = commitOwner(trust, {
    useRights: ['read', 'write', 'approve'],
    grantableRights: ['read', 'write'],
    addDevice: true,
  })

  // Missing add-device authority.
  const noAdd = commitOwner(trust, { key: KEY_D, useRights: ['read'], grantableRights: ['read'], addDevice: false })
  stage(trust, {
    key: KEY_C,
    method: 'sponsor',
    sponsorGrantId: noAdd.grant.grantId,
    sponsorRevision: noAdd.grant.revision,
    useRights: ['read'],
    candidateId: 'no-add-child',
  })
  expectCode(() => trust.commitEnrollment({
    candidateId: 'no-add-child',
    candidatePublicKey: KEY_C,
    useRights: ['read'],
    approver: { kind: 'sponsor', grantId: noAdd.grant.grantId, revision: noAdd.grant.revision },
  }), 'forbidden')

  // Rights above the sponsor ceiling.
  stage(trust, {
    key: KEY_C,
    method: 'sponsor',
    sponsorGrantId: owner.grant.grantId,
    sponsorRevision: owner.grant.revision,
    useRights: ['read', 'approve'],
    candidateId: 'above-ceiling',
  })
  expectCode(() => trust.commitEnrollment({
    candidateId: 'above-ceiling',
    candidatePublicKey: KEY_C,
    useRights: ['read', 'approve'],
    approver: { kind: 'sponsor', grantId: owner.grant.grantId, revision: owner.grant.revision },
  }), 'forbidden')

  // A sponsored grant cannot carry a ceiling or add-device.
  stage(trust, {
    key: KEY_C,
    method: 'sponsor',
    sponsorGrantId: owner.grant.grantId,
    sponsorRevision: owner.grant.revision,
    useRights: ['read'],
    grantableRights: ['read'],
    candidateId: 'delegate-ceiling',
  })
  expectCode(() => trust.commitEnrollment({
    candidateId: 'delegate-ceiling',
    candidatePublicKey: KEY_C,
    useRights: ['read'],
    grantableRights: ['read'],
    approver: { kind: 'sponsor', grantId: owner.grant.grantId, revision: owner.grant.revision },
  }), 'forbidden')

  const child = sponsorFor(trust, owner.grant, { key: KEY_C })
  assert.deepEqual(child.grant.useRights, ['read'])
  assert.deepEqual(child.grant.grantableRights, [])
  assert.equal(child.grant.addDevice, false)
  assert.equal(child.grant.approverKind, 'sponsor')
  assert.equal(child.grant.sponsorGrantId, owner.grant.grantId)

  // A sponsor commit must state the revision it observed.
  stage(trust, {
    key: KEY_B,
    method: 'sponsor',
    sponsorGrantId: owner.grant.grantId,
    useRights: ['read'],
    candidateId: 'missing-sponsor-revision',
  })
  expectCode(() => trust.commitEnrollment({
    candidateId: 'missing-sponsor-revision',
    candidatePublicKey: KEY_B,
    useRights: ['read'],
    approver: { kind: 'sponsor', grantId: owner.grant.grantId },
  }), 'invalid_argument')
})

test('sponsor authority and revision are rechecked at commit', () => {
  const { trust } = memory()
  const owner = commitOwner(trust, {
    useRights: ALL,
    grantableRights: ['read', 'write'],
    addDevice: true,
  })
  stage(trust, {
    key: KEY_C,
    method: 'sponsor',
    sponsorGrantId: owner.grant.grantId,
    sponsorRevision: owner.grant.revision,
    useRights: ['read', 'write'],
    candidateId: 'stale-sponsor',
  })
  // The owner reduces the ceiling after the candidate was displayed.
  trust.editGrant({ grantId: owner.grant.grantId, actor: { kind: 'owner' }, grantableRights: ['read'] })
  // The requested write rights no longer fit the current ceiling, even at the
  // observed revision.
  expectCode(() => trust.commitEnrollment({
    candidateId: 'stale-sponsor',
    candidatePublicKey: KEY_C,
    useRights: ['read', 'write'],
    approver: { kind: 'sponsor', grantId: owner.grant.grantId, revision: owner.grant.revision + 1 },
  }), 'forbidden')

  // A stale observed revision conflicts before any rights are considered.
  expectCode(() => trust.commitEnrollment({
    candidateId: 'stale-sponsor',
    candidatePublicKey: KEY_C,
    useRights: ['read', 'write'],
    approver: { kind: 'sponsor', grantId: owner.grant.grantId, revision: owner.grant.revision },
  }), 'conflict')

  // Changing the approved rights on the same candidate is a different request.
  expectCode(() => trust.commitEnrollment({
    candidateId: 'stale-sponsor',
    candidatePublicKey: KEY_C,
    useRights: ['read'],
    approver: { kind: 'sponsor', grantId: owner.grant.grantId, revision: owner.grant.revision + 1 },
  }), 'conflict')

  // A fresh candidate within the reduced live ceiling succeeds.
  const fresh = stage(trust, {
    key: KEY_C,
    method: 'sponsor',
    sponsorGrantId: owner.grant.grantId,
    sponsorRevision: owner.grant.revision + 1,
    useRights: ['read'],
    candidateId: 'fresh-sponsor',
  })
  assert.equal(fresh.created, true)
  const reduced = trust.commitEnrollment({
    candidateId: 'fresh-sponsor',
    candidatePublicKey: KEY_C,
    useRights: ['read'],
    approver: { kind: 'sponsor', grantId: owner.grant.grantId, revision: owner.grant.revision + 1 },
  })
  assert.deepEqual(reduced.grant.useRights, ['read'])
})

test('reducing use rights intersects the ceiling and increasing them never regrows it', () => {
  const { trust } = memory()
  const owner = commitOwner(trust, {
    useRights: ALL,
    grantableRights: ['read', 'write'],
    addDevice: true,
  })
  const shrunk = trust.editGrant({
    grantId: owner.grant.grantId,
    actor: { kind: 'owner' },
    useRights: ['read', 'write'],
    expectedRevision: owner.grant.revision,
  })
  assert.equal(shrunk.changed, true)
  assert.equal(shrunk.grant.revision, 2)
  assert.deepEqual(shrunk.grant.grantableRights, ['read', 'write'])

  const tightened = trust.editGrant({
    grantId: owner.grant.grantId,
    actor: { kind: 'owner' },
    useRights: ['read'],
    expectedRevision: 2,
  })
  assert.deepEqual(tightened.grant.useRights, ['read'])
  assert.deepEqual(tightened.grant.grantableRights, ['read'])
  assert.equal(tightened.grant.revision, 3)

  // A ceiling outside current use rights is always rejected.
  expectCode(() => trust.editGrant({
    grantId: owner.grant.grantId,
    actor: { kind: 'owner' },
    grantableRights: ['read', 'write'],
  }), 'forbidden')

  const regrownUse = trust.editGrant({
    grantId: owner.grant.grantId,
    actor: { kind: 'owner' },
    useRights: ['read', 'write', 'approve'],
    expectedRevision: 3,
  })
  assert.deepEqual(regrownUse.grant.useRights, ALL)
  assert.deepEqual(regrownUse.grant.grantableRights, ['read'])
  assert.equal(regrownUse.grant.revision, 4)

  // A stale revision conflicts.
  expectCode(() => trust.editGrant({
    grantId: owner.grant.grantId,
    actor: { kind: 'owner' },
    useRights: ['read'],
    expectedRevision: 2,
  }), 'conflict')
  // A no-op edit does not advance the revision.
  const noop = trust.editGrant({ grantId: owner.grant.grantId, actor: { kind: 'owner' }, useRights: ALL })
  assert.equal(noop.changed, false)
  assert.equal(noop.grant.revision, 4)
  // Only the owner may set the ceiling or add-device.
  expectCode(() => trust.editGrant({
    grantId: owner.grant.grantId,
    actor: { kind: 'sponsor', grantId: 'anything' },
    grantableRights: ['read'],
  }), 'forbidden')
})

test('sponsor edits stay inside its ceiling and cannot touch a foreign child', () => {
  const { trust } = memory()
  const owner = commitOwner(trust, { useRights: ALL, grantableRights: ['read', 'write'], addDevice: true })
  const child = sponsorFor(trust, owner.grant, { key: KEY_B, useRights: ['read'] })
  const other = sponsorFor(trust, owner.grant, { key: KEY_C, useRights: ['read'] })

  const raised = trust.editGrant({
    grantId: child.grant.grantId,
    actor: { kind: 'sponsor', grantId: owner.grant.grantId },
    useRights: ['read', 'write'],
  })
  assert.deepEqual(raised.grant.useRights, ['read', 'write'])
  expectCode(() => trust.editGrant({
    grantId: child.grant.grantId,
    actor: { kind: 'sponsor', grantId: owner.grant.grantId },
    useRights: ['read', 'approve'],
  }), 'forbidden')
  expectCode(() => trust.editGrant({
    grantId: other.grant.grantId,
    actor: { kind: 'sponsor', grantId: child.grant.grantId },
    useRights: ['read'],
  }), 'forbidden')
  // Reducing a child's rights intersects its (empty) ceiling without error.
  const reduced = trust.editGrant({
    grantId: child.grant.grantId,
    actor: { kind: 'sponsor', grantId: owner.grant.grantId },
    useRights: ['read'],
  })
  assert.deepEqual(reduced.grant.grantableRights, [])
  assert.equal(reduced.grant.revision, 3)
})

test('revocation fences pre-revocation candidates, keeps children live and never revives', () => {
  const { trust } = memory()
  const owner = commitOwner(trust, { useRights: ALL, grantableRights: ALL, addDevice: true })
  const child = sponsorFor(trust, owner.grant, { key: KEY_B, useRights: ['read'] })
  stage(trust, {
    key: KEY_C,
    method: 'sponsor',
    sponsorGrantId: owner.grant.grantId,
    sponsorRevision: owner.grant.revision,
    useRights: ['read'],
    candidateId: 'late-child',
  })
  stage(trust, { key: KEY_A, candidateId: 'late-same-key', useRights: ['read'] })

  const result = trust.revokeGrants({ grantIds: [owner.grant.grantId, owner.grant.grantId, 'missing-grant'] })
  assert.deepEqual(result.revoked, [owner.grant.grantId])
  assert.deepEqual(result.unknown, ['missing-grant'])
  assert.deepEqual(result.alreadyRevoked, [])
  assert.deepEqual(new Set(result.fencedCandidates), new Set(['late-child', 'late-same-key']))
  assert.equal(trust.getGrant(owner.grant.grantId).state, 'revoked')
  assert.notEqual(trust.getGrant(owner.grant.grantId).revokedAt, null)
  assert.equal(trust.getGrant(child.grant.grantId).state, 'active')
  assert.deepEqual(trust.listChildren(owner.grant.grantId).map((grant) => grant.grantId), [child.grant.grantId])

  // Late losing attempts stay closed.
  expectCode(() => trust.commitEnrollment({
    candidateId: 'late-child',
    candidatePublicKey: KEY_C,
    useRights: ['read'],
    approver: { kind: 'sponsor', grantId: owner.grant.grantId },
  }), 'pairing_fenced')
  expectCode(() => trust.commitEnrollment({
    candidateId: 'late-same-key',
    candidatePublicKey: KEY_A,
    useRights: ['read'],
    approver: { kind: 'owner' },
  }), 'pairing_fenced')

  // An exact retry observes the original decision and the revoked fence.
  const retried = trust.commitEnrollment({
    candidateId: owner.candidateId,
    candidatePublicKey: KEY_A,
    useRights: ALL,
    grantableRights: ALL,
    addDevice: true,
    approver: { kind: 'owner' },
  })
  assert.equal(retried.retried, true)
  assert.equal(retried.grant.state, 'revoked')
  assert.equal(trust.findActiveGrant({ hostKeyFingerprint: FP, devicePublicKey: KEY_A }), null)

  // A conflicting retry after revocation cannot alter or revive anything.
  expectCode(() => trust.commitEnrollment({
    candidateId: owner.candidateId,
    candidatePublicKey: KEY_A,
    useRights: ['read'],
    approver: { kind: 'owner' },
  }), 'conflict')
})

test('a second revocation reports the grant as already revoked and changes nothing', () => {
  const { trust } = memory()
  const owner = commitOwner(trust, { useRights: ALL, grantableRights: ALL, addDevice: true })
  trust.revokeGrants({ grantIds: [owner.grant.grantId] })
  const again = trust.revokeGrants({ grantIds: [owner.grant.grantId] })
  assert.deepEqual(again.revoked, [])
  assert.deepEqual(again.alreadyRevoked, [owner.grant.grantId])
  expectCode(() => trust.revokeGrants({ grantIds: 'nope' }), 'invalid_argument')
  expectCode(() => trust.revokeGrants({ grantIds: [''] }), 'invalid_argument')
})

test('expired, changed or mismatched candidates never commit', () => {
  const { trust, advance } = memory()
  stage(trust, { candidateId: 'expiring', key: KEY_B, useRights: ['read'], expiresAt: 2000 })
  advance(1000)
  expectCode(() => trust.commitEnrollment({
    candidateId: 'expiring',
    candidatePublicKey: KEY_B,
    useRights: ['read'],
    approver: { kind: 'owner' },
  }), 'expired')

  stage(trust, { candidateId: 'wrong-key', key: KEY_B, useRights: ['read'] })
  expectCode(() => trust.commitEnrollment({
    candidateId: 'wrong-key',
    candidatePublicKey: KEY_C,
    useRights: ['read'],
    approver: { kind: 'owner' },
  }), 'conflict')

  stage(trust, { candidateId: 'different-rights', key: KEY_B, useRights: ['read'] })
  expectCode(() => trust.commitEnrollment({
    candidateId: 'different-rights',
    candidatePublicKey: KEY_B,
    useRights: ['read', 'write'],
    approver: { kind: 'owner' },
  }), 'conflict')

  // A candidate that starts above use rights is rejected before persistence.
  expectCode(() => stage(trust, { candidateId: 'bad-ceiling', key: KEY_B, useRights: ['read'], grantableRights: ['write'] }), 'forbidden')
  // Local candidates cannot carry sponsor provenance.
  expectCode(() => stage(trust, { candidateId: 'bad-local', key: KEY_B, sponsorGrantId: 'x' }), 'invalid_argument')
  // Recovery candidates need a challenge.
  expectCode(() => stage(trust, { candidateId: 'bad-recovery', method: 'recovery', key: KEY_B }), 'invalid_argument')
  // Labels are bounded.
  expectCode(() => stage(trust, { candidateId: 'bad-label', key: KEY_B, deviceLabel: 'x'.repeat(129) }), 'invalid_argument')
  // A local candidate can only be approved by the owner.
  stage(trust, { candidateId: 'local-sponsor-approver', key: KEY_B, useRights: ['read'] })
  expectCode(() => trust.commitEnrollment({
    candidateId: 'local-sponsor-approver',
    candidatePublicKey: KEY_B,
    useRights: ['read'],
    approver: { kind: 'sponsor', grantId: 'grant-anything' },
  }), 'forbidden')
  // Unknown candidates are not created by an approval.
  expectCode(() => trust.commitEnrollment({
    candidateId: 'absent',
    candidatePublicKey: KEY_B,
    useRights: ['read'],
    approver: { kind: 'owner' },
  }), 'not_found')
  expectCode(() => trust.commitEnrollment({
    candidateId: 'bad-approver',
    candidatePublicKey: KEY_B,
    useRights: ['read'],
    approver: { kind: 'admin' },
  }), 'invalid_argument')
})

test('explicit rejection is durable and exact retries observe it', () => {
  const { trust } = memory()
  stage(trust, { candidateId: 'rejected-candidate', key: KEY_B, useRights: ['read'] })
  const rejected = trust.rejectCandidate({ candidateId: 'rejected-candidate' })
  assert.equal(rejected.changed, true)
  assert.equal(rejected.candidate.state, 'rejected')
  const again = trust.rejectCandidate({ candidateId: 'rejected-candidate' })
  assert.equal(again.changed, false)
  const retried = trust.commitEnrollment({
    candidateId: 'rejected-candidate',
    candidatePublicKey: KEY_B,
    useRights: ['read'],
    approver: { kind: 'owner' },
  })
  assert.equal(retried.decision.outcome, 'rejected')
  assert.equal(retried.grant, null)
  // A different request against the decision conflicts.
  expectCode(() => trust.commitEnrollment({
    candidateId: 'rejected-candidate',
    candidatePublicKey: KEY_B,
    useRights: ['write'],
    approver: { kind: 'owner' },
  }), 'conflict')
  expectCode(() => trust.rejectCandidate({ candidateId: 'missing' }), 'not_found')
  stage(trust, { candidateId: 'reject-twice', key: KEY_C, useRights: ['read'] })
  trust.rejectCandidate({ candidateId: 'reject-twice' })
  assert.equal(trust.rejectCandidate({ candidateId: 'reject-twice' }).changed, false)
})

test('candidate staging is idempotent for the same request and conflicts on a changed one', () => {
  const { trust } = memory()
  const first = stage(trust, { candidateId: 'idempotent', key: KEY_B, useRights: ['read'] })
  assert.equal(first.created, true)
  const again = stage(trust, { candidateId: 'idempotent', key: KEY_B, useRights: ['read'] })
  assert.equal(again.created, false)
  expectCode(() => stage(trust, { candidateId: 'idempotent', key: KEY_B, useRights: ['write'] }), 'conflict')
  expectCode(() => stage(trust, { candidateId: 'idempotent', key: KEY_C, useRights: ['read'] }), 'conflict')
  // Already expired at staging is refused.
  expectCode(() => stage(trust, { candidateId: 'past', key: KEY_C, expiresAt: 1000, now: 1000 }), 'expired')
})

test('the pending candidate budget is bounded', () => {
  const { trust } = memory()
  for (let index = 0; index < MAX_PENDING_CANDIDATES; index += 1) {
    stage(trust, { candidateId: `budget-${index}`, key: KEY_B, useRights: ['read'] })
  }
  expectCode(() => stage(trust, { candidateId: 'budget-overflow', key: KEY_C, useRights: ['read'] }), 'rate_limited')
})

test('recovery commits once, consumes its challenge and cannot mint a second grant', () => {
  const { trust } = memory()
  const verifier = trust.registerRecoveryVerifier({ hostKeyFingerprint: FP, publicKey: KEY_E, allowedRights: ['read', 'write'] })
  assert.equal(verifier.revision, 1)
  assert.equal(verifier.active, true)
  assert.deepEqual(verifier.allowedRights, ['read', 'write'])
  assert.equal(trust.getActiveRecoveryVerifier(FP).revision, 1)

  // Rights above the verifier scope cannot be issued.
  expectCode(() => trust.issueRecoveryChallenge({
    hostKeyFingerprint: FP,
    verifierRevision: 1,
    candidatePublicKey: KEY_A,
    deviceId: 'phone-a',
    useRights: ['approve'],
    expiresAt: 10_000,
  }), 'forbidden')

  const challenge = trust.issueRecoveryChallenge({
    hostKeyFingerprint: FP,
    verifierRevision: 1,
    candidatePublicKey: KEY_A,
    deviceId: 'phone-a',
    deviceLabel: 'Phone A',
    useRights: ['read', 'write'],
    expiresAt: 9000,
    challengeId: 'recovery-one',
  })
  assert.equal(challenge.challengeId, 'recovery-one')
  assert.match(challenge.nonce, /^[A-Za-z0-9_-]{43}$/)
  assert.equal(trust.getRecoveryChallenge('recovery-one').consumed, false)
  assert.equal(trust.getCandidate('recovery-one').method, 'recovery')

  const committed = trust.commitRecovery({
    challengeId: 'recovery-one',
    nonce: challenge.nonce,
    verifierRevision: 1,
    candidatePublicKey: KEY_A,
    useRights: ['read', 'write'],
  })
  assert.equal(committed.decision.outcome, 'approved')
  assert.equal(committed.grant.approverKind, 'recovery')
  assert.equal(committed.grant.recoveryVerifierRevision, 1)
  assert.deepEqual(committed.grant.grantableRights, [])
  assert.equal(committed.grant.addDevice, false)
  assert.equal(trust.getRecoveryChallenge('recovery-one').consumed, true)

  const retried = trust.commitRecovery({
    challengeId: 'recovery-one',
    nonce: challenge.nonce,
    verifierRevision: 1,
    candidatePublicKey: KEY_A,
    useRights: ['read', 'write'],
  })
  assert.equal(retried.retried, true)
  assert.equal(retried.decision.grantId, committed.grant.grantId)
  assert.equal(trust.listGrants({ state: 'active' }).length, 1)

  // A wrong nonce is never an exact retry.
  expectCode(() => trust.commitRecovery({
    challengeId: 'recovery-one',
    nonce: 'not-the-nonce',
    verifierRevision: 1,
    candidatePublicKey: KEY_A,
    useRights: ['read', 'write'],
  }), 'unauthorized')
  expectCode(() => trust.commitRecovery({
    challengeId: 'recovery-one',
    nonce: challenge.nonce,
    verifierRevision: 1,
    candidatePublicKey: KEY_B,
    useRights: ['read', 'write'],
  }), 'conflict')
  // A proof bound to a different revision cannot commit.
  expectCode(() => trust.commitRecovery({
    challengeId: 'recovery-one',
    nonce: challenge.nonce,
    verifierRevision: 2,
    candidatePublicKey: KEY_A,
    useRights: ['read', 'write'],
  }), 'conflict')
  expectCode(() => trust.commitRecovery({
    challengeId: 'absent',
    nonce: challenge.nonce,
    verifierRevision: 1,
    candidatePublicKey: KEY_A,
    useRights: ['read'],
  }), 'not_found')
})

test('recovery rotation fences the old revision and re-registration gets a fresh revision', () => {
  const { trust } = memory()
  trust.registerRecoveryVerifier({ hostKeyFingerprint: FP, publicKey: KEY_E, allowedRights: ['read', 'write'] })
  const challenge = trust.issueRecoveryChallenge({
    hostKeyFingerprint: FP,
    verifierRevision: 1,
    candidatePublicKey: KEY_A,
    deviceId: 'phone-a',
    useRights: ['read'],
    expiresAt: 9000,
    challengeId: 'rotation-old',
  })
  // Rotation retires revision 1 and fences its pending candidate.
  const rotated = trust.registerRecoveryVerifier({ hostKeyFingerprint: FP, publicKey: KEY_D, allowedRights: ['read'] })
  assert.equal(rotated.revision, 2)
  assert.equal(rotated.active, true)
  assert.equal(trust.getRecoveryVerifier(1).active, false)
  assert.equal(trust.getCandidate('rotation-old').state, 'fenced')
  expectCode(() => trust.commitRecovery({
    challengeId: 'rotation-old',
    nonce: challenge.nonce,
    verifierRevision: 1,
    candidatePublicKey: KEY_A,
    useRights: ['read'],
  }), 'pairing_fenced')
  // An old revision cannot issue a new challenge.
  expectCode(() => trust.issueRecoveryChallenge({
    hostKeyFingerprint: FP,
    verifierRevision: 1,
    candidatePublicKey: KEY_A,
    deviceId: 'phone-a',
    useRights: ['read'],
    expiresAt: 9000,
  }), 'conflict')

  // Retirement is idempotent, and re-registering the same key bytes after a
  // retirement still gets a never-reused revision.
  assert.equal(trust.retireRecoveryVerifier({ revision: 2 }).changed, true)
  assert.equal(trust.retireRecoveryVerifier({ revision: 2 }).changed, false)
  const reinstalled = trust.registerRecoveryVerifier({ hostKeyFingerprint: FP, publicKey: KEY_D, allowedRights: ['read'] })
  assert.equal(reinstalled.revision, 3)
  assert.equal(reinstalled.active, true)
  expectCode(() => trust.retireRecoveryVerifier({ revision: 99 }), 'not_found')

  // A challenge issued by the current revision still commits.
  const current = trust.issueRecoveryChallenge({
    hostKeyFingerprint: FP,
    verifierRevision: 3,
    candidatePublicKey: KEY_B,
    deviceId: 'phone-b',
    useRights: ['read'],
    expiresAt: 9000,
    challengeId: 'rotation-current',
  })
  const committed = trust.commitRecovery({
    challengeId: 'rotation-current',
    nonce: current.nonce,
    verifierRevision: 3,
    candidatePublicKey: KEY_B,
    useRights: ['read'],
  })
  assert.equal(committed.grant.state, 'active')
  assert.equal(committed.grant.recoveryVerifierRevision, 3)
  assert.equal(trust.listRecoveryVerifiers({ hostKeyFingerprint: FP }).length, 3)
})

test('a recovery policy reduction invalidates old challenges and bounds new ones', () => {
  const { trust } = memory()
  trust.registerRecoveryVerifier({ hostKeyFingerprint: FP, publicKey: KEY_E, allowedRights: ['read', 'write'] })
  const old = trust.issueRecoveryChallenge({
    hostKeyFingerprint: FP,
    verifierRevision: 1,
    candidatePublicKey: KEY_A,
    deviceId: 'phone-a',
    useRights: ['write'],
    expiresAt: 9000,
    challengeId: 'policy-old',
  })
  const revised = trust.updateRecoveryPolicy({ revision: 1, allowedRights: ['read'] })
  assert.equal(revised.revision, 2)
  assert.deepEqual(revised.allowedRights, ['read'])
  assert.equal(trust.getRecoveryVerifier(1).active, false)
  assert.equal(trust.getCandidate('policy-old').state, 'fenced')
  expectCode(() => trust.commitRecovery({
    challengeId: 'policy-old',
    nonce: old.nonce,
    verifierRevision: 1,
    candidatePublicKey: KEY_A,
    useRights: ['write'],
  }), 'pairing_fenced')
  expectCode(() => trust.updateRecoveryPolicy({ revision: 1, allowedRights: ['read'] }), 'conflict')
  expectCode(() => trust.issueRecoveryChallenge({
    hostKeyFingerprint: FP,
    verifierRevision: 2,
    candidatePublicKey: KEY_A,
    deviceId: 'phone-a',
    useRights: ['write'],
    expiresAt: 9000,
  }), 'forbidden')
  const current = trust.issueRecoveryChallenge({
    hostKeyFingerprint: FP,
    verifierRevision: 2,
    candidatePublicKey: KEY_A,
    deviceId: 'phone-a',
    useRights: ['read'],
    expiresAt: 9000,
    challengeId: 'policy-current',
  })
  const committed = trust.commitRecovery({
    challengeId: 'policy-current',
    nonce: current.nonce,
    verifierRevision: 2,
    candidatePublicKey: KEY_A,
    useRights: ['read'],
  })
  assert.deepEqual(committed.grant.useRights, ['read'])
})

test('restart keeps grants, decisions and consumed challenges durable', () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-grants-restart-'))
  const path = join(dir, 'state.sqlite')
  try {
    let ownerGrantId
    let recoveryNonce
    let firstStore = new StateStore({ path })
    try {
      const trust = new DeviceTrustStore({ store: firstStore, now: () => 1000 })
      const owner = commitOwner(trust, { useRights: ALL, grantableRights: ['read'], addDevice: true })
      ownerGrantId = owner.grant.grantId
      trust.registerRecoveryVerifier({ hostKeyFingerprint: FP, publicKey: KEY_E, allowedRights: ['read'] })
      const challenge = trust.issueRecoveryChallenge({
        hostKeyFingerprint: FP,
        verifierRevision: 1,
        candidatePublicKey: KEY_B,
        deviceId: 'phone-b',
        useRights: ['read'],
        expiresAt: 9000,
        challengeId: 'restart-challenge',
      })
      recoveryNonce = challenge.nonce
      trust.commitRecovery({
        challengeId: 'restart-challenge',
        nonce: recoveryNonce,
        verifierRevision: 1,
        candidatePublicKey: KEY_B,
        useRights: ['read'],
      })
    } finally {
      firstStore.close()
    }

    const secondStore = new StateStore({ path })
    try {
      const trust = new DeviceTrustStore({ store: secondStore, now: () => 1000 })
      assert.equal(trust.getGrant(ownerGrantId).state, 'active')
      assert.equal(trust.listGrants({ state: 'active' }).length, 2)
      assert.equal(trust.getActiveRecoveryVerifier(FP).revision, 1)
      assert.equal(trust.getRecoveryChallenge('restart-challenge').consumed, true)
      const replay = trust.commitRecovery({
        challengeId: 'restart-challenge',
        nonce: recoveryNonce,
        verifierRevision: 1,
        candidatePublicKey: KEY_B,
        useRights: ['read'],
      })
      assert.equal(replay.retried, true)
      assert.equal(trust.listGrants({ state: 'active' }).length, 2)
    } finally {
      secondStore.close()
    }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('operation authorization uses current rights and denies unknown operations', () => {
  const { trust } = memory()
  const owner = commitOwner(trust, {
    useRights: ['read'],
    grantableRights: [],
    addDevice: false,
  })
  const grantId = owner.grant.grantId
  assert.deepEqual(trust.authorizeOperation({ grantId, operation: 'list_sessions' }), {
    allowed: true, reason: 'ok', right: 'read', grantId, revision: 1,
  })
  assert.deepEqual(trust.authorizeOperation({ grantId, operation: 'send_message' }), {
    allowed: false, reason: 'missing_right', right: 'write', grantId, revision: 1,
  })
  assert.equal(trust.authorizeOperation({ grantId, operation: 'resolve_approval' }).reason, 'missing_right')
  assert.equal(trust.authorizeApproval(grantId).reason, 'missing_right')
  assert.equal(trust.authorizeOperation({ grantId, operation: 'not_a_wire_operation' }).reason, 'unknown_operation')
  assert.equal(trust.authorizeOperation({ grantId, operation: 'ping' }).allowed, true)
  assert.equal(trust.authorizeOperation({ grantId, operation: 'ping' }).right, null)
  assert.equal(trust.authorizeOperation({ grantId: 'missing', operation: 'ping' }).reason, 'unknown_grant')
  assert.equal(trust.authorizeRight({ grantId, right: 'read' }).allowed, true)
  assert.equal(trust.authorizeRight({ grantId, right: 'admin' }).reason, 'unknown_right')
  assert.equal(trust.authorizeRight({ grantId, right: undefined }).reason, 'unknown_right')

  trust.editGrant({ grantId, actor: { kind: 'owner' }, useRights: ALL })
  assert.equal(trust.authorizeOperation({ grantId, operation: 'send_message' }).allowed, true)
  assert.equal(trust.authorizeApproval(grantId).allowed, true)

  trust.revokeGrants({ grantIds: [grantId] })
  assert.equal(trust.authorizeOperation({ grantId, operation: 'list_sessions' }).reason, 'grant_revoked')
  assert.equal(trust.authorizeOperation({ grantId: undefined, operation: 'list_sessions' }).reason, 'unknown_grant')
})

test('the schema enforces one active grant per key with a partial unique index', () => {
  const { store } = memory()
  const db = store.trustStore().db
  const tables = new Set(db.prepare("SELECT name FROM sqlite_master WHERE type = 'table'").all().map((row) => row.name))
  for (const name of ['device_grants', 'enrollment_candidates', 'recovery_verifiers', 'recovery_challenges', 'command_journal']) {
    assert.equal(tables.has(name), true, `missing ${name}`)
  }
  const index = db.prepare("SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'device_grants_one_active'").get()
  assert.match(index.sql, /CREATE UNIQUE INDEX/)
  assert.match(index.sql, /WHERE state = 'active'/)
  db.prepare(`
    INSERT INTO device_grants (
      grant_id, host_key_fingerprint, device_public_key, device_id, device_label,
      use_rights, grantable_rights, add_device, approver_kind, sponsor_grant_id,
      recovery_verifier_revision, revision, state, created_at, updated_at, revoked_at
    ) VALUES ('manual-one', ?, ?, 'phone-a', '', '["read"]', '[]', 0, 'owner', NULL, NULL, 1, 'active', 1, 1, NULL)
  `).run(FP, KEY_A)
  assert.throws(() => db.prepare(`
    INSERT INTO device_grants (
      grant_id, host_key_fingerprint, device_public_key, device_id, device_label,
      use_rights, grantable_rights, add_device, approver_kind, sponsor_grant_id,
      recovery_verifier_revision, revision, state, created_at, updated_at, revoked_at
    ) VALUES ('manual-two', ?, ?, 'phone-a', '', '["read"]', '[]', 0, 'owner', NULL, NULL, 1, 'active', 1, 1, NULL)
  `).run(FP, KEY_A))
  // A revoked row is outside the partial index, so history can retain it.
  db.prepare("UPDATE device_grants SET state = 'revoked', revoked_at = 1 WHERE grant_id = 'manual-one'").run()
  db.prepare(`
    INSERT INTO device_grants (
      grant_id, host_key_fingerprint, device_public_key, device_id, device_label,
      use_rights, grantable_rights, add_device, approver_kind, sponsor_grant_id,
      recovery_verifier_revision, revision, state, created_at, updated_at, revoked_at
    ) VALUES ('manual-two', ?, ?, 'phone-a', '', '["read"]', '[]', 0, 'owner', NULL, NULL, 1, 'active', 1, 1, NULL)
  `).run(FP, KEY_A)
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM device_grants WHERE state = 'active'").get().n, 1)
})

test('grant reads validate bounds and list children for loss review', () => {
  const { trust } = memory()
  const owner = commitOwner(trust, { useRights: ALL, grantableRights: ALL, addDevice: true })
  const child = sponsorFor(trust, owner.grant, { key: KEY_B })
  assert.equal(trust.getGrant('missing'), null)
  assert.equal(trust.getGrant(''), null)
  assert.equal(trust.getCandidate('missing'), null)
  assert.equal(trust.getRecoveryChallenge(''), null)
  assert.deepEqual(trust.listGrants({ limit: 1 }).map((grant) => grant.grantId), [owner.grant.grantId])
  assert.deepEqual(trust.listGrants({ hostKeyFingerprint: FP, state: 'revoked' }), [])
  assert.deepEqual(trust.listChildren(owner.grant.grantId).map((grant) => grant.grantId), [child.grant.grantId])
  expectCode(() => trust.listGrants({ limit: 0 }), 'invalid_argument')
  expectCode(() => trust.listGrants({ state: 'unknown' }), 'invalid_argument')
  expectCode(() => trust.findActiveGrant({ hostKeyFingerprint: FP, devicePublicKey: Buffer.alloc(31) }), 'invalid_argument')
  expectCode(() => trust.editGrant({ grantId: 'missing', actor: { kind: 'owner' } }), 'not_found')
  expectCode(() => trust.editGrant({ grantId: owner.grant.grantId, actor: {} }), 'invalid_argument')
  expectCode(() => trust.editGrant({ grantId: owner.grant.grantId, actor: { kind: 'owner' }, addDevice: 'yes' }), 'invalid_argument')
  assert.equal(trust.getRecoveryVerifier(0), null)
  assert.equal(trust.getRecoveryVerifier('1'), null)
})