import test from 'node:test'
import assert from 'node:assert/strict'
import { startHarness } from './support/harness.js'

function resolvePayload(approval) {
  return {
    resolve_approval: {
      approval_id: approval.approvalId,
      decision: 'APPROVAL_DECISION_APPROVE',
      expected_digest: approval.digest,
      expected_version: approval.version,
    },
  }
}

function countResolutions(harness) {
  const counts = { resolve: 0 }
  const execute = harness.adapter.execute.bind(harness.adapter)
  harness.adapter.execute = async (...args) => {
    if (args[1]?.resolve_approval) counts.resolve += 1
    return execute(...args)
  }
  return counts
}

test('a revoked device cannot deliver an approval decision on its old channel', async () => {
  const harness = await startHarness({ sessions: [{ sessionId: 's1', running: true }] })
  try {
    const stale = await harness.newPairedDevice('device-stale')
    const counts = countResolutions(harness)
    const approval = harness.adapter.requestApproval({ sessionId: 's1', reason: 'fixture' })
    harness.host.security.forgetPairedDevice('device-stale')
    const result = await stale.command(resolvePayload(approval))
    assert.equal(result.status, 'rejected')
    assert.equal(result.errorCode, 'unauthorized')
    assert.equal(counts.resolve, 0)
    assert.equal((await harness.adapter.getApproval(approval.approvalId)).state, 'pending')
  } finally {
    await harness.stop()
  }
})

test('revocation and a decision racing on two devices settle at most once', async () => {
  const harness = await startHarness({ sessions: [{ sessionId: 's1', running: true }] })
  try {
    const stale = await harness.newPairedDevice('device-stale')
    const active = await harness.newPairedDevice('device-active')
    const counts = countResolutions(harness)
    const approval = harness.adapter.requestApproval({ sessionId: 's1', reason: 'fixture' })
    const accepted = await active.command(resolvePayload(approval))
    assert.equal(accepted.status, 'succeeded')
    assert.equal(counts.resolve, 1)
    harness.host.security.forgetPairedDevice('device-stale')
    const late = await stale.command(resolvePayload(approval))
    assert.equal(late.status, 'rejected')
    assert.equal(late.errorCode, 'unauthorized')
    assert.equal(counts.resolve, 1)
    assert.equal((await harness.adapter.getApproval(approval.approvalId)).state, 'approved')
  } finally {
    await harness.stop()
  }
})

test('a revoked device cannot reach any source mutation, including approval resolution', async () => {
  const harness = await startHarness({ sessions: [{ sessionId: 's1', running: true }] })
  try {
    const stale = await harness.newPairedDevice('device-stale')
    let sourceCalls = 0
    const execute = harness.adapter.execute.bind(harness.adapter)
    harness.adapter.execute = async (...args) => { sourceCalls += 1; return execute(...args) }
    harness.host.security.forgetPairedDevice('device-stale')
    const approval = harness.adapter.requestApproval({ sessionId: 's1', reason: 'fixture' })
    const result = await stale.command(resolvePayload(approval))
    assert.equal(result.status, 'rejected')
    assert.equal(result.errorCode, 'unauthorized')
    assert.equal(sourceCalls, 0)
    assert.equal((await harness.adapter.getApproval(approval.approvalId)).state, 'pending')
  } finally {
    await harness.stop()
  }
})
