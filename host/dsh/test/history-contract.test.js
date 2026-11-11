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
  adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{phase:'chunk',stream_id:'a',revision:3,index:7,text:'x'}})
  adapter.emit('s',{type:'assistant/message',kind:'output',data:{reply_id:'1:1',message:{content:[{type:'text',text:'x'}]},nativePrivate:true}})
  await new Promise(resolve=>queueMicrotask(resolve))
  assert.equal(seen[1].live.first_seq,2)
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
 const emit=data=>adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{stream_id:'a',...data}})
 emit({phase:'start',revision:1,reply_id:'1:1'})
 emit({phase:'chunk',revision:2,text:'x',block_id:'b'})
 emit({phase:'chunk',revision:2,text:'x',block_id:'b'})
 emit({phase:'chunk',revision:3,block_id:'b'})
 emit({phase:'chunk',revision:4,block_id:'b',block_text:'x'})
 emit({phase:'chunk',revision:5,block_id:'b',block_text:'xy'})
 emit({phase:'end',revision:6,outcome:'abandoned'})
 emit({phase:'invalid',revision:7})
 assert.equal(seen.filter(e=>e.live?.delta).length,1)
 assert.equal(seen.filter(e=>e.live?.reset).length,1)
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

test('when DSH advertises live text, unsupported STEER delivery and sending into a running session fail without adding messages',async()=>{
 const adapter=await source();const session=await adapter.getSession('s')
 assert.equal(session.sourceCapabilities.live_text,true)
 assert.deepEqual(session.sourceCapabilities.delivery_modes,['START_IF_IDLE'])
 const before=(await adapter.getSessionSnapshot('s')).events.length
 await assert.rejects(adapter.execute('s',{send_message:{text:'x',delivery_mode:'STEER'}}),e=>e.code==='unsupported_capability')
 adapter.setRunning('s',true)
 await assert.rejects(adapter.execute('s',{send_message:{text:'x'}}),e=>e.code==='conflict')
 await assert.rejects(adapter.getSession('missing'),e=>e.code==='not_found')
 await assert.rejects(adapter.execute('s',{}),e=>e.code==='unsupported_capability')
 assert.equal((await adapter.getSessionSnapshot('s')).events.length,before)
 adapter.setRunning('s',false)
 const accepted=await adapter.execute('s',{send_message:{text:'fixture visible text',delivery_mode:'START_IF_IDLE'}})
 assert.ok(accepted.command_accepted.message_id)
 assert.equal((await adapter.getSessionSnapshot('s')).events.filter(event=>event.type==='user/message').length,1)
 await adapter.close()
})

test('joining after native generation starts still correlates the final Message without consumer-side guessing',async()=>{
 const adapter=await source()
 adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{phase:'start',stream_id:'a',reply_id:'1:1',revision:1}})
 const seen=[],watch=adapter.watch('s',e=>seen.push(e))
 adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{phase:'chunk',stream_id:'a',revision:2,text:'x'}})
 adapter.emit('s',{type:'assistant/message',kind:'output',data:{reply_id:'1:1',text:'x'}})
 await new Promise(resolve=>queueMicrotask(resolve))
 assert.equal(seen[0].live.first_seq,2)
 assert.equal(seen.find(e=>e.history).history.changes[0].origin.attempt_id,'a')
 watch.close();await adapter.close()
})

test('native settlement sequence binds a final Message when plugin observation missed the Attempt start',async()=>{
 const adapter=await source(),seen=[],watch=adapter.watch('s',e=>seen.push(e))
 adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{phase:'chunk',stream_id:'a',revision:8,text:'x'}})
 const committed=adapter.emit('s',{type:'assistant/message',kind:'output',data:{text:'xy'}})
 adapter.emit('s',{type:'assistant/preview',kind:'output',ephemeral:true,data:{phase:'end',stream_id:'a',revision:9,outcome:'committed',native_commit_seq:committed.seq}})
 await new Promise(resolve=>queueMicrotask(resolve))
 const update=seen.find(e=>e.history)
 assert.equal(update.history.changes[0].origin.attempt_id,'a')
 assert.equal('native_commit_seq' in update,false)
 watch.close();await adapter.close()
})

test('a source-native record at position zero advances from the empty history position',async()=>{
 const adapter=await source()
 const batch=adapter.conversation.history.notification({sessionId:'s',dshSeq:0,type:'assistant/message',data:{text:'x'},createdAt:1})
 assert.notEqual(batch.from_cursor,batch.to_cursor)
 assert.equal(batch.changes.length,1)
 await adapter.close()
})
