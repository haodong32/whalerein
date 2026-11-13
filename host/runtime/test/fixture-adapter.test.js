import test from 'node:test'
import assert from 'node:assert/strict'
import net from 'node:net'
import { createInterface } from 'node:readline'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { FixtureAdapter } from '../../../tests/integration/fixtures/adapter.mjs'
import { AdapterError, DefinitiveExecutionError } from '../src/errors.js'

function fixture() {
  const adapter = new FixtureAdapter()
  const calls = []
  adapter.call = async (method, ...args) => {
    calls.push({method,args})
    if (method === 'listSessions') return [{sessionId:'source',sourceCapabilities:{session_fork:true}}]
    return {}
  }
  return {adapter,calls}
}

test('fixture getSession omits undefined options and finds the source session', async () => {
  const {adapter,calls} = fixture()
  assert.equal((await adapter.getSession('source')).sessionId,'source')
  assert.deepEqual(calls,[{method:'listSessions',args:[]}])
  assert.deepEqual(await adapter.listSessions(null),[{sessionId:'source',sourceCapabilities:{session_fork:true}}])
})

test('fixture proxies keep AbortSignal local while forwarding only serializable arguments', async () => {
  const {adapter,calls}=fixture()
  const controller=new AbortController(), signal=controller.signal
  await adapter.listSessions({signal})
  await adapter.listModels({signal})
  await adapter.readSessionModel('s',{signal})
  await adapter.readPendingQueue('s',{signal})
  await adapter.listQuestionPage('s',{cursor:'cursor',signal})
  await adapter.getQuestion('s','batch',{signal})
  await adapter.updatePendingQueue('s',{item_id:'id'}, {signal,expiresAt:123})
  await adapter.answerContinuedQuestion({batch_id:'batch'},{signal,expiresAt:123})
  await adapter.resolveApproval('a','approve',new Uint8Array([1]),3,{signal,expiresAt:123})
  await adapter.executeHost({create_session:{}},{signal,expiresAt:123})
  assert.deepEqual(calls.map(call => call.method),['listSessions','listModels','readSessionModel','readPendingQueue','listQuestionPage','getQuestion','updatePendingQueue','answerContinuedQuestion','resolveApproval','executeHost'])
  assert.deepEqual(calls[0].args,[])
  assert.deepEqual(calls[4].args,['s',{cursor:'cursor'}])
  assert.deepEqual(calls[6].args,['s',{item_id:'id'},{expiresAt:123}])
  assert.ok(calls.every(call => !JSON.stringify(call.args).includes('signal')))
})

test('fixture local cancellation rejects promptly and ignores a late source response', async () => {
  const {adapter,calls}=fixture()
  let resolve
  adapter.call = (method,...args) => {calls.push({method,args});return new Promise(done=>{resolve=done})}
  const controller=new AbortController()
  const pending=adapter.readPendingQueue('source',{signal:controller.signal})
  controller.abort('expired')
  await assert.rejects(pending,error => error instanceof AdapterError && error.code === 'expired' && !(error instanceof DefinitiveExecutionError))
  resolve({available:true})
  await Promise.resolve()
  assert.equal(calls.length,1)
  const before=calls.length
  await assert.rejects(adapter.listSessions({signal:controller.signal}),error => error.code === 'expired')
  assert.equal(calls.length,before)
})

test('fixture open question cancellation sends an explicit cancel without forwarding a signal', async () => {
  const {adapter,calls}=fixture()
  let resolve
  adapter.call=(method,...args)=>{
    calls.push({method,args})
    return new Promise(done=>{resolve=done})
  }
  adapter.notify=(method,...args)=>{calls.push({method,args})}
  const controller=new AbortController()
  const pending=adapter.answerOpenQuestion({batch_id:'batch'},controller.signal,{expiresAt:123})
  controller.abort()
  await assert.rejects(pending,error => error instanceof AdapterError && !(error instanceof DefinitiveExecutionError))
  assert.deepEqual(calls.map(call=>call.method),['answerOpenQuestion','cancelOpenQuestion'])
  assert.equal(calls[0].args[1],calls[1].args[0])
  assert.ok(calls.every(call => !JSON.stringify(call.args).includes('signal')))
  resolve({phase:'QUESTION_SETTLED'})
})

test('fixture wait-state cancellation interrupts an outstanding approval read locally', async () => {
  const {adapter,calls}=fixture()
  let resolve
  adapter.call = (method,...args) => {
    calls.push({method,args})
    if (method === 'listSessions') return Promise.resolve([{sessionId:'source',running:false}])
    return new Promise(done=>{resolve=done})
  }
  const controller=new AbortController()
  const pending=adapter.readWaitState('source',controller.signal)
  while (!resolve) await Promise.resolve()
  controller.abort()
  await assert.rejects(pending,error => error instanceof AdapterError)
  resolve([])
  await Promise.resolve()
  assert.deepEqual(calls,[{method:'listSessions',args:[]},{method:'listApprovals',args:['source']}])
})


test('fixture abort removes the real RPC pending entry and safely ignores a late reply', async () => {
  const dir=mkdtempSync(join(tmpdir(),'whalerein-fixture-proxy-'))
  const socketPath=join(dir,'s.sock')
  const previous=process.env.WHALEREIN_DSH_SOCKET
  process.env.WHALEREIN_DSH_SOCKET=socketPath
  let peer, firstId, deliverFirst
  const received=new Promise(resolve => {deliverFirst=resolve})
  let seen=0
  const server=net.createServer(socket=>{
    peer=socket
    createInterface({input:socket}).on('line',line=>{
      const request=JSON.parse(line)
      if (++seen === 1) {firstId=request.id;deliverFirst(request)}
      else socket.write(JSON.stringify({id:request.id,value:[]})+'\n')
    })
  })
  await new Promise(resolve=>server.listen(socketPath,resolve))
  const adapter=new FixtureAdapter()
  try {
    await adapter.open()
    const controller=new AbortController()
    const pending=adapter.listSessions({signal:controller.signal})
    const request=await received
    assert.deepEqual(request.args,[])
    assert.equal(adapter.pending.size,1)
    controller.abort()
    await assert.rejects(pending,error=>error instanceof AdapterError)
    assert.equal(adapter.pending.size,0)
    peer.write(JSON.stringify({id:firstId,value:[{sessionId:'late'}]})+'\n')
    assert.deepEqual(await adapter.listSessions(),[])
    assert.equal(adapter.pending.size,0)
  } finally {
    await adapter.close()
    peer?.destroy()
    await new Promise(resolve=>server.close(resolve))
    if (previous === undefined) delete process.env.WHALEREIN_DSH_SOCKET
    else process.env.WHALEREIN_DSH_SOCKET=previous
    rmSync(dir,{recursive:true,force:true})
  }
})
