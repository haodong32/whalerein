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
  const adapter = new RealDshAdapter({ ctx, now: () => now, logger: options.logger, config: { hostId: 'host-1', ...options.config } })
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
  const { outcome, approval } = request({ arguments: { command: 'npm test' } })
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
  const next = request({ reason: 'after expiry', arguments: { command: 'npm test' } })
  assert.ok(next.approval.version > expired.approval.version)
  await adapter.resolveApproval(next.approval.approvalId, 'approve', next.approval.digest, next.approval.version)
  assert.equal(await next.outcome, 'allowed-once')
  await adapter.close()
})

test('native approval text is bounded and the raw argument map never reaches the approval surface', async () => {
  const { adapter, request } = await openAdapter()
  const secret = 'tool-argument-secret-value'
  const { approval } = request({
    reason: 'r'.repeat(5_000),
    arguments: { command: `deploy --token=${secret}` },
    parameters: { token: secret },
    input: { private: secret },
  })
  assert.ok(approval.reason.length > 0 && approval.reason.length <= 1024)
  const surface = JSON.stringify(await adapter.listApprovals('s1'))
    + JSON.stringify(await adapter.getApproval(approval.approvalId))
  assert.equal(surface.includes(secret), false)
  assert.equal(surface.includes('arguments'), false)
  assert.equal(surface.includes('parameters'), false)
  assert.equal(surface.includes('--token'), false)
  // Only the bounded executable basename and generic impact are projected.
  assert.equal(approval.reviewAction, 'Run command')
  assert.equal(approval.reviewTarget, 'deploy')
  assert.equal(approval.reviewImpact, 'One shell command')
  assert.equal(approval.reviewAvailability, 'complete')
  const expected = approvalDigest({
    approvalId: approval.approvalId,
    sessionId: approval.sessionId,
    toolName: approval.toolName,
    callId: approval.callId,
    reason: approval.reason,
    reviewAction: approval.reviewAction,
    reviewTarget: approval.reviewTarget,
    reviewImpact: approval.reviewImpact,
    reviewAvailability: approval.reviewAvailability,
    version: approval.version,
    expiresAt: approval.expiresAt,
  })
  assert.deepEqual([...approval.digest], [...expected])
  await adapter.close()
})

test('a changed review target invalidates the previous digest before native resolution', async () => {
  const { adapter, request } = await openAdapter()
  const { approval, outcome } = request({
    toolName: 'write',
    arguments: { path: '/repo/a.txt', content: 'one\ntwo' },
  })
  assert.equal(approval.reviewAvailability, 'complete')
  const changed = approvalDigest({
    ...approval,
    reviewTarget: '/repo/b.txt',
  })
  await assert.rejects(
    adapter.resolveApproval(approval.approvalId, 'approve', changed, approval.version),
    (error) => error.code === 'approval_version_mismatch',
  )
  assert.equal(await adapter.getApproval(approval.approvalId).then((item) => item.state), 'pending')
  await adapter.resolveApproval(approval.approvalId, 'approve', approval.digest, approval.version)
  assert.equal(await outcome, 'allowed-once')
  await adapter.close()
})

test('a native request without an argument map reports incomplete review without inventing facts', async () => {
  // DSH 0.2.2-rc.1 exposes toolName/callId/reason/signal only. The exact tool
  // name establishes the action class, but no target or argument detail is
  // invented and the reason free text is not mined for review facts.
  const { adapter, request } = await openAdapter()
  const { approval } = request({ toolName: 'bash', reason: 'escalate sandbox to danger-full-access: needs network' })
  assert.equal(approval.reviewAction, 'Run command')
  assert.equal(approval.reviewTarget, '')
  assert.equal(approval.reviewImpact, 'One shell command')
  assert.equal(approval.reviewAvailability, 'incomplete')
  assert.equal(JSON.stringify({
    action: approval.reviewAction, target: approval.reviewTarget, impact: approval.reviewImpact,
  }).includes('danger-full-access'), false)
  await adapter.close()

  const unknown = await openAdapter()
  const { approval: opaque } = unknown.request({ toolName: 'acme_unknown_tool', reason: 'fixture' })
  assert.deepEqual(
    [opaque.reviewAction, opaque.reviewTarget, opaque.reviewImpact, opaque.reviewAvailability],
    ['', '', '', 'unavailable'],
  )
  await unknown.adapter.close()
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

test('projected review values never enter adapter logs', async () => {
  const lines = []
  const logger = {
    debug: (message, data) => lines.push([message, data]),
    info: (message, data) => lines.push([message, data]),
    warn: (message, data) => lines.push([message, data]),
    error: (message, data) => lines.push([message, data]),
  }
  const { adapter, request } = await openAdapter({ logger })
  const target = '/repo/secret-project/internal/app.js'
  const { approval } = request({
    toolName: 'edit',
    arguments: { path: target, old_string: 'a', new_string: 'b' },
  })
  assert.equal(approval.reviewTarget, target)
  await adapter.close()
  const serialized = JSON.stringify(lines)
  assert.equal(serialized.includes(target), false)
  assert.equal(serialized.includes('Edit file'), false)
  assert.equal(serialized.includes('line changed'), false)
})

test('remote Allow is accepted for incomplete and unavailable reviews while reject stays available', async () => {
  const { adapter, request } = await openAdapter()
  // DSH 0.2.2-rc.1 shape: toolName only, no argument map, no target.
  const incomplete = request({ toolName: 'write', reason: 'needs wider access' })
  assert.equal(incomplete.approval.reviewAvailability, 'incomplete')
  const approved = await adapter.resolveApproval(
    incomplete.approval.approvalId, 'approve', incomplete.approval.digest, incomplete.approval.version,
  )
  assert.deepEqual(approved, { state: 'approved', decision: 'approve' })
  assert.equal(await incomplete.outcome, 'allowed-once')
  // The resolution reply carries no projected review facts.
  assert.deepEqual(Object.keys(approved), ['state', 'decision'])

  // An unknown tool yields no review facts at all; Allow is still accepted.
  const unavailable = request({ toolName: 'acme_unknown_tool', reason: 'opaque tool' })
  assert.equal(unavailable.approval.reviewAvailability, 'unavailable')
  assert.equal((await adapter.resolveApproval(
    unavailable.approval.approvalId, 'approve', unavailable.approval.digest, unavailable.approval.version,
  )).state, 'approved')
  assert.equal(await unavailable.outcome, 'allowed-once')

  // Reject is unchanged and settles an incomplete review directly.
  const rejectable = request({ toolName: 'write', reason: 'decline' })
  assert.equal(rejectable.approval.reviewAvailability, 'incomplete')
  await adapter.resolveApproval(
    rejectable.approval.approvalId, 'reject', rejectable.approval.digest, rejectable.approval.version,
  )
  assert.equal(await rejectable.outcome, 'rejected')

  const complete = request({ toolName: 'write', arguments: { path: '/repo/a.txt', content: 'one\ntwo' } })
  assert.equal(complete.approval.reviewAvailability, 'complete')
  assert.deepEqual(
    [complete.approval.reviewAction, complete.approval.reviewTarget, complete.approval.reviewImpact],
    ['Write file', '/repo/a.txt', 'Whole file, 2 lines, 7 bytes'],
  )
  assert.equal((await adapter.resolveApproval(
    complete.approval.approvalId, 'approve', complete.approval.digest, complete.approval.version,
  )).state, 'approved')
  assert.equal(await complete.outcome, 'allowed-once')
  await assert.rejects(
    adapter.resolveApproval(complete.approval.approvalId, 'approve', complete.approval.digest, complete.approval.version),
    (error) => error.code === 'approval_already_resolved',
  )
  await adapter.close()
})

test('digest and version bind remote Allow, and the resolution reply carries no review facts', async () => {
  const { adapter, request } = await openAdapter()
  const { approval, outcome } = request({ toolName: 'write', reason: 'no facts' })
  assert.equal(approval.reviewAvailability, 'incomplete')
  await assert.rejects(
    adapter.resolveApproval(approval.approvalId, 'approve', new Uint8Array(32), approval.version),
    (error) => error.code === 'approval_version_mismatch',
  )
  await assert.rejects(
    adapter.resolveApproval(approval.approvalId, 'approve', approval.digest, approval.version + 1),
    (error) => error.code === 'approval_version_mismatch',
  )
  // The exact guard still binds the projected facts, and Allow is accepted.
  const resolved = await adapter.resolveApproval(approval.approvalId, 'approve', approval.digest, approval.version)
  assert.deepEqual(resolved, { state: 'approved', decision: 'approve' })
  assert.equal(await outcome, 'allowed-once')
  for (const value of [approval.reviewAction, approval.reviewTarget, approval.reviewImpact]) {
    if (value) assert.equal(JSON.stringify(resolved).includes(value), false)
  }
  await adapter.close()
})

test('an incomplete review never blocks local waterfall settlement', async () => {
  const { adapter, request } = await openAdapter()
  const controller = new AbortController()
  const { outcome, approval } = request({ toolName: 'write', signal: controller.signal })
  assert.equal(approval.reviewAvailability, 'incomplete')
  // Native cancellation settles the held waterfall directly; it is not a
  // remote decision and is not subject to any review predicate.
  controller.abort()
  assert.equal(await outcome, 'cancelled')
  assert.equal((await adapter.getApproval(approval.approvalId)).state, 'unavailable')
  await adapter.close()
})
