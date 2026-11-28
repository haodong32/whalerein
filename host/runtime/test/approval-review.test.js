// AC-47: review facts travel only on the approval-right detail path. The list
// path is read-scoped, so it must withhold them; terminal ledger views have no
// stored review data and must report unavailable instead of reconstructing it.

import test from 'node:test'
import assert from 'node:assert/strict'
import { CommandService } from '../src/commands/service.js'
import { TestSource } from './support/source.js'
import { StateStore } from '../src/state/store.js'
import { codec } from '../src/protocol.js'
import { approvalDigest, sha256Bytes } from '../src/security/digest.js'

const proto = codec()
let serial = 0

function makeRequest({ deviceId = 'device-1', commandId = `review-cmd-${++serial}`, expiresAt = 10_000, kind = 'REQUEST_KIND_QUERY' } = {}) {
  const body = new TextEncoder().encode(`review body ${commandId}`)
  return {
    request_id: `review-req-${serial}`,
    host_id: 'host-1',
    device_id: deviceId,
    kind,
    command_id: commandId,
    payload_digest: sha256Bytes(body),
    expires_at_unix_ms: expiresAt,
    trace_id: 'trace',
    protocol_version: 1,
    channel_id: `channel-${deviceId}`,
    body,
  }
}

function makeService(options = {}) {
  let now = options.now ?? 1000
  const store = new StateStore({ path: ':memory:', now: () => now })
  const adapter = new TestSource({ now: () => now, sessions: [{ sessionId: 's1', running: true }] })
  const service = new CommandService({
    store,
    adapter,
    events: { readHistory: async () => ({}) },
    hostId: 'host-1',
    now: () => now,
    codec: proto,
  })
  return { service, store, adapter, advance: (ms) => { now += ms } }
}

function completeApproval(adapter, extra = {}) {
  return adapter.requestApproval({
    sessionId: 's1',
    reason: 'edit one file',
    toolName: 'edit',
    reviewAction: 'Edit file',
    reviewTarget: '/repo/src/app.js',
    reviewImpact: '3 lines changed',
    reviewAvailability: 'complete',
    ...extra,
  })
}

test('GetApproval carries complete review facts and the proto enum', async () => {
  const { service, adapter } = makeService()
  const approval = completeApproval(adapter)
  const result = await service.handle({
    request: makeRequest(), payload: { get_approval: { approval_id: approval.approvalId } },
  })
  assert.equal(result.status, 'succeeded')
  const view = result.response.approval
  assert.equal(view.review_action, 'Edit file')
  assert.equal(view.review_target, '/repo/src/app.js')
  assert.equal(view.review_impact, '3 lines changed')
  assert.equal(view.review_availability, 'approval_review_complete')
})

test('ListApprovals (read scope) withholds review facts for the same live approval', async () => {
  const { service, adapter } = makeService()
  completeApproval(adapter)
  const result = await service.handle({
    request: makeRequest(), payload: { list_approvals: { session_id: 's1' } },
  })
  assert.equal(result.status, 'succeeded')
  const [listed] = result.response.approval_list.approvals
  assert.equal(typeof listed.approval_id, 'string')
  assert.equal(listed.state, 'APPROVAL_STATE_PENDING')
  assert.equal(listed.review_action, '')
  assert.equal(listed.review_target, '')
  assert.equal(listed.review_impact, '')
  assert.equal(listed.review_availability, 'approval_review_unavailable')
})

test('a terminal ledger view never invents review facts after the native source is gone', async () => {
  const { service, store, adapter } = makeService()
  const approval = completeApproval(adapter)
  await service.handle({ request: makeRequest(), payload: { get_approval: { approval_id: approval.approvalId } } })
  // Review facts are not persisted in the approval ledger.
  const record = store.getApprovalRecord(approval.approvalId)
  assert.equal('reviewTarget' in record, false)
  assert.equal('reviewAction' in record, false)
  adapter.approvals.clear()
  service.recover()
  const result = await service.handle({
    request: makeRequest(), payload: { get_approval: { approval_id: approval.approvalId } },
  })
  assert.equal(result.status, 'succeeded')
  assert.equal(result.response.approval.state, 'APPROVAL_STATE_UNAVAILABLE')
  assert.equal(result.response.approval.review_action, '')
  assert.equal(result.response.approval.review_target, '')
  assert.equal(result.response.approval.review_impact, '')
  assert.equal(result.response.approval.review_availability, 'approval_review_unavailable')
})

test('a decision bound to a changed review target is rejected before native resolution', async () => {
  const { service, adapter } = makeService()
  const approval = completeApproval(adapter)
  const changedDigest = approvalDigest({ ...approval, reviewTarget: '/repo/src/other.js' })
  const result = await service.handle({
    request: makeRequest({ kind: 'REQUEST_KIND_COMMAND' }),
    payload: {
      resolve_approval: {
        approval_id: approval.approvalId,
        decision: 'APPROVAL_DECISION_APPROVE',
        expected_digest: changedDigest,
        expected_version: approval.version,
      },
    },
  })
  assert.equal(result.status, 'failed')
  assert.equal(result.errorCode, 'approval_version_mismatch')
  assert.equal((await adapter.getApproval(approval.approvalId)).state, 'pending')
})

test('the Approval proto round-trips the review fields and availability enum', async () => {
  const { service, adapter } = makeService()
  const approval = completeApproval(adapter)
  const result = await service.handle({
    request: makeRequest(), payload: { get_approval: { approval_id: approval.approvalId } },
  })
  const bytes = proto.encode('whalerein.v1.Approval', result.response.approval)
  const decoded = proto.decode('whalerein.v1.Approval', bytes)
  assert.equal(decoded.review_action, 'Edit file')
  assert.equal(decoded.review_target, '/repo/src/app.js')
  assert.equal(decoded.review_impact, '3 lines changed')
  assert.equal(decoded.review_availability, 'approval_review_complete')
  assert.deepEqual([...decoded.digest], [...approval.digest])
})

test('every review availability token maps to its proto enum on the detail path', async () => {
  const { service, adapter } = makeService()
  const cases = [
    ['incomplete', 'approval_review_incomplete'],
    ['unavailable', 'approval_review_unavailable'],
  ]
  for (const [availability, expected] of cases) {
    const approval = completeApproval(adapter, {
      reviewAvailability: availability,
      ...(availability === 'unavailable' ? { reviewAction: '', reviewTarget: '', reviewImpact: '' } : {}),
    })
    const result = await service.handle({
      request: makeRequest(), payload: { get_approval: { approval_id: approval.approvalId } },
    })
    assert.equal(result.response.approval.review_availability, expected)
  }
})
test('the detail serializer masks a credential that a source failed to mask', async () => {
  const { service, adapter } = makeService()
  const secret = 'sk-live-abcdefghijklmnop'
  const approval = completeApproval(adapter, {
    reviewTarget: `/repo/${secret}/config.json`,
    reviewImpact: 'Bearer abcdefghijklmnopqrstuvwxyz',
  })
  const result = await service.handle({
    request: makeRequest(), payload: { get_approval: { approval_id: approval.approvalId } },
  })
  const view = result.response.approval
  assert.equal(view.review_target, '/repo/<redacted>/config.json')
  assert.equal(view.review_impact, 'Bearer <redacted>')
  assert.equal(JSON.stringify(view).includes(secret), false)
  assert.equal(JSON.stringify(view).includes('abcdefghijklmnopqrstuvwxyz'), false)
})
