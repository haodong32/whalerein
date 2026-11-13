import test from 'node:test'
import assert from 'node:assert/strict'
import { codec } from '../src/protocol.js'
import { QuestionPager } from '../src/commands/question-pager.js'

const proto = codec()
const batch = (id, size = 20) => ({ session_id: 's', batch_id: id, source_epoch: 'e', version: 1,
  digest: new Uint8Array(32), phase: 'QUESTION_OPEN', answerable: true,
  questions: [{ id: 'q', question: 'x'.repeat(size) }] })
const size = page => proto.encode('whalerein.v1.ResponsePayload', { question_page: page }).length

test('question pages preserve order, bind cursors to device and snapshot, and never omit oversized batches', () => {
  const pager = new QuestionPager(proto), source = { batches: [batch('one'), batch('two'), batch('three')] }
  const inputs = { deviceId: 'd1', sessionId: 's', cursor: '', source, maxBytes: 1000 }
  assert.deepEqual(pager.page(inputs).batches.map(item => item.batch_id), ['one', 'two', 'three'])
  // Find an exact byte budget for one complete entry and its actual cursor.
  let budget = 1
  while (budget < 1000) {
    try { const page = pager.page({ ...inputs, maxBytes: budget }); if (page.batches[0]?.questions?.length === 1) break } catch {}
    budget++
  }
  const first = pager.page({ ...inputs, maxBytes: budget })
  assert.deepEqual(first.batches.map(item => item.batch_id), ['one'])
  assert.equal(size(first), budget)
  assert.ok(first.next_cursor)
  const second = pager.page({ ...inputs, cursor: first.next_cursor, maxBytes: budget })
  assert.equal(second.batches[0].batch_id, 'two')
  assert.throws(() => pager.page({ ...inputs, cursor: first.next_cursor, deviceId: 'd2' }), { code: 'conflict' })
  assert.throws(() => pager.page({ ...inputs, cursor: first.next_cursor + 'x' }), { code: 'conflict' })
  assert.throws(() => pager.page({ ...inputs, cursor: first.next_cursor, source: { batches: [batch('changed'), ...source.batches.slice(1)] } }), { code: 'conflict' })
  assert.throws(() => pager.page({ ...inputs, source: { batches: [batch('one'), batch('one')] } }), { code: 'unavailable' })
  assert.throws(() => pager.page({ ...inputs, source: { batches: [{ ...batch('other'), session_id: 'other' }] } }), { code: 'unavailable' })
  assert.throws(() => pager.page({ ...inputs, source: { batches: [], next_cursor: 'unsupported' } }), { code: 'unsupported_capability' })
  assert.throws(() => pager.page({ ...inputs, maxBytes: 0 }), { code: 'invalid_argument' })
  assert.throws(() => pager.page({ ...inputs, maxBytes: 1 }), { code: 'payload_too_large' })
  const large = pager.page({ ...inputs, source: { batches: [batch('large', 500)] }, maxBytes: 150 })
  assert.equal(large.batches[0].content_unavailable, true)
  assert.equal(large.batches[0].answerable, false)
  assert.equal(large.batches[0].questions, undefined)
})
