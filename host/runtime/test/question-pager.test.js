import test from 'node:test'
import assert from 'node:assert/strict'
import { randomBytes } from 'node:crypto'
import { codec } from '../src/protocol.js'
import { QuestionPager } from '../src/commands/question-pager.js'

const proto = codec()
const batch = (id, size = 20, text = 'x') => ({ session_id: 's', batch_id: id, source_epoch: 'e', version: 1,
  digest: new Uint8Array(32), phase: 'QUESTION_OPEN', answerable: true,
  questions: [{ id: 'q', question: text.repeat(size) }] })
const size = page => proto.encode('whalerein.v1.ResponsePayload', { question_page: page }).length
const decode = page => proto.decode('whalerein.v1.QuestionPage', proto.encode('whalerein.v1.QuestionPage', page))

// Find the smallest budget that still returns the first complete entry.
function oneBatchBudget(pager, inputs) {
  let budget = 1
  while (budget < 1000) {
    try { const page = pager.page({ ...inputs, maxBytes: budget }); if (page.batches[0]?.questions?.length === 1) return budget } catch {}
    budget++
  }
  throw new Error('no one-batch budget')
}

test('question pages preserve order and bind cursors to device and session', () => {
  const pager = new QuestionPager(proto), source = { batches: [batch('one'), batch('two'), batch('three')] }
  const inputs = { deviceId: 'd1', sessionId: 's', cursor: '', source, maxBytes: 1000 }
  assert.deepEqual(pager.page(inputs).batches.map(item => item.batch_id), ['one', 'two', 'three'])
  const budget = oneBatchBudget(pager, inputs)
  const first = pager.page({ ...inputs, maxBytes: budget })
  assert.deepEqual(first.batches.map(item => item.batch_id), ['one'])
  assert.equal(size(first), budget)
  assert.ok(first.next_cursor)
  const second = pager.page({ ...inputs, cursor: first.next_cursor, maxBytes: budget })
  assert.equal(second.batches[0].batch_id, 'two')
  assert.throws(() => pager.page({ ...inputs, cursor: first.next_cursor, deviceId: 'd2' }), { code: 'conflict' })
  assert.throws(() => pager.page({ ...inputs, cursor: first.next_cursor, sessionId: 'other' }), { code: 'conflict' })
  assert.throws(() => pager.page({ ...inputs, cursor: first.next_cursor + 'x' }), { code: 'conflict' })
  assert.throws(() => pager.page({ ...inputs, cursor: first.next_cursor, source: { batches: [batch('one'), batch('one')] } }), { code: 'unavailable' })
  assert.throws(() => pager.page({ ...inputs, source: { batches: [{ ...batch('other'), session_id: 'other' }] } }), { code: 'unavailable' })
  assert.throws(() => pager.page({ ...inputs, source: { batches: [], next_cursor: 'unsupported' } }), { code: 'unsupported_capability' })
  assert.throws(() => pager.page({ ...inputs, maxBytes: 0 }), { code: 'invalid_argument' })
  assert.throws(() => pager.page({ ...inputs, maxBytes: 1 }), { code: 'payload_too_large' })
  assert.throws(() => pager.page({ ...inputs, cursor: { v: 2 } }), { code: 'invalid_argument' })
})

test('adding or answering batches mid-pagination continues instead of conflicting', () => {
  const pager = new QuestionPager(proto), inputs = { deviceId: 'd1', sessionId: 's', maxBytes: 1000 }
  const source = { batches: [batch('one'), batch('two'), batch('three')] }
  const budget = oneBatchBudget(pager, { ...inputs, source })
  const first = pager.page({ ...inputs, source, maxBytes: budget })
  assert.equal(first.batches[0].batch_id, 'one')
  // A batch added before the delivered anchor does not shift the continuation.
  const inserted = { batches: [batch('zero'), batch('one'), batch('two'), batch('three')] }
  assert.equal(pager.page({ ...inputs, source: inserted, cursor: first.next_cursor, maxBytes: budget }).batches[0].batch_id, 'two')
  // A batch added after the anchor is delivered next.
  const appended = { batches: [batch('one'), batch('four'), batch('two'), batch('three')] }
  assert.equal(pager.page({ ...inputs, source: appended, cursor: first.next_cursor, maxBytes: budget }).batches[0].batch_id, 'four')
  // Answering/removing the delivered anchor resumes at the recorded successor.
  const answered = { batches: [batch('two'), batch('three')] }
  assert.equal(pager.page({ ...inputs, source: answered, cursor: first.next_cursor, maxBytes: budget }).batches[0].batch_id, 'two')
  // Removing the recorded successor skips it without an opaque conflict.
  const successorGone = { batches: [batch('one'), batch('three')] }
  assert.equal(pager.page({ ...inputs, source: successorGone, cursor: first.next_cursor, maxBytes: budget }).batches[0].batch_id, 'three')
  // A fully replaced snapshot falls back to the recorded position.
  const replaced = { batches: [batch('alpha'), batch('beta'), batch('gamma')] }
  assert.equal(pager.page({ ...inputs, source: replaced, cursor: first.next_cursor, maxBytes: budget }).batches[0].batch_id, 'beta')
  // A cursor past the end of a shrunk snapshot is an empty final page.
  const shrunk = pager.page({ ...inputs, source: { batches: [batch('one')] }, cursor: first.next_cursor, maxBytes: budget })
  assert.deepEqual(shrunk.batches, [])
  assert.equal(shrunk.next_cursor, '')
})

test('a persisted pager key continues paging across restart and key loss is explicit', () => {
  const key = randomBytes(32)
  const source = { batches: [batch('one'), batch('two'), batch('three')] }
  const inputs = { deviceId: 'd1', sessionId: 's', maxBytes: 1000 }
  const before = new QuestionPager(proto, { key })
  const budget = oneBatchBudget(before, { ...inputs, source })
  const first = before.page({ ...inputs, source, maxBytes: budget })
  assert.ok(first.next_cursor)
  const expected = before.page({ ...inputs, source, cursor: first.next_cursor, maxBytes: budget }).batches[0].batch_id
  // A restarted Host that reloads the same persisted key continues the cursor.
  const after = new QuestionPager(proto, { key })
  assert.equal(after.page({ ...inputs, source, cursor: first.next_cursor, maxBytes: budget }).batches[0].batch_id, expected)
  // A Host that lost its pager state reports an explicit restart, not a
  // misleading conflict, and never returns data for the unknown cursor.
  const lost = new QuestionPager(proto, { key: randomBytes(32) })
  assert.throws(() => lost.page({ ...inputs, source, cursor: first.next_cursor, maxBytes: budget }), { code: 'snapshot_required' })
})

test('Unicode, newline and multi-byte question content round-trips inside the byte budget', () => {
  const pager = new QuestionPager(proto)
  const text = '第一行\n第二行\t😀 café  '
  const source = { batches: [batch('unicode', 1, text)] }
  const inputs = { deviceId: 'd1', sessionId: 's', source }
  let budget = 1
  while (budget < 4000) {
    try { if (pager.page({ ...inputs, maxBytes: budget }).batches[0]?.questions?.length === 1) break } catch {}
    budget++
  }
  const page = pager.page({ ...inputs, maxBytes: budget })
  assert.ok(size(page) <= budget)
  assert.equal(decode(page).batches[0].questions[0].question, text)
  // Multi-byte content is measured in UTF-8 bytes, not code points: every page
  // at or above its exact encoded size stays within the advertised budget.
  assert.equal(size(pager.page({ ...inputs, maxBytes: size(page) })), size(page))
  for (let candidate = 1; candidate <= size(page); candidate++) {
    try { assert.ok(size(pager.page({ ...inputs, maxBytes: candidate })) <= candidate) } catch (error) { assert.equal(error.code, 'payload_too_large') }
  }
})

test('an oversized single batch is replaced by bounded metadata without losing its guard', () => {
  const pager = new QuestionPager(proto)
  const inputs = { deviceId: 'd1', sessionId: 's', cursor: '', maxBytes: 150 }
  const large = pager.page({ ...inputs, source: { batches: [batch('large', 500)] } })
  assert.equal(large.batches[0].content_unavailable, true)
  assert.equal(large.batches[0].answerable, false)
  assert.equal(large.batches[0].questions, undefined)
  assert.equal(large.batches[0].batch_id, 'large')
  assert.equal(large.batches[0].phase, 'QUESTION_OPEN')
  assert.equal(large.batches[0].digest.length, 32)
})
