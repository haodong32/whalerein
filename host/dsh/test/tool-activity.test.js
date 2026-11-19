import test from 'node:test'
import assert from 'node:assert/strict'
import { DshProjection } from '../src/adapter/projection.js'
import { FakeDshAdapter } from '../src/adapter/fake.js'
import { validateSessionUpdate } from 'whalerein-host/contracts/source.js'

const event = (seq, type, data) => ({ sessionId: 's', view: 'v', dshSeq: seq, type, data, createdAt: seq })
const project = (events) => {
  const projector = new DshProjection()
  return events.map(item => projector.project('s', 'v', event(item.dshSeq, item.type, item.data)))
    .filter(Boolean)
    .flatMap(message => message.blocks.map(block => block.tool_activity))
}

test('declaration, dispatch and result share one session-scoped call identity with evidenced stages', () => {
  const activities = project([
    { dshSeq: 1, type: 'assistant/message', data: { message: { content: [{ type: 'tool-call', id: 'call-1', name: 'bash', arguments: '{"private":true}' }] } } },
    { dshSeq: 2, type: 'tool/call', data: { turn: 1, step: 1, callId: 'call-1', name: 'bash', arguments: '{"private":true}' } },
    { dshSeq: 3, type: 'tool/result', data: { message: { content: [{ type: 'text', text: 'ok' }], source: { kind: 'tool', callId: 'call-1' }, isError: false } } },
  ])
  assert.deepEqual(activities.map(activity => activity.call_id), ['call-1', 'call-1', 'call-1'])
  assert.deepEqual(activities.map(activity => activity.stages), [
    ['TOOL_STAGE_PREPARATION'], ['TOOL_STAGE_EXECUTION'], ['TOOL_STAGE_SETTLEMENT'],
  ])
  assert.deepEqual(activities.map(activity => activity.outcome), [
    'TOOL_OUTCOME_UNSPECIFIED', 'TOOL_OUTCOME_UNSPECIFIED', 'TOOL_OUTCOME_SUCCEEDED',
  ])
  assert.equal(activities.at(-1).result[0].text.text, 'ok')
  assert.doesNotMatch(JSON.stringify(activities), /private/)
})

test('parallel calls settle out of order without crossing identities', () => {
  const activities = project([
    { dshSeq: 1, type: 'tool/call', data: { callId: 'call-a', name: 'read' } },
    { dshSeq: 2, type: 'tool/call', data: { callId: 'call-b', name: 'write' } },
    { dshSeq: 3, type: 'tool/result', data: { message: { content: [{ type: 'text', text: 'b done' }], source: { kind: 'tool', callId: 'call-b' }, isError: false } } },
    { dshSeq: 4, type: 'tool/result', data: { message: { content: [{ type: 'text', text: 'a failed' }], source: { kind: 'tool', callId: 'call-a' }, isError: true }, error: { code: 'E_A' } } },
  ])
  assert.deepEqual(activities.map(activity => [activity.call_id, activity.stages[0], activity.outcome]), [
    ['call-a', 'TOOL_STAGE_EXECUTION', 'TOOL_OUTCOME_UNSPECIFIED'],
    ['call-b', 'TOOL_STAGE_EXECUTION', 'TOOL_OUTCOME_UNSPECIFIED'],
    ['call-b', 'TOOL_STAGE_SETTLEMENT', 'TOOL_OUTCOME_SUCCEEDED'],
    ['call-a', 'TOOL_STAGE_SETTLEMENT', 'TOOL_OUTCOME_FAILED'],
  ])
  assert.equal(activities.at(-1).failure_code, 'E_A')
})

test('a result before its declaration establishes a settled activity and a late dispatch does not regress it', () => {
  const activities = project([
    { dshSeq: 1, type: 'tool/result', data: { message: { content: [{ type: 'text', text: 'done' }], source: { kind: 'tool', callId: 'call-late' }, isError: false } } },
    { dshSeq: 2, type: 'tool/call', data: { callId: 'call-late', name: 'read' } },
    { dshSeq: 3, type: 'tool/result', data: { message: { content: [{ type: 'text', text: 'changed' }], source: { kind: 'tool', callId: 'call-late' }, isError: false } } },
  ])
  assert.deepEqual(activities.map(activity => activity.stages), [
    ['TOOL_STAGE_SETTLEMENT'], ['TOOL_STAGE_EXECUTION'], ['TOOL_STAGE_SETTLEMENT'],
  ])
  // The adapter never rewrites a settled occurrence or invents precedence;
  // Core owns result precedence when it folds these records.
  assert.equal(activities[0].outcome, 'TOOL_OUTCOME_SUCCEEDED')
  assert.equal(activities[1].outcome, 'TOOL_OUTCOME_UNSPECIFIED')
})

test('a duplicate dispatch keeps the same evidenced stage without inventing a second identity', () => {
  const activities = project([
    { dshSeq: 1, type: 'tool/call', data: { callId: 'call-dup', name: 'bash' } },
    { dshSeq: 2, type: 'tool/call', data: { callId: 'call-dup', name: 'bash' } },
  ])
  assert.equal(new Set(activities.map(activity => activity.call_id)).size, 1)
  assert.deepEqual(activities.map(activity => activity.stages), [['TOOL_STAGE_EXECUTION'], ['TOOL_STAGE_EXECUTION']])
})

test('abandoned preparation stays a preparation and never claims execution', () => {
  const activities = project([
    { dshSeq: 1, type: 'assistant/message', data: { turn: 1, step: 1, interrupted: true, message: { content: [{ type: 'tool-call', id: 'call-prep', name: 'bash', arguments: '{}' }] } } },
  ])
  assert.deepEqual(activities.map(activity => activity.stages), [['TOOL_STAGE_PREPARATION']])
  assert.equal(activities[0].outcome, 'TOOL_OUTCOME_UNSPECIFIED')
})

test('result attachments are opaque ordered occurrence references, never bytes or storage payloads', () => {
  const activities = project([
    { dshSeq: 1, type: 'tool/result', data: { message: {
      content: [
        { type: 'text', text: 'before' },
        { type: 'image', attachment: { attachmentId: 'image-hash', mediaType: 'image/png', bytes: 10, name: 'shot.png' } },
        { type: 'file', attachment: { attachmentId: 'file-hash', name: 'report.bin', bytes: 20 } },
        { type: 'text', text: 'after' },
      ],
      source: { kind: 'tool', callId: 'call-file' }, isError: false,
    } } },
  ])
  const parts = activities[0].result
  assert.deepEqual(parts.map(part => Object.keys(part)[0]), ['text', 'attachment', 'attachment', 'text'])
  assert.equal(parts[1].attachment.media_type, 'image/png')
  assert.equal(parts[1].attachment.size, 10)
  assert.equal(parts[1].attachment.available, false)
  assert.ok(parts[1].attachment.attachment_ref)
  assert.equal(parts[2].attachment.name, 'report.bin')
  assert.equal(parts[2].attachment.available, false)
  assert.doesNotMatch(JSON.stringify(activities), /image-hash|file-hash/)
})

test('tool activity occurrences reach live watch and canonical history through the adapter', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's' }] })
  await adapter.open()
  const seen = []
  const watch = adapter.watch('s', update => seen.push(update))
  adapter.emit('s', { type: 'assistant/message', data: { turn: 1, step: 1, message: { content: [{ type: 'tool-call', id: 'call-live', name: 'bash', arguments: '{"private":true}' }], source: { kind: 'model' } } } })
  adapter.emit('s', { type: 'tool/call', data: { turn: 1, step: 1, callId: 'call-live', name: 'bash', arguments: '{"private":true}' } })
  adapter.emit('s', { type: 'tool/result', data: { turn: 1, step: 1, message: { content: [{ type: 'text', text: 'ok' }], source: { kind: 'tool', callId: 'call-live' }, isError: false } } })
  await new Promise(resolve => queueMicrotask(resolve))
  watch.close()
  const live = seen.flatMap(update => update.history?.changes ?? []).flatMap(change => change.message.blocks.map(block => block.tool_activity)).filter(Boolean)
  assert.deepEqual(live.map(activity => activity.call_id), ['call-live', 'call-live', 'call-live'])
  assert.doesNotMatch(JSON.stringify(live), /private/)
  const page = await adapter.readHistory('s', { snapshot: true })
  assert.doesNotThrow(() => validateSessionUpdate({ session: page.session, history: page.batch }))
  const history = page.batch.changes.flatMap(change => change.message.blocks.map(block => block.tool_activity))
  assert.deepEqual(history.map(activity => activity.call_id), ['call-live', 'call-live', 'call-live'])
  assert.deepEqual(history.map(activity => activity.stages[0]), ['TOOL_STAGE_PREPARATION', 'TOOL_STAGE_EXECUTION', 'TOOL_STAGE_SETTLEMENT'])
  await adapter.close()
})
