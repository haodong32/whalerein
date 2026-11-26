import test from 'node:test'
import assert from 'node:assert/strict'
import { randomBytes } from 'node:crypto'
import { LiveBuffer } from '../src/events/live-buffer.js'
import { WatchedEvents } from '../src/events/watches.js'
import { SourceAdapter } from '../src/contracts/source.js'
import { StateStore } from '../src/state/store.js'
import { SecurityManager } from '../src/security/manager.js'
import { generateKeypair } from '../src/security/noise.js'
import { codec } from '../src/protocol.js'
import { establishChannel } from './support/channel.js'
import { MqttAccess } from '../src/access/mqtt.js'
import { FakeBroker } from './support/fake-mqtt.js'
import { CommandService } from '../src/commands/service.js'
import { TestSource } from './support/source.js'
import { sha256Bytes } from '../src/security/digest.js'
import { createTokenizer } from '../src/observation/tokenizer.js'
import { batchToken } from '../src/observation/index.js'

const proto = codec()

function captureOutlet() {
  let clock = 1000
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

function eventsOf(facts, event, predicate = () => true) {
  return facts.filter(fact => fact.event === event && predicate(fact))
}

const CONV = { conversation: true, questions: false }
const settle = (ms = 30) => new Promise(resolve => setTimeout(resolve, ms))
function live(seq, text = 'x') {
  return {
    session: { session_id: 's' },
    live: {
      attempt: { live_epoch: 'run-native', attempt_id: 'raw-attempt' },
      block_id: 'raw-block', first_seq: seq, last_seq: seq,
      delta: { kind: 'BLOCK_KIND_TEXT', text },
    },
  }
}

class Source extends SourceAdapter {
  sinks = new Map()
  closed = 0
  async readState(id) {
    return { session: { session_id: id }, history_head: 'bookmark:alpha', live_epoch: 'run-native', revision: 1, run_phase: 'IDLE', approvals: [] }
  }
  watch(id, sink) { this.sinks.set(id, sink); return { close: () => { this.closed += 1; this.sinks.delete(id) } } }
  async readHistory(id) {
    return { session: { session_id: id }, batch: { from_cursor: 'bookmark:start', to_cursor: 'bookmark:alpha', batch_id: 'record:alpha', changes: [] }, cut_cursor: 'bookmark:alpha', complete: true }
  }
}

function watchFixture(t, observation, access = null) {
  let now = 100
  const store = new StateStore({ path: ':memory:' })
  const identity = generateKeypair()
  const security = new SecurityManager({ hostId: 'h', identity: generateKeypair(), store, codec: proto, now: () => now })
  const channel = establishChannel({ hostId: 'h', deviceId: 'd', security, store, identity })
  const source = new Source()
  const resolvedAccess = access ?? {
    connected: true,
    published: [],
    async publishEvent(value) { this.published.push(value) },
    async publishLive(value) { this.published.push(value); return true },
  }
  const events = new WatchedEvents({
    hostId: 'h', store, security, adapter: source, access: resolvedAccess,
    config: {}, codec: proto, now: () => now, observation,
  })
  events.start()
  t.after(async () => { await events.stop(); store.close() })
  return { events, source, store, security, channel, access: resolvedAccess }
}

test('latency/AC-01.coalesce coalesces adjacent previews into one typed fact', async () => {
  const facts = []
  const published = []
  const buffer = new LiveBuffer(update => published.push(update), 5, 8192, coalesced => facts.push(coalesced))
  buffer.push(live(1, 'ab'))
  buffer.push(live(2, 'cd'))
  buffer.push(live(3, 'ef'))
  await settle(20)
  assert.equal(facts.length, 1)
  assert.equal(facts[0].frames, 3)
  assert.equal(facts[0].bytes, 6)
  assert.equal(facts[0].seq_start, 1)
  assert.equal(facts[0].seq_end, 3)
  assert.equal(facts[0].stream, 'raw-attempt')
  assert.equal(published.length, 1)
  assert.equal(published[0].live.delta.text, 'abcdef')
})

test('latency/AC-01.dequeue records live queue enqueue/dequeue with depth, bytes and wait', async (t) => {
  const observation = captureOutlet()
  const ctx = watchFixture(t, observation)
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  ctx.source.sinks.get('s')(live(1, 'abc'))
  await settle()
  const subscribe = eventsOf(observation.facts, 'host_subscribe')
  assert.equal(subscribe.length, 1)
  assert.equal(subscribe[0].data.outcome, 'subscribed')
  const enqueue = eventsOf(observation.facts, 'host_enqueue')
  const dequeue = eventsOf(observation.facts, 'host_dequeue')
  assert.equal(enqueue.length, 1)
  assert.equal(dequeue.length, 1)
  assert.equal(enqueue[0].data.queue_depth, 1)
  assert.equal(enqueue[0].data.queue_bytes, enqueue[0].data.bytes)
  assert.equal(enqueue[0].data.stream, 'raw-attempt')
  assert.equal(enqueue[0].data.block, 'raw-block')
  assert.ok(enqueue[0].data.bytes > 3, 'the queue measures sealed envelope bytes')
  assert.ok(dequeue[0].data.wait_ns >= 0)
})

test('latency/AC-01.batch joins one preview across adapter, coalesce, queue and publish', async (t) => {
  const observation = captureOutlet()
  const broker = new FakeBroker()
  const access = new MqttAccess({
    url: 'mqtts://broker.test:8883', hostId: 'h', token: 'token',
    connect: broker.connectFn(), observation,
  })
  access.start({})
  await access.waitForConnected(3000)
  t.after(() => access.stop())
  const ctx = watchFixture(t, observation, access)
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  // The adapter side of the same preview: one native frame, one normalized
  // interval. The Host is the only component that tokenizes, and both sides
  // share the process outlet, so the batch must match exactly.
  const latency = {
    stream: 'raw-attempt', attempt: 'raw-attempt', block: 'raw-block',
    seq_start: 1, seq_end: 1, bytes: 3,
  }
  const batch = batchToken(observation, 'raw-attempt', 1, 1)
  observation.emit('dsh_callback', { ...latency, batch, native: 'assistant-preview' })
  observation.emit('dsh_handoff', { ...latency, batch })
  ctx.source.sinks.get('s')(live(1, 'abc'))
  await settle()
  const names = ['dsh_callback', 'dsh_handoff', 'host_coalesce', 'host_enqueue', 'host_dequeue']
  const facts = names.map(name => eventsOf(observation.facts, name))
  for (const [index, name] of names.entries()) {
    assert.equal(facts[index].length, 1, name)
    assert.equal(facts[index][0].data.batch, batch, `${name} keeps the adapter batch`)
  }
  assert.ok(facts[1][0].ts <= facts[3][0].ts, 'handoff is not after host_enqueue')
  const published = eventsOf(observation.facts, 'host_publish', fact => fact.data.kind === 'event')
  assert.equal(published.length, 1)
  const publish = published[0]
  assert.equal(publish.data.request, '')
  assert.equal(publish.data.attempt, 1)
  assert.equal(publish.data.outcome, 'accepted')
  assert.equal(publish.data.batch, batch)
  assert.deepEqual(
    [publish.data.stream, publish.data.seq_start, publish.data.seq_end],
    ['raw-attempt', 1, 1],
  )
  assert.equal(publish.data.bytes, facts[3][0].data.bytes)
  assert.equal(publish.data.queue_depth, facts[3][0].data.queue_depth)
  assert.equal(publish.data.queue_bytes, facts[3][0].data.queue_bytes)
})

test('a saturated live queue reports an observation gap without blocking committed delivery', async (t) => {
  const observation = captureOutlet()
  const blocked = Promise.withResolvers()
  const ctx = watchFixture(t, observation, {
    connected: true,
    async publishLive() { await blocked.promise; return true },
    async publishEvent() {},
  })
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  for (let seq = 1; seq <= 70; seq += 1) ctx.source.sinks.get('s')(live(seq, 'x'))
  await settle(10)
  assert.ok(ctx.store.countOutbox() === 0)
  blocked.resolve()
  await settle()
  const enqueued = eventsOf(observation.facts, 'host_enqueue')
  assert.ok(enqueued.length > 0)
  assert.ok(enqueued.length <= 64)
})

test('latency/AC-01.inline records local MQTT publish attempts and results', async () => {
  const observation = captureOutlet()
  const broker = new FakeBroker()
  const access = new MqttAccess({
    url: 'mqtts://broker.test:8883', hostId: 'host-1', token: 'token',
    connect: broker.connectFn(), observation,
  })
  access.start({})
  await access.waitForConnected(3000)
  const payload = proto.encode('whalerein.v1.HostResult', {
    request_id: 'raw-request', host_id: 'host-1', device_id: 'd1', status: 'succeeded',
    result_body: new Uint8Array([1]), protocol_version: 1, relay_version: 5,
  })
  await access.publishResult(payload, { requestId: 'raw-request', expiresAt: Date.now() + 30_000 })
  const preview = proto.encode('whalerein.v1.EventEnvelope', {
    host_id: 'host-1', device_id: 'd1', channel_id: 'c1',
    nonce: randomBytes(12), ciphertext: new Uint8Array([4, 5, 6]),
    created_at_unix_ms: 1, relay_version: 5,
  })
  assert.equal(await access.publishLive(preview, { deviceId: 'd1' }), true)
  await access.stop()
  const accepted = eventsOf(observation.facts, 'host_publish', fact => fact.data.outcome === 'accepted' && fact.data.kind !== 'ready')
  assert.equal(accepted.length, 2)
  const result = accepted.find(fact => fact.data.kind === 'result')
  assert.equal(result.data.request, 'raw-request')
  assert.equal(result.data.attempt, 1)
  assert.equal(result.data.bytes, payload.length)
  const inline = accepted.find(fact => fact.data.kind === 'event')
  assert.equal(inline.data.bytes, preview.length)
})

test('a disconnected MQTT publish reports failure without touching product delivery', async () => {
  const observation = captureOutlet()
  const access = new MqttAccess({
    url: 'mqtts://broker.test:8883', hostId: 'host-1', token: 'token',
    connect: () => { throw new Error('no transport') }, observation,
  })
  const payload = proto.encode('whalerein.v1.HostResult', {
    request_id: 'raw-request', host_id: 'host-1', device_id: 'd1', status: 'succeeded',
    result_body: new Uint8Array([1]), protocol_version: 1, relay_version: 5,
  })
  await assert.rejects(access.publishResult(payload, { requestId: 'raw-request' }))
  const failed = eventsOf(observation.facts, 'host_publish', fact => fact.data.outcome === 'failed')
  assert.equal(failed.length, 1)
  assert.equal(failed[0].data.kind, 'result')
})

test('latency/AC-02.expired exposes command start and finish with the measured outcome', async () => {
  const observation = captureOutlet()
  const store = new StateStore({ path: ':memory:', now: () => 1000 })
  const adapter = new TestSource({ now: () => 1000, sessions: [{ sessionId: 's1', running: false }] })
  const service = new CommandService({
    store, adapter,
    events: { readHistory: async sessionId => ({ session: { session_id: sessionId }, batch: { from_cursor: 'a', to_cursor: 'b', batch_id: 'b', changes: [] }, cut_cursor: 'b', complete: true }) },
    hostId: 'host-1', now: () => 1000, codec: proto, observation,
    onAccepted: async () => {},
  })
  const body = new Uint8Array([1, 2, 3])
  const request = {
    request_id: 'raw-request', host_id: 'host-1', device_id: 'device-1', kind: 'REQUEST_KIND_COMMAND',
    command_id: 'cmd-1', payload_digest: sha256Bytes(body), expires_at_unix_ms: 10_000,
    protocol_version: 1, channel_id: 'channel-1', body,
  }
  const result = await service.handle({ request, payload: { send_message: { session_id: 's1', text: 'hello' } } })
  assert.equal(result.status, 'succeeded')
  const stages = eventsOf(observation.facts, 'host_command')
  assert.deepEqual(stages.map(stage => stage.data.stage), ['start', 'finish'])
  assert.equal(stages[0].data.request, 'raw-request')
  assert.equal(stages[1].data.outcome, 'ok')
  const rejected = await service.handle({
    request: { ...request, command_id: 'cmd-2', payload_digest: sha256Bytes(body), body },
    payload: { unknown_operation: true },
  })
  assert.equal(rejected.errorCode, 'invalid_argument')
  const secondFinish = eventsOf(observation.facts, 'host_command', fact => fact.data.stage === 'finish')[1]
  assert.equal(secondFinish.data.outcome, 'error')
  // An expired request is rejected before native admission. No start/finish
  // pair is invented for a command stage that never ran.
  const before = eventsOf(observation.facts, 'host_command').length
  const expired = await service.handle({
    request: { ...request, command_id: 'cmd-3', expires_at_unix_ms: 500 },
    payload: { send_message: { session_id: 's1', text: 'late' } },
  })
  assert.equal(expired.errorCode, 'expired')
  assert.equal(eventsOf(observation.facts, 'host_command').length, before)
})
