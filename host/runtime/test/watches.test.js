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
function fixture(access={connected:true,published:[],async publishEvent(value){this.published.push(value)}}) {
  let now=100
  const store=new StateStore({path:':memory:'}),source=new Source()
  const security=new SecurityManager({hostId:'h',identity:generateKeypair(),store,codec:codec(),now:()=>now})
  const channel=establishChannel({hostId:'h',deviceId:'d',security,store})
  const events=new WatchedEvents({hostId:'h',store,security,adapter:source,access,codec:codec(),now:()=>now})
  events.start()
  return {events,source,store,security,channel,access,advance:ms=>{now+=ms}}
}
const settle=()=>new Promise(r=>setTimeout(r,30))
function live(seq=1){return {session:{session_id:'s'},live:{attempt:{live_epoch:'run-native',attempt_id:'generation'},block_id:'visible',first_seq:seq,last_seq:seq,delta:{text:'x'}}}}

test('watch registration precedes the baseline, shares native watches, and delivers only to interested recipients',async()=>{
 const ctx=fixture()
 const ready=await ctx.events.watch('d',ctx.channel.channelId,'s')
 assert.equal(ready.state.session.host_id,'h')
 const same=await ctx.events.watch('d',ctx.channel.channelId,'s')
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
 await ctx.events.watch('d',ctx.channel.channelId,'s')
 ctx.source.sinks.get('s')(live())
 await settle()
 ctx.source.sinks.get('s')({session:{session_id:'s'},history:{from_cursor:'bookmark:a',to_cursor:'bookmark:b',batch_id:'b',changes:[]}})
 await settle()
 assert.equal(calls,2)
 assert.equal(ctx.store.countOutbox(),0)
 ctx.events.onChannelReplaced('d',ctx.channel.channelId)
 assert.equal(ctx.events.describe().watches,0)
 release();await ctx.events.stop()
})

test('watch leases and source resource bounds reject invalid or expired selections without revoking identity',async()=>{
 const ctx=fixture(),ready=await ctx.events.watch('d',ctx.channel.channelId,'s')
 await assert.rejects(ctx.events.watch('other',ctx.channel.channelId,'s'),e=>e.code==='unauthorized')
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
  await ctx.events.watch('d', ctx.channel.channelId, 's')
  await ctx.events.watch('second', other.channelId, 's')
  ctx.source.sinks.get('s')(live())
  await settle()
  assert.deepEqual([...published].sort(), ['d', 'second'])
})

test('when a source emits a different session scope, no recipient receives that update', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  await ctx.events.watch('d', ctx.channel.channelId, 's')
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
  await ctx.events.watch('d', ctx.channel.channelId, 's')
  ctx.source.sinks.get('s')({ session: { session_id: 's' }, history: { from_cursor: 'a', to_cursor: 'b', batch_id: 'b', changes: [] } })
  assert.equal(ctx.store.countOutbox(), 1)
  assert.equal(await ctx.events.flushOutbox(), 0)
  assert.equal(ctx.store.countOutbox(), 1)
})

test('when refresh exceeds eight IDs or watch admission exceeds eight sessions, the host rejects without removing valid watches', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  const ready = await ctx.events.watch('d', ctx.channel.channelId, 's')
  await assert.rejects(ctx.events.refresh('d', ctx.channel.channelId, Array(9).fill(ready.watch_id)), error => error.code === 'invalid_argument')
  for (let index = 0; index < 7; index++) await ctx.events.watch('d', ctx.channel.channelId, `other-${index}`)
  await assert.rejects(ctx.events.watch('d', ctx.channel.channelId, 'limit'), error => error.code === 'rate_limited')
  assert.equal(ctx.events.describe().watches, 8)
  const refreshed = await ctx.events.refresh('d', ctx.channel.channelId, [ready.watch_id])
  assert.ok(refreshed)
  assert.ok(ctx.store.getPeerKey('d'))
})

test('when its channel has been cleared, refreshing an existing watch rejects expired without revoking the peer', async (t) => {
  const ctx = fixture()
  t.after(() => ctx.events.stop())
  const ready = await ctx.events.watch('d', ctx.channel.channelId, 's')
  ctx.security.clearChannels()
  await assert.rejects(ctx.events.refresh('d', ctx.channel.channelId, [ready.watch_id]), error => error.code === 'expired')
  assert.ok(ctx.store.getPeerKey('d'))
})

test('when channel replacement cancels watch installation, its eventual handle closes', async()=>{
 const ctx=fixture()
 try {
  const pending=ctx.events.watch('d',ctx.channel.channelId,'s')
  ctx.events.onChannelReplaced('d',ctx.channel.channelId)
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
 const pending=ctx.events.watch('d',ctx.channel.channelId,'s',abort.signal)
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
 await ctx.events.watch('d',ctx.channel.channelId,'s')
 await ctx.events.watch('second',other.channelId,'s')
 let entered,release,seenSignal
 const barrier=new Promise(resolve=>{release=resolve})
 const started=new Promise(resolve=>{entered=resolve})
 const original=ctx.source.readState.bind(ctx.source)
 ctx.source.readState=async(id,{signal}={})=>{seenSignal=signal;entered();await barrier;return original(id)}
 ctx.source.sinks.get('s')({session:{session_id:'s'},state_changed:true})
 await started
 ctx.events.onChannelReplaced('d',ctx.channel.channelId)
 assert.equal(seenSignal.aborted,false)
 ctx.events.onChannelReplaced('second',other.channelId)
 assert.equal(seenSignal.aborted,true)
 release()
 await settle()
 assert.equal(ctx.access.published.length,0)
 assert.equal(ctx.events.describe().watches,0)
 await ctx.events.stop()
})
