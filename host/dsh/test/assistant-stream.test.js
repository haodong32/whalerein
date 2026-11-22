import test from 'node:test'
import assert from 'node:assert/strict'
import { assistantReplyData, assistantStreamEvent } from '../src/adapter/assistant-stream.js'
import { DshConversation } from '../src/adapter/conversation.js'

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

test('the DSH adapter splits an oversized native delta before sequence allocation and preserves UTF-8 text', () => {
  const conversation = new DshConversation({})
  conversation.setPreviewBytes(5)
  const notification = (text) => ({
    sessionId: 's', ephemeral: true, type: 'assistant/preview',
    data: {
      stream_id: 'a', phase: 'chunk', revision: 1,
      block_id: 'text-0', block_kind: 'BLOCK_KIND_TEXT', text,
    },
    dshSeq: null,
  })
  const updates = conversation.notification(notification('éééé'))
  assert.ok(Array.isArray(updates))
  assert.ok(updates.length > 1, 'one large native delta becomes several complete previews')
  assert.equal(updates.map((update) => update.live.delta.text).join(''), 'éééé')
  assert.deepEqual(updates.map((update) => [update.live.first_seq, update.live.last_seq]),
    [[1, 1], [2, 2]])
  for (const update of updates) {
    assert.equal(update.live.delta.kind, 'BLOCK_KIND_TEXT')
    assert.equal(update.live.block_id, 'text-0')
    assert.equal(update.live.attempt.attempt_id, 'a')
    assert.ok(Buffer.byteLength(update.live.delta.text, 'utf8') <= 5)
  }
  // The next native delta keeps the contiguous normalized sequence.
  const next = conversation.notification({ ...notification('x'), data: { ...notification('x').data, revision: 2 } })
  assert.equal(Array.isArray(next), false)
  assert.equal(next.live.first_seq, 3)
})
