import test from 'node:test'
import assert from 'node:assert/strict'
import { assistantReplyData, assistantStreamEvent } from '../src/adapter/assistant-stream.js'

test('when assistant frame metadata is malformed, normalization rejects it without exposing input', () => {
  const agent = { session: { id: 's' } }
  const base = { attemptId: 'a', revision: 1 }
  for (const frame of [null, {}, { ...base, type: 'unknown' },
    { ...base, type: 'start' }, { ...base, type: 'chunk', index: -1 },
    { ...base, revision: -1 }, { ...base, attemptId: 'x'.repeat(513) }]) {
    assert.equal(assistantStreamEvent({ agent, frame }), null)
  }
  assert.equal(assistantStreamEvent(), null)
  const end = assistantStreamEvent({ agent, frame: { ...base, type: 'end' } }, () => 10)
  assert.equal(end.data.outcome, 'abandoned')
  assert.equal(end.createdAt, 10)
})


test('when a durable assistant reply has native coordinates, only the adapter constructs its correlation identity', () => {
  const data = { turn: 2, step: 3 }
  assert.equal(assistantReplyData('assistant/message', data).reply_id, '2:3')
  assert.equal(assistantReplyData('other', data), data)
  assert.equal(assistantReplyData('assistant/message', {} ).reply_id, undefined)
  assert.equal(assistantReplyData('assistant/message', null), null)
})
