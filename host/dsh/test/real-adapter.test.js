import { DshHostCredentials } from '../src/credentials.js'
import test from 'node:test'
import assert from 'node:assert/strict'
import { RealDshAdapter } from '../src/adapter/real.js'
import { AdapterError, DefinitiveExecutionError } from 'whalerein-host/errors.js'
import { generateKeypair } from 'whalerein-host/security/noise.js'
import { approvalDigest } from 'whalerein-host/security/digest.js'

function makeLiveSession({
  sessionId = 's1',
  events = [],
  createdAt = 10,
  cwd = '/tmp',
  running = true,
  agentAvailable = true,
  blank = false,
  updatedAt = 20,
  parentSession,
  origin,
  projections,
} = {}) {
  const list = [...events]
  return {
    id: sessionId,
    header: { id: sessionId, version: 4, createdAt, cwd, parentSession, origin },
    get seq() {
      return list.length + 1
    },
    snapshotEvents(fromSeq = 0) {
      return list.filter((event) => event.seq >= fromSeq)
    },
    events: list,
    running,
    agentAvailable,
    blank,
    updatedAt,
    projections,
  }
}

function makeCtx(options = {}) {
  const handlers = new Map()
  const calls = { prompt: [], cancel: [], state: [], list: 0, events: [] }
  const live = options.live ?? new Map()
  const records = new Map(options.records ?? [])
  const refs = new Map()
  const coldEvents = options.coldEvents ?? [
    { type: 'assistant/message', seq: 1, time: 1, data: { text: 'old' } },
    { type: 'turn/end', seq: 2, time: 2, data: { running: false } },
  ]
  const stateFor = options.stateFor ?? (() => ({
    active: true,
    running: true,
    queue: [{ id: 'q' }],
    items: [{ id: 'decoy' }, { id: 'decoy' }, { id: 'decoy' }, { id: 'decoy' }, { id: 'decoy' }],
  }))

  const ctx = {
    sessions: {
      get(id) {
        return live.get(id)
      },
      list() {
        return [...live.values()]
      },
    },
    sessionController: {
      async list() {
        calls.list++
        if (options.failList) throw new Error('session not found')
        return {
          // Decoy: the public list value has no `sessions` field.
          sessions: [{ sessionId: 'decoy', title: 'decoy' }],
          items: [...live.values()].map((session) => ({
            sessionId: session.id,
            updatedAt: session.updatedAt,
            running: session.running,
            blank: session.blank,
            agentAvailable: session.agentAvailable,
            ...(session.header.cwd === undefined ? {} : { cwd: session.header.cwd }),
            ...(session.header.parentSession === undefined ? {} : { parentSessionId: session.header.parentSession }),
            ...(session.header.origin === undefined ? {} : { origin: session.header.origin }),
            // Decoys for names the list row does not use.
            title: 'not-the-title',
            queuedPrompts: 99,
            parentSession: 'not-the-parent',
            ...(session.projections === undefined ? {} : { projections: session.projections }),
          })),
        }
      },
      async state(request) {
        calls.state.push(request)
        return stateFor(request.sessionId)
      },
      async prompt(request, signal) {
        calls.prompt.push(request)
        if (options.failPrompt) throw new Error('session/agent-busy')
        return { accepted: true }
      },
      cancel(request) {
        calls.cancel.push(request)
        if (options.failCancel) throw new Error('session/not-found')
        return { accepted: true }
      },
      async events(request) {
        calls.events.push(request)
        if (options.failEvents) throw new Error('unavailable')
        const before = request.beforeSeq
        const eligible = coldEvents.filter((event) => before === undefined || event.seq < before)
        const page = eligible.slice(-Math.min(request.maxMessages ?? 10, 500))
        return {
          // Decoy: the public page is `records`, not `events`.
          events: [{ type: 'event', event: { type: 'assistant/message', seq: 9, time: 9, data: { text: 'decoy' } } }],
          records: page.map((event) => ({ type: 'event', event })),
          hasMore: eligible.length > page.length,
          cursor: page.length > 0 ? page[page.length - 1].seq : 0,
        }
      },
    },
    credentials: options.credentials ?? {
      async readRecord(key) {
        return records.get(key)
      },
      async modifyRecord(key, mutate) {
        const next = await mutate(records.get(key))
        if (next !== undefined) records.set(key, next)
        return next
      },
      async resolve(ref) {
        return refs.get(ref)
      },
      async set(ref, value) {
        refs.set(ref, { value, source: 'test' })
      },
    },
    on(event, handler) {
      const list = handlers.get(event) ?? []
      list.push(handler)
      handlers.set(event, list)
      return () => {
        const current = handlers.get(event) ?? []
        handlers.set(event, current.filter((entry) => entry !== handler))
      }
    },
    emit(event, ...args) {
      return (handlers.get(event) ?? []).map((handler) => handler(...args))
    },
    handlers,
    calls,
    records,
  }
  return ctx
}

test('when live DSH sessions expose projections, listing and snapshots preserve the documented fields and report truncated history as incomplete', async () => {
  const live = new Map()
  live.set('s1', makeLiveSession({
    events: [
      { type: 'assistant/message', seq: 1, time: 1, data: { text: 'hello' } },
      { type: 'turn/end', seq: 2, time: 2, data: { running: false } },
    ],
    parentSession: 'parent-1',
    origin: 'subagent',
    projections: {
      kind: 'sequenced',
      asOfSeq: 2,
      values: {
        title: 'Hello',
        workingDirectory: '/elsewhere',
        inbox: { 'next-turn': [{ id: 'm1' }, { id: 'm2' }], 'next-step': [{ id: 'm3' }] },
      },
    },
  }))
  live.set('s2', makeLiveSession({
    sessionId: 's2',
    running: false,
    blank: true,
    agentAvailable: false,
    updatedAt: 50,
    projections: { kind: 'cached', asOfSeq: 0, values: { title: null } },
  }))
  const ctx = makeCtx({ live })
  const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1', snapshotMaxEvents: 10 } })
  await adapter.open()
  assert.equal(adapter.describe().mode, 'real')

  const sessions = await adapter.listSessions()
  assert.equal(sessions[0].sessionId, 's1')
  assert.equal(sessions[0].title, 'Hello')
  assert.equal(sessions[0].cwd, '/tmp')
  assert.equal(sessions[0].updatedAt, 20)
  assert.equal(sessions[0].running, true)
  assert.equal(sessions[0].agentAvailable, true)
  assert.equal(sessions[0].blank, false)
  assert.equal(sessions[0].parentSessionId, 'parent-1')
  assert.equal(sessions[0].origin, 'subagent')
  assert.equal(sessions[0].queuedPrompts, 3)
  assert.equal(sessions[0].createdAt, 0)
  assert.equal(sessions[1].sessionId, 's2')
  assert.equal(sessions[1].title, '')
  assert.equal(sessions[1].running, false)
  assert.equal(sessions[1].blank, true)
  assert.equal(sessions[1].agentAvailable, false)
  assert.equal(sessions[1].updatedAt, 50)
  assert.equal(sessions[1].queuedPrompts, 0)
  assert.equal(sessions[1].parentSessionId, '')
  const liveSnapshot = await adapter.getSessionSnapshot('s1')
  assert.equal(liveSnapshot.complete, true)
  assert.equal(liveSnapshot.summary.title, 'Hello')
  assert.equal(liveSnapshot.summary.queuedPrompts, 3)
  assert.equal(liveSnapshot.events.length, 2)
  assert.equal(liveSnapshot.events[0].kind, 'output')
  assert.equal(liveSnapshot.events[0].createdAt, 1)
  assert.equal(liveSnapshot.events[0].dshSeq, 1)
  assert.equal(liveSnapshot.events[0].data.text, 'hello')
  assert.equal(liveSnapshot.events[0].ephemeral, false)
  assert.equal(liveSnapshot.events[1].kind, 'session_state')
  assert.equal(liveSnapshot.source, 'dsh')

  const truncated = new RealDshAdapter({ ctx, config: { snapshotMaxEvents: 1 } })
  await truncated.open()
  assert.equal((await truncated.getSessionSnapshot('s1')).complete, false)
  await truncated.close()

  await adapter.close()
})

test('when a cold DSH session is read, the adapter requests its native address and returns committed text with source positions', async () => {
  const ctx = makeCtx()
  const adapter = new RealDshAdapter({ ctx, config: { snapshotMaxEvents: 10 } })
  await adapter.open()
  try {
  const coldSnapshot = await adapter.getSessionSnapshot('cold')
  assert.equal(coldSnapshot.complete, true)
  assert.equal(coldSnapshot.events.length, 2)
  assert.equal(coldSnapshot.events[0].data.text, 'old')
  assert.equal(coldSnapshot.events[0].createdAt, 1)
  assert.equal(coldSnapshot.events[0].dshSeq, 1)
  assert.equal(ctx.calls.events[0].address.kind, 'session')
  assert.equal(ctx.calls.events[0].address.sessionId, 'cold')
  assert.equal(ctx.calls.events[0].beforeSeq, undefined)

  } finally { await adapter.close() }
})

test('when native prompt and interrupt controls succeed, the adapter forwards the selected session and visible text once', async () => {
  const ctx = makeCtx({ live: new Map([['s1', makeLiveSession()]]) })
  const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1' } })
  await adapter.open()
  try {
  const message = await adapter.sendUserMessage('s1', 'remote text')
  assert.equal(ctx.calls.prompt[0].requestId, message.messageId)
  assert.equal(ctx.calls.prompt[0].sessionId, 's1')
  assert.equal(ctx.calls.prompt[0].mode, 'queue')
  assert.equal(ctx.calls.prompt[0].clientTimeZone, undefined)
  assert.deepEqual(ctx.calls.prompt[0].content, [{ type: 'text', text: 'remote text' }])
  await assert.rejects(adapter.sendUserMessage('s1', '   '), (error) => error.code === 'invalid_argument')
  assert.deepEqual(await adapter.interrupt('s1'), { accepted: true })
  assert.equal(ctx.calls.cancel[0].sessionId, 's1')
  assert.equal(ctx.calls.prompt.length, 1)
  assert.equal(ctx.calls.cancel.length, 1)
  } finally { await adapter.close() }
})

test('when paging a cold snapshot, the real adapter uses beforeSeq and ignores the events field', async () => {
  const coldEvents = Array.from({ length: 501 }, (_, index) => ({
    type: 'assistant/message',
    seq: index + 1,
    time: index + 1,
    data: { text: 'kept' },
  }))
  const ctx = makeCtx({ coldEvents })
  const adapter = new RealDshAdapter({ ctx, config: { snapshotMaxEvents: 1000 } })
  await adapter.open()
  const snapshot = await adapter.getSessionSnapshot('cold')
  assert.equal(ctx.calls.events[0].address.kind, 'session')
  assert.equal(ctx.calls.events[0].address.sessionId, 'cold')
  assert.equal(ctx.calls.events[0].maxMessages, 500)
  assert.equal(ctx.calls.events[0].beforeSeq, undefined)
  assert.equal(ctx.calls.events[1].beforeSeq, 2)
  assert.equal(ctx.calls.events[1].maxMessages, 500)
  assert.equal(snapshot.events.length, 501)
  assert.equal(snapshot.events[0].dshSeq, 1)
  assert.equal(snapshot.events[0].data.text, 'kept')
  assert.equal(snapshot.events[500].dshSeq, 501)
  assert.equal(snapshot.complete, true)
  await adapter.close()
})

test('when DSH commits events, the real adapter maps supported events and filters approval audit records', async () => {
  const ctx = makeCtx()
  const adapter = new RealDshAdapter({ ctx, config: {} })
  await adapter.open()
  const seen = []
  const off = adapter.subscribeEvents((event) => seen.push(event))
  const session = makeLiveSession()
  ctx.emit('session/event', session, {
    type: 'assistant/message',
    seq: 1,
    time: 5,
    data: { text: 'x' },
    createdAt: 999,
    ignorable: true,
  })
  ctx.emit('session/event', session, { type: 'turn/start', seq: 2, time: 6, data: {} })
  ctx.emit('session/event', session, { type: 'hook/error', seq: 3, time: 7, data: {} })
  ctx.emit('session/event', session, { type: 'custom/unknown', seq: 4, time: 8, data: {} })
  ctx.emit('session/event', session, { type: 'approval/asked', seq: 5, time: 9, data: {} })
  ctx.emit('session/event', session, { type: 'approval/decided', seq: 6, time: 10, data: {} })
  ctx.emit('session/event', null, { type: 'assistant/message' })
  ctx.emit('session/event', session, { type: 'assistant/message', seq: 'bad' })
  assert.deepEqual(seen.map((event) => event.kind), ['output', 'session_state', 'notice', 'output', 'output'])
  assert.equal(seen[0].createdAt, 5)
  assert.equal(seen[0].dshSeq, 1)
  assert.equal(seen[0].data.text, 'x')
  assert.equal(seen[0].ephemeral, false)
  assert.equal(seen[4].dshSeq, null)
  off()
  ctx.emit('session/event', session, { type: 'assistant/message', seq: 7, time: 11, data: {} })
  assert.equal(seen.length, 5)
  adapter.close()
})

test('when DSH supports credential records, only the grant private-key field is read; reference lookup is used only when the record API is absent', async () => {
  const ctx = makeCtx()
  const adapter = new DshHostCredentials({ ctx, hostId: 'host-1' })
  assert.equal(await adapter.getCredential(), null)
  await adapter.setCredential('whalerein.host-private-key.v1', 'base64-value')
  assert.equal(await adapter.getCredential(), 'base64-value')
  const storedKey = [...ctx.records.keys()][0]
  ctx.records.set(storedKey, { kind: 'grant', payload: { secret: 'hidden', value: 'hidden' } })
  assert.equal(await adapter.getCredential(), null)
  ctx.records.set(storedKey, { kind: 'api-key', secret: 'hidden' })
  assert.equal(await adapter.getCredential(), null)
  let referenceReads = 0
  ctx.credentials.resolve = async () => { referenceReads++; return { value: 'decoy-reference' } }
  assert.equal(await adapter.getCredential(), null)
  assert.equal(referenceReads, 0, 'Malformed records must not silently select a different key')
  ctx.credentials.readRecord = async () => { throw new Error('credential read unavailable') }
  await assert.rejects(adapter.getCredential(), error => error.code === 'unavailable' || error.code === 'internal')
  assert.equal(referenceReads, 0)

  const refCtx = makeCtx({
    credentials: {
      async resolve(ref) {
        return ref === 'WHALEREIN_HOST_PRIVATE_KEY' ? { value: 'ref-secret', source: 'env' } : undefined
      },
      async set(ref, value) {
        this.value = { ref, value }
      },
    },
  })
  const refAdapter = new DshHostCredentials({ ctx: refCtx, hostId: 'host-2' })
  assert.equal(await refAdapter.getCredential(), 'ref-secret')
  await refAdapter.setCredential('x', 'y')
  assert.deepEqual(refCtx.credentials.value, { ref: 'WHALEREIN_HOST_PRIVATE_KEY', value: 'y' })

  const emptyCtx = makeCtx({ credentials: {} })
  const emptyAdapter = new DshHostCredentials({ ctx: emptyCtx })
  assert.equal(await emptyAdapter.getCredential(), null)
  await assert.rejects(emptyAdapter.setCredential('x', 'y'), (error) => error.code === 'unsupported_capability')
})

test('when an approval is requested, the real adapter builds its digest, resolves once, and emits lifecycle events', async () => {
  const session = makeLiveSession({
    events: [{ type: 'approval/asked', seq: 1, time: 1, data: { id: 'audit-1', toolName: 'bash', callId: 'call-1', reason: 'r' } }],
  })
  const ctx = makeCtx({ live: new Map([['s1', session]]) })
  const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1', approvalTtlMs: 60_000 } })
  await adapter.open()
  const events = []
  adapter.subscribeApprovals((event) => events.push(event))
  const outcome = ctx.emit('approval/request', {
    agent: { session, sessionId: 'wrong-session' },
    toolName: 'bash',
    callId: 'call-1',
    reason: 'r',
  }, async () => 'unavailable')[0]
  const approval = await adapter.getApproval('audit-1')
  assert.equal(approval.sessionId, 's1')
  assert.equal(approval.toolName, 'bash')
  assert.equal(approval.callId, 'call-1')
  assert.equal(approval.reason, 'r')
  assert.equal(approval.state, 'pending')
  assert.deepEqual([...approval.digest], [...approvalDigest(approval)])
  assert.equal(events[0].type, 'requested')

  const resolved = await adapter.resolveApproval('audit-1', 'approve', approval.digest, approval.version)
  assert.equal(resolved.state, 'approved')
  assert.equal(await outcome, 'allowed-once')
  assert.equal(events[1].type, 'resolved')
  await assert.rejects(adapter.resolveApproval('audit-1', 'approve', approval.digest, approval.version), (error) => error.code === 'approval_already_resolved')
  await assert.rejects(adapter.resolveApproval('missing', 'approve', approval.digest, approval.version), (error) => error.code === 'approval_unavailable')
  await adapter.close()
})

test('when approval digest/version, expiry, session state, or decision is invalid, the real adapter rejects resolution', async () => {
  let now = 1000
  const session = makeLiveSession()
  const state = { active: true, running: true }
  const ctx = makeCtx({
    live: new Map([['s1', session]]),
    stateFor: () => state,
  })
  const adapter = new RealDshAdapter({
    ctx,
    config: { hostId: 'host-1', approvalTtlMs: 100 },
    now: () => now,
  })
  await adapter.open()
  const requested = []
  adapter.subscribeApprovals((event) => {
    if (event.type === 'requested') requested.push(event.approval)
  })
  const open = () => ctx.emit('approval/request', { agent: { session }, toolName: 'bash' }, async () => 'unavailable')[0]

  let outcome = open()
  const first = requested[0]
  await assert.rejects(adapter.resolveApproval(first.approvalId, 'approve', new Uint8Array(32), first.version), (error) => error.code === 'approval_version_mismatch')
  await assert.rejects(adapter.resolveApproval(first.approvalId, 'approve', first.digest, first.version + 1), (error) => error.code === 'approval_version_mismatch')
  await assert.rejects(adapter.resolveApproval(first.approvalId, 'maybe', first.digest, first.version), (error) => error.code === 'invalid_argument')
  await adapter.resolveApproval(first.approvalId, 'reject', first.digest, first.version)
  assert.equal(await outcome, 'rejected')

  outcome = open()
  state.active = false
  const second = requested[1]
  await assert.rejects(adapter.resolveApproval(second.approvalId, 'approve', second.digest, second.version), (error) => error.code === 'approval_unavailable')
  state.active = true
  state.running = false
  await assert.rejects(adapter.resolveApproval(second.approvalId, 'approve', second.digest, second.version), (error) => error.code === 'approval_unavailable')
  state.running = true
  await adapter.resolveApproval(second.approvalId, 'approve', second.digest, second.version)
  assert.equal(await outcome, 'allowed-once')

  outcome = open()
  const third = requested[2]
  now += 200
  assert.equal((await adapter.getApproval(third.approvalId)).state, 'expired')
  await assert.rejects(adapter.resolveApproval(third.approvalId, 'approve', third.digest, third.version), (error) => error.code === 'approval_expired')
  assert.equal(await outcome, 'unavailable')
  await adapter.close()
})

test('when an approval signal is already aborted it returns cancelled, and a transient state-read failure rejects resolution without consuming the pending action', async () => {
  const session = makeLiveSession()
  let failState = false
  const ctx = makeCtx({
    live: new Map([['s1', session]]),
    stateFor: () => {
      if (failState) throw new Error('unavailable')
      return { active: true, running: true }
    },
  })
  const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1', approvalTtlMs: 60_000 } })
  await adapter.open()
  const requested = []
  adapter.subscribeApprovals((event) => {
    if (event.type === 'requested') requested.push(event.approval)
  })
  const aborted = new AbortController()
  aborted.abort()
  const outcome = ctx.emit('approval/request', { agent: { session }, toolName: 'bash', signal: aborted.signal }, async () => 'unavailable')[0]
  assert.equal(await outcome, 'cancelled')
  assert.equal(requested.length, 0)

  const pendingOutcome = ctx.emit('approval/request', { agent: { session }, toolName: 'bash' }, async () => 'unavailable')[0]
  const approval = requested[0]
  failState = true
  await assert.rejects(adapter.resolveApproval(approval.approvalId, 'approve', approval.digest, approval.version), (error) => error.code === 'approval_unavailable')
  assert.equal((await adapter.getApproval(approval.approvalId)).state, 'pending')
  failState = false
  await adapter.resolveApproval(approval.approvalId, 'approve', approval.digest, approval.version)
  assert.equal(await pendingOutcome, 'allowed-once')
  await assert.rejects(adapter.resolveApproval(approval.approvalId, 'approve', approval.digest, approval.version), error => error.code === 'approval_already_resolved')
  await adapter.close()
})

test('when an approval is delegated, aborted or closed, the native callback receives rejected, cancelled or unavailable respectively', async () => {
  const session = makeLiveSession()
  const ctx = makeCtx({ live: new Map([['s1', session]]) })
  const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1', approvalMode: 'delegate' } })
  await adapter.open()
  const delegated = ctx.emit('approval/request', { agent: { session } }, async () => 'rejected')[0]
  assert.equal(await delegated, 'rejected')
  assert.equal((await adapter.getApproval('anything')), null)
  await adapter.close()

  const claimCtx = makeCtx({ live: new Map([['s1', session]]) })
  const claimAdapter = new RealDshAdapter({ ctx: claimCtx, config: { hostId: 'host-1', approvalMode: 'claim' } })
  await claimAdapter.open()
  const requested = []
  claimAdapter.subscribeApprovals((event) => {
    if (event.type === 'requested') requested.push(event.approval)
  })
  const controller = new AbortController()
  const aborted = claimCtx.emit('approval/request', { agent: { session }, toolName: 'bash', signal: controller.signal }, async () => 'unavailable')[0]
  const pending = requested[0]
  controller.abort()
  assert.equal(await aborted, 'cancelled')
  assert.equal((await claimAdapter.getApproval(pending.approvalId)).state, 'unavailable')

  const closeOutcome = claimCtx.emit('approval/request', { agent: { session }, toolName: 'bash' }, async () => 'unavailable')[0]
  await claimAdapter.close()
  assert.equal(await closeOutcome, 'unavailable')
  assert.equal(claimCtx.handlers.get('approval/request').length, 0)
})

test('when DSH services are missing or calls fail, the real adapter returns stable error codes', async () => {
  await assert.rejects(new RealDshAdapter({ ctx: null }).open(), (error) => error.code === 'unavailable')
  await assert.rejects(new RealDshAdapter({ ctx: {} }).open(), (error) => error.code === 'unavailable')

  const ctx = makeCtx({ live: new Map([['s1', makeLiveSession()]]), failList: true, failPrompt: true, failCancel: true })
  const adapter = new RealDshAdapter({ ctx, config: {} })
  await adapter.open()
  await assert.rejects(adapter.listSessions(), (error) => error.code === 'not_found')
  await assert.rejects(adapter.sendUserMessage('s1', 'x'), (error) => error.code === 'conflict')
  await assert.rejects(adapter.interrupt('s1'), (error) => error.code === 'not_found')

  const bare = makeCtx()
  bare.sessionController = undefined
  bare.sessions = undefined
  const bareAdapter = new RealDshAdapter({ ctx: bare, config: {} })
  await assert.rejects(bareAdapter.open(), (error) => error.code === 'unavailable')

  const fallbackCtx = makeCtx({ live: new Map([['s1', makeLiveSession()]]), failList: true })
  const fallbackAdapter = new RealDshAdapter({ ctx: fallbackCtx, config: {} })
  await fallbackAdapter.open()
  await assert.rejects(fallbackAdapter.getSessionSnapshot(''), (error) => error.code === 'invalid_argument')
  const fallbackSnapshot = await fallbackAdapter.getSessionSnapshot('s1')
  assert.equal(fallbackSnapshot.summary.sessionId, 's1')
  assert.equal(fallbackSnapshot.summary.createdAt, 10)
  assert.equal(fallbackSnapshot.summary.cwd, '/tmp')
  assert.equal(fallbackSnapshot.summary.running, true)
  assert.equal(fallbackSnapshot.summary.queuedPrompts, 1)
  await fallbackAdapter.close()

  const listFallbackCtx = makeCtx({
    live: new Map([['s1', makeLiveSession({ parentSession: 'parent-live' })]]),
  })
  delete listFallbackCtx.sessionController.list
  const listFallbackAdapter = new RealDshAdapter({ ctx: listFallbackCtx, config: {} })
  await listFallbackAdapter.open()
  const fallbackList = await listFallbackAdapter.listSessions()
  assert.equal(fallbackList[0].sessionId, 's1')
  assert.equal(fallbackList[0].parentSessionId, 'parent-live')
  assert.equal(fallbackList[0].createdAt, 10)
  assert.equal(fallbackList[0].updatedAt, 10)
  assert.equal(fallbackList[0].cwd, '/tmp')
  assert.equal(fallbackList[0].running, false)
  assert.equal(fallbackList[0].blank, true)
  assert.equal(fallbackList[0].queuedPrompts, 0)
  await listFallbackAdapter.close()

  const brokenCredCtx = makeCtx({
    credentials: {
      async readRecord() { throw new Error('unavailable') },
      async modifyRecord() { throw new Error('unavailable') },
    },
  })
  const brokenCredAdapter = new DshHostCredentials({ ctx: brokenCredCtx })
  await assert.rejects(brokenCredAdapter.getCredential(), (error) => error.code === 'unavailable')
  await assert.rejects(brokenCredAdapter.setCredential('x', 'y'), (error) => error.code === 'unavailable')

  const throwingCtx = makeCtx()
  throwingCtx.on = () => () => { throw new Error('dispose failed') }
  const throwingAdapter = new RealDshAdapter({ ctx: throwingCtx, config: {} })
  await throwingAdapter.open()
  await throwingAdapter.close()

  const noEvents = makeCtx()
  noEvents.sessionController.events = undefined
  const noEventsAdapter = new RealDshAdapter({ ctx: noEvents, config: {} })
  await noEventsAdapter.open()
  await assert.rejects(noEventsAdapter.getSessionSnapshot('cold'), (error) => error.code === 'unsupported_capability')
  assert.equal(noEventsAdapter.describe().available, true)
  await noEventsAdapter.close()

  const noPrompt = makeCtx()
  noPrompt.sessionController.prompt = undefined
  const noPromptAdapter = new RealDshAdapter({ ctx: noPrompt, config: {} })
  await noPromptAdapter.open()
  await assert.rejects(noPromptAdapter.sendUserMessage('s1', 'x'), (error) => error.code === 'unsupported_capability')
  noPrompt.sessionController.cancel = undefined
  await assert.rejects(noPromptAdapter.interrupt('s1'), (error) => error.code === 'unsupported_capability')
  noPrompt.sessionController.state = undefined
  assert.equal((await noPromptAdapter.getSessionSnapshot('s1')).complete, true)
  await noPromptAdapter.close()
})

test('when DSH emits assistant frames, the adapter forwards visible deltas and lifecycle without tool arguments', async () => {
  const ctx = makeCtx()
  const adapter = new RealDshAdapter({ ctx })
  await adapter.open()
  const seen = []
  adapter.subscribeEvents(event => seen.push(event))
  const agent = { session: { id: 's1' } }
  const emit = frame => ctx.emit('agent/assistant-stream', { agent, frame: { attemptId: 'attempt-1', ...frame } })
  emit({ type: 'start', revision: 1, turn: 2, step: 3 })
  emit({ type: 'chunk', revision: 2, index: 0, chunk: { type: 'text-delta', text: 'x' } })
  emit({ type: 'chunk', revision: 3, index: 1, chunk: { type: 'tool-call-delta', argumentsDelta: 'fixture-argument' } })
  emit({ type: 'end', revision: 4, outcome: { kind: 'committed', seq: 9 } })
  assert.equal(seen.length, 4)
  assert.equal(seen[1].data.text.length, 1)
  assert.equal(seen[1].type, 'assistant/preview')
  assert.equal(seen[1].ephemeral, true)
  assert.equal(seen[1].dshSeq, null)
  assert.equal('text' in seen[2].data, false)
  assert.equal('argumentsDelta' in seen[2].data, false)
  assert.equal('committed_seq' in seen[3].data, false)
  ctx.emit('agent/assistant-stream', { agent: { session: { id: 's1' } }, frame: { attemptId: 'attempt-1', type: 'start', revision: 1, turn: 2, step: 3 } })
  assert.notEqual(seen[0].data.stream_id, seen[4].data.stream_id)
  assert.equal(seen[0].data.stream_id, seen[1].data.stream_id)
  await adapter.close()
  emit({ type: 'start', revision: 5, turn: 2, step: 4 })
  assert.equal(seen.length, 5)
})

test('native history watermarks use live source positions or the actual cold-record fence', async () => {
  const live=makeLiveSession({events:[{type:'assistant/message',seq:1,time:1,data:{text:'x'}}]})
  const ctx=makeCtx({live:new Map([['s1',live]])})
  const adapter=new RealDshAdapter({ctx,config:{}})
  await adapter.open()
  assert.equal(await adapter.nativeHead('s1'),1)
  const cold=await adapter.getSessionSnapshot('cold')
  assert.equal(await adapter.nativeHead('cold'),cold.cursor)
  assert.equal(cold.retained_from,1)
  await adapter.close()
})

for (const operation of ['prompt', 'cancel']) {
  test(`when native ${operation} acts but loses its reply, the real adapter preserves uncertainty despite a stable error code`, async () => {
    const ctx = makeCtx({ live: new Map([['s1', makeLiveSession({ running: false })]]) })
    let nativeCalls = 0
    ctx.sessionController[operation] = async () => {
      nativeCalls++
      throw new Error('transport unavailable after native action')
    }
    const adapter = new RealDshAdapter({ ctx, config: {} })
    await adapter.open()
    try {
      const command = operation === 'prompt' ? { send_message: { text: 'fixture message' } } : { interrupt: {} }
      await assert.rejects(adapter.execute('s1', command), error =>
        error instanceof AdapterError && error.code === 'unavailable' && !(error instanceof DefinitiveExecutionError))
      assert.equal(nativeCalls, 1)
    } finally {
      await adapter.close()
    }
  })
}

test('when native controls are unavailable before invocation, the real adapter explicitly proves definitive failure', async () => {
  const adapter = new RealDshAdapter({ ctx: {}, config: {} })
  await assert.rejects(adapter.sendUserMessage('s1', 'fixture message'), error => error instanceof DefinitiveExecutionError && error.code === 'unsupported_capability')
  await assert.rejects(adapter.interrupt('s1'), error => error instanceof DefinitiveExecutionError && error.code === 'unsupported_capability')
})

for (const operation of ['prompt', 'cancel']) {
  test(`when native ${operation} throws a nested definitive error, that proof does not apply to the enclosing invocation`, async () => {
    const ctx = makeCtx({ live: new Map([['s1', makeLiveSession({ running: false })]]) })
    ctx.sessionController[operation] = async () => { throw new DefinitiveExecutionError('unavailable') }
    const adapter = new RealDshAdapter({ ctx, config: {} })
    await adapter.open()
    try {
      const command = operation === 'prompt' ? { send_message: { text: 'fixture message' } } : { interrupt: {} }
      await assert.rejects(adapter.execute('s1', command), error =>
        error instanceof AdapterError && error.code === 'unavailable' && !(error instanceof DefinitiveExecutionError))
    } finally {
      await adapter.close()
    }
  })
}

test('when diagnostics throw after an approval settles, a nested definitive error cannot falsely prove approval execution failed', async () => {
  const session = makeLiveSession()
  const ctx = makeCtx({ live: new Map([['s1', session]]) })
  const adapter = new RealDshAdapter({ ctx, config: { approvalMode: 'claim' }, logger: {
    info(event) { if (event === 'approval_resolved') throw new DefinitiveExecutionError('internal') },
  } })
  await adapter.open()
  const requested = []
  adapter.subscribeApprovals(event => { if (event.type === 'requested') requested.push(event.approval) })
  try {
    const outcome = ctx.emit('approval/request', { agent: { session }, toolName: 'bash' }, async () => 'unavailable')[0]
    const approval = requested[0]
    await assert.rejects(adapter.resolveApproval(approval.approvalId, 'approve', approval.digest, approval.version), error =>
      error instanceof AdapterError && !(error instanceof DefinitiveExecutionError))
    assert.equal(await outcome, 'allowed-once')
    assert.equal((await adapter.getApproval(approval.approvalId)).state, 'approved')
  } finally {
    await adapter.close()
  }
})

test('when a live history tail is truncated, delta readers cannot skip the omitted range', async () => {
  const live = makeLiveSession({sessionId:'s1',events:[0,1].map(seq=>({seq,type:'user/message',time:seq,data:{text:''}}))})
  const ctx=makeCtx({live:new Map([['s1',live]])})
  const adapter=new RealDshAdapter({ctx,config:{snapshotMaxEvents:2}})
  await adapter.open()
  try {
    const baseline=await adapter.readHistory('s1',{snapshot:true})
    for(let seq=2;seq<6;seq++)live.events.push({seq,type:'user/message',time:seq,data:{text:''}})
    const snapshot=await adapter.getSessionSnapshot('s1')
    assert.equal(snapshot.retained_from,4)
    await assert.rejects(adapter.readHistory('s1',{after_cursor:baseline.batch.to_cursor}),error=>error.code==='snapshot_required')
  } finally {await adapter.close()}
})

test('Agent-scoped native approvals reach the remote adapter through Cordis routing', async () => {
  const { Context } = await import('@deepseek-ai/cordis')
  const bus = new Context()
  const session = makeLiveSession()
  // A previously registered UI answerer is terminal, as the native Remote bridge is.
  bus.on('approval/request', () => 'rejected', { global: true })
  const ctx = makeCtx({ live: new Map([[session.id, session]]) })
  ctx.on = bus.on.bind(bus)
  const adapter = new RealDshAdapter({ ctx })
  await adapter.open()
  try {
    // Native Agent carriers filter unrelated plugin contexts. A deployment-wide
    // approval answerer must explicitly participate in that dispatch.
    const carrier = { [Context.filter]: () => false }
    const outcome = bus.waterfall(carrier, 'approval/request', {
      agent: { session }, toolName: 'native-fixture',
    }, () => Promise.resolve('unavailable'))
    await new Promise(resolve => setImmediate(resolve))
    const pending = await adapter.listApprovals(session.id)
    assert.equal(pending.length, 1)
    await adapter.close()
    assert.equal(await outcome, 'unavailable')
  } finally {
    await adapter.close()
  }
})
