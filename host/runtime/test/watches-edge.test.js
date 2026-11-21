import test from 'node:test'
import assert from 'node:assert/strict'
import { SourceAdapter } from '../src/contracts/source.js'
import { WatchedEvents } from '../src/events/watches.js'
import { StateStore } from '../src/state/store.js'
import { SecurityManager } from '../src/security/manager.js'
import { generateKeypair } from '../src/security/noise.js'
import { codec } from '../src/protocol.js'
import { establishChannel, openEventEnvelope } from './support/channel.js'

// Focused branch coverage for watch admission, reconciliation, bounded replay
// memory, and oversize/outbox pressure. The fixture is intentionally small so
// each case exercises one real source error or transport pressure path.

class TrackedSource extends SourceAdapter {
  sinks = new Map()
  closed = 0
  stateCalls = 0
  stampCalls = 0
  stateError = null
  stampError = null
  stateValue = null
  descriptor = null
  async readState(id) {
    this.stateCalls++
    if (this.stateError) throw this.stateError
    return this.stateValue ?? { session: { session_id: id }, history_head: 'bookmark:alpha', live_epoch: 'run-native', revision: 1, run_phase: 'IDLE', approvals: [] }
  }
  watch(id, sink) { this.sinks.set(id, sink); return { close: () => { this.closed++; this.sinks.delete(id) } } }
  async watch_throw() { throw new Error('unused') }
  async readHistory(id) { return { session: { session_id: id }, batch: { from_cursor: 'bookmark:start', to_cursor: 'bookmark:alpha', batch_id: 'record:alpha', changes: [] }, cut_cursor: 'bookmark:alpha', complete: true } }
  async readQuestionStamp() {
    this.stampCalls++
    if (this.stampError) throw this.stampError
    return this.descriptor ?? { availability: 'available', stamp: { observation_epoch: 'epoch-one', revision: 1 } }
  }
  async listQuestionPage() { return { batches: [], next_cursor: '', stamp: { observation_epoch: 'epoch-one', revision: 1 } } }
}

function fixture({ source = new TrackedSource(), access, config = {}, logger } = {}) {
  let now = 100
  const store = new StateStore({ path: ':memory:' })
  const security = new SecurityManager({ hostId: 'h', identity: generateKeypair(), store, codec: codec(), now: () => now })
  const channel = establishChannel({ hostId: 'h', deviceId: 'd', security, store })
  const published = []
  const transport = access ?? {
    connected: true, published,
    async publishEvent(bytes) { published.push(bytes) },
    async publishLive(bytes) { published.push(bytes); return true },
  }
  const events = new WatchedEvents({
    hostId: 'h', store, security, adapter: source, access: transport,
    codec: codec(), now: () => now, config, logger,
  })
  events.start()
  return {
    events, source, store, security, channel, access: transport,
    advance(ms) { now += ms },
  }
}

const CONV = { conversation: true, questions: false }
const QUESTIONS = { conversation: false, questions: true }
const settle = () => new Promise(resolve => setTimeout(resolve, 30))
const tick = () => new Promise(resolve => setImmediate(resolve))

function live(seq = 1, overrides = {}) {
  return {
    session: { session_id: 's' },
    live: {
      attempt: { live_epoch: 'run-native', attempt_id: 'generation' },
      block_id: 'visible', first_seq: seq, last_seq: seq,
      delta: { kind: 'BLOCK_KIND_TEXT', text: `x${seq}` },
      ...overrides,
    },
  }
}

function payloads(ctx) {
  const out = []
  for (const bytes of ctx.access.published) {
    const outer = codec().decode('whalerein.v1.EventEnvelope', bytes)
    out.push(openEventEnvelope({ envelope: outer, channel: ctx.channel, hostId: 'h', deviceId: 'd' }))
  }
  return out
}

function updates(ctx) {
  return payloads(ctx).map(payload => payload.update)
}

test('watch admission rejects invalid interests, aborted signals, wrong channels, and synchronous registration failures', async () => {
  const ctx = fixture()
  try {
    await assert.rejects(ctx.events.watch('d', ctx.channel.channelId, 's', undefined, { conversation: true }), error => error.code === 'invalid_argument')
    await assert.rejects(ctx.events.watch('d', ctx.channel.channelId, 's', undefined, []), error => error.code === 'invalid_argument')
    const aborted = new AbortController()
    aborted.abort('caller_gone')
    await assert.rejects(ctx.events.watch('d', ctx.channel.channelId, 's', aborted.signal, CONV), error => error.code === 'unavailable')
    await assert.rejects(ctx.events.watch('other', ctx.channel.channelId, 's', undefined, CONV), error => error.code === 'unauthorized')
    ctx.source.watch = () => { throw new Error('synchronous native failure') }
    await assert.rejects(ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV), /synchronous native failure/)
    assert.equal(ctx.events.describe().watches, 0)
    assert.equal(ctx.events.describe().sessions, 0)
    assert.equal(ctx.source.closed, 0)
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('invalid and fatal question stamps are contained or close with the session reason they prove', async () => {
  const logger = { warns: [], warn(event, fields) { this.warns.push({ event, fields }) }, info() {} }
  const ctx = fixture({ source: new TrackedSource(), logger })
  try {
    // A structurally invalid descriptor is a contained unavailability, not a
    // readiness failure or a fabricated empty collection.
    ctx.source.descriptor = {}
    const ready = await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, QUESTIONS)
    assert.equal(ready.questions.availability, 'QUESTION_AVAILABILITY_UNAVAILABLE')
    assert.equal(ready.questions.unavailable_code, 'unavailable')

    // A fatal code learned during reconciliation proves the session is gone:
    // every attached recipient gets a Host-authored close with the cause.
    const expired = new Error('expired')
    expired.code = 'expired'
    ctx.source.stampError = expired
    ctx.source.sinks.get('s')({ session: { session_id: 's' }, questions_changed: true })
    await settle()
    const closed = payloads(ctx).filter(payload => payload.watch_closed)
    assert.equal(closed.length, 1)
    assert.equal(closed[0].watch_closed.reason, 'session_expired')
    assert.equal(ctx.events.describe().watches, 0)
    assert.equal(ctx.events.describe().sessions, 0)
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('a source state read that contradicts the watch scope and one that fails transiently never publish', async () => {
  const logger = { warns: [], warn(event, fields) { this.warns.push({ event, fields }) }, info() {} }
  const ctx = fixture({ source: new TrackedSource(), logger })
  try {
    await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
    ctx.source.stateValue = { session: { session_id: 'other' }, history_head: 'bookmark:alpha', live_epoch: 'run-native' }
    ctx.source.sinks.get('s')({ session: { session_id: 's' }, state_changed: true })
    await settle()
    assert.equal(ctx.access.published.length, 0)
    assert.ok(logger.warns.some(entry => entry.event === 'source_state_unavailable' && entry.fields.code === 'invalid_argument'))

    ctx.source.stateValue = null
    const unavailable = new Error('temporary')
    unavailable.code = 'unavailable'
    ctx.source.stateError = unavailable
    ctx.source.sinks.get('s')({ session: { session_id: 's' }, state_changed: true })
    await settle()
    assert.equal(ctx.access.published.length, 0)
    assert.ok(logger.warns.some(entry => entry.event === 'source_state_unavailable' && entry.fields.code === 'unavailable'))
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('conversation-only and questions-only observers ignore unrelated change hints', async () => {
  const ctx = fixture({ source: new TrackedSource() })
  try {
    await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
    // A conversation-only source has no question facet to reconcile.
    ctx.source.sinks.get('s')({ session: { session_id: 's' }, questions_changed: true })
    await settle()
    assert.equal(ctx.source.stampCalls, 0, 'no question inventory read for a conversation-only watch')

    const questionChannel = establishChannel({ hostId: 'h', deviceId: 'question-device', security: ctx.security, store: ctx.store })
    const conversationCalls = ctx.source.stateCalls
    await ctx.events.watch('question-device', questionChannel.channelId, 'questions', undefined, QUESTIONS)
    // A questions-only source has no conversation facet, so a state hint is
    // acknowledged without an authoritative conversation read.
    ctx.source.sinks.get('questions')({ session: { session_id: 'questions' }, state_changed: true })
    await settle()
    assert.equal(ctx.source.stateCalls, conversationCalls, 'no conversation read for a questions-only watch')
    assert.equal(ctx.source.stampCalls, 1, 'the questions baseline was read once')
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('duplicate starts, duplicate previews, and previews after retirement are dropped while a projection switch forgets old attempts', async () => {
  const ctx = fixture()
  try {
    await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
    const sink = ctx.source.sinks.get('s')
    sink({ session: { session_id: 's' }, live: { attempt: { live_epoch: 'run-native', attempt_id: 'generation' }, first_seq: 1, last_seq: 1, started: {} } })
    await settle()
    sink({ session: { session_id: 's' }, live: { attempt: { live_epoch: 'run-native', attempt_id: 'generation' }, first_seq: 1, last_seq: 1, started: {} } })
    await settle()
    assert.equal(updates(ctx).filter(update => update.live).length, 1, 'a replayed start for the same sequence is dropped')

    sink(live(1))
    await settle()
    sink(live(1))
    await settle()
    assert.equal(updates(ctx).filter(update => update.live).length, 2, 'the same block sequence is delivered once')

    sink({ session: { session_id: 's' }, live: { attempt: { live_epoch: 'run-native', attempt_id: 'generation' }, block_id: 'visible', first_seq: 2, last_seq: 2, ended: { outcome: 'COMMITTED' } } })
    await settle()
    const afterEnd = updates(ctx).length
    sink(live(3))
    await settle()
    assert.equal(updates(ctx).length, afterEnd, 'a retired attempt cannot reopen after AttemptEnded')

    // A projection switch invalidates remembered attempt identity. The exact
    // old sequence is deliverable again only under the new projection.
    sink({ session: { session_id: 's' }, state: { projection_id: 'projection-two' } })
    await settle()
    const afterProjection = updates(ctx).length
    sink(live(1))
    await settle()
    assert.equal(updates(ctx).length, afterProjection + 1)
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('bounded committed replay memory eventually forgets old batches instead of growing without limit', async () => {
  const ctx = fixture({ config: { outboxPerDeviceLimit: 10_000, outboxTotalLimit: 10_000 } })
  try {
    await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
    const sink = ctx.source.sinks.get('s')
    const batch = index => ({
      session: { session_id: 's' },
      history: {
        from_cursor: `bookmark:${index - 1}`, to_cursor: `bookmark:${index}`,
        batch_id: `batch-${index}`,
        changes: [{ origin: {}, message: { message_id: `m-${index}`, role: 'ASSISTANT', blocks: [{ block_id: 't', text: { text: 'answer' } }] } }],
      },
    })
    for (let index = 1; index <= 600; index++) sink(batch(index))
    await settle()
    assert.equal(updates(ctx).filter(update => update.history).length, 600)

    // The earliest batch has been evicted from the bounded seen-set, so the
    // retry is a fresh commit rather than a swallowed duplicate.
    sink(batch(1))
    await settle()
    assert.equal(updates(ctx).filter(update => update.history).length, 601)
    // The newest batch is still remembered and suppressed.
    sink(batch(600))
    await settle()
    assert.equal(updates(ctx).filter(update => update.history).length, 601)
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('oversized events emit at most one reset per channel and outbox pressure marks snapshot repair without amplification', async () => {
  const ctx = fixture({ config: { outboxPerDeviceLimit: 1, outboxTotalLimit: 1 } })
  try {
    await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
    ctx.security.getChannelReceive = () => ({ budget: { envelope_bytes: 1024 } })
    const sink = ctx.source.sinks.get('s')
    const large = (batchId) => ({ session: { session_id: 's' }, history: {
      from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: batchId,
      changes: [{ message: { message_id: `m-${batchId}`, role: 'ASSISTANT', blocks: [{ block_id: 't', text: { text: 'x'.repeat(4096) } }] } }],
    } })
    sink(large('first'))
    await settle()
    const first = updates(ctx)
    assert.equal(first.length, 1)
    assert.equal(first[0].reset.reason, 'snapshot_required')

    // The second oversize is marked dropped but cannot add another durable or
    // live reset for the same channel.
    sink(large('second'))
    await settle()
    assert.equal(updates(ctx).length, 1)
    assert.equal(ctx.events.describe().snapshotRequired, 1)
    assert.equal(ctx.store.countOutbox(), 1, 'only the single reset is durable')
    assert.equal(ctx.events.describe().pending, 1)
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('a receive budget smaller than the reset envelope records snapshot repair without writing any frame', async () => {
  const ctx = fixture()
  try {
    await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
    ctx.security.getChannelReceive = () => ({ budget: { envelope_bytes: 64 } })
    ctx.source.sinks.get('s')({ session: { session_id: 's' }, history: {
      from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'tiny',
      changes: [{ message: { message_id: 'm1', role: 'ASSISTANT', blocks: [{ block_id: 't', text: { text: 'x'.repeat(512) } }] } }],
    } })
    await settle()
    assert.equal(ctx.access.published.length, 0, 'even the reset is too large for the advertised route')
    assert.equal(ctx.store.countOutbox(), 0)
    assert.equal(ctx.events.describe().snapshotRequired, 1)
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('a terminal close that cannot be persisted still removes the watch and marks the channel for snapshot repair', async () => {
  const ctx = fixture({ config: { outboxPerDeviceLimit: 0 } })
  try {
    const watch = await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
    const gone = new Error('gone')
    gone.code = 'forbidden'
    ctx.source.readState = async () => { throw gone }
    await assert.rejects(ctx.events.refresh('d', ctx.channel.channelId, [watch.watch_id]), error => error.code === 'not_found')
    await settle()
    assert.equal(ctx.events.describe().watches, 0)
    assert.equal(ctx.store.countOutbox(), 0)
    assert.equal(ctx.events.describe().snapshotRequired, 1)
    assert.equal(ctx.events.describe().droppedDevices, 0, 'a close-only drop does not claim a canonical update was lost')
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('flushOutbox discards rows for a channel that is no longer authenticated', async () => {
  const ctx = fixture({ access: { connected: false, published: [], async publishEvent(bytes) { this.published.push(bytes) } } })
  try {
    await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
    ctx.source.sinks.get('s')({ session: { session_id: 's' }, history: {
      from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'offline',
      changes: [],
    } })
    await settle()
    assert.equal(ctx.store.countOutbox(), 1)
    ctx.access.connected = true
    ctx.security.clearChannels()
    assert.equal(await ctx.events.flushOutbox(), 0)
    assert.equal(ctx.store.countOutbox(), 0)
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('the bounded per-connection live queue drops overflow instead of queueing unbounded work', async () => {
  let calls = 0
  const blocked = new Promise(() => {})
  const ctx = fixture({ access: { connected: true, async publishLive() { calls++; await blocked } } })
  try {
    await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
    const sink = ctx.source.sinks.get('s')
    for (let index = 1; index <= 70; index++) sink(live(index, { block_id: `block-${index}` }))
    await settle()
    assert.equal(calls, 64, 'the live queue admits its packet bound and drops the rest')
    assert.equal(ctx.store.countOutbox(), 0, 'a dropped preview is never durable')
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('a committed publication is not sent after its watch is removed while queued', async () => {
  const release = {}
  const blocked = new Promise(resolve => { release.resolve = resolve })
  const published = []
  const ctx = fixture({ access: { connected: true, async publishEvent(bytes) { published.push(bytes); await blocked } } })
  try {
    const ready = await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
    const sink = ctx.source.sinks.get('s')
    sink({ session: { session_id: 's' }, history: { from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'first', changes: [] } })
    await tick()
    sink({ session: { session_id: 's' }, history: { from_cursor: 'bookmark:b', to_cursor: 'bookmark:c', batch_id: 'second', changes: [] } })
    await tick()
    const before = published.length
    ctx.events.unwatch('d', ctx.channel.channelId, ready.watch_id)
    release.resolve()
    await settle()
    assert.equal(published.length, before, 'the queued committed frame does not publish for a removed watch')
    assert.equal(ctx.store.countOutbox(), 1, 'its durable copy remains for reconnect snapshots')
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('an authoritative state notification is applied and published once', async () => {
  const ctx = fixture()
  try {
    await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
    ctx.source.sinks.get('s')({ session: { session_id: 's' }, state_changed: true })
    await settle()
    const state = updates(ctx).filter(update => update.state)
    assert.equal(state.length, 1)
    assert.equal(state[0].state.session.host_id, 'h')
    assert.equal(state[0].state.session.session_id, 's')
    assert.equal(ctx.source.stateCalls, 2, 'the baseline and the authoritative notification each read state')
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('a malformed committed change is contained and later valid delivery still works', async () => {
  const logger = { warns: [], warn(event, fields) { this.warns.push({ event, fields }) }, info() {} }
  const ctx = fixture({ logger })
  try {
    await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
    const sink = ctx.source.sinks.get('s')
    const message = { message_id: 'm1', role: 'ASSISTANT', blocks: [{ block_id: 't', text: { text: 'answer' } }] }
    sink({ session: { session_id: 's' }, history: {
      from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'bad-origin',
      changes: [{ message, get origin() { throw new Error('malformed origin') } }],
    } })
    await settle()
    assert.equal(ctx.access.published.length, 0)
    assert.ok(logger.warns.some(entry => entry.event === 'source_update_rejected' && entry.fields.reason === 'stream'))

    sink({ session: { session_id: 's' }, history: {
      from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'recovered', changes: [],
    } })
    await settle()
    assert.equal(updates(ctx).filter(update => update.history).length, 1, 'the watch survives a malformed change')
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})

test('an oversize event whose reset cannot enter the outbox is counted as dropped, not retried', async () => {
  const ctx = fixture({ config: { outboxPerDeviceLimit: 0 } })
  try {
    await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
    ctx.security.getChannelReceive = () => ({ budget: { envelope_bytes: 1024 } })
    ctx.source.sinks.get('s')({ session: { session_id: 's' }, history: {
      from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'no-room',
      changes: [{ message: { message_id: 'm1', role: 'ASSISTANT', blocks: [{ block_id: 't', text: { text: 'x'.repeat(4096) } }] } }],
    } })
    await settle()
    assert.equal(ctx.access.published.length, 0)
    assert.equal(ctx.store.countOutbox(), 0)
    assert.equal(ctx.events.describe().snapshotRequired, 1)
    assert.equal(ctx.events.describe().outboxDrops, 1)
  } finally {
    await ctx.events.stop(); ctx.store.close()
  }
})
