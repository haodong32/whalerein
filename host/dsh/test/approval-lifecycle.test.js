// APPR-02/05/13/14/20: connector-owned approval lifecycle. The Host cannot
// create source-side atomicity; the adapter must settle one decision, bound
// native text, and advertise approvals only when it can actually serve them.

import test from 'node:test'
import assert from 'node:assert/strict'
import { RealDshAdapter } from '../src/adapter/real.js'
import { approvalDigest } from 'whalerein-host/security/digest.js'

function makeLiveSession({ sessionId = 's1', running = true } = {}) {
  return {
    id: sessionId,
    header: { id: sessionId, version: 4, createdAt: 10, cwd: '/tmp' },
    seq: 1,
    snapshotEvents() { return [] },
    events: [],
    running,
    agentAvailable: true,
    blank: false,
    updatedAt: 20,
  }
}

function makeCtx({ session = makeLiveSession(), state = () => ({ active: true, running: true }) } = {}) {
  const handlers = new Map()
  const live = new Map([[session.id, session]])
  return {
    sessions: {
      get: (id) => live.get(id),
      list: () => [...live.values()],
    },
    sessionController: {
      async list() {
        return { items: [...live.values()].map((row) => ({
          sessionId: row.id, updatedAt: row.updatedAt, running: row.running,
          blank: row.blank, agentAvailable: row.agentAvailable, cwd: row.header.cwd,
        })) }
      },
      async state() { return state() },
    },
    on(event, handler) {
      const list = handlers.get(event) ?? []
      list.push(handler)
      handlers.set(event, list)
      return () => handlers.set(event, (handlers.get(event) ?? []).filter((entry) => entry !== handler))
    },
    emit(event, ...args) {
      return (handlers.get(event) ?? []).map((handler) => handler(...args))
    },
    handlers,
  }
}

async function openAdapter(options = {}) {
  let now = options.now ?? 1000
  const session = options.session ?? makeLiveSession()
  const ctx = makeCtx({ session, state: options.state })
  const adapter = new RealDshAdapter({ ctx, now: () => now, config: { hostId: 'host-1', ...options.config } })
  await adapter.open()
  const requested = []
  adapter.subscribeApprovals((event) => {
    if (event.type === 'requested') requested.push(event.approval)
  })
  const request = (fields = {}) => {
    const outcome = ctx.emit('approval/request', {
      agent: { session }, toolName: 'bash', callId: `call-${requested.length}`, reason: 'fixture reason', ...fields,
    }, async () => 'unavailable')[0]
    return { outcome, approval: requested[requested.length - 1] }
  }
  return { adapter, ctx, session, request, requested, advance: (ms) => { now += ms }, now: () => now }
}

test('two concurrent decisions on one approval settle exactly once', async () => {
  const { adapter, request } = await openAdapter()
  const { outcome, approval } = request()
  const results = await Promise.allSettled([
    adapter.resolveApproval(approval.approvalId, 'approve', approval.digest, approval.version),
    adapter.resolveApproval(approval.approvalId, 'reject', approval.digest, approval.version),
  ])
  assert.equal(results.filter((result) => result.status === 'fulfilled').length, 1)
  const rejected = results.find((result) => result.status === 'rejected')
  assert.equal(rejected.reason.code, 'approval_already_resolved')
  assert.equal(await outcome, 'allowed-once')
  const resolved = await adapter.getApproval(approval.approvalId)
  assert.equal(resolved.state, 'approved')
  assert.equal(resolved.decision, 'approve')
  await adapter.close()
})

test('concurrent approvals keep independent expiries, stable order and increasing versions', async () => {
  const { adapter, request, advance } = await openAdapter({ config: { approvalTtlMs: 1_000 } })
  const first = request({ reason: 'first' })
  advance(500)
  const second = request({ reason: 'second' })
  const third = request({ reason: 'third' })
  assert.deepEqual([first.approval.version, second.approval.version, third.approval.version], [1, 2, 3])
  assert.ok(second.approval.expiresAt > first.approval.expiresAt)
  assert.equal(second.approval.expiresAt, third.approval.expiresAt)
  advance(600)
  for (let attempt = 0; attempt < 3; attempt++) {
    const listed = await adapter.listApprovals('s1')
    assert.deepEqual(listed.map((item) => item.approvalId), [second.approval.approvalId, third.approval.approvalId])
  }
  assert.equal((await adapter.getApproval(first.approval.approvalId)).state, 'expired')
  assert.equal(await first.outcome, 'cancelled')
  await adapter.close()
})

test('an expired approval releases its native turn and the next request in the same session still works', async () => {
  const { adapter, request, advance } = await openAdapter({ config: { approvalTtlMs: 1 } })
  const expired = request({ reason: 'expires' })
  advance(2)
  assert.equal((await adapter.getApproval(expired.approval.approvalId)).state, 'expired')
  assert.equal(await expired.outcome, 'cancelled')
  const next = request({ reason: 'after expiry' })
  assert.ok(next.approval.version > expired.approval.version)
  await adapter.resolveApproval(next.approval.approvalId, 'approve', next.approval.digest, next.approval.version)
  assert.equal(await next.outcome, 'allowed-once')
  await adapter.close()
})

test('native approval text is bounded and tool arguments never reach the approval surface', async () => {
  const { adapter, request } = await openAdapter()
  const secret = 'tool-argument-secret-value'
  const { approval } = request({
    reason: 'r'.repeat(5_000),
    arguments: { command: secret },
    parameters: { token: secret },
    input: { private: secret },
  })
  assert.ok(approval.reason.length > 0 && approval.reason.length <= 1024)
  const surface = JSON.stringify(await adapter.listApprovals('s1'))
    + JSON.stringify(await adapter.getApproval(approval.approvalId))
  assert.equal(surface.includes(secret), false)
  assert.equal(surface.includes('arguments'), false)
  assert.equal(surface.includes('parameters'), false)
  const expected = approvalDigest({
    approvalId: approval.approvalId,
    sessionId: approval.sessionId,
    toolName: approval.toolName,
    callId: approval.callId,
    reason: approval.reason,
    version: approval.version,
    expiresAt: approval.expiresAt,
  })
  assert.deepEqual([...approval.digest], [...expected])
  await adapter.close()
})

test('delegate mode withholds approvals from capabilities and never claims the native action', async () => {
  const { adapter, request } = await openAdapter({ config: { approvalMode: 'delegate' } })
  assert.equal(adapter.describe().capabilities.includes('approvals'), false)
  const sessions = await adapter.listSessions()
  assert.equal(sessions[0].sourceCapabilities.approvals, false)
  const { outcome, requested } = request()
  assert.equal(await outcome, 'unavailable')
  assert.equal(requested, undefined)

  const claim = await openAdapter({ config: { approvalMode: 'claim' } })
  assert.equal(claim.adapter.describe().capabilities.includes('approvals'), true)
  assert.equal((await claim.adapter.listSessions())[0].sourceCapabilities.approvals, true)
  await claim.adapter.close()
  await adapter.close()
})
