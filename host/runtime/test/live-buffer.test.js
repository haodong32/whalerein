import test from 'node:test'
import assert from 'node:assert/strict'
import { LiveBuffer, MAX_PREVIEW_BYTES } from '../src/events/live-buffer.js'

const TEXT = 'BLOCK_KIND_TEXT'
const REASONING = 'BLOCK_KIND_REASONING'
const frame = (seq, text = 'x', id = 'a', block = 'b', kind = TEXT) => ({
  session: { session_id: 's' },
  live: {
    attempt: { live_epoch: 'e', attempt_id: id },
    block_id: block,
    first_seq: seq,
    last_seq: seq,
    delta: { kind, text },
  },
})

test('typed live coalescing preserves contiguous intervals and flushes at lifecycle and identity changes', async () => {
  const out = []
  const buffer = new LiveBuffer((update) => out.push(update))
  buffer.push(frame(1))
  buffer.push(frame(2))
  assert.equal(out.length, 0)
  await new Promise((resolve) => setTimeout(resolve, 30))
  assert.equal(out[0].live.last_seq, 2)
  assert.equal(out[0].live.delta.text.length, 2)
  assert.equal(out[0].live.delta.kind, TEXT)
  buffer.push(frame(3))
  buffer.push(frame(4, 'x', 'other'))
  buffer.push(frame(5, 'x', 'other', 'different'))
  buffer.push({ session: { session_id: 's' }, live: { ended: { outcome: 'COMMITTED' } } })
  assert.equal(out.length, 5)
  buffer.flush()
})

test('large or noncontiguous deltas flush without crossing stream positions', () => {
  const out = []
  const buffer = new LiveBuffer((update) => out.push(update))
  buffer.push(frame(1))
  buffer.push(frame(3))
  buffer.push(frame(4, 'x'.repeat(MAX_PREVIEW_BYTES + 1)))
  assert.equal(out.length, 3)
  buffer.flush()
})

test('reasoning and text previews never coalesce across their checked block kind', () => {
  const out = []
  const buffer = new LiveBuffer((update) => out.push(update))
  buffer.push(frame(1, 'think', 'a', 'reasoning', REASONING))
  buffer.push(frame(2, 'answer', 'a', 'reasoning', TEXT))
  assert.equal(out.length, 1, 'a different kind flushes the reasoning preview')
  buffer.push(frame(3, 'more', 'a', 'reasoning', TEXT))
  buffer.flush()
  assert.equal(out.length, 2)
  assert.deepEqual(out.map((update) => update.live.delta.kind), [REASONING, TEXT])
  assert.equal(out[1].live.delta.text, 'answermore')
})

test('a reset or attempt change flushes the pending preview first', () => {
  const out = []
  const buffer = new LiveBuffer((update) => out.push(update))
  buffer.push(frame(1))
  buffer.push({
    session: { session_id: 's' },
    live: {
      attempt: { live_epoch: 'e', attempt_id: 'a' },
      block_id: 'b',
      first_seq: 2,
      last_seq: 2,
      reset: { kind: TEXT, text: 'replacement' },
    },
  })
  assert.deepEqual(out.map((update) => Boolean(update.live.delta)), [true, false])
  assert.equal(out[0].live.last_seq, 1)
})

test('a pending preview never exceeds the bounded UTF-8 target', () => {
  const out = []
  const buffer = new LiveBuffer((update) => out.push(update))
  const half = 'x'.repeat(MAX_PREVIEW_BYTES / 2)
  buffer.push(frame(1, half))
  buffer.push(frame(2, half))
  assert.equal(out.length, 1)
  assert.equal(Buffer.byteLength(out[0].live.delta.text, 'utf8'), MAX_PREVIEW_BYTES)
  buffer.push(frame(3, 'y'))
  buffer.flush()
  assert.equal(out.length, 2, 'the next preview starts a fresh bounded batch')
})
