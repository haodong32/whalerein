import test from 'node:test'
import assert from 'node:assert/strict'
import { FakeDshAdapter } from '../src/adapter/fake.js'
import { RealDshAdapter } from '../src/adapter/real.js'
import { dshLatencyFields } from '../src/adapter/base.js'
import { batchToken, createTokenizer } from 'whalerein-host/observation/index.js'

function captureOutlet() {
  let clock = 1000
  const facts = []
  const tokenizer = createTokenizer('0123456789abcdef0123456789abcdef')
  return {
    facts,
    nowNs: () => BigInt(clock++),
    token: (domain, raw) => tokenizer.token(domain, raw),
    emit(event, data) { facts.push({ event, data }); return true },
    emitAt(ts, event, data) { facts.push({ event, data, ts: Number(ts) }); return true },
    stats: () => ({ enabled: true }),
  }
}

function eventsOf(facts, event) {
  return facts.filter(fact => fact.event === event)
}

test('latency/AC-14.fake-source marks fake preview and committed message origins without content', async () => {
  const observation = captureOutlet()
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's1' }], observation })
  await adapter.open({})
  adapter.emit('s1', {
    type: 'assistant/preview', ephemeral: true,
    data: {
      stream_id: 'raw-stream', revision: 1, phase: 'chunk', index: 0,
      block_id: 'raw-block', block_kind: 'BLOCK_KIND_TEXT', text: 'hello',
    },
  })
  adapter.emit('s1', {
    type: 'assistant/message', kind: 'output',
    data: { turn: 1, step: 1, message: { content: [{ type: 'text', text: 'abcd' }] } },
  })
  await adapter.close()
  const callbacks = eventsOf(observation.facts, 'dsh_callback')
  const handoffs = eventsOf(observation.facts, 'dsh_handoff')
  assert.deepEqual(callbacks.map(fact => fact.data.native), ['assistant-preview', 'assistant-message'])
  assert.deepEqual(handoffs.map(fact => fact.data.bytes), [5, 4])
  assert.equal(callbacks[0].data.stream, 'raw-stream')
  assert.equal(callbacks[0].data.block, 'raw-block')
  assert.ok(callbacks[1].data.seq_start >= 0)
  for (const fact of [...callbacks, ...handoffs]) {
    assert.equal(Object.hasOwn(fact.data, 'text'), false)
    assert.equal(Object.hasOwn(fact.data, 'message'), false)
  }
  assert.ok(callbacks[0].ts <= callbacks[1].ts)
})

test('latency/AC-01.batch stamps one batch on each callback and handoff before publish', async () => {
  const observation = timestampedOutlet()
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's1' }], observation })
  await adapter.open({})
  try {
    const publishedAt = []
    adapter.watch('s1', () => publishedAt.push(observation.facts.length))
    adapter.emit('s1', {
      type: 'assistant/preview', ephemeral: true,
      data: {
        stream_id: 'raw-stream', revision: 4, phase: 'chunk', index: 0,
        block_id: 'raw-block', block_kind: 'BLOCK_KIND_TEXT', text: 'hello',
      },
    })
    const callback = eventsOf(observation.facts, 'dsh_callback').at(-1)
    const handoff = eventsOf(observation.facts, 'dsh_handoff').at(-1)
    // The native revision (4) is not the Host's normalized sequence: the first
    // published preview is sequence 1. Only the normalized interval can join.
    const expected = batchToken(observation, 'raw-stream', 1, 1)
    assert.notEqual(expected, '')
    assert.equal(callback.data.seq_start, 1)
    assert.equal(callback.data.seq_end, 1)
    assert.equal(callback.data.bytes, 5)
    assert.equal(callback.data.batch, expected)
    assert.equal(handoff.data.batch, expected)
    assert.ok(callback.ts <= handoff.ts)
    // The adapter published only after both facts were recorded.
    assert.equal(publishedAt[0], 2)

    adapter.emit('s1', {
      type: 'assistant/message', kind: 'output',
      data: { turn: 1, step: 1, message: { content: [{ type: 'text', text: 'abcd' }] } },
    })
    const committedCallback = eventsOf(observation.facts, 'dsh_callback').at(-1)
    const committedHandoff = eventsOf(observation.facts, 'dsh_handoff').at(-1)
    assert.equal(committedCallback.data.native, 'assistant-message')
    assert.equal(
      committedHandoff.data.batch,
      batchToken(observation, committedCallback.data.stream, committedCallback.data.seq_start, committedCallback.data.seq_end),
    )
    assert.equal(committedHandoff.data.batch, committedCallback.data.batch)
    await new Promise(resolve => setImmediate(resolve))
    // Both committed facts exist before the ordered projection reaches a Host sink.
    assert.equal(publishedAt[1], 4)
  } finally {
    await adapter.close()
  }
})

test('latency/AC-01.split hands off every published part with its own batch', async () => {
  const observation = timestampedOutlet()
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's1' }], observation })
  await adapter.open({})
  try {
    adapter.setPreviewBytes(2)
    const published = []
    adapter.watch('s1', update => published.push(update.live.first_seq))
    adapter.emit('s1', {
      type: 'assistant/preview', ephemeral: true,
      data: {
        stream_id: 'raw-stream', revision: 9, phase: 'chunk', index: 0,
        block_id: 'raw-block', block_kind: 'BLOCK_KIND_TEXT', text: 'hello',
      },
    })
    const callbacks = eventsOf(observation.facts, 'dsh_callback')
    const handoffs = eventsOf(observation.facts, 'dsh_handoff')
    assert.deepEqual(callbacks.map(fact => [fact.data.seq_start, fact.data.seq_end]), [[1, 1], [2, 2], [3, 3]])
    assert.deepEqual(handoffs.map(fact => [fact.data.seq_start, fact.data.seq_end]), [[1, 1], [2, 2], [3, 3]])
    for (const fact of handoffs) {
      assert.equal(fact.data.batch, batchToken(observation, 'raw-stream', fact.data.seq_start, fact.data.seq_end))
    }
    assert.deepEqual(published, [1, 2, 3])
  } finally {
    await adapter.close()
  }
})

test('a suppressed native preview keeps its callback but has no handoff or batch', async () => {
  const observation = timestampedOutlet()
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's1' }], observation })
  await adapter.open({})
  try {
    const preview = {
      type: 'assistant/preview', ephemeral: true,
      data: {
        stream_id: 'raw-stream', revision: 3, phase: 'chunk', index: 0,
        block_id: 'raw-block', block_kind: 'BLOCK_KIND_TEXT', text: 'hello',
      },
    }
    adapter.emit('s1', preview)
    adapter.emit('s1', preview)
    const callbacks = eventsOf(observation.facts, 'dsh_callback')
    assert.equal(callbacks.length, 2)
    assert.equal(eventsOf(observation.facts, 'dsh_handoff').length, 1)
    assert.equal(callbacks[0].data.batch, batchToken(observation, 'raw-stream', 1, 1))
    assert.equal(Object.hasOwn(callbacks[1].data, 'batch'), false)
  } finally {
    await adapter.close()
  }
})

test('a native preview block-end carries replacement bytes and no visible text', () => {
  const fields = dshLatencyFields({
    sessionId: 's', dshSeq: null, ephemeral: true, data: {
      stream_id: 'stream', revision: 4, phase: 'chunk', block_id: 'text-0',
      block_kind: 'BLOCK_KIND_TEXT', block_text: 'replaced',
    },
  }, 'assistant-preview')
  assert.equal(fields.bytes, 8)
  assert.equal(fields.stream, 'stream')
  assert.equal(fields.block, 'text-0')
  assert.equal(fields.seq_start, 4)
  assert.equal(dshLatencyFields({ sessionId: 's', data: { revision: 1 } }, 'assistant-preview'), null)
  assert.equal(dshLatencyFields(null, 'assistant-preview'), null)
  assert.equal(dshLatencyFields({ sessionId: 's', data: {} }, 'history'), null)
})

test('a committed message without reply coordinates still gets a bounded native fact', () => {
  const fields = dshLatencyFields({
    sessionId: 's', dshSeq: 7, type: 'assistant/message', kind: 'output',
    data: { message: { content: [{ type: 'reasoning', text: 'hidden' }, { type: 'text', text: 'ab' }] } },
  }, 'assistant-message')
  assert.equal(fields.bytes, 2)
  assert.equal(fields.attempt, '7')
  assert.equal(fields.block, 'message')
  assert.equal(fields.seq_start, 7)
})

test('an adapter without a capture keeps every native behavior unchanged', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's1' }] })
  await adapter.open({})
  const observed = []
  const unsubscribe = adapter.subscribeEvents(event => observed.push(event))
  adapter.emit('s1', {
    type: 'assistant/message', kind: 'output',
    data: { turn: 1, step: 1, message: { content: [{ type: 'text', text: 'visible' }] } },
  })
  await new Promise(resolve => setTimeout(resolve, 5))
  assert.equal(observed.length, 1)
  assert.equal(adapter.observationNow(), null)
  unsubscribe()
  await adapter.close()
})

function nativeCtx() {
  const handlers = new Map()
  const session = { id: 's1', running: true, events: [] }
  const ctx = {
    sessions: { get: id => (id === 's1' ? session : undefined), list: () => [session] },
    sessionController: { list: async () => ({ items: [] }) },
    credentials: {
      async readRecord() { return undefined },
      async modifyRecord() { return undefined },
      async resolve() { return undefined },
      async set() {},
    },
    on(event, handler) {
      const list = handlers.get(event) ?? []
      list.push(handler)
      handlers.set(event, list)
      return () => handlers.set(event, (handlers.get(event) ?? []).filter(entry => entry !== handler))
    },
    emit(event, ...args) {
      return (handlers.get(event) ?? []).map(handler => handler(...args))
    },
  }
  return { ctx, session, emit: (event, ...args) => ctx.emit(event, ...args) }
}

function timestampedOutlet() {
  let clock = 100
  const facts = []
  const tokenizer = createTokenizer('0123456789abcdef0123456789abcdef')
  return {
    facts,
    nowNs: () => BigInt(clock++),
    token: (domain, raw) => tokenizer.token(domain, raw),
    emit(event, data) { facts.push({ event, data, ts: clock++ }); return true },
    emitAt(ts, event, data) { facts.push({ event, data, ts: Number(ts) }); return true },
    stats: () => ({ enabled: true }),
  }
}

test('latency/AC-01.native stamps the real native callback before normalization and the handoff after it', async () => {
  const observation = timestampedOutlet()
  const { ctx, emit } = nativeCtx()
  const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1' }, observation })
  await adapter.open()
  try {
    const agent = { session: { id: 's1' } }
    emit('agent/assistant-stream', {
      agent,
      frame: {
        attemptId: 's1:1', revision: 1, type: 'chunk', index: 0,
        chunk: { index: 0, type: 'text-delta', text: 'hello' },
      },
    })
    const callbacks = eventsOf(observation.facts, 'dsh_callback')
    const handoffs = eventsOf(observation.facts, 'dsh_handoff')
    assert.equal(callbacks.length, 1)
    assert.equal(handoffs.length, 1)
    assert.equal(callbacks[0].data.native, 'assistant-preview')
    assert.equal(callbacks[0].data.bytes, 5)
    assert.ok(callbacks[0].data.stream.endsWith(':s1:1'), 'the Agent epoch prefixes the native attempt identity')
    assert.ok(callbacks[0].ts < handoffs[0].ts, 'handoff follows the native callback')
    assert.equal(Object.hasOwn(callbacks[0].data, 'text'), false)

    emit('session/event', { id: 's1' }, {
      type: 'assistant/message', seq: 3, time: 3, data: { text: 'abcd', turn: 1, step: 1 },
    })
    const messageCallback = eventsOf(observation.facts, 'dsh_callback').at(-1)
    assert.equal(messageCallback.data.native, 'assistant-message')
    assert.equal(messageCallback.data.bytes, 4)
    assert.equal(eventsOf(observation.facts, 'dsh_handoff').length, 2)
  } finally {
    await adapter.close()
  }
})

test('latency/AC-14.native-source reports the pinned real adapter provenance for native facts', async () => {
  const observation = timestampedOutlet()
  const { ctx } = nativeCtx()
  const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1' }, observation })
  assert.equal(adapter.constructor.name, 'RealDshAdapter')
  const description = adapter.describe()
  assert.equal(description.mode, 'real')
  assert.equal(description.available, true)
  await adapter.open()
  await adapter.close()
  // Integration procedure: run the pinned real-DSH CLI with the same build and
  // the fixture's `WHALEREIN_BENCH_*` outlet, then require the `dsh_callback`
  // and `dsh_handoff` records to carry producer `dsh` from this real adapter.
  // This unit contract proves the adapter mode; the native integration run
  // proves provenance on the supported DSH build.
})

test('latency/AC-14.missing-native fails explicitly when required native services are absent', async () => {
  const observation = timestampedOutlet()
  const adapter = new RealDshAdapter({ ctx: {}, config: { hostId: 'host-1' }, observation })
  await assert.rejects(adapter.open(), error => error?.code === 'unavailable')
  assert.equal(observation.facts.length, 0, 'a missing native source never produces a silent passing fact')
  assert.equal(adapter.describe().mode, 'real')
})
