import test from 'node:test'
import assert from 'node:assert/strict'
import { FakeDshAdapter } from '../src/adapter/fake.js'

async function source() {
  const adapter=new FakeDshAdapter({sessions:[{sessionId:'s',running:false}]})
  await adapter.open()
  return adapter
}

test('source history pages pin a cut, bind cursors to a session, and keep message IDs stable',async()=>{
  const adapter=await source()
  adapter.output('s','x')
  adapter.output('s','x')
  const first=await adapter.readHistory('s',{snapshot:true,max_messages:1})
  assert.equal(first.batch.changes.length,1)
  assert.equal(first.complete,false)
  adapter.output('s','x')
  const second=await adapter.readHistory('s',{snapshot:true,after_cursor:first.batch.to_cursor,cut_cursor:first.cut_cursor})
  assert.equal(second.batch.changes.length,1)
  assert.equal(second.complete,true)
  const repeat=await adapter.readHistory('s',{snapshot:true,max_messages:1,cut_cursor:first.cut_cursor})
  assert.equal(repeat.batch.changes[0].message.message_id,first.batch.changes[0].message.message_id)
  adapter.createSession({sessionId:'other'})
  await assert.rejects(adapter.readHistory('other',{after_cursor:first.batch.to_cursor}),e=>e.code==='snapshot_required')
  await assert.rejects(adapter.readHistory('s',{after_cursor:'invalid'}),e=>e.code==='snapshot_required')
  await adapter.close()
})

test('source watches emit typed live sequences and final messages without copying native provider data',async()=>{
  const adapter=await source(),seen=[]
  const watch=adapter.watch('s',e=>seen.push(e))
  adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{phase:'start',stream_id:'a',reply_id:'1:1',revision:1}})
  adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{phase:'chunk',stream_id:'a',revision:3,block_id:'text-7',block_kind:'BLOCK_KIND_TEXT',text:'x'}})
  adapter.emit('s',{type:'assistant/message',kind:'output',data:{reply_id:'1:1',message:{id:'m',role:'assistant',content:[{type:'text',text:'x'}],source:{kind:'model',provider:'p',model:'m'}},nativePrivate:true}})
  await new Promise(resolve=>queueMicrotask(resolve))
  assert.equal(seen[1].live.first_seq,2)
  assert.equal(seen[1].live.delta.kind,'BLOCK_KIND_TEXT')
  assert.equal(seen[1].live.delta.text.length,1)
  assert.equal(seen[2].history.changes[0].origin.attempt_id,'a')
  assert.equal('nativePrivate' in seen[2].history.changes[0].message,false)
  assert.equal('data_json' in seen[2].history.changes[0].message,false)
  const before=seen.length
  watch.close()
  adapter.output('s','x')
  assert.equal(seen.length,before)
  await adapter.close()
})

test('native text completion emits a reset only for rewritten content and ignores duplicate or non-text chunks',async()=>{
 const adapter=await source(),seen=[];const watch=adapter.watch('s',e=>seen.push(e))
 const emit=data=>adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{stream_id:'a',block_id:'text-0',block_kind:'BLOCK_KIND_TEXT',...data}})
 emit({phase:'start',revision:1,reply_id:'1:1'})
 emit({phase:'chunk',revision:2,text:'x'})
 emit({phase:'chunk',revision:2,text:'x'})
 emit({phase:'chunk',revision:3})
 emit({phase:'chunk',revision:4,block_text:'x'})
 emit({phase:'chunk',revision:5,block_text:'xy'})
 emit({phase:'end',revision:6,outcome:'abandoned'})
 emit({phase:'invalid',revision:7})
 assert.equal(seen.filter(e=>e.live?.delta).length,1)
 assert.equal(seen.filter(e=>e.live?.reset).length,1)
 assert.equal(seen.find(e=>e.live?.delta).live.delta.kind,'BLOCK_KIND_TEXT')
 assert.equal(seen.find(e=>e.live?.reset).live.reset.kind,'BLOCK_KIND_TEXT')
 assert.equal(seen.at(-1).live.ended.outcome,'ABANDONED')
 assert.equal(seen[2].live.first_seq,3)
 watch.close();await adapter.close()
})

test('native view replacement resets live identity and stale or unretained cursors fail explicitly',async()=>{
 const adapter=await source();adapter.output('s','x');adapter.output('s','x')
 const page=await adapter.readHistory('s',{snapshot:true})
 const old=(await adapter.readState('s')).live_epoch
 adapter.nativeHead=async()=>0
 const current=await adapter.readState('s');assert.notEqual(current.live_epoch,old)
 await assert.rejects(adapter.readHistory('s',{after_cursor:page.batch.to_cursor}),e=>e.code==='snapshot_required')
 adapter.getSessionSnapshot=async()=>({events:[],cursor:10,complete:false,retained_from:10})
 await assert.rejects(adapter.readHistory('s',{snapshot:true}),e=>e.code==='unavailable')
 await assert.rejects(adapter.readHistory('s',{snapshot:false}),e=>e.code==='snapshot_required')
 await adapter.close()
})

test('delivery modes preserve default queue, explicit start checks, and stable receipts',async()=>{
 const adapter=await source();const session=await adapter.getSession('s')
 assert.equal(session.sourceCapabilities.live_text,true)
 assert.deepEqual(session.sourceCapabilities.delivery_modes,['START_IF_IDLE','ENQUEUE_AFTER_RUN','STEER'])
 adapter.setRunning('s',true)
 await assert.rejects(adapter.execute('s',{send_message:{text:'x',delivery_mode:'START_IF_IDLE'}}),e=>e.code==='conflict')
 await assert.rejects(adapter.execute('s',{send_message:{text:'x',delivery_mode:99}}),e=>e.code==='invalid_argument')
 await assert.rejects(adapter.execute('s',{}),e=>e.code==='unsupported_capability')
 const accepted=await adapter.execute('s',{send_message:{text:'x'}},{operationId:'delivery-1'})
 assert.equal(accepted.command_accepted.delivery_id,'delivery-1')
 const unspecified=await adapter.execute('s',{send_message:{text:'x',delivery_mode:'DELIVERY_MODE_UNSPECIFIED'}},{operationId:'delivery-default'})
 assert.equal(unspecified.command_accepted.delivery_id,'delivery-default')
 const steer=await adapter.execute('s',{send_message:{text:'x',delivery_mode:'STEER'}},{operationId:'delivery-2'})
 assert.equal(steer.command_accepted.delivery_id,'delivery-2')
 await adapter.close()
})

test('joining after native generation starts still correlates the final Message without consumer-side guessing',async()=>{
 const adapter=await source()
 adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{phase:'start',stream_id:'a',reply_id:'1:1',revision:1}})
 const seen=[],watch=adapter.watch('s',e=>seen.push(e))
 adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{phase:'chunk',stream_id:'a',revision:2,block_id:'text-0',block_kind:'BLOCK_KIND_TEXT',text:'x'}})
 adapter.emit('s',{type:'assistant/message',kind:'output',data:{reply_id:'1:1',message:{id:'m',role:'assistant',content:[{type:'text',text:'x'}],source:{kind:'model'}}}})
 await new Promise(resolve=>queueMicrotask(resolve))
 assert.equal(seen[0].live.first_seq,2)
 assert.equal(seen.find(e=>e.history).history.changes[0].origin.attempt_id,'a')
 watch.close();await adapter.close()
})

test('native settlement sequence binds a final Message when plugin observation missed the Attempt start',async()=>{
 const adapter=await source(),seen=[],watch=adapter.watch('s',e=>seen.push(e))
 adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{phase:'chunk',stream_id:'a',revision:8,block_id:'text-0',block_kind:'BLOCK_KIND_TEXT',text:'x'}})
 const committed=adapter.emit('s',{type:'assistant/message',kind:'output',data:{message:{id:'m',role:'assistant',content:[{type:'text',text:'xy'}],source:{kind:'model'}}}})
 adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{phase:'end',stream_id:'a',revision:9,outcome:'committed',native_commit_seq:committed.seq}})
 await new Promise(resolve=>queueMicrotask(resolve))
 const update=seen.find(e=>e.history)
 assert.equal(update.history.changes[0].origin.attempt_id,'a')
 assert.equal('native_commit_seq' in update,false)
 watch.close();await adapter.close()
})

test('a source-native record at position zero advances from the empty history position',async()=>{
 const adapter=await source()
 const batch=adapter.conversation.history.notification({sessionId:'s',dshSeq:0,type:'assistant/message',data:{message:{id:'m',role:'assistant',content:[{type:'text',text:'x'}],source:{kind:'model'}}},createdAt:1})
 assert.notEqual(batch.from_cursor,batch.to_cursor)
 assert.equal(batch.changes.length,1)
 await adapter.close()
})

test('fake queue retains a delivery while running and admits it after the current turn', async () => {
 const adapter=await source()
 adapter.setRunning('s',true)
 const receipt=await adapter.execute('s',{send_message:{text:'fixture',delivery_mode:'ENQUEUE_AFTER_RUN'}},{operationId:'queued-1'})
 assert.equal(receipt.command_accepted.delivery_id,'queued-1')
 await adapter.execute('s',{send_message:{text:'fixture',delivery_mode:'ENQUEUE_AFTER_RUN'}},{operationId:'queued-2'})
 await adapter.execute('s',{send_message:{text:'fixture',delivery_mode:'STEER'}},{operationId:'steer-1'})
 assert.equal((await adapter.getSession('s')).running,true)
 assert.equal((await adapter.getSession('s')).queuedPrompts,3)
 assert.equal((await adapter.getSessionSnapshot('s')).events.filter(event=>event.type==='user/message').length,0)
 adapter.setRunning('s',false)
 await new Promise(resolve=>setImmediate(resolve))
 assert.equal((await adapter.getSession('s')).queuedPrompts,0)
 assert.equal((await adapter.getSessionSnapshot('s')).events.filter(event=>event.type==='user/message').length,3)
 await adapter.close()
})

test('a legacy-only source rejects queue and steer without fallback', async () => {
 const adapter=await source()
 adapter.deliveryModes=['START_IF_IDLE']
 const before=(await adapter.getSessionSnapshot('s')).events.length
 await assert.rejects(adapter.execute('s',{send_message:{text:'fixture'}}),error=>error.code==='unsupported_capability')
 await assert.rejects(adapter.execute('s',{send_message:{text:'fixture',delivery_mode:'STEER'}}),error=>error.code==='unsupported_capability')
 assert.equal((await adapter.getSessionSnapshot('s')).events.length,before)
 const accepted=await adapter.execute('s',{send_message:{text:'fixture',delivery_mode:'START_IF_IDLE'}})
 assert.ok(accepted.command_accepted.delivery_id)
 await adapter.close()
})

test('interleaved reasoning and answer deltas keep separate identities, apply block-end corrections only to their own block, and drop late frames after commit',async()=>{
 const adapter=await source(),seen=[];const watch=adapter.watch('s',e=>seen.push(e))
 const emit=data=>adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{stream_id:'a',...data}})
 emit({phase:'start',revision:1,reply_id:'1:1'})
 emit({phase:'chunk',revision:2,block_id:'reasoning-0',block_kind:'BLOCK_KIND_REASONING',text:'R'})
 emit({phase:'chunk',revision:3,block_id:'text-1',block_kind:'BLOCK_KIND_TEXT',text:'A'})
 emit({phase:'chunk',revision:4,block_id:'reasoning-0',block_kind:'BLOCK_KIND_REASONING',block_text:'R2'})
 emit({phase:'chunk',revision:5,block_id:'text-1',block_kind:'BLOCK_KIND_TEXT',text:'B'})
 emit({phase:'chunk',revision:6,block_id:'text-1',block_kind:'BLOCK_KIND_TEXT',block_text:'AB'})
 emit({phase:'chunk',revision:7,block_id:'text-1',block_kind:'BLOCK_KIND_TEXT',text:'C'})
 const committed=adapter.emit('s',{type:'assistant/message',kind:'output',data:{reply_id:'1:1',message:{id:'m',role:'assistant',content:[{type:'reasoning',text:'R2'},{type:'text',text:'ABC'}],source:{kind:'model'}}}})
 emit({phase:'end',revision:8,outcome:'committed',native_commit_seq:committed.seq})
 emit({phase:'chunk',revision:9,block_id:'text-1',block_kind:'BLOCK_KIND_TEXT',text:'late'})
 await new Promise(resolve=>queueMicrotask(resolve))
 watch.close()
 const live=seen.filter(e=>e.live?.delta||e.live?.reset).map(e=>e.live.delta??e.live.reset)
 assert.deepEqual(live.map(block=>[block.kind,block.text]),[
  ['BLOCK_KIND_REASONING','R'],['BLOCK_KIND_TEXT','A'],['BLOCK_KIND_REASONING','R2'],
  ['BLOCK_KIND_TEXT','B'],['BLOCK_KIND_TEXT','C'],
 ])
 assert.equal(seen.some(e=>e.live?.delta?.text==='late'),false)
 const update=seen.find(e=>e.history)
 assert.equal(update.history.changes[0].origin.attempt_id,'a')
 assert.deepEqual(update.history.changes[0].message.blocks.map(block=>block.block_id),['reasoning-0','text-1'])
 await adapter.close()
})

test('a raw Core continuation and a malformed page cursor fail explicitly instead of being reinterpreted',async()=>{
 const adapter=await source()
 await assert.rejects(adapter.readHistory('s',{continuation:'c1'}),e=>e.code==='snapshot_required')
 await assert.rejects(adapter.readHistory('s',{page:{before_cursor:'not-a-token'}}),e=>e.code==='snapshot_required')
 await assert.rejects(adapter.readHistory('s',{page:{},cut_cursor:'not-a-token'}),e=>e.code==='snapshot_required')
 await adapter.close()
})
