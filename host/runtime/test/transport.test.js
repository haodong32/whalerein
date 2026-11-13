import test from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { LogicalTransport, encodeFrame, decodeFrame, CHUNK_BYTES, PACKET_BYTES, MESSAGE_BYTES } from '../src/access/transport.js'
const hash = (bytes) => createHash('sha256').update(bytes).digest()
const frame = (overrides = {}) => ({version:2,transfer_id:'a'.repeat(32),topic:'data',index:0,count:2,total_bytes:CHUNK_BYTES+1,digest:hash(Buffer.alloc(CHUNK_BYTES+1)),payload:Buffer.alloc(CHUNK_BYTES),...overrides})

test('when an envelope is empty or exceeds the 16 MiB wire budget, sending rejects before publishing any frame', async () => {
  let publications = 0
  const sender = new LogicalTransport({ publish: async () => { publications++ } })
  assert.equal(MESSAGE_BYTES, 16 * 1024 * 1024)
  await assert.rejects(sender.send('data', Buffer.alloc(0), 'ack', true), error => error.code === 'invalid_argument')
  await assert.rejects(sender.send('data', Buffer.alloc(16 * 1024 * 1024 + 1), 'ack', true), error => error.code === 'payload_too_large')
  assert.equal(publications, 0)
})

test('when receipts are lost, logical transfer retries within bounds and delivers the message once', async () => {
  const delivered = []; const packets = []; let drop = true
  let sender
  const receiver = new LogicalTransport({publish:async (topic,bytes) => { const ack=decodeFrame(bytes);if(ack.index===0 && drop){drop=false;return}await sender.receive(topic,bytes,'',()=>{}) }})
  sender = new LogicalTransport({ackTimeoutMs:5,publish:async(topic,bytes)=>{packets.push(bytes);const consumed=await receiver.receive(topic,bytes,'ack',async(body)=>delivered.push(body.length));if(!consumed)delivered.push(bytes.length)}})
  await sender.send('data',Buffer.alloc(2*1024*1024),'ack',true)
  assert.deepEqual(delivered,[2*1024*1024]);assert.ok(packets.every(bytes=>bytes.length<=PACKET_BYTES))
  await receiver.receive('data',packets[0],'ack',()=>{throw new Error('duplicate delivered')})
  await sender.send('data',Buffer.alloc(3),'ack',false);assert.equal(delivered.at(-1),3)
  await assert.rejects(sender.send('data',Buffer.alloc(CHUNK_BYTES+1),'ack',false),error=>error.code==='protocol_version_incompatible')
  await assert.rejects(sender.send('data',Buffer.alloc(MESSAGE_BYTES+1),'ack',true))
})

test('when a transport frame is malformed, mismatched, conflicting, or corrupted, the receiver rejects it', async () => {
  const receiver=new LogicalTransport({publish:async()=>{}})
  for(const item of [frame({version:3}),frame({transfer_id:'bad'}),frame({digest:Buffer.alloc(1)}),frame({topic:'other'}),frame({count:3}),frame({total_bytes:0}),frame({index:2}),frame({payload:Buffer.alloc(2)})]) await assert.rejects(receiver.receive('data',encodeFrame(item),'ack',()=>{}))
  assert.throws(()=>decodeFrame(Buffer.concat([Buffer.from('WRMQTT2'),Buffer.alloc(PACKET_BYTES)])))
  assert.equal(await receiver.receive('data',Buffer.from('legacy'),'ack',()=>{}),false)
  await receiver.receive('data',encodeFrame(frame()),'ack',()=>{throw new Error('incomplete')})
  await assert.rejects(receiver.receive('data',encodeFrame(frame({payload:Buffer.alloc(CHUNK_BYTES,1)})),'ack',()=>{}))
  await assert.rejects(receiver.receive('data',encodeFrame(frame({digest:Buffer.alloc(32,1)})),'ack',()=>{}))
  await assert.rejects(receiver.receive('data',encodeFrame(frame({index:1,payload:Buffer.from([1])})),'ack',()=>{}))
  await assert.rejects(receiver.receive('data',encodeFrame(frame()),'',()=>{}))
})

test('when four bulk receive slots are occupied, the fifth is rate-limited and expired slots admit a new transfer', async () => {
  let now = 0
  let delivered = 0
  const receiver = new LogicalTransport({ now: () => now, ttlMs: 10, publish: async () => {} })
  for (let index = 0; index < 4; index++) {
    await receiver.receive('data', encodeFrame(frame({ transfer_id: index.toString(16).padStart(32, '0') })), 'ack', () => { delivered++ })
  }
  await assert.rejects(receiver.receive('data', encodeFrame(frame()), 'ack', () => { delivered++ }), error => error.code === 'rate_limited')
  assert.equal(delivered, 0)
  now = 10
  await receiver.receive('data', encodeFrame(frame()), 'ack', () => { delivered++ })
  assert.equal(delivered, 0, 'Admission of one chunk must not dispatch an incomplete transfer')
  await receiver.receive('data', encodeFrame(frame({ index: 1, payload: Buffer.alloc(1) })), 'ack', () => { delivered++ })
  await new Promise(resolve => queueMicrotask(resolve))
  assert.equal(delivered, 1)
  receiver.reset()
})

test('when business delivery rejects, completion still deduplicates the transfer and reports one delivery error', async () => {
  let deliveryErrors = 0
  let deliveries = 0
  const receiver = new LogicalTransport({ publish: async () => {}, onDeliveryError: () => { deliveryErrors++ } })
  const body = Buffer.alloc(1)
  const complete = frame({ transfer_id: 'b'.repeat(32), count: 1, total_bytes: 1, payload: body, digest: hash(body) })
  await receiver.receive('data', encodeFrame(complete), 'ack', () => { deliveries++; throw new Error('delivery rejected') })
  await new Promise(resolve => queueMicrotask(resolve))
  await receiver.receive('data', encodeFrame(complete), 'ack', () => { deliveries++ })
  assert.equal(deliveryErrors, 1)
  assert.equal(deliveries, 1)
  await assert.rejects(receiver.receive('data', encodeFrame({ ...complete, total_bytes: 2, payload: Buffer.alloc(2) }), 'ack', () => { deliveries++ }), error => error.code === 'invalid_argument')
  assert.equal(deliveries, 1)
  receiver.reset()
})

test('when a link reset interrupts an in-flight send, it rejects unavailable', async () => {
  let sent
  const started = new Promise(resolve => { sent = resolve })
  const sender = new LogicalTransport({ publish: async () => sent(), ackTimeoutMs: 20 })
  const pending = sender.send('data', Buffer.alloc(CHUNK_BYTES + 1), 'ack', true)
  const rejected = assert.rejects(pending, error => error.code === 'unavailable')
  await started
  sender.reset()
  await rejected
})

test('when no receipt arrives, each chunk retries at most three times and the send rejects unavailable', async () => {
  const packets = []
  const sender = new LogicalTransport({ publish: async (_topic, bytes) => packets.push(decodeFrame(bytes)), ackTimeoutMs: 2 })
  await assert.rejects(sender.send('data', Buffer.alloc(CHUNK_BYTES + 1), 'ack', true), error => error.code === 'unavailable')
  const counts = new Map()
  for (const packet of packets) counts.set(packet.index, (counts.get(packet.index) ?? 0) + 1)
  assert.ok(counts.size > 0)
  for (const count of counts.values()) assert.equal(count, 3)
  sender.reset()
})

test('when a receiver explicitly rejects a transfer, the sender reports unavailable without reporting delivery', async () => {
  const sender = new LogicalTransport({ publish: async (_topic, bytes) => {
    const value = decodeFrame(bytes)
    await sender.receive('ack', encodeFrame({ ...value, payload: undefined, ack: true, rejected: true }), '', () => { throw new Error('An ACK must not dispatch business data') })
  }, ackTimeoutMs: 2 })
  await assert.rejects(sender.send('data', Buffer.alloc(CHUNK_BYTES + 1), 'ack', true), error => error.code === 'unavailable')
  sender.reset()
})

test('when bulk receive slots are full, small control frames remain admissible', async () => {
  let delivered=0
  const receiver=new LogicalTransport({publish:async()=>{}})
  for(let index=0;index<4;index++) await receiver.receive('data',encodeFrame(frame({transfer_id:index.toString(16).padStart(32,'0')})),'ack',()=>{})
  const bytes=Buffer.alloc(1)
  await receiver.receive('data',encodeFrame(frame({transfer_id:'f'.repeat(32),count:1,total_bytes:1,payload:bytes,digest:hash(bytes)})),'ack',()=>{delivered++})
  assert.equal(delivered,1)
})

test('part receipts across a receiver reset cannot masquerade as message completion', async () => {
  let sender;let parts=0;let delivered=0
  const receiver=new LogicalTransport({publish:(topic,bytes)=>sender.receive(topic,bytes,'',()=>{})})
  sender=new LogicalTransport({ackTimeoutMs:2,publish:async(topic,bytes)=>{
    await receiver.receive(topic,bytes,'ack',()=>{delivered++})
    if(++parts===4)receiver.reset()
  }})
  await assert.rejects(sender.send('data',Buffer.alloc(CHUNK_BYTES*4+1),'ack',true))
  assert.equal(delivered,0)
})

test('wire completion does not wait for business execution', async () => {
  let sender;let finish
  const pending=new Promise(resolve=>{finish=resolve})
  const receiver=new LogicalTransport({publish:(topic,bytes)=>sender.receive(topic,bytes,'',()=>{})})
  sender=new LogicalTransport({ackTimeoutMs:2,publish:(topic,bytes)=>receiver.receive(topic,bytes,'ack',()=>pending)})
  await sender.send('data',Buffer.alloc(1),'ack',true)
  finish()
})

test('concurrent bulk sends wait for bounded transport admission instead of failing application operations',async()=>{
 let sender,receiver,delivered=0
 const gate=[]
 sender=new LogicalTransport({publish:async(topic,bytes)=>{
   if(gate.length<2&&decodeFrame(bytes).index===0)await new Promise(resolve=>gate.push(resolve))
   await receiver.receive(topic,bytes,'ack',()=>{delivered++})
 },ackTimeoutMs:100})
 receiver=new LogicalTransport({publish:async(topic,bytes)=>sender.receive(topic,bytes,'',()=>{})})
 const sends=Array.from({length:3},()=>sender.send('data',Buffer.alloc(CHUNK_BYTES+1),'ack',true))
 await new Promise(resolve=>setTimeout(resolve,5))
 for(const release of gate)release()
 await Promise.all(sends)
 assert.equal(delivered,3)
})

test('logical transport accepts envelopes at and immediately below its encoded limit', async () => {
  const delivered = []
  let sender
  const receiver = new LogicalTransport({publish: async (topic, bytes) => sender.receive(topic, bytes, '', () => {})})
  sender = new LogicalTransport({publish: async (topic, bytes) => receiver.receive(topic, bytes, 'ack', body => delivered.push(body.length))})
  for (const size of [MESSAGE_BYTES - 1, MESSAGE_BYTES]) {
    await sender.send('data', Buffer.alloc(size), 'ack', true)
  }
  await new Promise(resolve => setImmediate(resolve))
  assert.deepEqual(delivered, [MESSAGE_BYTES - 1, MESSAGE_BYTES])
})
