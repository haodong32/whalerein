import test from 'node:test'
import assert from 'node:assert/strict'
import { FakeDshAdapter } from '../src/adapter/fake.js'

test('read-only version markers are stable across reads and never advance the state revision', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's' }] })
  await adapter.open({})
  try {
    const first = await adapter.readVersionMarkers('s', { conversation: true })
    const second = await adapter.readVersionMarkers('s', { conversation: true })
    assert.deepEqual(second, first, 'an unchanged observation returns the same markers')
    assert.equal(adapter.conversation.revision, 0, 'a version check does not reuse or advance the state read counter')
    assert.equal(typeof first.revision, 'string')
    assert.ok(first.live_epoch)
    assert.ok(first.history_head)
    assert.ok(first.projection_id)
    assert.equal(first.run_phase, 'IDLE')
  } finally {
    await adapter.close()
  }
})

test('a questions-only version check reads the question stamp without conversation facets', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's' }], behavior: { questions: true } })
  await adapter.open({})
  try {
    let conversationReads = 0
    const original = adapter.readVersionMarkers.bind(adapter)
    adapter.readVersionMarkers = async (sessionId, facets, options) => {
      if (facets.conversation) conversationReads++
      return original(sessionId, facets, options)
    }
    const markers = await adapter.readVersionMarkers('s', { questions: true })
    assert.equal(conversationReads, 0)
    assert.equal(markers.questions.availability, 'available')
    assert.equal(adapter.conversation.revision, 0)
  } finally {
    await adapter.close()
  }
})

test('an authoritative absence during a version check surfaces as not_found', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's' }] })
  await adapter.open({})
  try {
    adapter.deleteSession('s')
    await assert.rejects(adapter.readVersionMarkers('s', { conversation: true }), error => error.code === 'not_found')
  } finally {
    await adapter.close()
  }
})
