import test from 'node:test'
import assert from 'node:assert/strict'
import { FakeDshAdapter } from '../src/adapter/fake.js'
import { RealDshAdapter } from '../src/adapter/real.js'

async function source(options = {}) {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's' }, { sessionId: 'other' }], hostId: 'host-1', ...options })
  await adapter.open()
  return adapter
}

test('a committed submission resolves to its canonical message reference and exact read', async () => {
  const adapter = await source()
  try {
    await adapter.execute('s', { send_message: { text: 'first reply' } }, { operationId: 'op-1' })
    const reply = await adapter.readMessageReply('s', 'op-1')
    assert.equal(reply.state, 'MESSAGE_REPLY_COMMITTED')
    assert.deepEqual(reply.pending_input, [])
    assert.equal(reply.committed.session.host_id, 'host-1')
    assert.equal(reply.committed.session.session_id, 's')
    assert.ok(reply.committed.message_id)
    assert.ok(reply.committed.source_locator)
    assert.ok(reply.committed.projection_id)
    const page = await adapter.readHistory('s', { exact: reply.committed })
    assert.equal(page.batch.changes[0].message.message_id, reply.committed.message_id)
    assert.equal(page.batch.changes[0].message.blocks[0].text.text, 'first reply')
  } finally {
    await adapter.close()
  }
})

test('a queued send stays pending until its own turn starts', async () => {
  const adapter = await source()
  try {
    adapter.setRunning('s', true)
    await adapter.execute('s', { send_message: { text: 'queued' } }, { operationId: 'queued-1' })
    const pending = await adapter.readMessageReply('s', 'queued-1')
    assert.equal(pending.state, 'MESSAGE_REPLY_PENDING')
    assert.equal(pending.committed, undefined)

    adapter.setRunning('s', false)
    await new Promise(resolve => setImmediate(resolve))
    const committed = await adapter.readMessageReply('s', 'queued-1')
    assert.equal(committed.state, 'MESSAGE_REPLY_COMMITTED')
    const projected = await adapter.readHistory('s', { exact: committed.committed })
    assert.equal(projected.batch.changes[0].message.blocks[0].text.text, 'queued')
  } finally {
    await adapter.close()
  }
})

test('overlapping sends own distinct replies and cannot satisfy each other', async () => {
  const adapter = await source()
  try {
    adapter.setRunning('s', true)
    await adapter.execute('s', { send_message: { text: 'one' } }, { operationId: 'send-1' })
    await adapter.execute('s', { send_message: { text: 'two' } }, { operationId: 'send-2' })
    assert.equal((await adapter.readMessageReply('s', 'send-2')).state, 'MESSAGE_REPLY_PENDING')

    adapter.setRunning('s', false)
    await new Promise(resolve => setImmediate(resolve))
    const first = await adapter.readMessageReply('s', 'send-1')
    const second = await adapter.readMessageReply('s', 'send-2')
    assert.equal(first.state, 'MESSAGE_REPLY_COMMITTED')
    assert.equal(second.state, 'MESSAGE_REPLY_COMMITTED')
    assert.notEqual(first.committed.message_id, second.committed.message_id)
    assert.equal((await adapter.readHistory('s', { exact: first.committed })).batch.changes[0].message.blocks[0].text.text, 'one')
    assert.equal((await adapter.readHistory('s', { exact: second.committed })).batch.changes[0].message.blocks[0].text.text, 'two')
  } finally {
    await adapter.close()
  }
})

test('a late unrelated submission never inherits another reply', async () => {
  const adapter = await source()
  try {
    await adapter.execute('s', { send_message: { text: 'reply' } }, { operationId: 'op-known' })
    assert.equal((await adapter.readMessageReply('s', 'op-unknown')).state, 'MESSAGE_REPLY_UNCERTAIN')

    adapter.setRunning('s', true)
    const pending = await adapter.readMessageReply('s', 'op-unknown')
    assert.equal(pending.state, 'MESSAGE_REPLY_PENDING')
  } finally {
    await adapter.close()
  }
})

test('ambiguous submission evidence stays uncertain instead of selecting one reply', async () => {
  const adapter = await source()
  try {
    adapter.emit('s', {
      type: 'user/message',
      data: { id: 'dup', role: 'user', content: [{ type: 'text', text: 'first' }], source: { kind: 'user', rpcId: 'dup' } },
    })
    adapter.output('s', 'answer one')
    adapter.emit('s', { type: 'turn/end', data: { running: false } })
    adapter.emit('s', {
      type: 'user/message',
      data: { id: 'dup', role: 'user', content: [{ type: 'text', text: 'second' }], source: { kind: 'user', rpcId: 'dup' } },
    })
    const reply = await adapter.readMessageReply('s', 'dup')
    assert.equal(reply.state, 'MESSAGE_REPLY_UNCERTAIN')
    assert.equal(reply.committed, undefined)
  } finally {
    await adapter.close()
  }
})

test('a settled final message with only non-text blocks is a committed reply', async () => {
  const adapter = await source()
  try {
    adapter.emit('s', {
      type: 'user/message',
      data: { id: 'nontext', role: 'user', content: [{ type: 'text', text: 'go' }], source: { kind: 'user', rpcId: 'nontext' } },
    })
    adapter.emit('s', {
      type: 'assistant/message',
      data: {
        turn: 1, step: 1,
        message: {
          id: 'visible-nontext', role: 'assistant',
          content: [{ type: 'reasoning', text: 'only reasoning is visible here' }],
          source: { kind: 'model' },
        },
      },
    })
    adapter.emit('s', { type: 'turn/end', data: { running: false } })
    const reply = await adapter.readMessageReply('s', 'nontext')
    assert.equal(reply.state, 'MESSAGE_REPLY_COMMITTED')
    const page = await adapter.readHistory('s', { exact: reply.committed })
    assert.deepEqual(page.batch.changes[0].message.blocks.map(block => Object.keys(block).at(-1)), ['reasoning'])
  } finally {
    await adapter.close()
  }
})

test('pending input is reported only for the active submitted operation', async () => {
  const adapter = await source({ behavior: { longRunning: true, approvalOnFirstMessage: true, approvalTtlMs: 300_000 } })
  try {
    await adapter.execute('s', { send_message: { text: 'needs approval' } }, { operationId: 'op-approval' })
    const reply = await adapter.readMessageReply('s', 'op-approval')
    assert.equal(reply.state, 'MESSAGE_REPLY_INPUT_REQUIRED')
    assert.deepEqual(reply.pending_input, ['INPUT_APPROVAL'])
    assert.equal(reply.committed, undefined)

    // An unrelated submission has no evidenced owning work.
    const unrelated = await adapter.readMessageReply('s', 'op-other')
    assert.equal(unrelated.state, 'MESSAGE_REPLY_PENDING')
  } finally {
    await adapter.close()
  }
})

test('an unknown session fails with a stable error instead of an empty reply', async () => {
  const adapter = await source()
  try {
    await assert.rejects(adapter.readMessageReply('missing', 'op'), error => error.code === 'not_found')
  } finally {
    await adapter.close()
  }
})

test('the real adapter advertises send follow-up only when prompt and passive history exist', async () => {
  const liveSession = () => ({ id: 's', header: { id: 's' }, snapshotEvents: () => [] })
  const ctx = {
    sessionController: { prompt: async () => ({ accepted: true }), events: async () => ({ records: [], hasMore: false, cursor: 0 }) },
    sessions: { list: () => [liveSession()], get: (id) => (id === 's' ? liveSession() : undefined) },
  }
  const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1' } })
  await adapter.open()
  assert.equal((await adapter.getSession('s')).sourceCapabilities.send_followup, true)
  await adapter.close()

  const noHistory = {
    sessionController: { prompt: async () => ({ accepted: true }) },
    sessions: { list: () => [liveSession()], get: (id) => (id === 's' ? liveSession() : undefined) },
  }
  const without = new RealDshAdapter({ ctx: noHistory, config: { hostId: 'host-1' } })
  await without.open()
  assert.equal((await without.getSession('s')).sourceCapabilities.send_followup, false)
  await without.close()
})
