import test from 'node:test'
import assert from 'node:assert/strict'
import {
  MAX_SPLIT_BYTES,
  partitionInterval,
  splitLivePreview,
  splitUtf8,
  utf8ByteLength,
} from '../src/events/preview-split.js'

function live(firstSeq, lastSeq, text, overrides = {}) {
  return {
    session: { session_id: 's' },
    live: {
      attempt: { live_epoch: 'epoch', attempt_id: 'attempt' },
      block_id: 'block',
      first_seq: firstSeq,
      last_seq: lastSeq,
      delta: { kind: 'BLOCK_KIND_TEXT', text },
      ...overrides,
    },
  }
}

test('UTF-8 splitting never cuts a multi-byte sequence or a surrogate pair', () => {
  for (const [text, maxBytes] of [
    ['aé😀b', 4],
    ['😀😀', 4],
    ['héllo wörld', 3],
    ['\u{1F600}\u{1F601}\u{1F602}', 4],
  ]) {
    const parts = splitUtf8(text, maxBytes)
    assert.equal(parts.join(''), text, 'the exact text appears once')
    for (const part of parts) {
      assert.ok(utf8ByteLength(part) <= maxBytes)
      assert.equal(Buffer.from(part, 'utf8').toString('utf8'), part)
      assert.equal(part.includes('\uFFFD'), false)
    }
  }
  assert.deepEqual(splitUtf8('ééé', 5), ['éé', 'é'])
  assert.throws(() => splitUtf8('😀', 2), (error) => error.code === 'payload_too_large')
})

test('a normalized interval is partitioned into contiguous, gap-free and overlap-free parts', () => {
  const intervals = partitionInterval(10, 19, 4)
  assert.deepEqual(intervals, [[10, 11], [12, 14], [15, 16], [17, 19]])
  for (let index = 1; index < intervals.length; index++) {
    assert.equal(intervals[index][0], intervals[index - 1][1] + 1)
  }
  assert.equal(intervals[0][0], 10)
  assert.equal(intervals.at(-1)[1], 19)
  assert.throws(() => partitionInterval(1, 2, 3), (error) => error.code === 'invalid_argument')
})

test('an oversized live delta is split into ordered updates that tile its interval with the exact prefix once', () => {
  const text = Array.from({ length: 9 }, (_, index) => `${index}`.padStart(100, 'x')).join('')
  const update = live(1, 9, text)
  const parts = splitLivePreview(update, { maxTextBytes: 100 })
  assert.equal(parts.length, 9)
  assert.equal(parts.map((part) => part.live.delta.text).join(''), text)
  assert.deepEqual(parts.map((part) => [part.live.first_seq, part.live.last_seq]), [
    [1, 1], [2, 2], [3, 3], [4, 4], [5, 5], [6, 6], [7, 7], [8, 8], [9, 9],
  ])
  for (const part of parts) {
    assert.equal(part.live.delta.kind, 'BLOCK_KIND_TEXT')
    assert.equal(part.live.attempt.attempt_id, 'attempt')
    assert.equal(part.live.block_id, 'block')
    assert.equal(part.session.session_id, 's')
  }
})

test('splitting preserves UTF-8 boundaries under an asymmetric text budget', () => {
  const text = 'é'.repeat(300)
  const parts = splitLivePreview(live(1, 20, text), { maxTextBytes: 30 })
  assert.equal(parts.map((part) => part.live.delta.text).join(''), text)
  for (const part of parts) {
    assert.ok(utf8ByteLength(part.live.delta.text) <= 30)
    assert.equal(utf8ByteLength(part.live.delta.text) % 2, 0, 'no half code point')
  }
})

test('a delta that already fits, or a one-unit interval, is never expanded', () => {
  const small = live(1, 2, 'tiny')
  assert.deepEqual(splitLivePreview(small, { maxTextBytes: 100 }), [small])
  assert.equal(splitLivePreview(live(1, 1, 'x'.repeat(100)), { maxTextBytes: 10 }), null)
})

test('the split count and byte amplification bounds refuse a split rather than dropping text', () => {
  const update = live(1, 1000, 'x'.repeat(1000))
  assert.equal(splitLivePreview(update, { maxTextBytes: 1, maxParts: 10 }), null)
  assert.equal(splitLivePreview(update, { maxTextBytes: 10, maxBytes: MAX_SPLIT_BYTES }), null)
  assert.equal(splitLivePreview(update, { maxTextBytes: 10, maxParts: 100, maxBytes: 100 }), null)
})

test('invalid split inputs fail closed or fall back to the complete-message carrier', () => {
  assert.throws(() => splitUtf8('x', 0), (error) => error.code === 'invalid_argument')
  assert.equal(splitLivePreview({ session: { session_id: 's' } }, { maxTextBytes: 10 }), null)
  assert.equal(splitLivePreview(live(1, 4, 'text'), { maxTextBytes: 0 }), null)
  assert.equal(splitLivePreview({ ...live(1, 4, 'text'), live: { ...live(1, 4, 'text').live, delta: { kind: 'BLOCK_KIND_TEXT', text: 5 } } }, { maxTextBytes: 10 }), null)
  assert.equal(splitLivePreview(live(1, 4, '😀😀'), { maxTextBytes: 2 }), null)
})
