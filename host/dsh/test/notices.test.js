import test from 'node:test'
import assert from 'node:assert/strict'
import { DshProjection } from '../src/adapter/projection.js'
import { FakeDshAdapter } from '../src/adapter/fake.js'
import { validateSessionUpdate } from 'whalerein-host/contracts/source.js'

const event = (seq, type, data) => ({ sessionId: 's', view: 'v', dshSeq: seq, type, data, createdAt: seq })
const project = (events, session = 's', view = 'v') => {
  const projector = new DshProjection()
  return events.map(item => ({ seq: item.dshSeq, message: projector.project(session, view, event(item.dshSeq, item.type, item.data)) }))
    .filter(entry => entry.message)
}

test('model retry scheduled, started and cancelled states follow the owning step boundary', () => {
  const scheduled = project([
    { dshSeq: 1, type: 'step/start', data: { turn: 1, step: 1 } },
    { dshSeq: 2, type: 'llm/retry', data: { retryId: 'r1', turn: 1, step: 1, retry: 1, maxRetries: 3, failure: { message: 'raw provider private', code: 'X' } } },
    { dshSeq: 3, type: 'step/end', data: { turn: 1, step: 1 } },
  ])
  assert.equal(scheduled[0].message.blocks[0].notice.category, 'NOTICE_CATEGORY_RETRY')
  assert.equal(scheduled[0].message.blocks[0].notice.state, 'NOTICE_STATE_SCHEDULED')
  assert.match(scheduled[0].message.blocks[0].notice.summary, /retry 1 of 3 scheduled/)
  assert.equal(scheduled[1].message.blocks[0].notice.state, 'NOTICE_STATE_CANCELLED')
  assert.doesNotMatch(JSON.stringify(scheduled), /raw provider private/)

  const started = project([
    { dshSeq: 1, type: 'llm/retry', data: { retryId: 'r2', turn: 1, step: 1, retry: 1, maxRetries: 2 } },
    { dshSeq: 2, type: 'llm/retry-started', data: { retryId: 'r2', turn: 1, step: 1, retry: 1 } },
    { dshSeq: 3, type: 'step/end', data: { turn: 1, step: 1 } },
  ])
  assert.deepEqual(started.map(entry => entry.message.blocks[0].notice.state), ['NOTICE_STATE_SCHEDULED', 'NOTICE_STATE_STARTED'])
})

test('a turn that closes while a retry is still scheduled reports cancellation once', () => {
  const entries = project([
    { dshSeq: 1, type: 'llm/retry', data: { retryId: 'r1', turn: 4, step: 2, retry: 1, maxRetries: 1 } },
    { dshSeq: 2, type: 'turn/end', data: { turn: 4, reason: { kind: 'max-tokens' } } },
    { dshSeq: 3, type: 'turn/end', data: { turn: 4, reason: { kind: 'completed' } } },
  ])
  assert.equal(entries.length, 2)
  assert.deepEqual(entries[1].message.blocks.map(block => [block.notice.category, block.notice.state]), [
    ['NOTICE_CATEGORY_MAX_OUTPUT', 'NOTICE_STATE_REACHED'],
    ['NOTICE_CATEGORY_RETRY', 'NOTICE_STATE_CANCELLED'],
  ])
})

test('compaction start, landed completion with summary and missing-summary stay truthful', () => {
  const landed = project([
    { dshSeq: 1, type: 'compaction/start', data: { compactionId: 'c1', turn: 1 } },
    { dshSeq: 2, type: 'compaction/summary', data: { compactionId: 'c1', summary: [{ type: 'text', text: 'short summary' }] } },
    { dshSeq: 3, type: 'user/message', data: { id: 'u', role: 'user', content: [{ type: 'text', text: 'private compact input' }], source: { kind: 'compact-checkpoint', compactionId: 'c1' } } },
    { dshSeq: 4, type: 'compaction/end', data: { compactionId: 'c1', turn: 1 } },
  ])
  assert.deepEqual(landed.map(entry => entry.message.blocks[0].notice), [
    { category: 'NOTICE_CATEGORY_COMPACTION', state: 'NOTICE_STATE_STARTED', summary: 'Compaction started' },
    { category: 'NOTICE_CATEGORY_COMPACTION', state: 'NOTICE_STATE_COMPLETED', summary: 'short summary' },
  ])
  assert.doesNotMatch(JSON.stringify(landed), /private compact input/)

  const missing = project([
    { dshSeq: 1, type: 'user/message', data: { id: 'u', role: 'user', content: [{ type: 'text', text: 'private compact input' }], source: { kind: 'compact-checkpoint', compactionId: 'c2' } } },
  ])
  assert.deepEqual(missing[0].message.blocks[0].notice, {
    category: 'NOTICE_CATEGORY_COMPACTION', state: 'NOTICE_STATE_COMPLETED',
  })
  assert.doesNotMatch(JSON.stringify(missing), /private compact input/)

  const failed = project([
    { dshSeq: 1, type: 'compaction/start', data: { compactionId: 'c3', turn: 1 } },
    { dshSeq: 2, type: 'compaction/end', data: { compactionId: 'c3', turn: 1, error: 'private failure text' } },
  ])
  assert.equal(failed.at(-1).message.blocks[0].notice.state, 'NOTICE_STATE_FAILED')
  assert.doesNotMatch(JSON.stringify(failed), /private failure text/)
})

test('slash-command notices expose name and state but never raw arguments or outcome text', () => {
  const entries = project([
    { dshSeq: 1, type: 'command/run', data: { commandId: 'cmd1', name: 'compact', args: ' --secret argument', source: { kind: 'user' } } },
    { dshSeq: 2, type: 'command/done', data: { commandId: 'cmd1', kind: 'success', text: 'private outcome text' } },
    { dshSeq: 3, type: 'command/run', data: { commandId: 'cmd2', name: 'help', args: 'other secret', source: { kind: 'user' } } },
    { dshSeq: 4, type: 'command/done', data: { commandId: 'cmd2', kind: 'error', text: 'private error text' } },
  ])
  assert.deepEqual(entries.map(entry => [entry.message.blocks[0].notice.state, entry.message.blocks[0].notice.summary]), [
    ['NOTICE_STATE_STARTED', 'Command compact started'],
    ['NOTICE_STATE_COMPLETED', 'Command compact completed'],
    ['NOTICE_STATE_STARTED', 'Command help started'],
    ['NOTICE_STATE_FAILED', 'Command help failed'],
  ])
  assert.doesNotMatch(JSON.stringify(entries), /secret|private outcome|private error/)
})

test('watch and history project notices at their Source positions through the adapter seam', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's' }] })
  await adapter.open()
  const seen = []
  const watch = adapter.watch('s', update => seen.push(update))
  adapter.emit('s', { type: 'step/start', data: { turn: 1, step: 1 } })
  adapter.emit('s', { type: 'llm/retry', data: { retryId: 'r', turn: 1, step: 1, retry: 1, maxRetries: 2 } })
  adapter.emit('s', { type: 'step/end', data: { turn: 1, step: 1 } })
  await new Promise(resolve => queueMicrotask(resolve))
  watch.close()
  const live = seen.flatMap(update => update.history?.changes ?? []).map(change => change.message.blocks[0].notice)
  assert.deepEqual(live.map(notice => notice.state), ['NOTICE_STATE_SCHEDULED', 'NOTICE_STATE_CANCELLED'])
  const page = await adapter.readHistory('s', { snapshot: true })
  assert.doesNotThrow(() => validateSessionUpdate({ session: page.session, history: page.batch }))
  assert.deepEqual(page.batch.changes.map(change => change.message.blocks[0].notice.state), ['NOTICE_STATE_SCHEDULED', 'NOTICE_STATE_CANCELLED'])
  await adapter.close()
})
