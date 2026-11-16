// APPR-02/03/06/09/11/12/13/14: Host-level approval claim, expiry, revision and
// terminal-state semantics. The connector owns native atomicity; these tests
// use a source that honours the documented approval contract and prove the Host
// maps every definitive outcome to a stable code, never to a generic unknown.

import test from 'node:test'
import assert from 'node:assert/strict'
import { CommandService } from '../src/commands/service.js'
import { TestSource } from './support/source.js'
import { StateStore } from '../src/state/store.js'
import { codec } from '../src/protocol.js'
import { DefinitiveExecutionError } from '../src/errors.js'
import { bytesEqual, sha256Bytes } from '../src/security/digest.js'

const proto = codec()
let serial = 0

function makeRequest({ deviceId = 'device-1', commandId = `approval-cmd-${++serial}`, expiresAt = 10_000, kind = 'REQUEST_KIND_COMMAND' } = {}) {
  const body = new TextEncoder().encode(`approval body ${commandId}`)
  return {
    request_id: `approval-req-${serial}`,
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

function resolvePayload(approval, extra = {}) {
  return { resolve_approval: {
    approval_id: extra.approvalId ?? approval.approvalId,
    decision: extra.decision ?? 'APPROVAL_DECISION_APPROVE',
    expected_digest: extra.digest ?? approval.digest,
    expected_version: extra.version ?? approval.version,
  } }
}

/** TestSource plus the native single-winner contract and an optional gate. */
class ApprovalSource extends TestSource {
  resolutions = []
  frozen = false
  gateUntil = 0
  reads = 0
  #entered = 0
  #gate = null
  #openGate = null
  #readGate = null
  #openReadGate = null

  gate() {
    this.gateUntil = 2
    this.#entered = 0
    this.#gate = new Promise((resolve) => { this.#openGate = resolve })
  }

  get entered() { return this.#entered }

  blockNextRead() {
    this.#readGate = new Promise((resolve) => { this.#openReadGate = resolve })
  }

  releaseRead() {
    this.#openReadGate?.()
    this.#readGate = null
  }

  async getApproval(id) {
    this.reads += 1
    if (this.#readGate) {
      const gate = this.#readGate
      this.#readGate = null
      await gate
    }
    const live = this.approvals.get(id)
    if (!live) return null
    if (this.frozen) return { ...live }
    return super.getApproval(id)
  }

  async execute(sessionId, command, context = {}) {
    if (!command.resolve_approval) return super.execute(sessionId, command, context)
    const resolve = command.resolve_approval
    const approval = this.approvals.get(resolve.approval_id)
    if (!approval || approval.sessionId !== sessionId) {
      throw new DefinitiveExecutionError('approval_unavailable')
    }
    if (approval.state === 'pending' && approval.expiresAt <= this.now()) approval.state = 'expired'
    if (approval.state === 'expired') throw new DefinitiveExecutionError('approval_expired')
    if (approval.state !== 'pending') throw new DefinitiveExecutionError('approval_already_resolved')
    if (!(resolve.expected_digest instanceof Uint8Array) || !bytesEqual(resolve.expected_digest, approval.digest)) {
      throw new DefinitiveExecutionError('approval_version_mismatch')
    }
    if (Number(resolve.expected_version) !== approval.version) {
      throw new DefinitiveExecutionError('approval_version_mismatch')
    }
    if (this.#gate) {
      this.#entered += 1
      if (this.#entered >= this.gateUntil) this.#openGate()
      await this.#gate
    }
    if (context.signal?.aborted) throw new DefinitiveExecutionError('unavailable')
    if (approval.state !== 'pending') throw new DefinitiveExecutionError('approval_already_resolved')
    approval.state = resolve.decision === 'APPROVAL_DECISION_APPROVE' ? 'approved' : 'rejected'
    approval.decision = resolve.decision === 'APPROVAL_DECISION_APPROVE' ? 'approve' : 'reject'
    this.resolutions.push({ approvalId: approval.approvalId, decision: approval.decision })
    return {}
  }
}

function makeService(options = {}) {
  let now = options.now ?? 1000
  const store = new StateStore({ path: ':memory:', now: () => now })
  const adapter = new ApprovalSource({ now: () => now, sessions: [{ sessionId: 's1', running: true }] })
  const service = new CommandService({
    store,
    adapter,
    events: { readHistory: async () => ({}) },
    hostId: 'host-1',
    now: () => now,
    codec: proto,
  })
  return { service, store, adapter, advance: (ms) => { now += ms }, now: () => now }
}

async function decide(service, approval, deviceId = 'device-1', extra = {}) {
  const request = makeRequest({ deviceId })
  const result = await service.handle({ request, payload: resolvePayload(approval, extra) })
  return { request, result }
}

test('two devices deciding the same approval settle once and the loser gets approval_already_resolved', async () => {
  const { service, store, adapter } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 60_000 })
  await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { list_approvals: { session_id: 's1' } } })
  adapter.frozen = true
  adapter.gate()
  const [first, second] = await Promise.all([
    decide(service, approval, 'device-1'),
    decide(service, approval, 'device-2'),
  ])
  const results = [first.result, second.result]
  assert.equal(results.filter((result) => result.status === 'succeeded').length, 1)
  const loser = results.find((result) => result.status !== 'succeeded')
  assert.equal(loser.status, 'failed')
  assert.equal(loser.errorCode, 'approval_already_resolved')
  assert.equal(adapter.resolutions.length, 1)
  assert.equal(store.getApprovalRecord(approval.approvalId).state, 'approved')
  assert.equal(store.getApprovalRecord(approval.approvalId).revision, 2)
})

test('a repeated decision from the other device never reaches the source twice', async () => {
  const { service, adapter } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 60_000 })
  const winner = await decide(service, approval, 'device-1')
  assert.equal(winner.result.status, 'succeeded')
  const loser = await decide(service, approval, 'device-2')
  assert.equal(loser.result.status, 'failed')
  assert.equal(loser.result.errorCode, 'approval_already_resolved')
  assert.equal(adapter.resolutions.length, 1)
})

test('stale digest, stale version and expiry surface approval codes without reaching the source', async () => {
  const { service, adapter, advance, store } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 60_000 })
  const digest = await decide(service, approval, 'device-1', { digest: new Uint8Array(32) })
  assert.equal(digest.result.status, 'failed')
  assert.equal(digest.result.errorCode, 'approval_version_mismatch')
  const version = await decide(service, approval, 'device-1', { version: 99 })
  assert.equal(version.result.errorCode, 'approval_version_mismatch')
  assert.equal(adapter.resolutions.length, 0)

  const expiring = adapter.requestApproval({ sessionId: 's1', ttlMs: 10 })
  advance(20)
  const expired = await decide(service, expiring, 'device-1')
  assert.equal(expired.result.status, 'failed')
  assert.equal(expired.result.errorCode, 'approval_expired')
  assert.equal(adapter.resolutions.length, 0)
  assert.equal(store.getApprovalRecord(expiring.approvalId).state, 'expired')
})

test('terminal approval state is observable through GetApproval after resolution and expiry', async () => {
  const { service, adapter, advance } = makeService()
  const approved = adapter.requestApproval({ sessionId: 's1', ttlMs: 60_000 })
  await decide(service, approved)
  const afterApprove = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }),
    payload: { get_approval: { approval_id: approved.approvalId } } })
  assert.equal(afterApprove.status, 'succeeded')
  assert.equal(afterApprove.response.approval.state, 'APPROVAL_STATE_APPROVED')
  assert.equal(afterApprove.response.approval.decision, 'APPROVAL_DECISION_APPROVE')
  assert.equal(afterApprove.response.approval.session_id, 's1')

  const expiring = adapter.requestApproval({ sessionId: 's1', ttlMs: 10 })
  advance(20)
  const afterExpiry = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }),
    payload: { get_approval: { approval_id: expiring.approvalId } } })
  assert.equal(afterExpiry.response.approval.state, 'APPROVAL_STATE_EXPIRED')
})

test('a pending approval lost with the previous process is surfaced as unavailable, never unknown', async () => {
  const { service, adapter } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 60_000 })
  await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { list_approvals: {} } })
  adapter.approvals.clear()
  service.recover()
  const read = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }),
    payload: { get_approval: { approval_id: approval.approvalId } } })
  assert.equal(read.status, 'succeeded')
  assert.equal(read.response.approval.state, 'APPROVAL_STATE_UNAVAILABLE')

  const late = await decide(service, approval, 'device-2')
  assert.equal(late.result.status, 'failed')
  assert.equal(late.result.errorCode, 'approval_unavailable')
  assert.equal(adapter.resolutions.length, 0)
})

test('an approval resolved before a restart replays approval_already_resolved from durable state', async () => {
  const { service, store, adapter } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 60_000 })
  await decide(service, approval)
  assert.equal(store.getApprovalRecord(approval.approvalId).state, 'approved')
  adapter.approvals.clear()
  service.recover()
  assert.equal(store.getApprovalRecord(approval.approvalId).state, 'approved')
  const replay = await decide(service, approval, 'device-2')
  assert.equal(replay.result.status, 'failed')
  assert.equal(replay.result.errorCode, 'approval_already_resolved')
  assert.equal(adapter.resolutions.length, 1)
})

test('an aborted approval precheck is journaled failed and a same-command retry replays the exact code', async () => {
  const { service, store, adapter } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 60_000 })
  adapter.blockNextRead()
  const request = makeRequest()
  const pending = service.handle({ request, payload: resolvePayload(approval) })
  for (let i = 0; i < 200 && adapter.reads === 0; i++) await new Promise((resolve) => setImmediate(resolve))
  assert.equal(adapter.reads, 1)
  service.cancelRequest(request.request_id, 'client_poll_expired')
  const rejected = await pending
  assert.equal(rejected.status, 'failed')
  assert.equal(rejected.errorCode, 'unavailable')
  const recorded = store.getCommand(request.device_id, request.command_id)
  assert.equal(recorded.status, 'failed')
  assert.equal(recorded.errorCode, 'unavailable')
  assert.ok(recorded.resultPayload instanceof Uint8Array && recorded.resultPayload.length > 0)
  assert.equal(adapter.resolutions.length, 0)

  // Release the read that raced the abort, then replay the same command id.
  adapter.releaseRead()
  const reads = adapter.reads
  const retry = await service.handle({ request, payload: resolvePayload(approval) })
  assert.equal(retry.status, 'failed')
  assert.equal(retry.errorCode, 'unavailable')
  assert.deepEqual([...retry.resultPayload], [...rejected.resultPayload])
  assert.equal(adapter.reads, reads)
  assert.equal(adapter.resolutions.length, 0)
  assert.equal((await adapter.getApproval(approval.approvalId)).state, 'pending')

  // A fresh command identity still resolves the untouched approval once.
  const fresh = await decide(service, approval, 'device-2')
  assert.equal(fresh.result.status, 'succeeded')
  assert.equal(adapter.resolutions.length, 1)
})

test('a precheck read failure is journaled as a definitive failure, not unknown', async () => {
  const { service, store, adapter } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 60_000 })
  const original = adapter.getApproval.bind(adapter)
  adapter.getApproval = async () => { throw new Error('native read failed') }
  const request = makeRequest()
  const result = await service.handle({ request, payload: resolvePayload(approval) })
  assert.equal(result.status, 'failed')
  assert.equal(result.errorCode, 'internal')
  const recorded = store.getCommand(request.device_id, request.command_id)
  assert.equal(recorded.status, 'failed')
  assert.equal(recorded.errorCode, 'internal')
  const retry = await service.handle({ request, payload: resolvePayload(approval) })
  assert.equal(retry.errorCode, 'internal')
  adapter.getApproval = original
  assert.equal(adapter.resolutions.length, 0)
})

test('approval ledger storage failures fail closed and never turn a decision into unknown or success twice', async () => {
  const { service, store, adapter } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 60_000 })
  for (const method of ['recordApproval', 'getApprovalRecord', 'settleApproval', 'listApprovalRecords', 'invalidatePendingApprovals']) {
    store[method] = () => { throw new Error('ledger unavailable') }
  }
  const listed = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }),
    payload: { list_approvals: { session_id: 's1' } } })
  assert.equal(listed.status, 'succeeded')
  assert.equal(listed.response.approval_list.approvals.length, 1)

  const resolved = await decide(service, approval)
  assert.equal(resolved.result.status, 'succeeded')
  assert.equal(adapter.resolutions.length, 1)
  // Recovery still runs and journals command state even with an unreadable ledger.
  assert.doesNotThrow(() => service.recover())

  // With the native record gone and the ledger unreadable, a later decision is
  // a definitive approval_unavailable and never reaches the source.
  adapter.approvals.clear()
  const lost = await decide(service, approval, 'device-2')
  assert.equal(lost.result.status, 'failed')
  assert.equal(lost.result.errorCode, 'approval_unavailable')
  assert.equal(adapter.resolutions.length, 1)
})

test('a restart-invalidated record reopens when the native source still proves pending', async () => {
  const { service, store, adapter } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 60_000 })
  await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { list_approvals: {} } })
  service.recover()
  assert.equal(store.getApprovalRecord(approval.approvalId).state, 'unavailable')
  // The source is durable (for example an external DSH authority), so a
  // live pending observation reopens the same identity instead of losing it.
  const retry = await decide(service, approval)
  assert.equal(retry.result.status, 'succeeded')
  assert.equal(store.getApprovalRecord(approval.approvalId).state, 'approved')
  assert.ok(store.getApprovalRecord(approval.approvalId).revision >= 3)
})

test('device_offline aborts an in-flight decision without approving and releases the claim for retry', async () => {
  const { service, adapter, store } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 60_000 })
  adapter.frozen = true
  adapter.gate()
  const inFlight = decide(service, approval, 'device-1')
  for (let i = 0; i < 200 && adapter.entered === 0; i++) await new Promise((resolve) => setImmediate(resolve))
  assert.equal(adapter.entered, 1)
  service.cancelDevice('device-1', 'device_offline')
  const aborted = await inFlight
  assert.equal(aborted.result.status, 'unknown')
  assert.notEqual(aborted.result.status, 'succeeded')
  assert.equal(adapter.resolutions.length, 0)
  assert.equal(store.getApprovalRecord(approval.approvalId).state, 'pending')
  const retry = await decide(service, approval, 'device-2')
  assert.equal(retry.result.status, 'succeeded')
  assert.equal(adapter.resolutions.length, 1)
})

test('concurrent approvals keep independent TTLs, stable order and monotonic revisions', async () => {
  const { service, store, adapter, advance, now } = makeService()
  const first = adapter.requestApproval({ sessionId: 's1', ttlMs: 50 })
  const second = adapter.requestApproval({ sessionId: 's1', ttlMs: 60_000 })
  for (let i = 0; i < 3; i++) {
    const listed = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }),
      payload: { list_approvals: { session_id: 's1' } } })
    assert.deepEqual(listed.response.approval_list.approvals.map((item) => item.approval_id), [first.approvalId, second.approvalId])
  }
  advance(60)
  const listed = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }),
    payload: { list_approvals: { session_id: 's1' } } })
  assert.deepEqual(listed.response.approval_list.approvals.map((item) => item.approval_id), [second.approvalId])
  assert.equal(store.getApprovalRecord(first.approvalId).state, 'expired')
  const resolved = await decide(service, second)
  assert.equal(resolved.result.status, 'succeeded')
  assert.equal(now(), 1060)
  assert.deepEqual(store.listApprovalRecords({ sessionId: 's1' }).map((record) => record.approvalId),
    [first.approvalId, second.approvalId])
})

test('approval text is bounded on the wire and native tool arguments never enter it', async () => {
  const { service, adapter } = makeService()
  const longReason = 'escalate '.repeat(2_000).trim()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 60_000, reason: longReason })
  const listed = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }),
    payload: { list_approvals: { session_id: 's1' } } })
  const wire = listed.response.approval_list.approvals[0]
  assert.ok(wire.reason.length > 0 && wire.reason.length <= 1024)
  assert.deepEqual(Object.keys(wire).sort(), [
    'approval_id', 'call_id', 'decision', 'digest', 'expires_at_unix_ms', 'reason', 'session_id', 'state', 'tool_name', 'version',
  ])
  assert.equal(JSON.stringify(wire).includes('arguments'), false)
  assert.equal(JSON.stringify(wire).includes('parameters'), false)
})
