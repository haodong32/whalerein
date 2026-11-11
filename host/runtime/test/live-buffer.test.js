import test from 'node:test'
import assert from 'node:assert/strict'
import { LiveBuffer } from '../src/events/live-buffer.js'
const frame=(seq,text='x',id='a',block='b')=>({session:{session_id:'s'},live:{attempt:{live_epoch:'e',attempt_id:id},block_id:block,first_seq:seq,last_seq:seq,delta:{text}}})
test('typed live coalescing preserves contiguous intervals and flushes at lifecycle and identity changes',async()=>{
 const out=[],buffer=new LiveBuffer(update=>out.push(update))
 buffer.push(frame(1));buffer.push(frame(2));assert.equal(out.length,0)
 await new Promise(r=>setTimeout(r,30));assert.equal(out[0].live.last_seq,2);assert.equal(out[0].live.delta.text.length,2)
 buffer.push(frame(3));buffer.push(frame(4,'x','other'));buffer.push(frame(5,'x','other','different'));buffer.push({session:{session_id:'s'},live:{ended:{outcome:'COMMITTED'}}})
 assert.equal(out.length,5);buffer.flush()
})
test('large or noncontiguous deltas flush without crossing stream positions',()=>{
 const out=[],buffer=new LiveBuffer(update=>out.push(update));buffer.push(frame(1));buffer.push(frame(3));buffer.push(frame(4,'x'.repeat(9000)));assert.equal(out.length,3);buffer.flush()
})
