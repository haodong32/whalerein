import { DshHostCredentials } from '../src/credentials.js'
import test from 'node:test'
import assert from 'node:assert/strict'
import { RealDshAdapter } from '../src/adapter/real.js'
import { AdapterError, DefinitiveExecutionError } from 'whalerein-host/errors.js'
import { generateKeypair } from 'whalerein-host/security/noise.js'
import { approvalDigest } from 'whalerein-host/security/digest.js'
import { codec } from 'whalerein-host/protocol.js'

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
        // The passive native page service serves hot and cold Sessions. The
        // fixture mirrors that: a live Session's own events feed the page.
        const sessionId = request.address?.sessionId
        const liveSession = live.get(sessionId)
        const sourceEvents = liveSession ? liveSession.events : coldEvents
        const before = request.beforeSeq
        const through = request.throughSeq
        const eligible = sourceEvents.filter((event) =>
          (through === undefined || event.seq <= through) && (before === undefined || event.seq < before))
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

test('when live DSH sessions expose projections, listing and snapshots preserve the documented fields and complete history', async () => {
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

  // The former 5,000-event tail truncation is removed: a passive snapshot is
  // complete and never silently substitutes a retained tail for the log.
  const full = new RealDshAdapter({ ctx, config: { snapshotMaxEvents: 1 } })
  await full.open()
  const fullSnapshot = await full.getSessionSnapshot('s1')
  assert.equal(fullSnapshot.complete, true)
  assert.equal(fullSnapshot.events.length, 2)
  assert.equal(fullSnapshot.retained_from, 1)
  await full.close()

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
    arguments: { command: 'npm test' },
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
  const open = () => ctx.emit('approval/request', { agent: { session }, toolName: 'bash', arguments: { command: 'npm test' } }, async () => 'unavailable')[0]

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
  assert.equal(await outcome, 'cancelled')
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

  const pendingOutcome = ctx.emit('approval/request', { agent: { session }, toolName: 'bash', arguments: { command: 'npm test' } }, async () => 'unavailable')[0]
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
  emit({ type: 'chunk', revision: 2, index: 0, chunk: { type: 'text-delta', index: 0, text: 'x' } })
  emit({ type: 'chunk', revision: 3, index: 1, chunk: { type: 'reasoning-delta', index: 1, text: 'why' } })
  emit({ type: 'chunk', revision: 4, index: 2, chunk: { type: 'tool-call-delta', index: 2, id: 'call-1', argumentsDelta: 'fixture-argument' } })
  emit({ type: 'end', revision: 5, outcome: { kind: 'committed', seq: 9 } })
  // The tool-call frame carries no displayable live text and never crosses the
  // seam; the committed message contributes its typed ToolActivity instead.
  assert.equal(seen.length, 4)
  assert.equal(seen[1].data.text, 'x')
  assert.equal(seen[1].data.block_kind, 'BLOCK_KIND_TEXT')
  assert.equal(seen[2].data.text, 'why')
  assert.equal(seen[2].data.block_kind, 'BLOCK_KIND_REASONING')
  assert.equal(seen[1].type, 'assistant/preview')
  assert.equal(seen[1].ephemeral, true)
  assert.equal(seen[1].dshSeq, null)
  assert.equal('argumentsDelta' in seen[2].data, false)
  assert.equal('committed_seq' in seen[3].data, false)
  ctx.emit('agent/assistant-stream', { agent: { session: { id: 's1' } }, frame: { attemptId: 'attempt-1', type: 'start', revision: 1, turn: 2, step: 3 } })
  assert.notEqual(seen[0].data.stream_id, seen[4].data.stream_id)
  assert.equal(seen[0].data.stream_id, seen[1].data.stream_id)
  await adapter.close()
  emit({ type: 'start', revision: 6, turn: 2, step: 4 })
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
      const command = operation === 'prompt' ? { send_message: { text: 'fixture message', delivery_mode: 'START_IF_IDLE' } } : { interrupt: {} }
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
      const command = operation === 'prompt' ? { send_message: { text: 'fixture message', delivery_mode: 'START_IF_IDLE' } } : { interrupt: {} }
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
    const outcome = ctx.emit('approval/request', { agent: { session }, toolName: 'bash', arguments: { command: 'npm test' } }, async () => 'unavailable')[0]
    const approval = requested[0]
    await assert.rejects(adapter.resolveApproval(approval.approvalId, 'approve', approval.digest, approval.version), error =>
      error instanceof AdapterError && !(error instanceof DefinitiveExecutionError))
    assert.equal(await outcome, 'allowed-once')
    assert.equal((await adapter.getApproval(approval.approvalId)).state, 'approved')
  } finally {
    await adapter.close()
  }
})

test('a complete passive snapshot keeps appends in a forward read instead of fabricating a retained tail', async () => {
  const live = makeLiveSession({sessionId:'s1',events:[0,1].map(seq=>({seq,type:'user/message',time:seq,data:{text:''}}))})
  const ctx=makeCtx({live:new Map([['s1',live]])})
  const adapter=new RealDshAdapter({ctx,config:{snapshotMaxEvents:2}})
  await adapter.open()
  try {
    const baseline=await adapter.readHistory('s1',{snapshot:true})
    assert.equal(baseline.complete,true)
    for(let seq=2;seq<6;seq++)live.events.push({seq,type:'user/message',time:seq,data:{text:''}})
    const snapshot=await adapter.getSessionSnapshot('s1')
    assert.equal(snapshot.complete,true)
    assert.equal(snapshot.events.length,6)
    assert.equal(snapshot.retained_from,0)
    const next=await adapter.readHistory('s1',{after_cursor:baseline.batch.to_cursor})
    assert.equal(next.batch.changes.length,4)
    assert.equal(next.complete,true)
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

test('delivery modes are open by default and native prompt mapping retains operation identity', async () => {
  const ctx = makeCtx({ live: new Map([['s1', makeLiveSession({ running: false })]]) })
  const calls = []
  ctx.sessionController.prompt = async request => { calls.push(request); return {accepted: true} }
  const adapter = new RealDshAdapter({ ctx, config: {} })
  await adapter.open()
  try {
    assert.deepEqual((await adapter.getSession('s1')).sourceCapabilities.delivery_modes, ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER'])
    const queued = await adapter.sendUserMessage('s1', 'fixture', 'queue', 'op-1')
    const steered = await adapter.sendUserMessage('s1', 'fixture', 'steer', 'op-2')
    assert.deepEqual(calls.map(({mode, requestId}) => ({mode, requestId})), [{mode: 'queue', requestId: 'op-1'}, {mode: 'steer', requestId: 'op-2'}])
    assert.equal(queued.deliveryId, 'op-1')
    assert.equal(steered.deliveryId, 'op-2')
  } finally { await adapter.close() }
})

test('a native prompt without explicit acceptance remains uncertain after source entry', async () => {
  for (const outcome of [undefined, null, {}, {accepted: false}, {accepted: 'true'}, {accepted: 1}]) {
    const ctx = makeCtx({live: new Map([['s1', makeLiveSession({running: false})]])})
    let calls = 0
    ctx.sessionController.prompt = async () => { calls++; return outcome }
    const adapter = new RealDshAdapter({ctx, config: {}})
    await adapter.open()
    try {
      await assert.rejects(
        adapter.execute('s1', {send_message: {text: 'fixture', delivery_mode: 'START_IF_IDLE'}}, {operationId: 'unconfirmed'}),
        error => error instanceof AdapterError && !(error instanceof DefinitiveExecutionError),
      )
      assert.equal(calls, 1)
    } finally { await adapter.close() }
  }
})

test('a real adapter without native prompt advertises no delivery mode and fails closed', async () => {
  const ctx = makeCtx({live: new Map([['s1', makeLiveSession({running: false})]])})
  delete ctx.sessionController.prompt
  const adapter = new RealDshAdapter({ctx, config: {}})
  await adapter.open()
  try {
    assert.deepEqual((await adapter.getSession('s1')).sourceCapabilities.delivery_modes, ['START_IF_IDLE'])
    await assert.rejects(adapter.execute('s1', {send_message: {text: 'fixture'}}), error => error.code === 'unsupported_capability')
    await assert.rejects(adapter.execute('s1', {send_message: {text: 'fixture', delivery_mode: 'STEER'}}), error => error.code === 'unsupported_capability')
  } finally { await adapter.close() }
})

test('start-if-idle uses only a sampled state and reports the actual later native receipt', async () => {
  const session = makeLiveSession({running: true})
  const ctx = makeCtx({live: new Map([['s1', session]])})
  let calls = 0
  ctx.sessionController.prompt = async () => { calls++; return {accepted: true} }
  const adapter = new RealDshAdapter({ctx, config: {}})
  await adapter.open()
  try {
    await assert.rejects(adapter.execute('s1', {send_message: {text: 'fixture', delivery_mode: 'START_IF_IDLE'}}), error => error.code === 'conflict')
    assert.equal(calls, 0)
    session.running = false
    const list = ctx.sessionController.list.bind(ctx.sessionController)
    ctx.sessionController.list = async () => {
      const sampled = await list()
      session.running = true
      return sampled
    }
    const receipt = await adapter.execute('s1', {send_message: {text: 'fixture', delivery_mode: 'START_IF_IDLE'}}, {operationId: 'race-1'})
    assert.equal(calls, 1)
    assert.equal(receipt.command_accepted.delivery_id, 'race-1')
  } finally { await adapter.close() }
})

test('native wait discards output and preserves cold idle state', async () => {
  const ctx = makeCtx()
  ctx.sessionController.wait = async (request, signal) => {
    assert.equal(request.sessionId, 's1')
    assert.ok(request.timeoutMs > 0 && request.timeoutMs <= 1000)
    assert.equal(signal.aborted, false)
    return { reason: 'idle', state: { active: false, running: false, queue: null, pendingInput: [] }, output: { private: 'synthetic' } }
  }
  const adapter = new RealDshAdapter({ ctx })
  await adapter.open()
  assert.deepEqual(await adapter.waitSession('s1', 1000, new AbortController().signal), {
    reason: 'idle', state: { active: false, running: false, queueAvailable: false, pendingInput: [] },
  })
  await adapter.close()
})

test('native wait wakes for claimed approval and cancellation never interrupts the agent', async () => {
  const session = makeLiveSession()
  const ctx = makeCtx({ live: new Map([['s1', session]]) })
  ctx.sessionController.wait = async (_request, signal) => new Promise((resolve, reject) => {
    signal.addEventListener('abort', () => reject(signal.reason), { once: true })
  })
  const adapter = new RealDshAdapter({ ctx, config: { approvalMode: 'claim' } })
  await adapter.open()
  const wait = adapter.waitSession('s1', 1000, new AbortController().signal)
  const approval = ctx.emit('approval/request', { agent: { session }, toolName: 'fixture' }, async () => 'unavailable')[0]
  const passive = await adapter.readWaitState('s1', new AbortController().signal)
  assert.deepEqual(passive.pendingInput, ['approval'])
  const otherSession = await adapter.readWaitState('s2', new AbortController().signal)
  assert.deepEqual(otherSession.pendingInput, [])
  const result = await wait
  assert.equal(result.reason, 'input-required')
  assert.deepEqual(result.state.pendingInput, ['approval'])
  await adapter.close()
  await approval
  assert.deepEqual(ctx.calls.cancel, [])
})

test('native wait forwards cancellation and source failure without exposing output', async () => {
  const ctx = makeCtx()
  const adapter = new RealDshAdapter({ ctx })
  await adapter.open()
  await assert.rejects(adapter.waitSession('s1', 1000, new AbortController().signal), error => error.code === 'unsupported_capability')
  ctx.sessionController.wait = async (_request, signal) => new Promise((resolve, reject) => signal.addEventListener('abort', () => reject(signal.reason), { once: true }))
  adapter.listClaimedQuestions = () => []
  adapter.subscribeQuestions = undefined
  await assert.rejects(adapter.waitSession('s1', 1000, new AbortController().signal), error => error.code === 'unsupported_capability')
  delete adapter.listClaimedQuestions
  delete adapter.subscribeQuestions
  const abort = new AbortController()
  const pending = adapter.waitSession('s1', 1000, abort.signal)
  abort.abort()
  await assert.rejects(pending)
  ctx.sessionController.wait = async () => { throw new Error('native failure') }
  await assert.rejects(adapter.waitSession('s1', 1000, new AbortController().signal), AdapterError)
  assert.deepEqual(ctx.calls.cancel, [])
  await adapter.close()
})

test('native session creation preserves optional inputs and confirmed identity without private receipt fields', async () => {
  const ctx = makeCtx()
  const calls = []
  ctx.sessionController.create = async request => { calls.push(request); return {sessionId: 'created-1', agentPreset: 'private'} }
  const adapter = new RealDshAdapter({ ctx })
  assert.deepEqual(await adapter.describeHostCapabilities(), {model_catalog: false, model_read: false, session_create: true, session_fork: false})
  assert.deepEqual(await adapter.executeHost({create_session:{}}), {entered:true, identity:{session_id:'created-1'}})
  assert.deepEqual(await adapter.executeHost({create_session:{cwd:'/synthetic',agent_preset:'p',preferred_session_id:'preferred'}}), {entered:true, identity:{session_id:'created-1'}})
  assert.deepEqual(calls, [{}, {cwd:'/synthetic',agentPreset:'p',sessionId:'preferred'}])
  for (const input of [{cwd:'relative'},{cwd:'/x',workspace_id:'w'},{workspace_id:' '},{preferred_session_id:''}]) {
    await assert.rejects(adapter.executeHost({create_session:input}), error => error instanceof DefinitiveExecutionError && error.code === 'invalid_argument')
  }
  assert.equal(calls.length, 2)
})

test('native structured attachment failure without verified identity remains unresolved facts', async () => {
  const ctx = makeCtx()
  ctx.sessionController.create = async () => { throw {isDSHRemoteError:true,code:'session/workspace-attach-failed',details:{sessionId:'child',workspaceId:'private'},message:'private'} }
  const adapter = new RealDshAdapter({ctx})
  // Error details alone are not identity proof; the list projection has no child.
  assert.deepEqual(await adapter.executeHost({create_session:{workspace_id:'private'}}), {entered:true, failure_code:'workspace_attach_failed'})
  ctx.sessionController.create = async () => { throw {isDSHRemoteError:true,code:'workspace/not-found',details:{}} }
  assert.deepEqual(await adapter.executeHost({create_session:{workspace_id:'missing'}}), {entered:true, failure_code:'not_found'})
  ctx.sessionController.create = async () => { throw new Error('private diagnostic') }
  assert.deepEqual(await adapter.executeHost({create_session:{}}), {entered:true, failure_code:'internal'})
})

test('verified workspace attach failure reports warning facts with observed identity', async () => {
  const live = new Map([['s1', makeLiveSession({sessionId:'s1'})]])
  const ctx = makeCtx({live})
  ctx.sessionController.create = async () => {
    live.set('child', makeLiveSession({sessionId:'child'}))
    throw {isDSHRemoteError:true,code:'session/workspace-attach-failed',details:{sessionId:'child',workspaceId:'w'},message:'private'}
  }
  const adapter = new RealDshAdapter({ctx})
  assert.deepEqual(await adapter.executeHost({create_session:{workspace_id:'w'}}),
    {entered:true, identity:{session_id:'child'}, attach_error:'workspace_attach_failed'})
})

test('creation preflight reports registry facts without invoking native create', async () => {
  const live = new Map([['preferred', makeLiveSession({sessionId:'preferred', blank:true, running:false})]])
  const ctx = makeCtx({live})
  let calls = 0
  ctx.sessionController.create = async () => { calls++; return {sessionId:'native'} }
  ctx.agentPresets = { resolve: async id => {
    if (id === 'known') return { id }
    throw Object.assign(new Error('agent-preset/not-found'), {code:'agent-preset/not-found'})
  } }
  ctx.workspaceRegistry = { get: id => id === 'w' ? { attachSession: async () => {} } : undefined, archivedSessionIds: [] }
  const adapter = new RealDshAdapter({ctx})
  assert.deepEqual(await adapter.preflightCreateSession({create_session:{workspace_id:'missing'}}), {workspace:'missing', preset:'not_configured', reuse:''})
  assert.deepEqual(await adapter.preflightCreateSession({create_session:{workspace_id:'w',agent_preset:'missing'}}), {workspace:'present', preset:'missing', reuse:''})
  assert.deepEqual(await adapter.preflightCreateSession({create_session:{workspace_id:'w',agent_preset:'known'}}), {workspace:'present', preset:'present', reuse:''})
  assert.equal(calls, 0)
})

test('creation preflight claims reuse only for a proven blank unarchived identity', async () => {
  const live = new Map([['preferred', makeLiveSession({sessionId:'preferred', blank:true, running:false})]])
  const ctx = makeCtx({live})
  ctx.sessionController.create = async () => ({sessionId:'native'})
  const workspaces = { 'w': { attachSession: async () => {}, sessionIds: ['preferred'] }, 'other': { attachSession: async () => {}, sessionIds: [] } }
  ctx.workspaceRegistry = { get: id => workspaces[id], archivedSessionIds: [] }
  const adapter = new RealDshAdapter({ctx})
  assert.equal((await adapter.preflightCreateSession({create_session:{preferred_session_id:'preferred',workspace_id:'w'}})).reuse, 'preferred')
  ctx.workspaceRegistry.archivedSessionIds = ['preferred']
  assert.equal((await adapter.preflightCreateSession({create_session:{preferred_session_id:'preferred',workspace_id:'w'}})).reuse, '')
  ctx.workspaceRegistry.archivedSessionIds = []
  assert.equal((await adapter.preflightCreateSession({create_session:{preferred_session_id:'preferred',workspace_id:'other'}})).reuse, '')
  // Without an archived projection the reuse cannot be proved.
  delete ctx.workspaceRegistry.archivedSessionIds
  assert.equal((await adapter.preflightCreateSession({create_session:{preferred_session_id:'preferred'}})).reuse, '')
})

test('the workspace registry marks archived sessions in list and single reads', async () => {
  const live = new Map([
    ['live', makeLiveSession({ sessionId: 'live' })],
    ['gone', makeLiveSession({ sessionId: 'gone' })],
  ])
  const ctx = makeCtx({ live })
  ctx.workspaceRegistry = { archivedSessionIds: ['gone'] }
  const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1' } })
  await adapter.open()
  const sessions = await adapter.listSessions()
  assert.deepEqual(sessions.map((session) => [session.sessionId, session.archived]), [['live', false], ['gone', true]])
  assert.equal((await adapter.getSession('gone')).archived, true)
  assert.equal((await adapter.getSession('live')).archived, false)
  // The store/list fallback path carries the same projection.
  delete ctx.sessionController.list
  const fallback = await adapter.listSessions()
  assert.deepEqual(fallback.map((session) => [session.sessionId, session.archived]), [['live', false], ['gone', true]])
})

test('a missing or unreadable archive registry reports no archived sessions', async () => {
  const live = new Map([['s1', makeLiveSession({ sessionId: 's1' })]])
  const ctx = makeCtx({ live })
  const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1' } })
  await adapter.open()
  assert.equal((await adapter.listSessions())[0].archived, false)
  ctx.workspaceRegistry = { get archivedSessionIds() { throw new Error('registry unavailable') } }
  assert.equal((await adapter.listSessions())[0].archived, false)
})

test('fork preflight proves a completed turn only from readable native projection', async () => {
  const complete = new Map([['parent', makeLiveSession({sessionId:'parent', events:[
    {type:'user/message',seq:1,time:1,data:{text:'x'}},
    {type:'turn/end',seq:2,time:2,data:{running:false}},
  ]})]])
  const ctx = makeCtx({live: complete})
  const adapter = new RealDshAdapter({ctx})
  assert.deepEqual(await adapter.preflightForkSession('parent'), {completed_turn:true})
  const empty = new Map([['parent', makeLiveSession({sessionId:'parent', events:[]})]])
  assert.deepEqual(await new RealDshAdapter({ctx:makeCtx({live:empty})}).preflightForkSession('parent'), {completed_turn:false})
  const unreadable = new RealDshAdapter({ctx:makeCtx({live:complete})})
  unreadable.getSessionSnapshot = async () => { throw new AdapterError('unavailable') }
  assert.deepEqual(await unreadable.preflightForkSession('parent'), {completed_turn:'unknown'})
})

test('pending queue distinguishes cold state and protects unowned content', async () => {
  let state = {running:false,queue:null}
  const ctx = makeCtx({stateFor:()=>state})
  const adapter = new RealDshAdapter({ctx,config:{pendingQueueEnabled:true}})
  assert.deepEqual(await adapter.readPendingQueue('s1'), {session_id:'s1',available:false,running:false,items:[]})
  state = {running:true,queue:[
    {id:'a',placement:'queued',rpcId:'delivery',content:[{type:'text',text:'fixture'}, {type:'image',attachment:{attachmentId:'private',mediaType:'image/png',bytes:42,name:'fixture.png'}}, {type:'file',attachment:{attachmentId:'private',bytes:7,name:'../untrusted'}}]},
    {id:'b',placement:'context',content:[{type:'text',text:'private'}]},
    {id:'c',placement:'future-kind',rpcId:'request',content:[{type:'text',text:'private'}]},
  ]}
  const queue = await adapter.readPendingQueue('s1')
  assert.equal(queue.available,true)
  assert.deepEqual(queue.items[0].actions,['EDIT','REMOVE','PROMOTE'])
  assert.equal(queue.items[0].delivery_id,'delivery')
  assert.equal(queue.items[1].delivery_id,'')
  assert.deepEqual(queue.items[0].blocks[1].attachment,{media_type:'image/png',name:'fixture.png',size:42,available:false})
  assert.deepEqual(queue.items[0].blocks[2].attachment,{media_type:'',name:'',size:7,available:false})
  assert.deepEqual(queue.items[1].actions,[])
  assert.deepEqual(queue.items[2].actions,[])
  assert.equal(queue.items[2].delivery_id,'')
  assert.equal(JSON.stringify(queue).includes('private'),false)
})

test('pending queue mutations require known ownership and preserve uncertain promotion', async () => {
  const ctx = makeCtx({stateFor:()=>({running:true,queue:[{id:'a',placement:'queued',rpcId:'r',content:[{type:'text',text:'fixture'}]}]})})
  const calls=[]
  ctx.sessionController.updateQueue = async (request) => { calls.push(request); if(request.action.kind==='steer') throw new Error('late failure') }
  const adapter = new RealDshAdapter({ctx,config:{pendingQueueEnabled:true}})
  await adapter.updatePendingQueue('s1',{item_id:'a',action:'EDIT',text:'replacement'})
  assert.deepEqual(calls[0].action,{kind:'edit',content:[{type:'text',text:'replacement'}]})
  await assert.rejects(adapter.updatePendingQueue('s1',{item_id:'a',action:'PROMOTE'}),error=>error instanceof AdapterError && !(error instanceof DefinitiveExecutionError))
  await assert.rejects(adapter.updatePendingQueue('s1',{item_id:'missing',action:'REMOVE'}),error=>error.code==='not_found')
  await assert.rejects(adapter.updatePendingQueue('s1',{item_id:'a',action:'EDIT',text:'  '}),error=>error.code==='invalid_argument')
  const disabled = new RealDshAdapter({ctx,config:{pendingQueueEnabled:false}})
  await assert.rejects(disabled.readPendingQueue('s1'),error=>error.code==='unsupported_capability')
})


test('fork delegates the completed boundary and preserves confirmed identities', async () => {
  const ctx = makeCtx()
  let request
  ctx.sessionController.fork = async input => { request = input; return {sessionId:'child'} }
  const adapter = new RealDshAdapter({ctx})
  assert.deepEqual(await adapter.forkSession('parent'), {entered:true, identity:{session_id:'child',parent_session_id:'parent'}})
  assert.deepEqual(request, {sessionId:'parent'})
  const nativeError = (code, details = {}) => Object.assign(new Error('synthetic'), {isDSHRemoteError:true,code,details})
  for (const [code, expected] of [
    ['session/workspace-attach-failed', {entered:true, failure_code:'workspace_attach_failed'}],
    ['session/not-found', {entered:false, failure_code:'not_found'}],
    ['session/fork-unavailable', {entered:false, failure_code:'conflict'}],
  ]) {
    ctx.sessionController.fork = async () => { throw nativeError(code, {sessionId:'unrelated'}) }
    assert.deepEqual(await adapter.forkSession('parent'), expected, code)
  }
  ctx.sessionController.fork = async () => { throw nativeError('gateway/internal') }
  assert.deepEqual(await adapter.forkSession('parent'), {entered:true, failure_code:'internal'})
})

test('verified fork attach failure retains child and parent identities', async () => {
  const live = new Map([['parent', makeLiveSession({sessionId:'parent'})]])
  const ctx = makeCtx({live})
  ctx.sessionController.fork = async () => {
    live.set('child', makeLiveSession({sessionId:'child'}))
    throw {isDSHRemoteError:true,code:'session/workspace-attach-failed',details:{sessionId:'child'},message:'private'}
  }
  const adapter = new RealDshAdapter({ctx})
  assert.deepEqual(await adapter.forkSession('parent'),
    {entered:true, identity:{session_id:'child',parent_session_id:'parent'}, attach_error:'workspace_attach_failed'})
})

test('native wait reconciles scoped claimed-question lifecycle without exposing question content', async () => {
  const ctx = makeCtx()
  ctx.sessionController.wait = async (_request, signal) => new Promise((resolve, reject) => {
    signal.addEventListener('abort', () => reject(signal.reason), { once: true })
  })
  const adapter = new RealDshAdapter({ ctx })
  const handles = new Map()
  const subscribers = new Set()
  adapter.listClaimedQuestions = sessionId => handles.has(sessionId) ? [{ sessionId, batchId: handles.get(sessionId) }] : []
  adapter.subscribeQuestions = callback => { subscribers.add(callback); return () => subscribers.delete(callback) }
  await adapter.open()
  const pending = adapter.waitSession('s1', 3000, new AbortController().signal)
  handles.set('s2', 'foreign')
  for (const callback of subscribers) callback({ type: 'requested', sessionId: 's2', batchId: 'foreign' })
  assert.equal((await adapter.readWaitState('s1', new AbortController().signal)).pendingInput.length, 0)
  handles.set('s1', 'owned')
  for (const callback of subscribers) callback({ type: 'requested', sessionId: 's1', batchId: 'owned' })
  const result = await pending
  assert.equal(result.reason, 'input-required')
  assert.deepEqual(result.state.pendingInput, ['question'])
  assert.deepEqual(Object.keys(result.state).sort(), ['active', 'pendingInput', 'queueAvailable', 'running'])
  handles.delete('s1')
  assert.deepEqual((await adapter.readWaitState('s1', new AbortController().signal)).pendingInput, [])
  assert.equal(subscribers.size, 0)
  await adapter.close()
})


test('a blocked native wait can be cancelled without cancelling another observation', async () => {
  const ctx = makeCtx()
  const entered = []
  ctx.sessionController.wait = async (_request, signal) => {
    entered.push(signal)
    return new Promise((_resolve, reject) => signal.addEventListener('abort', () => reject(signal.reason), { once: true }))
  }
  const adapter = new RealDshAdapter({ ctx })
  await adapter.open()
  const firstAbort = new AbortController()
  const secondAbort = new AbortController()
  const first = adapter.waitSession('s1', 1000, firstAbort.signal)
  const second = adapter.waitSession('s1', 1000, secondAbort.signal)
  firstAbort.abort()
  await assert.rejects(first)
  assert.equal(entered.length, 2)
  assert.equal(entered[0].aborted, true)
  assert.equal(entered[1].aborted, false)
  secondAbort.abort()
  await assert.rejects(second)
  assert.deepEqual(ctx.calls.cancel, [])
  await adapter.close()
})


test('a question released before reconciliation does not produce stale input', async () => {
  const ctx = makeCtx()
  let calls = 0
  ctx.sessionController.wait = async (_request, signal) => {
    if (++calls > 1) return { reason: 'idle', state: { active: true, running: false, queue: [], pendingInput: [] } }
    return new Promise((_resolve, reject) => signal.addEventListener('abort', () => reject(signal.reason), { once: true }))
  }
  const adapter = new RealDshAdapter({ ctx })
  const subscribers = new Set()
  let active = false
  adapter.listClaimedQuestions = id => active && id === 's1' ? [{ sessionId: id, batchId: 'short-lived' }] : []
  adapter.subscribeQuestions = callback => { subscribers.add(callback); return () => subscribers.delete(callback) }
  await adapter.open()
  const pending = adapter.waitSession('s1', 1000, new AbortController().signal)
  active = true
  for (const callback of subscribers) callback({ type: 'requested', sessionId: 's1', batchId: 'short-lived' })
  active = false
  const result = await pending
  assert.equal(result.reason, 'idle')
  assert.deepEqual(result.state.pendingInput, [])
  assert.equal(calls, 2)
  assert.equal(subscribers.size, 0)
  await adapter.close()
})


test('native source failure is not swallowed by a simultaneous claimed-question wake', async () => {
  const ctx = makeCtx()
  let fail
  ctx.sessionController.wait = async () => new Promise((_resolve, reject) => { fail = reject })
  const adapter = new RealDshAdapter({ ctx })
  const subscribers = new Set()
  let active = false
  adapter.listClaimedQuestions = id => active && id === 's1' ? [{ sessionId: id, batchId: 'q' }] : []
  adapter.subscribeQuestions = callback => { subscribers.add(callback); return () => subscribers.delete(callback) }
  await adapter.open()
  const waiting = adapter.waitSession('s1', 1000, new AbortController().signal)
  active = true
  for (const callback of subscribers) callback({ type: 'requested', sessionId: 's1', batchId: 'q' })
  fail(new Error('synthetic source failure'))
  await assert.rejects(waiting, error => error instanceof AdapterError && error.code === 'internal')
  assert.equal(subscribers.size, 0)
  await adapter.close()
})

test('native inbox splices advance the source cursor without creating conversation messages', async () => {
  const session=makeLiveSession({events:[
    {type:'user/message',seq:1,time:1,data:{text:'fixture'}},
    {type:'agent/inbox/spliced',seq:2,time:2,data:{target:'next-turn',inserted:[{content:'synthetic-private'}]}},
    {type:'assistant/message',seq:3,time:3,data:{text:'fixture'}},
  ]})
  const ctx=makeCtx({live:new Map([['s1',session]])})
  const adapter=new RealDshAdapter({ctx})
  const snapshot=await adapter.getSessionSnapshot('s1')
  assert.equal(snapshot.cursor,3)
  assert.deepEqual(snapshot.events.map(event=>event.type),['user/message','agent/inbox/spliced','assistant/message'])
  assert.equal(snapshot.events[1].data,null)
  assert.equal(JSON.stringify(snapshot).includes('synthetic-private'),false)
  const notice=adapter.conversation.history.notification(snapshot.events[1])
  assert.deepEqual(notice.changes,[])
  assert.equal(JSON.parse(Buffer.from(notice.from_cursor,'base64url').toString())[2],1)
  assert.equal(JSON.parse(Buffer.from(notice.to_cursor,'base64url').toString())[2],2)
  const history=await adapter.readHistory('s1',{snapshot:true})
  assert.equal(history.batch.changes.length,2)
})

test('queue deadline expiring during native precheck prevents the mutation', async () => {
  let now=1000,updates=0
  const ctx=makeCtx({stateFor:()=>{now=2000;return {running:true,queue:[{id:'q',placement:'queued',rpcId:'r',content:[{type:'text',text:'fixture'}]}]}}})
  ctx.sessionController.updateQueue=async()=>{updates++}
  const adapter=new RealDshAdapter({ctx,now:()=>now,config:{pendingQueueEnabled:true}})
  await assert.rejects(adapter.execute('s1',{update_pending_queue:{item_id:'q',action:'REMOVE'}},{expiresAt:1500}),error=>error instanceof DefinitiveExecutionError && error.code==='expired')
  assert.equal(updates,0)
})

test('queue projection replaces unknown blocks with safe metadata without exposing tool parameters', async () => {
  const ctx=makeCtx({stateFor:()=>({running:true,queue:[{id:'q',placement:'queued',rpcId:'r',content:[{type:'tool-call',name:'fixture',arguments:{value:'synthetic-private-parameter'}}]}]})})
  const adapter=new RealDshAdapter({ctx,config:{pendingQueueEnabled:true}})
  const queue=await adapter.readPendingQueue('s1')
  assert.equal(queue.items[0].blocks[0].notice.category,'unsupported_content')
  assert.equal(JSON.stringify(queue).includes('synthetic-private-parameter'),false)
  assert.equal(JSON.stringify(queue).includes('arguments'),false)
})

test('a future non-text wire field without text cannot edit a native queued item', async () => {
  const ctx=makeCtx({stateFor:()=>({running:true,queue:[{id:'q',placement:'queued',rpcId:'r',content:[{type:'text',text:'fixture'}]}]})})
  let nativeUpdates=0
  ctx.sessionController.updateQueue=async()=>{nativeUpdates++}
  const adapter=new RealDshAdapter({ctx,config:{pendingQueueEnabled:true}})
  const wire=codec()
  const edit=wire.encode('whalerein.v1.UpdatePendingQueue',{session_id:'s1',item_id:'q',action:'EDIT'})
  // Field 5 is unknown to the current schema; no supported non-text field exists.
  const unknown=Buffer.from([0x2a,0x05,0x69,0x6d,0x61,0x67,0x65])
  const decoded=wire.decode('whalerein.v1.UpdatePendingQueue',Buffer.concat([edit,unknown]))
  await assert.rejects(adapter.updatePendingQueue('s1',decoded),error=>error instanceof DefinitiveExecutionError && error.code==='invalid_argument')
  assert.equal(nativeUpdates,0)
})

test('pending queue concurrent edits follow source order and consumed items are not recreated', async () => {
  let queue=[{id:'q',placement:'queued',rpcId:'request',content:[{type:'text',text:'initial'}]}]
  let releaseFirst
  let firstEntered
  const firstBlocked=new Promise(resolve=>{releaseFirst=resolve})
  const started=new Promise(resolve=>{firstEntered=resolve})
  const ctx=makeCtx({stateFor:()=>({running:true,queue})})
  ctx.sessionController.updateQueue=async request=>{
    if(request.action.content?.[0]?.text==='first') {firstEntered();await firstBlocked}
    const item=queue.find(value=>value.id===request.itemId)
    if(!item) throw Object.assign(new Error('fixture race'),{code:'session/queue-item-not-found'})
    item.content=request.action.content
  }
  const adapter=new RealDshAdapter({ctx,config:{pendingQueueEnabled:true}})
  const first=adapter.updatePendingQueue('s1',{item_id:'q',action:'EDIT',text:'first'})
  await started
  await adapter.updatePendingQueue('s1',{item_id:'q',action:'EDIT',text:'second'})
  releaseFirst();await first
  assert.equal((await adapter.readPendingQueue('s1')).items[0].blocks[0].text.text,'first')
  ctx.sessionController.updateQueue=async()=>{queue=[];throw Object.assign(new Error('fixture consumed'),{code:'session/queue-item-not-found'})}
  await assert.rejects(adapter.updatePendingQueue('s1',{item_id:'q',action:'EDIT',text:'third'}),error=>error instanceof AdapterError && !(error instanceof DefinitiveExecutionError))
  assert.deepEqual((await adapter.readPendingQueue('s1')).items,[])
  await assert.rejects(adapter.updatePendingQueue('s1',{item_id:'q',action:'REMOVE'}),error=>error instanceof DefinitiveExecutionError && error.code==='not_found')
})


test('queue mutation does not infer prechange failure from a nested native code', async () => {
  let queue=[{id:'q',placement:'queued',rpcId:'request',content:[{type:'text',text:'fixture'}]}]
  const ctx=makeCtx({stateFor:()=>({running:true,queue})})
  const adapter=new RealDshAdapter({ctx,config:{pendingQueueEnabled:true}})
  for(const code of ['session/steer-unavailable','session/queue-item-not-found']) {
    queue=[{id:'q',placement:'queued',rpcId:'request',content:[{type:'text',text:'fixture'}]}]
    ctx.sessionController.updateQueue=async()=>{queue=[];throw {isDSHRemoteError:true,code,details:{itemId:'q'}}}
    await assert.rejects(adapter.updatePendingQueue('s1',{item_id:'q',action:'PROMOTE'}),error=>error instanceof AdapterError && !(error instanceof DefinitiveExecutionError))
    assert.equal(queue.length,0)
  }
})

test('a start-if-idle send expiring during session lookup does not invoke native prompt', async () => {
  let now = 1000
  const ctx = makeCtx()
  const adapter = new RealDshAdapter({ctx,now:() => now})
  adapter.getSession = async () => { now = 2100; return {running:false} }
  await assert.rejects(adapter.execute('s1', {send_message:{text:'synthetic',delivery_mode:'START_IF_IDLE'}}, {expiresAt:2000}), error => error instanceof DefinitiveExecutionError && error.code === 'expired')
  assert.equal(ctx.calls.prompt.length, 0)
})

test('a pending approval expiring during native state lookup remains unsettled', async () => {
  let now = 1000
  const session = makeLiveSession()
  const ctx = makeCtx({live:new Map([['s1',session]]),stateFor:() => {now=2100;return {active:true,running:true}}})
  const adapter = new RealDshAdapter({ctx,config:{approvalTtlMs:60000},now:() => now})
  await adapter.open()
  let approval
  adapter.subscribeApprovals(event => {if(event.type === 'requested')approval=event.approval})
  const outcome = ctx.emit('approval/request',{agent:{session},toolName:'bash',arguments:{command:'npm test'}},async () => 'unavailable')[0]
  const command = {resolve_approval:{approval_id:approval.approvalId,decision:'APPROVAL_DECISION_APPROVE',expected_digest:approval.digest,expected_version:approval.version}}
  await assert.rejects(adapter.execute('s1',command,{expiresAt:2000}), error => error instanceof DefinitiveExecutionError && error.code === 'expired')
  assert.equal((await adapter.getApproval(approval.approvalId)).state,'pending')
  await adapter.resolveApproval(approval.approvalId,'reject',approval.digest,approval.version,{expiresAt:5000})
  assert.equal(await outcome,'rejected')
  await adapter.close()
})

test('expired create, fork, model and interrupt commands do not invoke native entry points', async () => {
  const ctx = makeCtx()
  let created=0,forked=0,selected=0
  ctx.sessionController.create=async () => {created++;return {sessionId:'child'}}
  ctx.sessionController.fork=async () => {forked++;return {sessionId:'child'}}
  ctx.sessionController.selectModel=async () => {selected++;return {selected:{provider:'p',model:'m'}}}
  const adapter = new RealDshAdapter({ctx,now:() => 2100})
  await assert.rejects(adapter.executeHost({create_session:{}},{expiresAt:2000}), error => error instanceof DefinitiveExecutionError && error.code === 'expired')
  await assert.rejects(adapter.execute('s1',{fork_session:{}},{expiresAt:2000}), error => error instanceof DefinitiveExecutionError && error.code === 'expired')
  await assert.rejects(adapter.execute('s1',{select_session_model:{provider:'p',model:'m'}},{expiresAt:2000}), error => error instanceof DefinitiveExecutionError && error.code === 'expired')
  await assert.rejects(adapter.execute('s1',{interrupt:{}},{expiresAt:2000}), error => error instanceof DefinitiveExecutionError && error.code === 'expired')
  assert.deepEqual([created,forked,selected,ctx.calls.cancel.length],[0,0,0,0])
})

test('channel lifecycle prevents queue mutation after native-state precheck', async () => {
  let release,entered,updates=0
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  const ctx=makeCtx({stateFor:async()=>{entered();await barrier;return {running:true,queue:[{id:'q',placement:'queued',rpcId:'r',content:[{type:'text',text:'fixture'}]}]}}})
  ctx.sessionController.updateQueue=async()=>{updates++}
  const adapter=new RealDshAdapter({ctx,config:{pendingQueueEnabled:true}})
  const abort=new AbortController()
  const pending=adapter.execute('s1',{update_pending_queue:{item_id:'q',action:'REMOVE'}},{signal:abort.signal})
  await started
  abort.abort('unavailable')
  release()
  await assert.rejects(pending,error=>error instanceof DefinitiveExecutionError && error.code==='unavailable')
  assert.equal(updates,0)
})

test('channel lifecycle prevents start-if-idle delivery after session precheck', async () => {
  let release,entered,sends=0
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  const ctx=makeCtx()
  const adapter=new RealDshAdapter({ctx})
  adapter.getSession=async()=>{entered();await barrier;return {running:false}}
  adapter.sendUserMessage=async()=>{sends++;return {}}
  const abort=new AbortController()
  const pending=adapter.execute('s1',{send_message:{text:'fixture',delivery_mode:'START_IF_IDLE'}},{signal:abort.signal})
  await started
  abort.abort('unavailable')
  release()
  await assert.rejects(pending,error=>error instanceof DefinitiveExecutionError && error.code==='unavailable')
  assert.equal(sends,0)
})

test('channel lifecycle prevents approval settlement after native state precheck', async () => {
  let release,entered
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  const session=makeLiveSession()
  const ctx=makeCtx({live:new Map([['s1',session]]),stateFor:async()=>{entered();await barrier;return {active:true,running:true}}})
  const adapter=new RealDshAdapter({ctx,config:{approvalTtlMs:60_000}})
  await adapter.open()
  const requests=[]
  adapter.subscribeApprovals(event=>{if(event.type==='requested')requests.push(event.approval)})
  const outcome=ctx.emit('approval/request',{agent:{session},toolName:'bash',arguments:{command:'npm test'}},async()=> 'unavailable')[0]
  const approval=requests[0]
  const abort=new AbortController()
  const pending=adapter.resolveApproval(approval.approvalId,'approve',approval.digest,approval.version,{signal:abort.signal})
  await started
  abort.abort('unavailable')
  release()
  await assert.rejects(pending,error=>error instanceof DefinitiveExecutionError && error.code==='unavailable')
  assert.equal((await adapter.getApproval(approval.approvalId)).state,'pending')
  await adapter.close()
  assert.equal(await outcome,'unavailable')
})

test('channel lifecycle suppresses a queue observation completed after cancellation', async () => {
  let release,entered
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  const ctx=makeCtx({stateFor:async()=>{entered();await barrier;return {running:true,queue:[{id:'q',placement:'queued',rpcId:'r',content:[{type:'text',text:'synthetic-private'}]}]}}})
  const adapter=new RealDshAdapter({ctx,config:{pendingQueueEnabled:true}})
  const abort=new AbortController()
  const pending=adapter.readPendingQueue('s1',{signal:abort.signal})
  await started
  abort.abort('unavailable')
  release()
  await assert.rejects(pending,error=>error instanceof AdapterError && error.code==='unavailable')
})

test('channel lifecycle stops cold history pagination after cancellation', async () => {
  let entered,release,pages=0,summaries=0
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  const ctx=makeCtx()
  ctx.sessionController.events=async()=>{pages++;entered();await barrier;return {records:[{event:{type:'assistant/message',seq:1,time:1,data:{text:'fixture'}}}],hasMore:true}}
  ctx.sessionController.list=async()=>{summaries++;return {items:[]}}
  const adapter=new RealDshAdapter({ctx})
  const abort=new AbortController()
  const pending=adapter.readHistory('s1',{snapshot:true},{signal:abort.signal})
  await started
  abort.abort('unavailable')
  release()
  await assert.rejects(pending,error=>error instanceof AdapterError && error.code==='unavailable')
  assert.equal(pages,1)
  assert.equal(summaries,0)
})

test('channel lifecycle stops watch state before the next native read', async () => {
  const adapter=new RealDshAdapter({ctx:makeCtx()})
  let entered,release,heads=0,approvals=0
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  adapter.listSessions=async()=>{entered();await barrier;return [{sessionId:'s1',running:true}]}
  adapter.nativeHead=async()=>{heads++;return 1}
  adapter.listApprovals=async()=>{approvals++;return []}
  const abort=new AbortController()
  const pending=adapter.readState('s1',{signal:abort.signal})
  await started
  abort.abort('unavailable')
  release()
  await assert.rejects(pending,error=>error.code==='unavailable')
  assert.equal(heads,0)
  assert.equal(approvals,0)
})

test('production capabilities are advertised from native methods without test switches', async () => {
  const ctx = makeCtx()
  Object.assign(ctx.sessionController, {
    updateQueue: async () => {},
    modelCatalog: async () => ({ default: null, groups: [], failures: [] }),
    projections: async () => ({ kind: 'sequenced', values: {} }),
    selectModel: async ({ provider, model }) => ({ selected: { provider, model } }),
    wait: async () => ({ reason: 'idle', state: { active: false, running: false, queue: null, pendingInput: [] } }),
    create: async () => ({ sessionId: 'created-1' }),
    fork: async () => ({ sessionId: 'child-1' }),
  })
  ctx.agents = { get: () => undefined, roots: () => [] }
  ctx.userQuestions = { answer: async () => true }
  const adapter = new RealDshAdapter({ ctx })
  await adapter.open()
  try {
    const capabilities = adapter.describe().capabilities
    for (const capability of [
      'sessions', 'events', 'messages', 'approvals', 'interrupt', 'sync', 'snapshot',
      'pending-queue-read', 'pending-queue-edit', 'pending-queue-remove', 'pending-queue-steer',
      'model-catalog', 'model-read', 'model-select', 'wait',
      'questions-read', 'questions-answer-open', 'questions-answer-continued', 'session-fork',
    ]) assert.ok(capabilities.includes(capability), capability)
    assert.deepEqual(await adapter.describeHostCapabilities(), {
      model_catalog: true, model_read: true, session_create: true, session_fork: true,
    })
    assert.deepEqual(adapter.supportedDeliveryModes(), ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER'])
  } finally { await adapter.close() }
})

test('production capabilities stay closed when the native method or service is absent', async () => {
  const ctx = { sessionController: {}, sessions: {} }
  const adapter = new RealDshAdapter({ ctx })
  assert.deepEqual(adapter.describe().capabilities, [])
  assert.deepEqual(await adapter.describeHostCapabilities(), {
    model_catalog: false, model_read: false, session_create: false, session_fork: false,
  })
  assert.deepEqual(adapter.supportedDeliveryModes(), ['START_IF_IDLE'])
})

test('pending queue and open question broker can be opted out without closing delivery', async () => {
  const ctx = makeCtx()
  ctx.sessionController.updateQueue = async () => {}
  ctx.sessionController.projections = async () => ({ kind: 'sequenced', values: {} })
  ctx.agents = { get: () => undefined, roots: () => [] }
  ctx.userQuestions = { answer: async () => true }
  const adapter = new RealDshAdapter({ ctx, config: { pendingQueueEnabled: false, enableOpenQuestionBroker: false } })
  await adapter.open()
  try {
    const capabilities = adapter.describe().capabilities
    for (const capability of ['pending-queue-read', 'pending-queue-edit', 'pending-queue-remove', 'pending-queue-steer', 'questions-answer-open']) {
      assert.equal(capabilities.includes(capability), false, capability)
    }
    assert.deepEqual(adapter.supportedDeliveryModes(), ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER'])
    await assert.rejects(adapter.readPendingQueue('s1'), error => error.code === 'unsupported_capability')
  } finally { await adapter.close() }
})

test('queue action availability re-reads once when the session becomes running', async () => {
  let reads = 0, updates = 0
  const item = { id: 'q', placement: 'queued', rpcId: 'r', content: [{ type: 'text', text: 'fixture' }] }
  const ctx = makeCtx({})
  ctx.sessionController.state = async () => {
    reads++
    // The first observation happens while idle (PROMOTE is not offered); the
    // session starts running before the native update.
    return { running: reads > 1, queue: [item] }
  }
  ctx.sessionController.updateQueue = async request => { updates++; assert.deepEqual(request.action, { kind: 'steer' }) }
  const adapter = new RealDshAdapter({ ctx, config: { pendingQueueEnabled: true } })
  await adapter.updatePendingQueue('s1', { item_id: 'q', action: 'PROMOTE' })
  assert.equal(reads, 2)
  assert.equal(updates, 1)
})

test('queue action availability stays a definitive conflict when the action never appears', async () => {
  let reads = 0, updates = 0
  const item = { id: 'q', placement: 'queued', rpcId: 'r', content: [{ type: 'text', text: 'fixture' }] }
  const ctx = makeCtx({})
  ctx.sessionController.state = async () => { reads++; return { running: false, queue: [item] } }
  ctx.sessionController.updateQueue = async () => { updates++ }
  const adapter = new RealDshAdapter({ ctx, config: { pendingQueueEnabled: true } })
  await assert.rejects(adapter.updatePendingQueue('s1', { item_id: 'q', action: 'PROMOTE' }),
    error => error.code === 'conflict' && error.name === 'DefinitiveExecutionError')
  assert.equal(reads, 2)
  assert.equal(updates, 0)
})

test('interrupt releases a claimed approval and emits a typed INTERRUPTED attempt end', async () => {
  const live = new Map()
  const session = makeLiveSession({ running: true })
  live.set('s1', session)
  const ctx = makeCtx({ live })
  const adapter = new RealDshAdapter({ ctx, config: { approvalTtlMs: 60_000 } })
  await adapter.open()
  try {
    const seen = []
    adapter.watch('s1', (update) => seen.push(update))
    const outcome = ctx.emit('approval/request', { agent: { session }, toolName: 'bash', reason: 'fixture' }, async () => 'unavailable')[0]
    const [approval] = await adapter.listApprovals('s1')
    assert.ok(approval)
    await adapter.interrupt('s1')
    assert.equal(await outcome, 'cancelled')
    assert.equal((await adapter.getApproval(approval.approvalId)).state, 'unavailable')
    assert.equal((await adapter.listApprovals('s1')).length, 0)
    const wait = await adapter.readWaitState('s1', new AbortController().signal)
    assert.equal(wait.pendingInput.includes('approval'), false)
    const ended = seen.filter((update) => update.live?.ended?.outcome === 'INTERRUPTED')
    assert.equal(ended.length, 1)
    assert.equal(ended[0].live.attempt.live_epoch, adapter.conversation.epoch)
  } finally {
    await adapter.close()
  }
})

test('delegate mode claims no approval and still publishes an interrupt attempt end', async () => {
  const live = new Map()
  const session = makeLiveSession({ running: true })
  live.set('s1', session)
  const ctx = makeCtx({ live })
  const adapter = new RealDshAdapter({ ctx, config: { approvalMode: 'delegate' } })
  await adapter.open()
  try {
    const seen = []
    adapter.watch('s1', (update) => seen.push(update))
    const outcome = ctx.emit('approval/request', { agent: { session }, toolName: 'bash' }, async () => 'allowed-once')[0]
    assert.equal(await outcome, 'allowed-once')
    await adapter.interrupt('s1')
    assert.equal((await adapter.listApprovals('s1')).length, 0)
    assert.equal(seen.filter((update) => update.live?.ended?.outcome === 'INTERRUPTED').length, 1)
  } finally {
    await adapter.close()
  }
})

test('real passive pages pin a cut and the same projection resolves an exact committed reference', async () => {
  const events = [
    { type: 'user/message', seq: 1, time: 1, data: { id: 'op-real', role: 'user', content: [{ type: 'text', text: 'hello' }], source: { kind: 'user', rpcId: 'op-real' } } },
    { type: 'assistant/message', seq: 2, time: 2, data: { turn: 1, step: 1, message: { content: [{ type: 'text', text: 'answer' }], source: { kind: 'model' } } } },
    { type: 'turn/end', seq: 3, time: 3, data: { running: false, reason: { kind: 'completed' } } },
  ]
  const live = makeLiveSession({ sessionId: 's', events })
  const ctx = makeCtx({ live: new Map([['s', live]]) })
  const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1' } })
  await adapter.open()
  try {
    const first = await adapter.readHistory('s', { page: {}, max_messages: 1 })
    assert.equal(first.batch.changes.length, 1)
    assert.equal(first.batch.changes[0].message.role, 'ASSISTANT')
    assert.equal(first.complete, false)
    assert.equal(ctx.calls.events.at(-1).throughSeq, 3)

    const reply = await adapter.readMessageReply('s', 'op-real')
    assert.equal(reply.state, 'MESSAGE_REPLY_COMMITTED')
    const exact = await adapter.readHistory('s', { exact: reply.committed })
    assert.equal(exact.batch.changes[0].message.message_id, reply.committed.message_id)
    assert.equal(exact.batch.changes[0].message.blocks[0].text.text, 'answer')

    // Appends do not enter the pinned traversal but do enter a fresh one.
    live.events.push({ type: 'assistant/message', seq: 4, time: 4, data: { turn: 2, step: 1, message: { content: [{ type: 'text', text: 'later' }], source: { kind: 'model' } } } })
    const fixed = await adapter.readHistory('s', { page: { before_cursor: first.continuation }, cut_cursor: first.cut_cursor })
    assert.equal(fixed.cut_cursor, first.cut_cursor)
    assert.deepEqual(fixed.batch.changes.map(change => change.message.blocks[0].text.text), ['hello'])
    const fresh = await adapter.readHistory('s', { page: {}, max_messages: 1 })
    assert.equal(fresh.batch.changes[0].message.blocks[0].text.text, 'later')
    assert.notEqual(fresh.cut_cursor, first.cut_cursor)
  } finally {
    await adapter.close()
  }
})

test('native send receipt identifies its projected user echo', async () => {
  const session = makeLiveSession({ sessionId: 's1', events: [] })
  const ctx = makeCtx({ live: new Map([['s1', session]]) })
  ctx.sessionController.prompt = async ({requestId, content}) => {
    session.events.push({
      type: 'user/message', seq: 1, time: 1,
      data: { id: requestId, role: 'user', content, source: { kind: 'user', rpcId: requestId } },
    })
    return { accepted: true }
  }
  const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1' } })
  await adapter.open()
  try {
    const receipt = await adapter.execute(
      's1', { send_message: { text: 'Fixture prompt' } }, { operationId: 'fixture-request' },
    )
    const page = await adapter.readHistory('s1', { page: {}, max_messages: 10 })
    const messages = page.batch.changes.map(change => change.message)
    assert.equal(messages.length, 1)
    assert.equal(messages[0].role, 'USER')
    assert.equal(
      receipt.command_accepted.delivery_id, messages[0].delivery_id,
      'The receipt and committed history must identify the same delivery',
    )
  } finally {
    await adapter.close()
  }
})

test('native user history identifies deliveries proven by the legacy message ID', async (t) => {
  for (const shape of ['flat', 'nested']) {
    await t.test(shape, async () => {
      const session = makeLiveSession({ sessionId: 's1', events: [] })
      const ctx = makeCtx({ live: new Map([['s1', session]]) })
      const operationId = 'a'.repeat(64)
      ctx.sessionController.prompt = async ({requestId, content}) => {
        const message = { id: requestId, role: 'user', content, source: { kind: 'user' } }
        session.events.push(
          { type: 'user/message', seq: 1, time: 1, data: shape === 'flat' ? message : { message } },
          {
            type: 'assistant/message', seq: 2, time: 2,
            data: { message: { content: [{ type: 'text', text: 'Fixture reply' }], source: { kind: 'model' } } },
          },
          { type: 'turn/end', seq: 3, time: 3, data: { running: false, reason: { kind: 'completed' } } },
        )
        return { accepted: true }
      }
      const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1' } })
      await adapter.open()
      try {
        const receipt = await adapter.execute('s1', { send_message: { text: 'Fixture prompt' } }, { operationId })
        const reply = await adapter.readMessageReply('s1', receipt.command_accepted.message_id)
        assert.equal(reply.state, 'MESSAGE_REPLY_COMMITTED')
        const page = await adapter.readHistory('s1', { page: {}, max_messages: 10 })
        const echo = page.batch.changes.find(change => change.message.role === 'USER')?.message
        assert.ok(echo)
        assert.equal(echo.delivery_id, receipt.command_accepted.delivery_id)
      } finally {
        await adapter.close()
      }
    })
  }
})
