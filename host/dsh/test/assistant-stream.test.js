import test from 'node:test'
import assert from 'node:assert/strict'
import { assistantReplyData, assistantStreamEvent } from '../src/adapter/assistant-stream.js'

const agent = { session: { id: 's' } }
const base = { attemptId: 'a', revision: 1 }
const chunk = (revision, value, index = 0) => assistantStreamEvent({
  agent, frame: { ...base, revision, type: 'chunk', index, chunk: value },
})

test('when assistant frame metadata is malformed, normalization rejects it without exposing input', () => {
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

test('text and reasoning deltas carry distinct checked block kinds and adapter block identities', () => {
  const text = chunk(2, { type: 'text-delta', index: 2, text: 'answer' })
  assert.equal(text.data.block_id, 'text-2')
  assert.equal(text.data.block_kind, 'BLOCK_KIND_TEXT')
  assert.equal(text.data.text, 'answer')
  const reasoning = chunk(3, { type: 'reasoning-delta', index: 0, text: 'think' })
  assert.equal(reasoning.data.block_id, 'reasoning-0')
  assert.equal(reasoning.data.block_kind, 'BLOCK_KIND_REASONING')
  assert.equal(reasoning.data.text, 'think')
  // Interleaved blocks keep their own identities and do not leak into each other.
  assert.notEqual(text.data.block_id, reasoning.data.block_id)
})

test('block-end is a full replacement for its own reasoning or text block only', () => {
  const text = chunk(2, { type: 'block-end', index: 1, block: { type: 'text', text: 'full answer' } })
  assert.equal(text.data.block_kind, 'BLOCK_KIND_TEXT')
  assert.equal(text.data.block_text, 'full answer')
  const reasoning = chunk(3, { type: 'block-end', index: 0, block: { type: 'reasoning', text: 'full reasoning' } })
  assert.equal(reasoning.data.block_kind, 'BLOCK_KIND_REASONING')
  assert.equal(reasoning.data.block_text, 'full reasoning')
})

test('tool-call, usage and finish chunks are not coerced into visible live text', () => {
  for (const value of [
    { type: 'tool-call-delta', index: 0, id: 'call-1', argumentsDelta: '{"private":true}' },
    { type: 'block-end', index: 0, block: { type: 'tool-call', id: 'call-1', name: 'bash', arguments: '{"private":true}' } },
    { type: 'usage', usage: { inputTokens: 1 } },
    { type: 'finish', reason: { kind: 'stop' } },
    { type: 'block-start', index: 0, blockType: 'text' },
    null,
  ]) {
    assert.equal(chunk(4, value), null)
  }
})

test('when a durable assistant reply has native coordinates, only the adapter constructs its correlation identity', () => {
  const data = { turn: 2, step: 3 }
  assert.equal(assistantReplyData('assistant/message', data).reply_id, '2:3')
  assert.equal(assistantReplyData('other', data), data)
  assert.equal(assistantReplyData('assistant/message', {} ).reply_id, undefined)
  assert.equal(assistantReplyData('assistant/message', null), null)
  assert.equal(assistantReplyData('assistant/message', { turn: 0, step: 0 }).reply_id, undefined)
})
