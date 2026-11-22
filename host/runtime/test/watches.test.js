import test from 'node:test'
import assert from 'node:assert/strict'
import { SourceAdapter } from '../src/contracts/source.js'
import { WatchedEvents } from '../src/events/watches.js'
import { StateStore } from '../src/state/store.js'
import { SecurityManager } from '../src/security/manager.js'
import { transportProfileBudget } from '../src/contracts/transport.js'
import { publishPacketBytes } from '../src/access/transport.js'
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
function fixture(access={connected:true,published:[],liveTopic:'whalerein/v1/h/h/live',async publishEvent(value){this.published.push(value)},async publishLive(value){this.published.push(value);return true}},source=new Source(),config={}) {
  let now=100
  const store=new StateStore({path:':memory:'})
  const security=new SecurityManager({hostId:'h',identity:generateKeypair(),store,codec:codec(),now:()=>now})
  const channel=establishChannel({hostId:'h',deviceId:'d',security,store})
  const events=new WatchedEvents({hostId:'h',store,security,adapter:source,access,config,codec:codec(),now:()=>now})
  events.start()
  return {events,source,store,security,channel,access,config,advance:ms=>{now+=ms}}
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
function live(seq=1,delta={kind:'BLOCK_KIND_TEXT',text:'x'}){return {session:{session_id:'s'},live:{attempt:{live_epoch:'run-native',attempt_id:'generation'},block_id:'visible',first_seq:seq,last_seq:seq,delta}}}

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
 let release,liveCalls=0,committedCalls=0
 const blocked=new Promise(r=>{release=r})
 const ctx=fixture({
   connected:true,
   async publishLive(){liveCalls++;await blocked;return true},
   async publishEvent(){committedCalls++},
 })
 await ctx.events.watch('d',ctx.channel.channelId,'s',undefined,CONV)
 ctx.source.sinks.get('s')(live())
 await settle()
 ctx.source.sinks.get('s')({session:{session_id:'s'},history:{from_cursor:'bookmark:a',to_cursor:'bookmark:b',batch_id:'b',changes:[]}})
 await settle()
 assert.equal(liveCalls,1)
 assert.equal(committedCalls,1,'the committed QoS 1 publication never queues behind the live preview')
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
  const ctx = fixture({ connected: true, async publishLive(bytes) {
    const envelope = codec().decode('whalerein.v1.EventEnvelope', bytes)
    published.push(envelope.device_id)
    if (envelope.device_id === 'd') await blocked
    return true
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
  ctx.access.publishLive = async function (bytes) {
    ctx.access.published.push(bytes)
    const outer = codec().decode('whalerein.v1.EventEnvelope', bytes)
    awaiting.delete(outer.device_id)
    if (awaiting.size === 0) delivered.resolve()
    return true
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
  t.after(async () => { releaseCommitted.resolve(); await ctx.events.stop() })
  const second = establishChannel({ hostId: 'h', deviceId: 'second', security: ctx.security, store: ctx.store })
  const channels = new Map([['d', ctx.channel], ['second', second]])
  const releaseCommitted = deferred()
  const bReady = deferred()
  const aReady = deferred()
  const published = []
  let blockedCommitted = false
  ctx.access.publishLive = async function (bytes) {
    const outer = codec().decode('whalerein.v1.EventEnvelope', bytes)
    const payload = openEventEnvelope({ envelope: outer, channel: channels.get(outer.device_id), hostId: 'h', deviceId: outer.device_id })
    published.push({ deviceId: outer.device_id, kind: 'live', batchId: null, watchId: payload.update.watch_id })
    return true
  }
  ctx.access.publishEvent = async function (bytes) {
    const outer = codec().decode('whalerein.v1.EventEnvelope', bytes)
    const payload = openEventEnvelope({ envelope: outer, channel: channels.get(outer.device_id), hostId: 'h', deviceId: outer.device_id })
    const update = payload.update
    published.push({ deviceId: outer.device_id, kind: 'committed',
      batchId: update.history?.batch_id ?? null, watchId: update.watch_id })
    if (outer.device_id === 'd' && !blockedCommitted) {
      blockedCommitted = true
      await releaseCommitted.promise
    }
    const committed = published.filter(item => item.deviceId === 'second' && item.kind === 'committed')
    if (committed.length === 3) bReady.resolve()
    const aCommitted = published.filter(item => item.deviceId === 'd' && item.kind === 'committed')
    if (aCommitted.length === 3) aReady.resolve()
  }

  await ctx.events.watch('d', ctx.channel.channelId, 's',undefined,CONV)
  await ctx.events.watch('second', second.channelId, 's',undefined,CONV)

  // A live preview is fire-and-forget, then A's first committed publication
  // stalls. B must keep advancing on its own committed lane.
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
    'B also receives the live preview while A is stalled')
  assert.equal(published.filter(item => item.deviceId === 'd' && item.kind === 'committed').length, 1,
    'A has not fabricated later committed delivery while its first publication is pending')

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

function deliveredUpdates(ctx, deviceId = 'd') {
  const out = []
  for (const bytes of ctx.access.published) {
    const outer = codec().decode('whalerein.v1.EventEnvelope', bytes)
    if (outer.device_id !== deviceId) continue
    const channel = deviceId === 'd'
      ? ctx.channel
      : { channelId: outer.channel_id, h2c: ctx.otherChannels?.get(deviceId)?.h2c }
    const payload = openEventEnvelope({ envelope: outer, channel, hostId: 'h', deviceId })
    if (payload?.update) out.push(payload.update)
  }
  return out
}

test('a preview retired by AttemptEnded or an authoritative commit is never reopened', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  const sink = ctx.source.sinks.get('s')
  sink(live(1))
  await settle()
  assert.equal(deliveredUpdates(ctx).filter(update => update.live).length, 1)

  sink({ session: { session_id: 's' }, live: {
    attempt: { live_epoch: 'run-native', attempt_id: 'generation' },
    block_id: 'visible', first_seq: 2, last_seq: 2, ended: { outcome: 'COMMITTED' },
  } })
  await settle()
  const afterEnd = deliveredUpdates(ctx).length
  sink(live(3))
  await settle()
  assert.equal(deliveredUpdates(ctx).length, afterEnd, 'a late delta after AttemptEnded is dropped')

  // A committed origin retires its Attempt as well.
  sink({ session: { session_id: 's' }, history: {
    from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'commit-batch',
    changes: [{ message: { message_id: 'm1', role: 'ASSISTANT', blocks: [{ block_id: 't', text: { text: 'answer' } }] },
      origin: { live_epoch: 'run-native', attempt_id: 'committed' } }],
  } })
  await settle()
  const afterCommit = deliveredUpdates(ctx).length
  sink({ session: { session_id: 's' }, live: {
    attempt: { live_epoch: 'run-native', attempt_id: 'committed' },
    block_id: 't', first_seq: 1, last_seq: 1, delta: { kind: 'BLOCK_KIND_TEXT', text: 'late' },
  } })
  await settle()
  assert.equal(deliveredUpdates(ctx).length, afterCommit, 'a late delta after a commit origin is dropped')
  // A replayed start cannot reopen a settled Attempt either.
  sink({ session: { session_id: 's' }, live: {
    attempt: { live_epoch: 'run-native', attempt_id: 'committed' },
    first_seq: 1, last_seq: 1, started: {},
  } })
  await settle()
  assert.equal(deliveredUpdates(ctx).length, afterCommit)
})

test('an exact duplicate committed batch is delivered once', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  const batch = { session: { session_id: 's' }, history: {
    from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'same-batch',
    changes: [{ message: { message_id: 'm1', role: 'ASSISTANT', blocks: [{ block_id: 't', text: { text: 'answer' } }] } }],
  } }
  ctx.source.sinks.get('s')(batch)
  await settle()
  ctx.source.sinks.get('s')(batch)
  await settle()
  assert.equal(deliveredUpdates(ctx).filter(update => update.history).length, 1)
  assert.equal(ctx.store.countOutbox(), 0)
})

test('an event that cannot fit the shared receive budget becomes a HistoryReset, never a partial frame', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  ctx.security.getChannelReceive = () => ({ budget: { envelope_bytes: 1024 } })
  ctx.source.sinks.get('s')({ session: { session_id: 's' }, history: {
    from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'too-large',
    changes: [{ message: { message_id: 'm1', role: 'ASSISTANT', blocks: [{ block_id: 't', text: { text: 'x'.repeat(4096) } }] } }],
  } })
  await settle()
  const updates = deliveredUpdates(ctx)
  assert.equal(updates.length, 1)
  assert.equal(updates[0].history, undefined, 'no partial committed batch is published')
  assert.equal(updates[0].reset.reason, 'snapshot_required')
  assert.equal(ctx.events.describe().snapshotRequired, 1)
})

test('a shared event route uses the smallest authenticated receiver budget', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  const other = establishChannel({ hostId: 'h', deviceId: 'second', security: ctx.security, store: ctx.store })
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  await ctx.events.watch('second', other.channelId, 's', undefined, CONV)
  ctx.security.getChannelReceive = (channelId) => ({ budget: { envelope_bytes: channelId === other.channelId ? 1024 : 16 * 1024 * 1024 } })
  ctx.source.sinks.get('s')({ session: { session_id: 's' }, history: {
    from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'route-min',
    changes: [{ message: { message_id: 'm1', role: 'ASSISTANT', blocks: [{ block_id: 't', text: { text: 'x'.repeat(4096) } }] } }],
  } })
  await settle()
  for (const bytes of ctx.access.published) {
    const outer = codec().decode('whalerein.v1.EventEnvelope', bytes)
    const channel = outer.channel_id === ctx.channel.channelId ? ctx.channel : other
    const payload = openEventEnvelope({ envelope: outer, channel, hostId: 'h', deviceId: outer.device_id })
    assert.equal(payload.update.history, undefined)
    assert.equal(payload.update.reset.reason, 'snapshot_required')
  }
  assert.equal(ctx.access.published.length, 2)
})

test('a shared event route uses a relation-valid overridden receive budget as its minimum', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  // This budget carries an operator override that matches no packaged profile
  // exactly. It is still relation-valid and must bound the shared route.
  const override = {
    ...transportProfileBudget('default'),
    packet_bytes: 1024,
    envelope_bytes: 2048,
    reserved_bytes: 32 * 1024,
  }
  const other = establishChannel({
    hostId: 'h', deviceId: 'second', security: ctx.security, store: ctx.store,
    receive: { profile: 'TRANSPORT_PROFILE_DEFAULT', budget: override, connection_generation: 9, relay_version: 4 },
  })
  assert.equal(ctx.security.getChannelReceive(other.channelId).budget.envelope_bytes, 2048)
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  await ctx.events.watch('second', other.channelId, 's', undefined, CONV)
  ctx.source.sinks.get('s')({ session: { session_id: 's' }, history: {
    from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'override-min',
    changes: [{ message: { message_id: 'm1', role: 'ASSISTANT', blocks: [{ block_id: 't', text: { text: 'x'.repeat(4096) } }] } }],
  } })
  await settle()
  assert.equal(ctx.access.published.length, 2)
  for (const bytes of ctx.access.published) {
    const outer = codec().decode('whalerein.v1.EventEnvelope', bytes)
    const channel = outer.channel_id === ctx.channel.channelId ? ctx.channel : other
    const payload = openEventEnvelope({ envelope: outer, channel, hostId: 'h', deviceId: outer.device_id })
    assert.equal(payload.update.history, undefined)
    assert.equal(payload.update.reset.reason, 'snapshot_required')
  }
})

test('an oversized live delta is split before sealing into packet-sized previews with exact prefix and contiguous intervals', async (t) => {
  const transportBudget = {
    packet_bytes: 2048,
    envelope_bytes: 16 * 1024,
    bulk_concurrency: 4,
    reserved_bytes: 32 * 1024,
    transfer_timeout_ms: 120_000,
  }
  const ctx = fixture(undefined, new Source(), { transportBudget, eventBatchMs: 1 })
  t.after(async () => { await ctx.events.stop(); ctx.store.close() })
  const watch = await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  const text = 'é'.repeat(4000)
  ctx.source.sinks.get('s')({
    session: { session_id: 's' },
    live: {
      attempt: { live_epoch: 'run-native', attempt_id: 'generation' },
      block_id: 'visible', first_seq: 1, last_seq: 40,
      delta: { kind: 'BLOCK_KIND_TEXT', text },
    },
  })
  await settle()
  assert.ok(ctx.access.published.length > 1, 'a 8 KiB delta cannot be one 2 KiB packet')
  const topic = 'whalerein/v1/h/h/live'
  const intervals = []
  const texts = []
  for (const bytes of ctx.access.published) {
    const packetSize = publishPacketBytes(topic, bytes.length, { qos: 0 })
    assert.ok(packetSize <= transportBudget.packet_bytes, `${packetSize} > ${transportBudget.packet_bytes}`)
    const envelope = codec().decode('whalerein.v1.EventEnvelope', bytes)
    assert.equal(envelope.relay_version, 4)
    assert.equal((envelope.ciphertext ?? new Uint8Array(0)).length > 0, true, 'previews stay inline on MQTT')
    const payload = openEventEnvelope({ envelope, channel: ctx.channel, hostId: 'h', deviceId: 'd' })
    assert.equal(payload.update.reset, undefined, 'a live preview never becomes a HistoryReset')
    assert.equal(payload.update.watch_id, watch.watch_id)
    assert.equal(payload.update.live.block_id, 'visible')
    assert.equal(payload.update.live.delta.kind, 'BLOCK_KIND_TEXT')
    intervals.push([payload.update.live.first_seq, payload.update.live.last_seq])
    texts.push(payload.update.live.delta.text)
  }
  assert.equal(texts.join(''), text, 'the exact prefix appears once')
  assert.equal(intervals[0][0], 1)
  assert.equal(intervals.at(-1)[1], 40)
  for (let index = 1; index < intervals.length; index++) {
    assert.equal(intervals[index][0], intervals[index - 1][1] + 1, 'no gap and no overlap')
  }
})

test('a shared route uses the smallest authenticated receive budget for every preview', async (t) => {
  const ctx = fixture()
  t.after(async () => { await ctx.events.stop(); ctx.store.close() })
  const override = {
    ...transportProfileBudget('default'),
    packet_bytes: 3000,
    envelope_bytes: 16 * 1024,
    reserved_bytes: 32 * 1024,
  }
  const other = establishChannel({
    hostId: 'h', deviceId: 'second', security: ctx.security, store: ctx.store,
    receive: {
      profile: 'TRANSPORT_PROFILE_DEFAULT', budget: override,
      connection_generation: 9, relay_version: 4,
    },
  })
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  await ctx.events.watch('second', other.channelId, 's', undefined, CONV)
  const text = 'x'.repeat(8000)
  ctx.source.sinks.get('s')({
    session: { session_id: 's' },
    live: {
      attempt: { live_epoch: 'run-native', attempt_id: 'generation' },
      block_id: 'visible', first_seq: 1, last_seq: 40,
      delta: { kind: 'BLOCK_KIND_TEXT', text },
    },
  })
  await settle()
  assert.ok(ctx.access.published.length >= 2, 'both recipients receive bounded previews')
  const topic = 'whalerein/v1/h/h/live'
  for (const bytes of ctx.access.published) {
    assert.ok(publishPacketBytes(topic, bytes.length, { qos: 0 }) <= 3000, 'the smaller receiver budget governs the shared route')
  }
  const perDevice = new Map()
  for (const bytes of ctx.access.published) {
    const envelope = codec().decode('whalerein.v1.EventEnvelope', bytes)
    perDevice.set(envelope.device_id, (perDevice.get(envelope.device_id) ?? 0) + 1)
  }
  assert.equal(perDevice.get('d') > 0, true)
  assert.equal(perDevice.get('second') > 0, true)
})

test('an unsplittable one-unit live delta is never replaced by a HistoryReset', async (t) => {
  const transportBudget = {
    packet_bytes: 1024,
    envelope_bytes: 16 * 1024,
    bulk_concurrency: 4,
    reserved_bytes: 32 * 1024,
    transfer_timeout_ms: 120_000,
  }
  const ctx = fixture(undefined, new Source(), { transportBudget, eventBatchMs: 1 })
  t.after(async () => { await ctx.events.stop(); ctx.store.close() })
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  ctx.source.sinks.get('s')({
    session: { session_id: 's' },
    live: {
      attempt: { live_epoch: 'run-native', attempt_id: 'generation' },
      block_id: 'visible', first_seq: 1, last_seq: 1,
      delta: { kind: 'BLOCK_KIND_TEXT', text: 'x'.repeat(5000) },
    },
  })
  await settle()
  assert.equal(ctx.events.describe().snapshotRequired, 0)
  assert.equal(ctx.access.published.length, 1)
  const envelope = codec().decode('whalerein.v1.EventEnvelope', ctx.access.published[0])
  const payload = openEventEnvelope({ envelope, channel: ctx.channel, hostId: 'h', deviceId: 'd' })
  assert.equal(payload.update.reset, undefined)
  assert.equal(payload.update.live.delta.text.length, 5000)
})

test('previews use only the live class and the committed message stays on the ordered control class', async (t) => {
  const control = []
  const liveUpdates = []
  const ctx = fixture({
    connected: true,
    liveTopic: 'whalerein/v1/h/h/live',
    async publishEvent(bytes) { control.push(bytes) },
    async publishLive(bytes) { liveUpdates.push(bytes); return true },
  })
  t.after(async () => { await ctx.events.stop(); ctx.store.close() })
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  ctx.source.sinks.get('s')(live(1))
  await settle()
  assert.equal(liveUpdates.length, 1, 'the preview goes to the live class')
  assert.equal(control.length, 0, 'no preview is published on the control event topic')

  ctx.source.sinks.get('s')({ session: { session_id: 's' }, history: {
    from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'commit',
    changes: [{ origin: { live_epoch: 'run-native', attempt_id: 'generation' },
      message: { message_id: 'm1', role: 'ASSISTANT', blocks: [{ block_id: 'visible', text: { text: 'done' } }] } }],
  } })
  await settle()
  assert.equal(control.length, 1, 'the committed message is control')
  const committed = openEventEnvelope({
    envelope: codec().decode('whalerein.v1.EventEnvelope', control[0]),
    channel: ctx.channel, hostId: 'h', deviceId: 'd',
  })
  assert.equal(committed.update.history.batch_id, 'commit')

  // The committed message settles the Attempt; a late preview cannot reopen it.
  ctx.source.sinks.get('s')(live(2))
  await settle()
  assert.equal(liveUpdates.length, 1, 'a preview after the commit is dropped')
  assert.equal(control.length, 1)
})

test('two previews can be in flight at once and neither waits for an acknowledgement', async (t) => {
  const gates = []
  const ctx = fixture({
    connected: true,
    async publishLive() {
      const gate = deferred()
      gates.push(gate)
      return gate.promise
    },
  })
  t.after(async () => { for (const gate of gates) gate.resolve(); await ctx.events.stop(); ctx.store.close() })
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  ctx.source.sinks.get('s')(live(1, { kind: 'BLOCK_KIND_TEXT', text: 'a' }))
  const other = live(2)
  other.live.block_id = 'other'
  ctx.source.sinks.get('s')(other)
  await settle()
  assert.equal(gates.length, 2, 'both previews are in flight before either resolves')
  gates.forEach(gate => gate.resolve(false))
  await settle()
  assert.equal(ctx.store.countOutbox(), 0)
})

test('a disconnected Host drops previews without falling back to the control topic', async (t) => {
  const control = []
  const liveUpdates = []
  const ctx = fixture({
    connected: false,
    async publishEvent(bytes) { control.push(bytes) },
    async publishLive(bytes) { liveUpdates.push(bytes); return true },
  })
  t.after(async () => { await ctx.events.stop(); ctx.store.close() })
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  ctx.source.sinks.get('s')(live(1))
  await settle()
  assert.equal(liveUpdates.length, 0)
  assert.equal(control.length, 0)
  assert.equal(ctx.store.countOutbox(), 0, 'a dropped preview is never durable')
})

test('an unsplittable preview never becomes a reference or a control reset', async (t) => {
  const transportBudget = {
    packet_bytes: 1024,
    envelope_bytes: 16 * 1024,
    bulk_concurrency: 4,
    reserved_bytes: 32 * 1024,
    transfer_timeout_ms: 120_000,
  }
  const control = []
  const liveUpdates = []
  const ctx = fixture({
    connected: true,
    liveTopic: 'whalerein/v1/h/h/live',
    async publishEvent(bytes) { control.push(bytes) },
    async publishLive(bytes) { liveUpdates.push(bytes); return true },
  }, new Source(), { transportBudget, eventBatchMs: 1 })
  t.after(async () => { await ctx.events.stop(); ctx.store.close() })
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  // One code unit larger than any packet-sized preview can carry.
  ctx.source.sinks.get('s')({
    session: { session_id: 's' },
    live: {
      attempt: { live_epoch: 'run-native', attempt_id: 'generation' },
      block_id: 'visible', first_seq: 1, last_seq: 1,
      delta: { kind: 'BLOCK_KIND_TEXT', text: 'x'.repeat(5000) },
    },
  })
  await settle()
  assert.equal(control.length, 0, 'an unsplittable preview never becomes control or a reset')
  assert.equal(ctx.store.countOutbox(), 0, 'an unsplittable preview is never durable')
  for (const bytes of liveUpdates) {
    const envelope = codec().decode('whalerein.v1.EventEnvelope', bytes)
    assert.equal(envelope.payload_reference, undefined, 'live never uses the reference carrier')
  }
})

test('two previews for one watch are not serialized behind each other', async (t) => {
  const order = []
  const first = deferred()
  const ctx = fixture({
    connected: true,
    async publishLive(bytes) {
      const payload = openEventEnvelope({
        envelope: codec().decode('whalerein.v1.EventEnvelope', bytes),
        channel: ctx.channel, hostId: 'h', deviceId: 'd',
      })
      order.push(payload.update.live.block_id)
      if (order.length === 1) await first.promise
      return true
    },
  })
  t.after(async () => { first.resolve(); await ctx.events.stop(); ctx.store.close() })
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  ctx.source.sinks.get('s')(live(1))
  const other = live(2)
  other.live.block_id = 'other'
  ctx.source.sinks.get('s')(other)
  await settle()
  assert.deepEqual(order, ['visible', 'other'], 'the second preview is published while the first is still pending')
  first.resolve()
})

test('a cleared channel cannot replay a parked preview into the next observation', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  ctx.source.sinks.get('s')(live(1))
  ctx.events.onChannelsCleared([{ deviceId: 'd', channelId: ctx.channel.channelId }])
  await settle()
  assert.equal(ctx.access.published.length, 0, 'the cleared channel has no pending preview')
  assert.equal(ctx.store.countOutbox(), 0, 'live previews are never durable replays')
  const next = establishChannel({ hostId: 'h', deviceId: 'd', security: ctx.security, store: ctx.store })
  ctx.channel = next
  await ctx.events.watch('d', next.channelId, 's', undefined, CONV)
  assert.equal(await ctx.events.flushOutbox(), 0)
  assert.equal(ctx.access.published.length, 0)
  assert.equal(deliveredUpdates(ctx).length, 0)
})

test('tool activity, notices and context authorship are carried unchanged and never merged', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  const preparation = {
    call_id: 'call-1', stages: ['TOOL_STAGE_PREPARATION'], name: 'read_file', summary: 'Read a file',
    outcome: 'TOOL_OUTCOME_UNSPECIFIED',
  }
  const settlement = {
    call_id: 'call-1', stages: ['TOOL_STAGE_SETTLEMENT'], name: 'read_file',
    result: [{ text: { text: 'ok' } }], outcome: 'TOOL_OUTCOME_SUCCEEDED',
  }
  const notice = { category: 'NOTICE_CATEGORY_RETRY', state: 'NOTICE_STATE_SCHEDULED', summary: 'Retry scheduled' }
  const context = { category: 'NOTICE_CATEGORY_CONTEXT', state: 'NOTICE_STATE_COMPLETED', summary: 'Workspace context' }
  ctx.source.sinks.get('s')({ session: { session_id: 's' }, history: {
    from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'typed-content',
    changes: [
      { message: { message_id: 'm1', role: 'TOOL', blocks: [
        { block_id: 'tool-a', tool_activity: preparation },
        { block_id: 'tool-b', tool_activity: settlement },
      ] } },
      { message: { message_id: 'm2', role: 'NOTICE', blocks: [{ block_id: 'notice', notice }] } },
      { message: { message_id: 'm3', role: 'MESSAGE_ROLE_CONTEXT', blocks: [{ block_id: 'context', notice: context }] } },
    ],
  } })
  await settle()
  const [update] = deliveredUpdates(ctx).filter(item => item.history)
  assert.deepEqual(update.history.changes[0].message.blocks.map(block => block.tool_activity.call_id), ['call-1', 'call-1'])
  assert.deepEqual(update.history.changes[0].message.blocks.map(block => block.tool_activity.stages), [['TOOL_STAGE_PREPARATION'], ['TOOL_STAGE_SETTLEMENT']])
  assert.deepEqual(update.history.changes[1].message.blocks[0].notice, notice)
  assert.deepEqual(update.history.changes[2].message.blocks[0].notice, context)
  assert.equal(update.history.changes[2].message.role, 'MESSAGE_ROLE_CONTEXT')
})

test('context or unknown authorship cannot smuggle a user body into a watch', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  await ctx.events.watch('d', ctx.channel.channelId, 's', undefined, CONV)
  for (const [role, block] of [
    ['MESSAGE_ROLE_CONTEXT', { block_id: 't', text: { text: 'private context' } }],
    ['MESSAGE_ROLE_UNKNOWN', { block_id: 't', text: { text: 'private context' } }],
    ['USER', { block_id: 'n', notice: { category: 'NOTICE_CATEGORY_CONTEXT', state: 'NOTICE_STATE_COMPLETED' } }],
  ]) {
    ctx.source.sinks.get('s')({ session: { session_id: 's' }, history: {
      from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: `bad-${role}`,
      changes: [{ message: { message_id: 'm1', role, blocks: [block] } }],
    } })
  }
  await settle()
  assert.equal(deliveredUpdates(ctx).filter(item => item.history).length, 0)
  assert.equal(ctx.access.published.length, 0)
})
