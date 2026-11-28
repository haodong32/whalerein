import { MemoryHostCredentials } from 'whalerein-host/security/credentials.js'
import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { FakeDshAdapter } from '../src/adapter/fake.js'
import { AdapterError } from 'whalerein-host/errors.js'
import { createAdapter, hasDshServices } from '../src/adapter/index.js'
import { approvalDigest } from 'whalerein-host/security/digest.js'

test('fake sessions carry the archived projection through list and single reads', async () => {
  const adapter = new FakeDshAdapter({ now: () => 1000 })
  await adapter.open()
  adapter.createSession({ sessionId: 'archived', archived: true })
  adapter.createSession({ sessionId: 'live' })
  const sessions = await adapter.listSessions()
  assert.deepEqual(sessions.map((session) => [session.sessionId, session.archived]), [['archived', true], ['live', false]])
  assert.equal((await adapter.getSession('archived')).archived, true)
  assert.equal((await adapter.getSession('live')).archived, false)
})

test('when FakeDSH is queried or emits committed events, the adapter returns sessions and snapshots', async () => {
  const adapter = new FakeDshAdapter({ now: () => 1000 })
  await adapter.open()
  adapter.createSession({ sessionId: 's1', title: 'One', cwd: '/tmp', running: false })
  assert.throws(() => adapter.createSession({ sessionId: 's1' }), (error) => error.code === 'conflict')
  const sessions = await adapter.listSessions()
  assert.equal(sessions.length, 1)
  assert.equal(sessions[0].sessionId, 's1')
  assert.equal(sessions[0].title, 'One')
  assert.equal(sessions[0].running, false)

  const seen = []
  const off = adapter.subscribeEvents((event) => seen.push(event))
  adapter.output('s1', 'hello')
  adapter.setRunning('s1', true)
  const event = adapter.emit('s1', { type: 'tool/result', kind: 'output', data: { ok: true } })
  assert.equal(event.seq, 2)
  off()
  adapter.output('s1', 'not delivered')
  assert.equal(seen.length, 2)
  assert.equal(seen[0].sessionId, 's1')
  assert.equal(seen[0].dshSeq, 1)
  assert.equal(seen[1].kind, 'output')

  const snapshot = await adapter.getSessionSnapshot('s1')
  assert.equal(snapshot.complete, true)
  assert.equal(snapshot.source, 'dsh')
  assert.equal(snapshot.events.length, 3)
  assert.equal(snapshot.summary.running, true)
  await assert.rejects(adapter.getSessionSnapshot('missing'), (error) => error.code === 'not_found')
  adapter.close()
})

test('when FakeDSH accepts a message, it returns a nonempty receipt and records one user message followed by the configured assistant reply and turn end', async () => {
  const adapter = new FakeDshAdapter({ now: () => 1000, sessions: [{ sessionId: 's1', running: false }], behavior: { emitOutputOnMessage: true } })
  await adapter.open()
  const message = await adapter.sendUserMessage('s1', 'hello')
  assert.equal(typeof message.messageId, 'string')
  const events = (await adapter.getSessionSnapshot('s1')).events
  assert.deepEqual(events.map((event) => event.type), ['user/message', 'assistant/message', 'turn/end'])
  assert.ok(message.messageId.length > 0)
  assert.equal(events[0].data.content[0].text, 'hello')
  assert.equal(events[1].data.message.content[0].text, 'hello')
  assert.equal(events[0].data.source.kind, 'user')
  await assert.rejects(adapter.sendUserMessage('s1', '  '), (error) => error.code === 'invalid_argument')
  await assert.rejects(adapter.sendUserMessage('missing', 'x'), (error) => error.code === 'not_found')

  assert.equal((await adapter.getSessionSnapshot('s1')).events.length, 3)
  await adapter.close()
})

test('when a running FakeDSH session is interrupted, it records one interrupt and becomes idle', async () => {
  const adapter = new FakeDshAdapter({ now: () => 1000, sessions: [{ sessionId: 's1', running: true }] })
  await adapter.open()
  await adapter.interrupt('s1')
  const interrupted = (await adapter.getSessionSnapshot('s1')).events.at(-1)
  assert.equal(interrupted.type, 'session/interrupted')
  assert.equal((await adapter.getSessionSnapshot('s1')).summary.running, false)
  assert.equal((await adapter.getSessionSnapshot('s1')).events.length, 1)
  await adapter.close()
})

test('when a fake approval expires or is resolved, expiry and one-shot consumption are enforced atomically', async () => {
  let now = 1000
  const adapter = new FakeDshAdapter({ now: () => now, sessions: [{ sessionId: 's1', running: true }] })
  await adapter.open()
  const requested = []
  adapter.subscribeApprovals((event) => requested.push(event))
  const approval = adapter.requestApproval({
    sessionId: 's1', toolName: 'bash', callId: 'call-1', reason: 'because', ttlMs: 100,
    arguments: { command: 'npm test' },
  })
  assert.equal(requested[0].type, 'requested')
  assert.deepEqual([...approval.digest], [...approvalDigest(approval)])
  assert.equal((await adapter.getApproval(approval.approvalId)).state, 'pending')
  assert.equal(await adapter.getApproval('missing'), null)

  const resolved = await adapter.resolveApproval(approval.approvalId, 'approve', approval.digest, approval.version)
  assert.equal(resolved.state, 'approved')
  assert.equal(requested[1].type, 'resolved')
  await assert.rejects(adapter.resolveApproval(approval.approvalId, 'approve', approval.digest, approval.version), (error) => error.code === 'approval_already_resolved')

  const second = adapter.requestApproval({ sessionId: 's1', ttlMs: 10 })
  now += 20
  const expired = await adapter.getApproval(second.approvalId)
  assert.equal(expired.state, 'expired')
  // TTL expiry releases the turn: the session returns to idle and stays usable.
  assert.equal((await adapter.getSessionSnapshot('s1')).summary.running, false)
  assert.equal((await adapter.getSessionSnapshot('s1')).events.at(-1).type, 'turn/end')
  adapter.setRunning('s1', true)
  const next = adapter.requestApproval({ sessionId: 's1', ttlMs: 100 })
  assert.ok(next.version > second.version)
  assert.equal((await adapter.resolveApproval(next.approvalId, 'reject', next.digest, next.version)).state, 'rejected')
  await assert.rejects(adapter.resolveApproval(second.approvalId, 'approve', second.digest, second.version), (error) => error.code === 'approval_expired')
  assert.throws(() => adapter.requestApproval({ sessionId: 'missing' }), (error) => error.code === 'not_found')
})

test('when approval digest/version is wrong, session stopped, or resolution repeated, FakeDSH rejects it', async () => {
  const adapter = new FakeDshAdapter({ now: () => 1000, sessions: [{ sessionId: 's1', running: true }] })
  await adapter.open()
  const approval = adapter.requestApproval({ sessionId: 's1' })
  await assert.rejects(adapter.resolveApproval(approval.approvalId, 'approve', new Uint8Array(32), approval.version), (error) => error.code === 'approval_version_mismatch')
  await assert.rejects(adapter.resolveApproval(approval.approvalId, 'approve', approval.digest, approval.version + 1), (error) => error.code === 'approval_version_mismatch')
  await assert.rejects(adapter.resolveApproval(approval.approvalId, 'maybe', approval.digest, approval.version), (error) => error.code === 'invalid_argument')
  adapter.setRunning('s1', false)
  await assert.rejects(adapter.resolveApproval(approval.approvalId, 'approve', approval.digest, approval.version), (error) => error.code === 'approval_unavailable')
  adapter.setRunning('s1', true)
  await adapter.simulateResolution(approval.approvalId, 'reject')
  assert.equal((await adapter.getApproval(approval.approvalId)).state, 'rejected')
  const local = adapter.requestApproval({ sessionId: 's1', arguments: { command: 'npm test' } })
  await Promise.all([
    adapter.resolveApproval(local.approvalId, 'approve', local.digest, local.version).then(() => 'won').catch((error) => error.code),
    adapter.resolveApproval(local.approvalId, 'reject', local.digest, local.version).then(() => 'won').catch((error) => error.code),
  ]).then((results) => {
    assert.equal(results.filter((result) => result === 'won').length, 1)
    assert.equal(results.filter((result) => result === 'approval_already_resolved').length, 1)
  })
})

test('remote Allow is accepted for incomplete and unavailable reviews without leaking review facts', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's1', running: true }] })
  await adapter.open()
  // toolName alone establishes the action class but no call facts arrive, so
  // the review is incomplete. This is the current real-DSH shape.
  const incomplete = adapter.requestApproval({ sessionId: 's1', toolName: 'write', reason: 'no argument map' })
  assert.equal(incomplete.reviewAvailability, 'incomplete')
  const approved = await adapter.resolveApproval(
    incomplete.approvalId, 'approve', incomplete.digest, incomplete.version,
  )
  assert.deepEqual(approved, { state: 'approved', decision: 'approve' })
  // The decision result carries the outcome only; projected review facts stay
  // on the approval surface instead of entering the resolution reply.
  assert.deepEqual(Object.keys(approved), ['state', 'decision'])

  const unavailable = adapter.requestApproval({ sessionId: 's1', toolName: 'acme_unknown_tool' })
  assert.equal(unavailable.reviewAvailability, 'unavailable')
  assert.equal(
    (await adapter.resolveApproval(unavailable.approvalId, 'approve', unavailable.digest, unavailable.version)).state,
    'approved',
  )

  // Reject stays available and settles the same incomplete review shape.
  const rejectable = adapter.requestApproval({ sessionId: 's1', toolName: 'write' })
  assert.equal(rejectable.reviewAvailability, 'incomplete')
  assert.equal(
    (await adapter.resolveApproval(rejectable.approvalId, 'reject', rejectable.digest, rejectable.version)).state,
    'rejected',
  )
  await adapter.close()
})

test('a complete review approves once, and the availability flag stays digest-bound', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's1', running: true }] })
  await adapter.open()
  const complete = adapter.requestApproval({
    sessionId: 's1',
    toolName: 'write',
    arguments: { path: '/repo/a.txt', content: 'one\ntwo' },
  })
  assert.equal(complete.reviewAvailability, 'complete')
  assert.equal((await adapter.resolveApproval(complete.approvalId, 'approve', complete.digest, complete.version)).state, 'approved')
  await assert.rejects(
    adapter.resolveApproval(complete.approvalId, 'approve', complete.digest, complete.version),
    (error) => error.code === 'approval_already_resolved',
  )

  // The review is advisory now, but the v2 digest still binds every projected
  // fact, including the claimed availability.
  const inconsistent = adapter.requestApproval({
    sessionId: 's1',
    toolName: 'write',
    reviewAction: 'Write file',
    reviewTarget: '',
    reviewImpact: 'Whole file',
    reviewAvailability: 'complete',
  })
  assert.equal(inconsistent.reviewAvailability, 'complete')
  const changedAvailability = approvalDigest({ ...inconsistent, reviewAvailability: 'incomplete' })
  await assert.rejects(
    adapter.resolveApproval(inconsistent.approvalId, 'approve', changedAvailability, inconsistent.version),
    (error) => error.code === 'approval_version_mismatch',
  )
  assert.equal((await adapter.getApproval(inconsistent.approvalId)).state, 'pending')
  assert.equal(
    (await adapter.resolveApproval(inconsistent.approvalId, 'approve', inconsistent.digest, inconsistent.version)).state,
    'approved',
  )
  await adapter.close()
})

test('wrong digest and version still bind remote Allow', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's1', running: true }] })
  await adapter.open()
  const incomplete = adapter.requestApproval({ sessionId: 's1', toolName: 'write' })
  assert.equal(incomplete.reviewAvailability, 'incomplete')
  await assert.rejects(
    adapter.resolveApproval(incomplete.approvalId, 'approve', new Uint8Array(32), incomplete.version),
    (error) => error.code === 'approval_version_mismatch',
  )
  await assert.rejects(
    adapter.resolveApproval(incomplete.approvalId, 'approve', incomplete.digest, incomplete.version + 1),
    (error) => error.code === 'approval_version_mismatch',
  )
  assert.equal((await adapter.getApproval(incomplete.approvalId)).state, 'pending')
  // The same incomplete review resolves once its exact guard is supplied.
  assert.equal(
    (await adapter.resolveApproval(incomplete.approvalId, 'approve', incomplete.digest, incomplete.version)).state,
    'approved',
  )
  await adapter.close()
})

test('when crash, failure, or hang injection is configured, FakeDSH simulates that outcome', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's1', running: true }] })
  await adapter.open()
  adapter.failNext('sendUserMessage', 'unavailable')
  await assert.rejects(adapter.sendUserMessage('s1', 'x'), (error) => error.code === 'unavailable')
  adapter.failNext('listSessions')
  await assert.rejects(adapter.listSessions(), (error) => error.code === 'unavailable')
  adapter.hangNext('sendUserMessage')
  const hanging = adapter.sendUserMessage('s1', 'x')
  assert.equal(hanging instanceof Promise, true)
  adapter.crash()
  assert.equal(adapter.describe().available, false)
  await assert.rejects(adapter.listSessions(), (error) => error.code === 'unavailable')
  assert.throws(() => adapter.emit('s1', {}), (error) => error.code === 'unavailable')
  adapter.recover()
  assert.equal(adapter.describe().available, true)
  assert.equal(await adapter.getApproval('missing'), null)
})

test('when fake credentials are stored, they stay out of SQLite and optionally persist in a temporary file', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-fake-cred-'))
  try {
    const file = join(dir, 'credentials.json')
    const adapter = new MemoryHostCredentials({ credentialFile: file })
    await adapter.setCredential('key', 'super-secret')
    assert.equal(await adapter.getCredential('key'), 'super-secret')
    assert.equal(await adapter.getCredential('missing'), null)
    assert.equal(JSON.parse(readFileSync(file, 'utf8')).key, 'super-secret')
    const reopened = new MemoryHostCredentials({ credentialFile: file })
    assert.equal(await reopened.getCredential('key'), 'super-secret')
    const missing = new MemoryHostCredentials({ credentialFile: join(dir, 'missing', 'credentials.json') })
    assert.equal(await missing.getCredential('key'), null)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('when adapter mode is auto, native session services select real DSH and absent services select FakeDSH; explicit overrides win', async () => {
  const fake = createAdapter({ mode: 'fake' })
  assert.equal(fake.describe().mode, 'fake')
  const auto = createAdapter({ mode: 'auto' })
  assert.equal(auto.describe().mode, 'fake')
  const injected = new FakeDshAdapter()
  assert.equal(createAdapter({ adapter: injected }), injected)
  assert.equal(hasDshServices(null), false)
  assert.equal(hasDshServices({}), false)
  assert.equal(hasDshServices({ sessions: {} }), true)
  const real = createAdapter({ mode: 'real', ctx: { sessions: { list: () => [] } }, config: {} })
  assert.equal(real.describe().mode, 'real')
  for (const ctx of [{ sessions: {} }, { sessionController: {} }]) {
    assert.equal(createAdapter({ mode: 'auto', ctx, config: {} }).describe().mode, 'real')
    assert.equal(createAdapter({ mode: 'fake', ctx }).describe().mode, 'fake')
  }
  for (const ctx of [null, {}, { sessions: null, sessionController: null }]) {
    assert.equal(createAdapter({ mode: 'auto', ctx }).describe().mode, 'fake')
  }
})

test('pending queue fixture preserves source-owned mutation semantics', async () => {
  const adapter = new FakeDshAdapter({behavior:{pendingQueueEnabled:true},sessions:[{sessionId:'s1',running:true,queue:[{id:'q1',text:'fixture',attachments:[{}]},{id:'q2',text:'second'}]}]})
  const initial = await adapter.readPendingQueue('s1')
  assert.equal(initial.items.length,2)
  await adapter.execute('s1',{update_pending_queue:{session_id:'s1',item_id:'q1',action:'EDIT',text:'replacement'}})
  assert.equal((await adapter.readPendingQueue('s1')).items[0].blocks[0].text.text,'replacement')
  await adapter.execute('s1',{update_pending_queue:{session_id:'s1',item_id:'q1',action:'PROMOTE'}})
  assert.equal((await adapter.readPendingQueue('s1')).items[0].placement,'STEERING')
  await adapter.execute('s1',{update_pending_queue:{session_id:'s1',item_id:'q2',action:'REMOVE'}})
  assert.equal((await adapter.readPendingQueue('s1')).items.length,1)
  await assert.rejects(adapter.execute('s1',{update_pending_queue:{session_id:'s1',item_id:'q2',action:'REMOVE'}}),error=>error.code==='not_found')
})


test('fork copies the latest completed prefix without running the child', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{sessionId:'source',cwd:'/synthetic',running:true}] })
  await assert.rejects(adapter.forkSession('source'), error => error.code === 'conflict')
  adapter.emit('source', {type:'user/message',data:{text:'synthetic'}})
  adapter.emit('source', {type:'turn/end'})
  adapter.emit('source', {type:'system/message',data:{text:'synthetic'}})
  adapter.emit('source', {type:'turn/start'})
  adapter.emit('source', {type:'user/message',data:{text:'queued'}})
  const receipt = (await adapter.forkSession('source')).lifecycle_receipt
  const child = await adapter.getSessionSnapshot(receipt.session_id)
  assert.equal(receipt.parent_session_id, 'source')
  assert.notEqual(receipt.session_id, 'source')
  assert.equal(child.summary.cwd, '/synthetic')
  assert.equal(child.summary.running, false)
  assert.deepEqual(child.events.map(event => event.type), ['user/message','turn/end','system/message'])
  assert.equal((await adapter.getSessionSnapshot('source')).events.length, 5)
})

test('fork excludes native inbox splice and subsequent queued work', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{sessionId:'source'}] })
  adapter.emit('source', {type:'turn/end'})
  adapter.emit('source', {type:'agent/inbox/spliced'})
  adapter.emit('source', {type:'system/message',data:{text:'synthetic'}})
  const child = (await adapter.forkSession('source')).lifecycle_receipt.session_id
  assert.deepEqual((await adapter.getSessionSnapshot(child)).events.map(event => event.type), ['turn/end'])
})

test('fake interrupt ends a running attempt once, releases approvals, and stays silent when idle', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's1', running: true }] })
  await adapter.open()
  try {
    const updates = []
    const events = []
    adapter.watch('s1', (update) => updates.push(update))
    adapter.subscribeEvents((event) => events.push(event))
    const approval = adapter.requestApproval({ sessionId: 's1', reason: 'fixture' })
    await adapter.interrupt('s1')
    assert.equal((await adapter.getSession('s1')).running, false)
    assert.equal((await adapter.getApproval(approval.approvalId)).state, 'unavailable')
    assert.deepEqual(await adapter.listApprovals('s1'), [])
    assert.equal(updates.filter((update) => update.live?.ended?.outcome === 'INTERRUPTED').length, 1)
    assert.equal(events.filter((event) => event.type === 'session/interrupted').length, 1)
    const count = updates.length
    await adapter.interrupt('s1')
    assert.equal(updates.length, count)
    assert.equal(events.filter((event) => event.type === 'session/interrupted').length, 1)
    assert.equal((await adapter.getApproval(approval.approvalId)).state, 'unavailable')
  } finally {
    await adapter.close()
  }
})
