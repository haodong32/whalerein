import test from 'node:test'
import assert from 'node:assert/strict'
import { SourceAdapter } from '../src/contracts/source.js'
import { WatchedEvents } from '../src/events/watches.js'
import { StateStore } from '../src/state/store.js'
import { SecurityManager } from '../src/security/manager.js'
import { generateKeypair } from '../src/security/noise.js'
import { codec } from '../src/protocol.js'
import { establishChannel, openEventEnvelope } from './support/channel.js'

// This fixture uses native string bookmarks and no DSH names or integer seqs.
class Source extends SourceAdapter {
  sinks=new Map()
  closed=0
  async readState(id){return {session:{session_id:id},history_head:'bookmark:alpha',live_epoch:'run-native',revision:1,run_phase:'IDLE',approvals:[]}}
  watch(id,sink){this.sinks.set(id,sink);return {close:()=>{this.closed++;this.sinks.delete(id)}}}
  async readHistory(id){return {session:{session_id:id},batch:{from_cursor:'bookmark:start',to_cursor:'bookmark:alpha',batch_id:'record:alpha',changes:[]},cut_cursor:'bookmark:alpha',complete:true}}
}
function fixture(access={connected:true,published:[],async publishEvent(value){this.published.push(value)}},source=new Source()) {
  let now=100
  const store=new StateStore({path:':memory:'})
  const security=new SecurityManager({hostId:'h',identity:generateKeypair(),store,codec:codec(),now:()=>now})
  const channel=establishChannel({hostId:'h',deviceId:'d',security,store})
  const events=new WatchedEvents({hostId:'h',store,security,adapter:source,access,codec:codec(),now:()=>now})
  events.start()
  return {events,source,store,security,channel,access,advance:ms=>{now+=ms}}
}
const CONV={conversation:true,questions:false}
const settle=()=>new Promise(r=>setTimeout(r,30))
const tick=()=>new Promise(r=>setImmediate(r))

test('connected devices require a current observation and are deduplicated by Device ID', async (t) => {
  const ctx = fixture()
  t.after(async () => { await ctx.events.stop(); ctx.store.close() })
  assert.deepEqual(ctx.events.observingDeviceIds(), [], 'a paired channel alone cannot prove a live observation')
  const first = await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  await ctx.events.watch('d', ctx.channel.channelId, 'other-session', undefined, CONV)
  const other = establishChannel({ hostId: 'h', deviceId: 'second', security: ctx.security, store: ctx.store })
  ctx.advance(100)
  await ctx.events.watch('second', other.channelId, 's', undefined, CONV)
  assert.deepEqual(ctx.events.observingDeviceIds(), ['d', 'second'])
  ctx.events.unwatch('d', ctx.channel.channelId, first.watch_id)
  assert.deepEqual(ctx.events.observingDeviceIds(), ['d', 'second'])
  ctx.advance(first.lease_ms - 100)
  assert.deepEqual(ctx.events.observingDeviceIds(), ['second'])
  ctx.events.onChannelsCleared([{ deviceId: 'second', channelId: other.channelId }])
  assert.deepEqual(ctx.events.observingDeviceIds(), [])
})

test('an abrupt watcher exit expires only its lease while the Host and another watcher stay connected', async (t) => {
  const ctx = fixture()
  t.after(async () => { await ctx.events.stop(); ctx.store.close() })
  const other = establishChannel({ hostId: 'h', deviceId: 'second', security: ctx.security, store: ctx.store })
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  ctx.advance(100)
  await ctx.events.watch('second', other.channelId, 's', undefined, CONV)
  assert.equal(ctx.access.connected, true, 'the Host transport stays connected')
  assert.deepEqual(ctx.events.observingDeviceIds(), ['d', 'second'])
  // The first CLI exits without unwatch. Only its lease lapses.
  ctx.advance(29_900)
  assert.deepEqual(ctx.events.observingDeviceIds(), ['second'])
  assert.equal(ctx.access.connected, true, 'lease expiry does not disconnect the Host')
  assert.equal(ctx.events.describe().watches, 1, 'the other watcher still holds an active observation')
  ctx.advance(200)
  assert.deepEqual(ctx.events.observingDeviceIds(), [], 'every expired lease leaves no active observation')
})

function deferred() {
  const value = {}
  value.promise = new Promise((resolve, reject) => { value.resolve = resolve; value.reject = reject })
  return value
}
function bounded(promise, message, timeoutMs = 5000) {
  let timer
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), timeoutMs)
  })
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer))
}
function live(seq=1){return {session:{session_id:'s'},live:{attempt:{live_epoch:'run-native',attempt_id:'generation'},block_id:'visible',first_seq:seq,last_seq:seq,delta:{text:'x'}}}}

test('watch registration precedes the baseline, shares native watches, and delivers only to interested recipients',async()=>{
 const ctx=fixture()
 const ready=await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,CONV)
 assert.equal(ready.state.session.host_id,'h')
 const same=await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,CONV)
 assert.equal(same.watch_id,ready.watch_id)
 ctx.source.sinks.get('s')(live())
 await settle()
 const outer=codec().decode('whalerein.v1.EventEnvelope',ctx.access.published[0])
 const payload=openEventEnvelope({envelope:outer,channel:ctx.channel,hostId:'h',deviceId:'d'})
 assert.equal(payload.update.watch_id,ready.watch_id)
 assert.equal(payload.update.live.delta.text.length,1)
 const page=await ctx.events.readHistory('s',{})
 assert.equal(page.session.host_id,'h')
 await ctx.events.refresh('d',ctx.channel.channelId,[ready.watch_id])
 assert.throws(()=>ctx.events.unwatch('other',ctx.channel.channelId,ready.watch_id),e=>e.code==='expired')
 ctx.events.unwatch('d',ctx.channel.channelId,ready.watch_id)
 assert.equal(ctx.source.closed,1)
 await ctx.events.stop()
})

test('a stalled preview does not block committed updates, and a changed channel drops its watch',async()=>{
 let release,calls=0
 const blocked=new Promise(r=>{release=r})
 const ctx=fixture({connected:true,async publishEvent(){if(++calls===1)await blocked}})
 await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,CONV)
 ctx.source.sinks.get('s')(live())
 await settle()
 ctx.source.sinks.get('s')({session:{session_id:'s'},history:{from_cursor:'bookmark:a',to_cursor:'bookmark:b',batch_id:'b',changes:[]}})
 await settle()
 assert.equal(calls,2)
 assert.equal(ctx.store.countOutbox(),0)
 ctx.events.onChannelsCleared([{ deviceId: 'd', channelId: ctx.channel.channelId }])
 assert.equal(ctx.events.describe().watches,0)
 release();await ctx.events.stop()
})

test('watch leases and source resource bounds reject invalid or expired selections without revoking identity',async()=>{
 const ctx=fixture(),ready=await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,CONV)
 await assert.rejects(ctx.events.watch('other',ctx.channel.channelId,'s',undefined,CONV),e=>e.code==='unauthorized')
 await assert.rejects(ctx.events.refresh('d',ctx.channel.channelId,[ready.watch_id,ready.watch_id]),e=>e.code==='invalid_argument')
 ctx.advance(30001)
 await assert.rejects(ctx.events.refresh('d',ctx.channel.channelId,[ready.watch_id]),e=>e.code==='expired')
 assert.ok(ctx.store.getPeerKey('d'))
 await ctx.events.stop()
})

test('a slow recipient cannot block another device subscribed to the same source', async (t) => {
  let release
  const blocked = new Promise(resolve => { release = resolve })
  const published = []
  const ctx = fixture({ connected: true, async publishEvent(bytes) {
    const envelope = codec().decode('whalerein.v1.EventEnvelope', bytes)
    published.push(envelope.device_id)
    if (envelope.device_id === 'd') await blocked
  } })
  t.after(async () => { release(); await ctx.events.stop() })
  const other = establishChannel({ hostId: 'h', deviceId: 'second', security: ctx.security, store: ctx.store })
  await ctx.events.watch('d', ctx.channel.channelId, 's',undefined,CONV)
  await ctx.events.watch('second', other.channelId, 's',undefined,CONV)
  ctx.source.sinks.get('s')(live())
  await settle()
  assert.deepEqual([...published].sort(), ['d', 'second'])
})

test('when a source emits a different session scope, no recipient receives that update', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  await ctx.events.watch('d', ctx.channel.channelId, 's',undefined,CONV)
  ctx.source.sinks.get('s')({ ...live(), session: { session_id: 'wrong' } })
  await settle()
  assert.equal(ctx.access.published.length, 0)
  ctx.source.sinks.get('s')(live())
  await settle()
  assert.equal(ctx.access.published.length, 1, 'A rejected scope must not disable later valid delivery')
})

test('when readHistory returns a different session scope, the host rejects invalid_argument', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  ctx.source.readHistory = async () => ({ session: { session_id: 'wrong' } })
  await assert.rejects(ctx.events.readHistory('s', {}), error => error.code === 'invalid_argument')
})

test('while Access is offline, a committed publication stays queued and an unsuccessful flush retains it', async (t) => {
  const ctx = fixture({ connected: false, async publishEvent() { throw new Error('offline') } })
  t.after(() => ctx.events.stop())
  await ctx.events.watch('d', ctx.channel.channelId, 's',undefined,CONV)
  ctx.source.sinks.get('s')({ session: { session_id: 's' }, history: { from_cursor: 'a', to_cursor: 'b', batch_id: 'b', changes: [] } })
  assert.equal(ctx.store.countOutbox(), 1)
  assert.equal(await ctx.events.flushOutbox(), 0)
  assert.equal(ctx.store.countOutbox(), 1)
})

test('when refresh exceeds eight IDs or watch admission exceeds eight sessions, the host rejects without removing valid watches', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  const ready = await ctx.events.watch('d', ctx.channel.channelId, 's',undefined,CONV)
  await assert.rejects(ctx.events.refresh('d', ctx.channel.channelId, Array(9).fill(ready.watch_id)), error => error.code === 'invalid_argument')
  for (let index = 0; index < 7; index++) await ctx.events.watch('d', ctx.channel.channelId, `other-${index}`,undefined,CONV)
  await assert.rejects(ctx.events.watch('d', ctx.channel.channelId, 'limit',undefined,CONV), error => error.code === 'rate_limited')
  assert.equal(ctx.events.describe().watches, 8)
  const refreshed = await ctx.events.refresh('d', ctx.channel.channelId, [ready.watch_id])
  assert.ok(refreshed)
  assert.ok(ctx.store.getPeerKey('d'))
})

test('when its channel has been cleared, refreshing an existing watch rejects expired without revoking the peer', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  const ready = await ctx.events.watch('d', ctx.channel.channelId, 's',undefined,CONV)
  ctx.security.clearChannels()
  await assert.rejects(ctx.events.refresh('d', ctx.channel.channelId, [ready.watch_id]), error => error.code === 'expired')
  assert.ok(ctx.store.getPeerKey('d'))
})

test('when channel replacement cancels watch installation, its eventual handle closes', async()=>{
 const ctx=fixture()
 try {
  const pending=ctx.events.watch('d',ctx.channel.channelId,'s',undefined,CONV)
  ctx.events.onChannelsCleared([{ deviceId: 'd', channelId: ctx.channel.channelId }])
  await assert.rejects(pending,error=>error.code==='expired')
  assert.equal(ctx.source.closed,1)
  assert.equal(ctx.events.describe().sessions,0)
 } finally {await ctx.events.stop();ctx.store.close()}
})

test('channel lifecycle aborts watch setup after source-state precheck', async () => {
 const ctx=fixture()
 let release,entered,seenSignal
 const barrier=new Promise(resolve=>{release=resolve})
 const started=new Promise(resolve=>{entered=resolve})
 const original=ctx.source.readState.bind(ctx.source)
 ctx.source.readState=async(id,{signal}={})=>{seenSignal=signal;entered();await barrier;return original(id)}
 const abort=new AbortController()
 const pending=ctx.events.watch('d',ctx.channel.channelId,'s',abort.signal,CONV)
 await started
 abort.abort('unavailable')
 assert.equal(seenSignal.aborted,true)
 release()
 await assert.rejects(pending,error=>error.code==='unavailable')
 assert.equal(ctx.events.describe().watches,0)
 assert.equal(ctx.source.closed,1)
 await ctx.events.stop()
})

test('channel lifecycle stops shared watch refresh only after the last recipient leaves', async () => {
 const ctx=fixture()
 const other=establishChannel({hostId:'h',deviceId:'second',security:ctx.security,store:ctx.store})
 await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,CONV)
 await ctx.events.watch('second',other.channelId,'s',undefined,CONV)
 let entered,release,seenSignal
 const barrier=new Promise(resolve=>{release=resolve})
 const started=new Promise(resolve=>{entered=resolve})
 const original=ctx.source.readState.bind(ctx.source)
 ctx.source.readState=async(id,{signal}={})=>{seenSignal=signal;entered();await barrier;return original(id)}
 ctx.source.sinks.get('s')({session:{session_id:'s'},state_changed:true})
 await started
 ctx.events.onChannelsCleared([{ deviceId: 'd', channelId: ctx.channel.channelId }])
 assert.equal(seenSignal.aborted,false)
 ctx.events.onChannelsCleared([{ deviceId: 'second', channelId: other.channelId }])
 assert.equal(seenSignal.aborted,true)
 release()
 await settle()
 assert.equal(ctx.access.published.length,0)
 assert.equal(ctx.events.describe().watches,0)
 await ctx.events.stop()
})

test('a rejected shared source registration fails every attached observer and a retry restores both', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  const second = establishChannel({ hostId: 'h', deviceId: 'second', security: ctx.security, store: ctx.store })
  const originalWatch = ctx.source.watch.bind(ctx.source)
  const registration = deferred()
  const started = deferred()
  let attempts = 0
  let successfulRegistrations = 0
  ctx.source.watch = (id, sink) => {
    attempts += 1
    if (attempts === 1) {
      started.resolve()
      return registration.promise
    }
    successfulRegistrations += 1
    return originalWatch(id, sink)
  }

  const first = ctx.events.watch('d', ctx.channel.channelId, 's',undefined,CONV)
  await started.promise
  let secondState = 'pending'
  const secondWatch = ctx.events.watch('second', second.channelId, 's',undefined,CONV)
  secondWatch.then(() => { secondState = 'ready' }, () => { secondState = 'failed' })
  await tick()
  // The shared source is published before registration resolves, but the
  // second observer must not report a ready watch without a live subscription.
  assert.equal(secondState, 'pending')

  registration.reject(new Error('native registration failed'))
  await assert.rejects(first, /native registration failed/)
  await assert.rejects(secondWatch, /native registration failed/)
  assert.equal(secondState, 'failed')
  assert.equal(ctx.events.describe().sessions, 0, 'a failed registration cannot retain a shared source')
  assert.equal(ctx.events.describe().watches, 0)
  assert.equal(ctx.source.sinks.size, 0)
  assert.equal(successfulRegistrations, 0)

  const readyA = await ctx.events.watch('d', ctx.channel.channelId, 's',undefined,CONV)
  const readyB = await ctx.events.watch('second', second.channelId, 's',undefined,CONV)
  assert.equal(successfulRegistrations, 1, 'the retry registers one shared native source')
  const delivered = deferred()
  const awaiting = new Set(['d', 'second'])
  ctx.access.publishEvent = async function (bytes) {
    ctx.access.published.push(bytes)
    const outer = codec().decode('whalerein.v1.EventEnvelope', bytes)
    awaiting.delete(outer.device_id)
    if (awaiting.size === 0) delivered.resolve()
  }
  ctx.source.sinks.get('s')(live())
  await bounded(delivered.promise, 'both intended observers did not receive the retried live update')
  const updates = new Map()
  for (const envelope of ctx.access.published) {
    const outer = codec().decode('whalerein.v1.EventEnvelope', envelope)
    const channel = outer.device_id === 'd' ? ctx.channel : second
    const payload = openEventEnvelope({ envelope: outer, channel, hostId: 'h', deviceId: outer.device_id })
    updates.set(outer.device_id, payload.update)
  }
  assert.equal(updates.size, 2, 'both intended observers receive the live update')
  assert.equal(updates.get('d').watch_id, readyA.watch_id)
  assert.equal(updates.get('second').watch_id, readyB.watch_id)
  assert.equal(updates.get('d').live.delta.text, 'x')
  assert.equal(updates.get('second').live.delta.text, 'x')
})

test('a stalled committed publication for one recipient does not block another recipient and later releases in order', async (t) => {
  const ctx = fixture()
  t.after(async () => { releaseLive.resolve(); releaseCommitted.resolve(); await ctx.events.stop() })
  const second = establishChannel({ hostId: 'h', deviceId: 'second', security: ctx.security, store: ctx.store })
  const channels = new Map([['d', ctx.channel], ['second', second]])
  const releaseLive = deferred()
  const releaseCommitted = deferred()
  const bReady = deferred()
  const aReady = deferred()
  const published = []
  let blockedLive = false
  let blockedCommitted = false
  ctx.access.publishEvent = async function (bytes) {
    const outer = codec().decode('whalerein.v1.EventEnvelope', bytes)
    const payload = openEventEnvelope({ envelope: outer, channel: channels.get(outer.device_id), hostId: 'h', deviceId: outer.device_id })
    const update = payload.update
    const entry = { deviceId: outer.device_id, kind: update.live ? 'live' : 'committed',
      batchId: update.history?.batch_id ?? null, watchId: update.watch_id }
    published.push(entry)
    if (outer.device_id === 'd' && update.live && !blockedLive) {
      blockedLive = true
      await releaseLive.promise
    }
    if (outer.device_id === 'd' && !update.live && !blockedCommitted) {
      blockedCommitted = true
      await releaseCommitted.promise
    }
    const committed = published.filter(item => item.deviceId === 'second' && item.kind === 'committed')
    if (committed.length === 3) bReady.resolve()
    const aCommitted = published.filter(item => item.deviceId === 'd' && item.kind === 'committed')
    if (aCommitted.length === 3 && published.some(item => item.deviceId === 'd' && item.kind === 'live')) aReady.resolve()
  }

  await ctx.events.watch('d', ctx.channel.channelId, 's',undefined,CONV)
  await ctx.events.watch('second', second.channelId, 's',undefined,CONV)

  // Live traffic is stalled for A first, then A's first committed publication.
  // B must keep advancing on its own committed lane.
  ctx.source.sinks.get('s')({ session: { session_id: 's' }, live: {
    attempt: { live_epoch: 'run-native', attempt_id: 'generation' }, first_seq: 1, last_seq: 1, started: {},
  } })
  await tick()
  for (const seq of [1, 2, 3]) {
    ctx.source.sinks.get('s')({ session: { session_id: 's' }, history: {
      from_cursor: `bookmark:${seq - 1}`, to_cursor: `bookmark:${seq}`, batch_id: `batch-${seq}`, changes: [],
    } })
  }

  await bounded(bReady.promise, 'B did not receive all committed updates while A was stalled')
  assert.equal(blockedCommitted, true, 'A keeps its first committed publication pending')
  const bCommitted = published.filter(item => item.deviceId === 'second' && item.kind === 'committed')
  assert.deepEqual(bCommitted.map(item => item.batchId), ['batch-1', 'batch-2', 'batch-3'])
  assert.equal(published.filter(item => item.deviceId === 'second' && item.kind === 'live').length, 1,
    'B also receives live traffic while A is stalled')
  assert.equal(published.filter(item => item.deviceId === 'd' && item.kind === 'committed').length, 1,
    'A has not fabricated later committed delivery while its first publication is pending')

  releaseLive.resolve()
  releaseCommitted.resolve()
  await bounded(aReady.promise, 'A did not receive its committed updates after release')
  const aCommitted = published.filter(item => item.deviceId === 'd' && item.kind === 'committed')
  assert.deepEqual(aCommitted.map(item => item.batchId), ['batch-1', 'batch-2', 'batch-3'])
  assert.ok(published.some(item => item.deviceId === 'd' && item.kind === 'live'))
  for (const entry of published) assert.ok(entry.watchId)
  await tick()
  assert.equal(ctx.store.countOutbox(), 0, 'released committed lanes drain their durable outbox')
})

class QuestionSource extends Source {
  questionCalls=0
  listCalls=0
  stateCalls=0
  stamp={observation_epoch:'epoch-one',revision:1}
  descriptor=null
  stampError=null
  async readState(id, options) { this.stateCalls++; return super.readState(id, options) }
  async readQuestionStamp(id, options) {
    this.questionCalls++
    if (this.stampError) throw this.stampError
    return this.descriptor ?? { availability: 'available', stamp: { ...this.stamp } }
  }
  async listQuestionPage(id) {
    this.listCalls++
    return { batches: [], next_cursor: '', stamp: { ...this.stamp } }
  }
}

function questionUpdates(ctx) {
  const out=[]
  for (const bytes of ctx.access.published) {
    const outer=codec().decode('whalerein.v1.EventEnvelope',bytes)
    if (outer.device_id!=='d') continue
    const payload=openEventEnvelope({envelope:outer,channel:ctx.channel,hostId:'h',deviceId:'d'})
    if (payload.update.questions) out.push(payload.update.questions)
  }
  return out
}
const QUESTIONS={conversation:false,questions:true}
const BOTH={conversation:true,questions:true}

test('session-observation/AC-02.questions-only observation never reads history or conversation state', async (t) => {
  const source=new QuestionSource(),ctx=fixture(undefined,source)
  t.after(()=>ctx.events.stop())
  const ready=await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,QUESTIONS)
  assert.equal(ready.state,undefined)
  assert.equal(ready.questions.availability,'QUESTION_AVAILABILITY_AVAILABLE')
  assert.equal(ready.questions.stamp.revision,1)
  assert.equal(source.stateCalls,0,'questions-only must not call readState')
  assert.equal(source.listCalls,0,'baseline uses the stamp read, not a content page')
})

test('session-observation/AC-03.unsupported or failing questions do not end conversation but lost authority does', async (t) => {
  const source=new QuestionSource(),ctx=fixture(undefined,source)
  t.after(()=>ctx.events.stop())
  source.descriptor={availability:'unsupported',code:'unsupported_capability'}
  const unsupported=await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,BOTH)
  assert.equal(unsupported.state.history_head,'bookmark:alpha')
  assert.equal(unsupported.questions.availability,'QUESTION_AVAILABILITY_UNSUPPORTED')
  source.descriptor=null
  source.stampError=Object.assign(new Error('temporary'),{code:'unavailable'})
  const failing=await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,QUESTIONS)
  assert.equal(failing.questions.availability,'QUESTION_AVAILABILITY_UNAVAILABLE')
  assert.equal(failing.questions.unavailable_code,'unavailable')
  source.stampError=Object.assign(new Error('denied'),{code:'forbidden'})
  await assert.rejects(ctx.events.watch('d',ctx.channel.channelId,'s',undefined,BOTH),error=>error.code==='forbidden')
  await assert.rejects(ctx.events.watch('d',ctx.channel.channelId,'s',undefined,{conversation:false,questions:false}),error=>error.code==='invalid_argument')
  await assert.rejects(ctx.events.watch('d',ctx.channel.channelId,'s',undefined,undefined),error=>error.code==='invalid_argument')
})

test('session-observation/AC-05.change-during-read is reconciled and a late old read cannot regress', async (t) => {
  const source=new QuestionSource(),ctx=fixture(undefined,source)
  t.after(()=>ctx.events.stop())
  await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,QUESTIONS)
  const reads=[]
  source.readQuestionStamp=()=>new Promise((resolve,reject)=>reads.push({resolve,reject}))
  ctx.source=source
  source.sinks.get('s')({session:{session_id:'s'},questions_changed:true})
  await tick()
  assert.equal(reads.length,1)
  // A second change while read one is in flight must guarantee another read.
  source.sinks.get('s')({session:{session_id:'s'},questions_changed:true})
  reads[0].resolve({availability:'available',stamp:{observation_epoch:'epoch-one',revision:2}})
  await tick()
  assert.equal(reads.length,2,'a dirty generation starts a subsequent authoritative read')
  reads[1].resolve({availability:'available',stamp:{observation_epoch:'epoch-one',revision:3}})
  await settle()
  const updates=questionUpdates(ctx)
  assert.ok(updates.length>=1)
  assert.deepEqual(updates.at(-1).stamp,{observation_epoch:'epoch-one',revision:3})
  assert.equal(updates.some(update=>update.stamp?.revision===2),false,'the late old read never publishes')
})

test('session-observation/AC-06.unchanged refresh keeps the stamp without a content page query', async (t) => {
  const source=new QuestionSource(),ctx=fixture(undefined,source)
  t.after(()=>ctx.events.stop())
  const ready=await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,QUESTIONS)
  source.sinks.get('s')({session:{session_id:'s'},questions_changed:true})
  await settle()
  const refreshed=await ctx.events.refresh('d',ctx.channel.channelId,[ready.watch_id])
  assert.deepEqual(refreshed.watches[0].questions.stamp,{observation_epoch:'epoch-one',revision:1})
  assert.equal(source.listCalls,0,'unchanged refresh performs no question-content page query')
  assert.equal(questionUpdates(ctx).length,0,'an unchanged stamp does not retransmit question content')
})

test('session-observation/AC-09.lost final hint is repaired by the existing refresh', async (t) => {
  const source=new QuestionSource(),ctx=fixture(undefined,source)
  t.after(()=>ctx.events.stop())
  const ready=await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,QUESTIONS)
  source.stamp={observation_epoch:'epoch-one',revision:2}
  const refreshed=await ctx.events.refresh('d',ctx.channel.channelId,[ready.watch_id])
  assert.equal(refreshed.watches[0].questions.stamp.revision,2)
  assert.equal(source.listCalls,0)
  assert.equal(questionUpdates(ctx).length,0,'refresh carries the new stamp without new history output')
})

test('session-observation/AC-10.owner replacement advances the epoch and rebaselines questions', async (t) => {
  const source=new QuestionSource(),ctx=fixture(undefined,source)
  t.after(()=>ctx.events.stop())
  await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,QUESTIONS)
  source.stamp={observation_epoch:'epoch-two',revision:1}
  source.sinks.get('s')({session:{session_id:'s'},questions_changed:true})
  await settle()
  const update=questionUpdates(ctx).at(-1)
  assert.equal(update.stamp.observation_epoch,'epoch-two')
  assert.equal(update.stamp.revision,1)
})

test('session-observation/AC-04.question-during-baseline closes the subscription race', async (t) => {
  const source=new QuestionSource(),ctx=fixture(undefined,source)
  t.after(()=>ctx.events.stop())
  let releaseBaseline
  const baseline=new Promise(resolve=>{releaseBaseline=resolve})
  const original=source.readQuestionStamp.bind(source)
  let calls=0
  source.readQuestionStamp=async(id,options)=>{
    if(++calls===1) return baseline
    return original(id,options)
  }
  const pending=ctx.events.watch('d',ctx.channel.channelId,'s',undefined,QUESTIONS)
  await tick()
  source.stamp={observation_epoch:'epoch-one',revision:2}
  source.sinks.get('s')({session:{session_id:'s'},questions_changed:true})
  releaseBaseline({availability:'available',stamp:{observation_epoch:'epoch-one',revision:1}})
  const ready=await pending
  await settle()
  const observed=source.stamp.revision
  const delivered=ready.questions.stamp.revision===observed || questionUpdates(ctx).some(update=>update.stamp.revision===observed)
  assert.equal(delivered,true,'a change during the baseline is observed through the baseline or a subsequent update')
})

test('session-observation/AC-12.a stalled question recipient cannot block another', async (t) => {
  const source=new QuestionSource(),ctx=fixture(undefined,source)
  let release
  const blocked=new Promise(resolve=>{release=resolve})
  const second=establishChannel({hostId:'h',deviceId:'second',security:ctx.security,store:ctx.store})
  const published=[]
  ctx.access.publishEvent=async function(bytes){
    const outer=codec().decode('whalerein.v1.EventEnvelope',bytes)
    published.push(outer.device_id)
    if(outer.device_id==='d') await blocked
  }
  t.after(async()=>{release(); await ctx.events.stop()})
  await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,QUESTIONS)
  await ctx.events.watch('second',second.channelId,'s',undefined,QUESTIONS)
  source.stamp={observation_epoch:'epoch-one',revision:2}
  source.sinks.get('s')({session:{session_id:'s'},questions_changed:true})
  await settle()
  assert.ok(published.includes('second'),'the healthy recipient receives the question update')
})

test('session-observation/AC-08.a failed reconciliation is explicit and later converges to the complete collection', async (t) => {
  const source=new QuestionSource(),ctx=fixture(undefined,source)
  t.after(()=>ctx.events.stop())
  const ready=await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,QUESTIONS)
  source.stampError=Object.assign(new Error('temporary'),{code:'unavailable'})
  source.sinks.get('s')({session:{session_id:'s'},questions_changed:true})
  await settle()
  const failed=questionUpdates(ctx).at(-1)
  assert.equal(failed.availability,'QUESTION_AVAILABILITY_UNAVAILABLE')
  assert.equal(failed.stamp,undefined,'a failure never publishes an available empty collection')
  // The observation remains usable; a later stable read installs the
  // complete collection (possibly empty) with its current stamp.
  source.stampError=null
  source.stamp={observation_epoch:'epoch-one',revision:2}
  source.sinks.get('s')({session:{session_id:'s'},questions_changed:true})
  await settle()
  const repaired=questionUpdates(ctx).at(-1)
  assert.equal(repaired.availability,'QUESTION_AVAILABILITY_AVAILABLE')
  assert.equal(repaired.stamp.revision,2)
  const refreshed=await ctx.events.refresh('d',ctx.channel.channelId,[ready.watch_id])
  assert.deepEqual(refreshed.watches[0].questions.stamp,{observation_epoch:'epoch-one',revision:2})
})

test('a source that no longer has the session closes the watch with a terminal event', async (t) => {
  const ctx = fixture()
  t.after(async () => { await ctx.events.stop(); ctx.store.close() })
  const watch = await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  ctx.source.readState = async () => {
    const error = new Error('session gone')
    error.code = 'not_found'
    throw error
  }
  await assert.rejects(ctx.events.refresh('d', ctx.channel.channelId, [watch.watch_id]), error => error.code === 'not_found')
  assert.equal(ctx.access.published.length, 1, 'the terminal close must be delivered')
  const outer = codec().decode('whalerein.v1.EventEnvelope', ctx.access.published[0])
  const payload = openEventEnvelope({ envelope: outer, channel: ctx.channel, hostId: 'h', deviceId: 'd' })
  assert.equal(payload.watch_closed.watch_id, watch.watch_id)
  assert.equal(payload.watch_closed.reason, 'session_deleted')
  assert.deepEqual(payload.watch_closed.session, { host_id: 'h', session_id: 's' })
  assert.equal(ctx.events.describe().watches, 0, 'the terminal close removes the watch')
  assert.equal(ctx.events.describe().sessions, 0, 'the terminal close releases the source')
})
