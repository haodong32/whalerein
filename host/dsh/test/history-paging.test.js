import test from 'node:test'
import assert from 'node:assert/strict'
import { FakeDshAdapter } from '../src/adapter/fake.js'

async function source(options = {}) {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's' }, { sessionId: 'other' }], hostId: 'host-1', ...options })
  await adapter.open()
  return adapter
}

async function page(adapter, selection = {}, options = {}) {
  return adapter.readHistory('s', { page: selection, ...options })
}

function texts(result) {
  return result.batch.changes.map(change => change.message.blocks[0].text?.text ?? '')
}

test('latest-first pages traverse backwards chronologically without gaps or duplicates', async () => {
  const adapter = await source()
  try {
    for (const text of ['m0', 'm1', 'm2', 'm3', 'm4', 'm5']) adapter.output('s', text)
    // Internal records between messages must not create gaps or duplicates.
    adapter.emit('s', { type: 'turn/start', data: { running: true } })
    adapter.output('s', 'm6')

    const first = await page(adapter, {}, { max_messages: 2 })
    assert.deepEqual(texts(first), ['m5', 'm6'])
    assert.equal(first.complete, false)
    assert.equal(first.source, 'HISTORY_SOURCE_ONLINE')
    assert.equal(first.batch.projection_id, first.projection_id)
    assert.equal(first.batch.from_cursor, first.continuation)

    const second = await page(adapter, { before_cursor: first.continuation }, { max_messages: 2, cut_cursor: first.cut_cursor })
    assert.deepEqual(texts(second), ['m3', 'm4'])
    assert.equal(second.complete, false)
    assert.equal(second.cut_cursor, first.cut_cursor)
    assert.equal(second.projection_id, first.projection_id)

    const third = await page(adapter, { before_cursor: second.continuation }, { max_messages: 2, cut_cursor: second.cut_cursor })
    assert.deepEqual(texts(third), ['m1', 'm2'])
    assert.equal(third.complete, false)

    const fourth = await page(adapter, { before_cursor: third.continuation }, { max_messages: 2, cut_cursor: third.cut_cursor })
    assert.deepEqual(texts(fourth), ['m0'])
    assert.equal(fourth.complete, true)
    assert.equal(fourth.continuation, '')
    assert.notEqual(fourth.batch.from_cursor, third.continuation)

    const seen = [...texts(first), ...texts(second), ...texts(third), ...texts(fourth)]
      .sort((left, right) => Number(left.slice(1)) - Number(right.slice(1)))
    assert.deepEqual(seen, ['m0', 'm1', 'm2', 'm3', 'm4', 'm5', 'm6'])
  } finally {
    await adapter.close()
  }
})

test('a fixed cut excludes concurrent appends while a new traversal sees them', async () => {
  const adapter = await source()
  try {
    adapter.output('s', 'a')
    adapter.output('s', 'b')
    const first = await page(adapter, {}, { max_messages: 2 })
    assert.deepEqual(texts(first), ['a', 'b'])
    assert.equal(first.complete, true)

    adapter.output('s', 'c')
    adapter.output('s', 'd')

    const fixed = await page(adapter, { before_cursor: first.continuation || first.batch.from_cursor }, { max_messages: 2, cut_cursor: first.cut_cursor })
    assert.equal(fixed.cut_cursor, first.cut_cursor)
    assert.equal(texts(fixed).includes('c'), false)
    assert.equal(texts(fixed).includes('d'), false)

    const fresh = await page(adapter, {}, { max_messages: 2 })
    assert.deepEqual(texts(fresh), ['c', 'd'])
    assert.notEqual(fresh.cut_cursor, first.cut_cursor)
  } finally {
    await adapter.close()
  }
})

test('latest-first page attests the registered Watch cut', async () => {
  const adapter = await source()
  try {
    adapter.output('s', 'a')
    adapter.output('s', 'b')
    const watch = await adapter.readState('s')
    adapter.output('s', 'c')

    const first = await page(adapter, {}, { cut_cursor: watch.history_head, max_messages: 1 })
    assert.deepEqual(texts(first), ['b'])
    assert.equal(first.cut_cursor, watch.history_head)
    assert.equal(first.batch.to_cursor, watch.history_head)
    const older = await page(adapter, { before_cursor: first.continuation }, { cut_cursor: first.cut_cursor })
    assert.deepEqual(texts(older), ['a'])
    assert.equal(older.cut_cursor, watch.history_head)

    const stale = error => error.code === 'snapshot_required'
    await assert.rejects(adapter.readHistory('other', { page: {}, cut_cursor: watch.history_head }), stale)
    const [session, projection, position] = JSON.parse(Buffer.from(watch.history_head, 'base64url'))
    const token = value => Buffer.from(JSON.stringify(value)).toString('base64url')
    for (const invalid of [
      [session, projection, 'page', position],
      [session, projection, String(position)],
      [session, projection, position + 10]
    ]) await assert.rejects(page(adapter, {}, { cut_cursor: token(invalid) }), stale)
    adapter.nativeHead = async () => 0
    await assert.rejects(page(adapter, {}, { cut_cursor: watch.history_head }), stale)
  } finally {
    await adapter.close()
  }
})

test('one canonical message larger than the soft target occupies its page alone', async () => {
  const adapter = await source()
  try {
    adapter.output('s', 'x'.repeat(4096))
    adapter.output('s', 'small')
    const result = await page(adapter, {}, { max_messages: 10, max_bytes: 1024 })
    assert.equal(result.batch.changes.length, 1)
    assert.equal(result.batch.changes[0].message.blocks[0].text.text, 'small')
    assert.equal(result.complete, false)
    const older = await page(adapter, { before_cursor: result.continuation }, { max_messages: 10, max_bytes: 1024, cut_cursor: result.cut_cursor })
    assert.equal(older.batch.changes.length, 1)
    assert.equal(older.batch.changes[0].message.blocks[0].text.text.length, 4096)
    assert.equal(older.complete, true)
  } finally {
    await adapter.close()
  }
})

test('a continuation binds its session, cut and projection and fails explicitly when they change', async () => {
  const adapter = await source()
  try {
    for (const text of ['a', 'b', 'c']) adapter.output('s', text)
    const first = await page(adapter, {})
    assert.equal(first.complete, true)
    adapter.output('s', 'd')
    adapter.output('s', 'e')
    const trimmed = await page(adapter, {}, { max_messages: 1 })
    assert.equal(trimmed.complete, false)
    const correct = await page(adapter, { before_cursor: trimmed.continuation }, { cut_cursor: trimmed.cut_cursor })
    assert.equal(correct.cut_cursor, trimmed.cut_cursor)
    await assert.rejects(adapter.readHistory('other', { page: { before_cursor: trimmed.continuation } }), error => error.code === 'snapshot_required')
    await assert.rejects(page(adapter, { before_cursor: trimmed.continuation }, { cut_cursor: 'bogus' }), error => error.code === 'snapshot_required')

    // A replaced native view invalidates the pinned cut.
    adapter.nativeHead = async () => 0
    await adapter.readState('s')
    await assert.rejects(page(adapter, { before_cursor: trimmed.continuation }, { cut_cursor: trimmed.cut_cursor }), error => error.code === 'snapshot_required')
  } finally {
    await adapter.close()
  }
})

test('exact reads resolve a canonical committed reference through the same projection', async () => {
  const adapter = await source()
  try {
    await adapter.execute('s', { send_message: { text: 'exact reply' } }, { operationId: 'op-exact' })
    adapter.output('s', 'later')
    const reply = await adapter.readMessageReply('s', 'op-exact')
    assert.equal(reply.state, 'MESSAGE_REPLY_COMMITTED')
    const exact = await adapter.readHistory('s', { exact: reply.committed })
    assert.equal(exact.batch.changes.length, 1)
    const message = exact.batch.changes[0].message
    assert.equal(message.message_id, reply.committed.message_id)
    assert.equal(message.role, 'ASSISTANT')
    assert.equal(message.blocks[0].text.text, 'exact reply')
    assert.equal(exact.projection_id, reply.committed.projection_id)
    assert.equal(exact.complete, true)
    assert.equal(exact.batch.projection_id, exact.projection_id)
  } finally {
    await adapter.close()
  }
})

test('a live notification ahead of the passive head never rotates the pinned projection', async () => {
  const adapter = await source()
  try {
    adapter.output('s', 'a')
    adapter.output('s', 'b')
    adapter.nativeHead = async () => 2
    const first = await adapter.readHistory('s', { snapshot: true, max_messages: 1 })
    // A committed notification reports the live sequence (3) before the
    // passive page cursor catches up. The next page still reads head 2; that
    // must not look like a rewind.
    adapter.output('s', 'c')
    adapter.nativeHead = async () => 2
    const second = await adapter.readHistory('s', { snapshot: true })
    assert.equal(second.projection_id, first.projection_id)
    const state = await adapter.readState('s')
    assert.equal(state.projection_id, first.projection_id)
  } finally {
    await adapter.close()
  }
})

test('exact reads reject stale locators, wrong scope and oversize canonical messages explicitly', async () => {
  const adapter = await source()
  try {
    await adapter.execute('s', { send_message: { text: 'x'.repeat(4096) } }, { operationId: 'op-big' })
    const reply = await adapter.readMessageReply('s', 'op-big')
    const reference = reply.committed

    await assert.rejects(adapter.readHistory('s', { exact: { ...reference, message_id: 'not-the-message' } }), error => error.code === 'not_found')
    await assert.rejects(adapter.readHistory('s', { exact: { ...reference, projection_id: 'another-projection' } }), error => error.code === 'snapshot_required')
    await assert.rejects(adapter.readHistory('s', { exact: { ...reference, source_locator: 'bogus' } }), error => error.code === 'snapshot_required')
    await assert.rejects(adapter.readHistory('s', {
      exact: { ...reference, session: { host_id: 'host-1', session_id: 'other' } },
    }), error => error.code === 'forbidden')
    await assert.rejects(adapter.readHistory('s', {
      exact: { ...reference, session: { host_id: 'other-host', session_id: 's' } },
    }), error => error.code === 'forbidden')
    await assert.rejects(adapter.readHistory('s', { exact: reference, max_bytes: 1024 }), error => error.code === 'payload_too_large')

    // A stale locator after a projection reset is rejected, not misresolved.
    adapter.nativeHead = async () => 0
    await adapter.readState('s')
    await assert.rejects(adapter.readHistory('s', { exact: reference }), error => error.code === 'snapshot_required')
  } finally {
    await adapter.close()
  }
})

test('a page traversal can be run while a forward synchronization read continues', async () => {
  const adapter = await source()
  try {
    adapter.output('s', 'a')
    adapter.output('s', 'b')
    const forward = await adapter.readHistory('s', { snapshot: true })
    assert.deepEqual(texts(forward), ['a', 'b'])
    adapter.output('s', 'c')
    const backwards = await page(adapter, {}, { max_messages: 1 })
    assert.deepEqual(texts(backwards), ['c'])
    const next = await adapter.readHistory('s', { after_cursor: forward.batch.to_cursor })
    assert.deepEqual(texts(next), ['c'])
    assert.equal(next.complete, true)
  } finally {
    await adapter.close()
  }
})
