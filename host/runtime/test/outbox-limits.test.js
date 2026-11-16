import test from 'node:test'
import assert from 'node:assert/strict'
import { StateStore } from '../src/state/store.js'
import { startHarness, waitFor } from './support/harness.js'

const payload = (text) => Buffer.from(`envelope:${text}`)

test('per-device and total outbox bounds reject new rows and name the exhausted bound', () => {
  const store = new StateStore({ path: ':memory:' })
  try {
    for (const index of [0, 1]) {
      assert.deepEqual(store.addOutbox({ deviceId: 'd1', channelId: 'c1', payload: payload(`d1-${index}`), perDeviceLimit: 2, totalLimit: 10 }), { id: index + 1, overflow: false, reason: null })
    }
    assert.deepEqual(store.addOutbox({ deviceId: 'd1', channelId: 'c1', payload: payload('d1-over'), perDeviceLimit: 2, totalLimit: 10 }), { id: null, overflow: true, reason: 'device' })
    assert.equal(store.countOutbox('d1'), 2)
    assert.equal(store.addOutbox({ deviceId: 'd2', channelId: 'c2', payload: payload('d2-0'), perDeviceLimit: 2, totalLimit: 3 }).overflow, false)
    assert.deepEqual(store.addOutbox({ deviceId: 'd2', channelId: 'c2', payload: payload('d2-over'), perDeviceLimit: 2, totalLimit: 3 }), { id: null, overflow: true, reason: 'total' })
    assert.equal(store.countOutbox(), 3)
    store.removeOutbox(store.listOutbox()[0].id)
    assert.equal(store.addOutbox({ deviceId: 'd2', channelId: 'c2', payload: payload('d2-after'), perDeviceLimit: 2, totalLimit: 3 }).overflow, false)
    assert.equal(store.countOutbox(), 3)
    store.deleteOutboxForDevice('d1')
    assert.equal(store.countOutbox('d1'), 0)
    assert.equal(store.countOutbox('d2'), 2)
  } finally {
    store.close()
  }
})

function message(index) {
  return { message_id: `m-${index}`, role: 'ASSISTANT', blocks: [{ block_id: `b-${index}`, text: { text: `message ${index}` } }], authored_at_unix_ms: 1000 + index }
}

function emitCommitted(harness, index) {
  const value = message(index)
  harness.adapter.messages.push({ sessionId: 's1', message: value })
  harness.adapter.publish('s1', {
    history: {
      from_cursor: `cursor-${index}`,
      to_cursor: `cursor-${index + 1}`,
      batch_id: `batch-${index}`,
      changes: [{ message: value }],
    },
  })
}

test('outbox overflow is accounted per device and a recovered Host drains retained envelopes while a snapshot fills the gap', async () => {
  const harness = await startHarness({
    sessions: [{ sessionId: 's1' }],
    config: { outboxPerDeviceLimit: 2, outboxTotalLimit: 3, reconnectMinMs: 50, reconnectMaxMs: 100 },
  })
  try {
    const first = await harness.newPairedDevice('device-1')
    const second = await harness.newPairedDevice('device-2')
    assert.ok(first.channel && second.channel)
    const publish = harness.host.access.publishEvent.bind(harness.host.access)
    let failing = true
    harness.host.access.publishEvent = async () => {
      if (failing) throw new Error('synthetic slow consumer')
      return undefined
    }
    emitCommitted(harness, 0)
    emitCommitted(harness, 1)
    emitCommitted(harness, 2)
    emitCommitted(harness, 3)
    await waitFor(() => harness.host.store.countOutbox() === 3)
    assert.equal(harness.host.store.countOutbox('device-1'), 2)
    assert.equal(harness.host.store.countOutbox('device-2'), 1)
    const diagnostics = harness.diagnostics()
    assert.equal(diagnostics.pendingCiphertext.count, 3)
    assert.equal(diagnostics.pendingCiphertext.limit, 3)
    assert.equal(diagnostics.pendingCiphertext.dropped, 5)
    assert.equal(diagnostics.cursors.snapshotRequired, 2)
    assert.equal(harness.host.events.describe().droppedDevices, 2)
    assert.equal(harness.host.events.describe().outboxDrops, 5)
    assert.equal(harness.host.events.describe().pending, 3)

    failing = false
    harness.host.access.publishEvent = publish
    assert.equal(await harness.host.events.flushOutbox(), 3)
    assert.equal(harness.host.store.countOutbox(), 0)
    assert.equal(harness.host.events.describe().pending, 0)

    // The dropped committed update is a gap in the watch stream. The source
    // snapshot still contains every committed message, so a resyncing client
    // recovers the dropped content without a second native mutation.
    const page = await first.query({ read_history: { session_id: 's1', snapshot: true } })
    assert.equal(page.status, 'succeeded')
    const texts = page.response.history_page.batch.changes.map((change) => change.message.blocks[0].text.text)
    assert.deepEqual(texts, ['message 0', 'message 1', 'message 2', 'message 3'])
    const secondPage = await second.query({ read_history: { session_id: 's1', snapshot: true } })
    assert.equal(secondPage.response.history_page.batch.changes.length, 4)
  } finally {
    await harness.stop()
  }
})

test('a channel replacement clears the previous outbox and its snapshot demand', async () => {
  const harness = await startHarness({
    sessions: [{ sessionId: 's1' }],
    config: { outboxPerDeviceLimit: 1, outboxTotalLimit: 4 },
  })
  try {
    const device = await harness.newPairedDevice('device-1')
    harness.host.access.publishEvent = async () => { throw new Error('synthetic slow consumer') }
    emitCommitted(harness, 0)
    emitCommitted(harness, 1)
    await waitFor(() => harness.host.store.countOutbox() === 1)
    assert.equal(harness.host.events.describe().snapshotRequired, 1)
    harness.host.events.onChannelReplaced('device-1', device.channel.channelId)
    assert.equal(harness.host.events.describe().snapshotRequired, 0)
    assert.equal(harness.host.events.describe().outboxDrops, 1)
    assert.equal(harness.host.store.countOutbox(), 0)
  } finally {
    await harness.stop()
  }
})
