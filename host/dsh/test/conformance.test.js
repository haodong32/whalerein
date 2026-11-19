import test from 'node:test'
import assert from 'node:assert/strict'
import { FakeDshAdapter } from '../src/adapter/fake.js'
import { TextOnlySource } from './support/text-source.js'
import { validateSessionUpdate } from 'whalerein-host/contracts/source.js'

const TYPED = ['live_reasoning', 'tool_activity', 'conversation_notices']
const EXPANDED = ['attachments', 'history_page', 'history_exact', 'send_followup']

async function sources() {
  const fake = new FakeDshAdapter({ sessions: [{ sessionId: 's' }] })
  await fake.open()
  fake.emit('s', { type: 'user/message', data: { id: 'u', role: 'user', content: [{ type: 'text', text: 'hello' }], source: { kind: 'user', rpcId: 'r' } } })
  fake.emit('s', { type: 'assistant/message', data: { turn: 1, step: 1, message: { content: [{ type: 'reasoning', text: 'because' }, { type: 'text', text: 'answer' }], source: { kind: 'model' } } } })
  const text = new TextOnlySource({ sessions: [{ sessionId: 's', messages: [{ role: 'USER', text: 'hello' }, { role: 'ASSISTANT', text: 'answer' }] }] })
  await text.open()
  return { fake, text }
}

test('two independently shaped sources expose the same client contract and only their real optional capabilities', async () => {
  const { fake, text } = await sources()
  try {
    const fakeCapabilities = (await fake.getSession('s')).sourceCapabilities
    const textCapabilities = (await text.getSession('s')).sourceCapabilities
    for (const capability of ['history', 'resumable_history', 'live_text']) {
      assert.equal(fakeCapabilities[capability], true, capability)
      assert.equal(textCapabilities[capability], true, capability)
    }
    for (const capability of TYPED) {
      assert.equal(fakeCapabilities[capability], true, `fake ${capability}`)
      assert.equal(textCapabilities[capability], false, `text-only ${capability}`)
    }
    // FakeDSH implements the full expanded contract; the independent text-only
    // Source must report each optional feature explicitly unavailable instead
    // of simulating or silently downgrading it.
    for (const capability of EXPANDED) {
      assert.equal(fakeCapabilities[capability], true, `fake ${capability}`)
      assert.equal(textCapabilities[capability], false, `text-only ${capability}`)
    }
    assert.deepEqual(fakeCapabilities.delivery_modes, ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER'])
    assert.deepEqual(textCapabilities.delivery_modes, ['START_IF_IDLE'])
  } finally {
    await fake.close()
    await text.close()
  }
})

test('both sources yield the same typed committed text shape and valid history batches', async () => {
  const { fake, text } = await sources()
  try {
    const fakePage = await fake.readHistory('s', { snapshot: true })
    const textPage = await text.readHistory('s', { snapshot: true })
    for (const page of [fakePage, textPage]) assert.doesNotThrow(() => validateSessionUpdate({ session: page.session, history: page.batch }))
    const normalize = page => page.batch.changes.map(change => ({
      role: change.message.role,
      blocks: change.message.blocks.map(block => ({ block_id: block.block_id, type: Object.keys(block).find(key => key !== 'block_id'), text: block.text?.text ?? block.reasoning?.text })),
    }))
    assert.deepEqual(normalize(fakePage), [
      { role: 'USER', blocks: [{ block_id: 'text-0', type: 'text', text: 'hello' }] },
      { role: 'ASSISTANT', blocks: [
        { block_id: 'reasoning-0', type: 'reasoning', text: 'because' },
        { block_id: 'text-1', type: 'text', text: 'answer' },
      ] },
    ])
    assert.deepEqual(normalize(textPage), [
      { role: 'USER', blocks: [{ block_id: 'text-0', type: 'text', text: 'hello' }] },
      { role: 'ASSISTANT', blocks: [{ block_id: 'text-0', type: 'text', text: 'answer' }] },
    ])
    for (const page of [fakePage, textPage]) {
      const forward = await (page === fakePage ? fake : text).readHistory('s', { after_cursor: page.batch.to_cursor, cut_cursor: page.cut_cursor })
      assert.doesNotThrow(() => validateSessionUpdate({ session: forward.session, history: forward.batch }))
      assert.equal(forward.batch.changes.length, 0)
    }
  } finally {
    await fake.close()
    await text.close()
  }
})

test('the text-only source explicitly rejects optional features instead of simulating them', async () => {
  const text = new TextOnlySource({ sessions: [{ sessionId: 's', messages: [{ role: 'USER', text: 'hi' }] }] })
  await text.open()
  try {
    for (const read of [{ page: {} }, { exact: { session: { host_id: 'h', session_id: 's' }, message_id: 'm', source_locator: 'l', projection_id: 'p' } }, { continuation: 'c1' }]) {
      await assert.rejects(text.readHistory('s', read), error => error.code === 'unsupported_capability')
    }
    await assert.rejects(text.openAttachment('s', 'ref'), error => error.code === 'unsupported_capability')
    await assert.rejects(text.readMessageReply('s', 'm'), error => error.code === 'unsupported_capability')
    const state = await text.readState('s')
    assert.equal(state.run_phase, 'IDLE')
  } finally {
    await text.close()
  }
})

test('a single watch on the text-only source emits the shared typed update shape and leaves other operations usable', async () => {
  const text = new TextOnlySource({ sessions: [{ sessionId: 's' }] })
  await text.open()
  try {
    const seen = []
    const watch = text.watch('s', update => seen.push(update))
    text.append('s', { role: 'USER', text: 'from text source' })
    text.append('s', { role: 'ASSISTANT', text: 'reply' })
    watch.close()
    assert.equal(seen.length, 2)
    for (const update of seen) assert.doesNotThrow(() => validateSessionUpdate(update))
    const page = await text.readHistory('s', { snapshot: true })
    assert.equal(page.batch.changes.length, 2)
  } finally {
    await text.close()
  }
})
