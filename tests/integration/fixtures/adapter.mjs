// Test-only DSH Adapter proxy. The FakeDSH authority survives plugin crashes.
import net from 'node:net'
import fs from 'node:fs'
import path from 'node:path'
import { randomUUID } from 'node:crypto'
import { createInterface } from 'node:readline'
import { DshAdapter } from '../../../host/dsh/src/adapter/base.js'
import { AdapterError, DefinitiveExecutionError } from '../../../host/runtime/src/errors.js'
export const encode = value => JSON.stringify(value, (_, v) => v instanceof Uint8Array ? { $bytes: Array.from(v) } : v?.type === 'Buffer' && Array.isArray(v.data) ? { $bytes: v.data } : v)
export const decode = value => JSON.parse(value, (_, v) => v?.$bytes ? Uint8Array.from(v.$bytes) : v)
const RPC_ID = Symbol('fixture-rpc-id')
export class FixtureAdapter extends DshAdapter {
  approvalMode = 'claim'
  pending = new Map()
  events = new Set()
  approvals = new Set()
  next = 0
  waitRecords = []
  recordWait() {
    const file = path.join(process.env.WHALEREIN_STATE_DIR, 'test-wait-observations.json')
    fs.writeFileSync(file, JSON.stringify(this.waitRecords))
  }
  async open(options) {
    await super.open(options)
    this.deliveryModes = ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER']
    this.socket = net.createConnection(process.env.WHALEREIN_DSH_SOCKET)
    await new Promise((resolve,reject) => { this.socket.once('connect',resolve); this.socket.once('error',reject) })
    createInterface({input:this.socket}).on('line',line => {
      const value=decode(line)
      if(value.event) {
        if(value.event==='events')this.observeNative(value.value)
        else if(value.event==='approvals')this.observeApproval(value.value)
        for(const callback of this[value.event]) callback(value.value); return
      }
      const pending=this.pending.get(value.id)
      this.pending.delete(value.id)
      if(value.error) pending?.reject(new (value.definitive ? DefinitiveExecutionError : AdapterError)(value.error))
      else pending?.resolve(value.value)
    })
    this.socket.on('close',()=>{for(const p of this.pending.values())p.reject(new AdapterError('unavailable'));this.pending.clear()})
  }
  async close() { this.socket?.destroy() }
  call(method,...args) {
    const id=++this.next
    const result = new Promise((resolve,reject)=>{
      this.pending.set(id,{resolve,reject})
      this.socket.write(encode({id,method,args})+'\n')
    })
    Object.defineProperty(result, RPC_ID, {value:id})
    return result
  }
  notify(method,...args) {
    const id=++this.next
    this.socket.write(encode({id,method,args})+'\n')
  }
  subscribeEvents(callback) { this.events.add(callback); return ()=>this.events.delete(callback) }
  subscribeApprovals(callback) { this.approvals.add(callback); return ()=>this.approvals.delete(callback) }
  describe() {
    const capabilities=['model-catalog','model-read','model-select','wait','questions-read','questions-answer-continued','questions-answer-open']
    // Delegate mode forwards native approvals to the local UI, so it must not
    // advertise a remote approval surface it cannot serve.
    if(this.approvalMode !== 'delegate') capabilities.push('approvals')
    if(this.pendingQueueEnabled !== false) capabilities.push('pending-queue-read','pending-queue-edit','pending-queue-remove','pending-queue-steer')
    return {mode:'fake',available:true,capabilities}
  }
  async readWaitState(sessionId, signal) {
    signal.throwIfAborted()
    const session = (await this.listSessions({signal})).find(value => value.sessionId === sessionId)
    if (!session) throw new AdapterError('not_found')
    const approvals = await this.callScoped('listApprovals', [sessionId], signal)
    signal.throwIfAborted()
    return { active: true, running: Boolean(session.running), queueAvailable: true,
      pendingInput: approvals.length ? ['approval'] : [] }
  }
  // Test-only wait for exercising the encrypted query path. Native stopping
  // semantics are established separately against real DSH.
  async waitSession(sessionId, timeoutMs, signal) {
    const until = performance.now() + timeoutMs
    const record = { sessionId, timeoutMs, status: 'active' }
    this.waitRecords.push(record)
    this.recordWait()
    try {
    const oversized = path.join(process.env.WHALEREIN_STATE_DIR, 'test-wait-oversized-result')
    if (fs.existsSync(oversized)) {
      fs.unlinkSync(oversized)
      return {reason:'input-required', state:{active:true,running:true,queueAvailable:true,
        pendingInput:Array(16 * 1024 * 1024).fill('approval')}}
    }
    const deny = path.join(process.env.WHALEREIN_STATE_DIR, 'test-wait-source-unauthorized')
    if (fs.existsSync(deny)) {
      fs.unlinkSync(deny)
      throw new AdapterError('unauthorized', 'private fixture detail')
    }
    for (;;) {
      signal.throwIfAborted()
      const state = await this.readWaitState(sessionId, signal)
      if (state.pendingInput.length) return { reason: 'input-required', state }
      if (!state.running) return { reason: 'idle', state }
      const forceTimeout = path.join(process.env.WHALEREIN_STATE_DIR, 'test-wait-force-timeout')
      if (fs.existsSync(forceTimeout)) {
        fs.unlinkSync(forceTimeout)
        return { reason: 'timeout', state }
      }
      const remaining = until - performance.now()
      if (remaining <= 0) return { reason: 'timeout', state }
      await new Promise((resolve, reject) => {
        const onAbort = () => { clearTimeout(timer); reject(signal.reason) }
        const timer = setTimeout(() => { signal.removeEventListener('abort', onAbort); resolve() }, Math.min(50, remaining))
        signal.addEventListener('abort', onAbort, { once: true })
        if (signal.aborted) onAbort()
      })
    }
    } finally {
      record.status = signal.aborted ? 'aborted' : 'completed'
      this.recordWait()
    }
  }
  async describeHostCapabilities() { return {session_create:true,session_fork:true, model_catalog:true, model_read:true} }
  async executeHost(command, context = {}) { return this.callScoped('executeHost', [command], context?.signal) }
}
// Signals belong to the local Host process. The test RPC carries only data;
// callers stop waiting on local abort, while a late source reply is drained by
// the existing pending-request handler and cannot change an already settled result.
FixtureAdapter.prototype.callScoped = function(method, args, signal) {
  const cancelled = () => new AdapterError(signal?.reason === 'expired' ? 'expired' : 'unavailable')
  if (signal?.aborted) return Promise.reject(cancelled())
  if (!signal) return this.call(method, ...args)
  let onAbort, request
  const aborted = new Promise((_, reject) => {
    onAbort = () => {
      if (request?.[RPC_ID] !== undefined) this.pending.delete(request[RPC_ID])
      reject(cancelled())
    }
    signal.addEventListener('abort', onAbort, {once:true})
  })
  if (signal.aborted) { signal.removeEventListener('abort', onAbort); return Promise.reject(cancelled()) }
  request = this.call(method, ...args)
  // An abort synchronously triggered by the transport still owns this RPC.
  if (signal.aborted) onAbort()
  return Promise.race([request, aborted]).finally(() => signal.removeEventListener('abort', onAbort))
}
FixtureAdapter.prototype.listSessions = function(options = {}) { return this.callScoped('listSessions', [], options?.signal) }
FixtureAdapter.prototype.listModels = function(options = {}) { return this.callScoped('listModels', [], options?.signal) }
FixtureAdapter.prototype.readSessionModel = function(sessionId, options = {}) { return this.callScoped('readSessionModel', [sessionId], options?.signal) }
FixtureAdapter.prototype.readPendingQueue = function(sessionId, options = {}) {
  if (this.pendingQueueEnabled === false) throw new AdapterError('unsupported_capability')
  return this.callScoped('readPendingQueue', [sessionId], options?.signal)
}
FixtureAdapter.prototype.listQuestionPage = function(sessionId, query = {}) {
  const {signal, ...data} = query ?? {}
  return this.callScoped('listQuestionPage', [sessionId, data], signal)
}
FixtureAdapter.prototype.getQuestion = function(sessionId, batchId, options = {}) { return this.callScoped('getQuestion', [sessionId, batchId], options?.signal) }
FixtureAdapter.prototype.updatePendingQueue = function(sessionId, command, context = {}) {
  if (this.pendingQueueEnabled === false) throw new DefinitiveExecutionError('unsupported_capability')
  return this.callScoped('updatePendingQueue', [sessionId, command, {expiresAt:context?.expiresAt ?? 0}], context?.signal)
}
FixtureAdapter.prototype.answerContinuedQuestion = function(request, context = {}) { return this.callScoped('answerContinuedQuestion', [request, {expiresAt:context?.expiresAt ?? 0}], context?.signal) }
FixtureAdapter.prototype.resolveApproval = function(id, decision, digest, version, context = {}) { return this.callScoped('resolveApproval', [id, decision, digest, version, {expiresAt:context?.expiresAt ?? 0}], context?.signal) }
FixtureAdapter.prototype.readMessageReply = function(sessionId, messageId, signal) {
  return this.callScoped('readMessageReply', [sessionId, messageId], signal)
}
for(const method of ['nativeHead','getSessionSnapshot','listApprovals','sendUserMessage','interrupt','getApproval','selectSessionModel','forkSession']) {
  FixtureAdapter.prototype[method]=function(...args){return this.call(method,...args)}
}
FixtureAdapter.prototype.listApprovals = function(sessionId = '') {
  if (this.approvalMode === 'delegate') return Promise.resolve([])
  return this.call('listApprovals', sessionId)
}
FixtureAdapter.prototype.getApproval = function(approvalId) {
  if (this.approvalMode === 'delegate') return Promise.resolve(null)
  return this.call('getApproval', approvalId)
}

// AbortSignal is local to the Host process and is never serialized to FakeDSH.
FixtureAdapter.prototype.answerOpenQuestion = function(request, signal) {
  if (signal?.aborted) throw new AdapterError('unavailable')
  const id = randomUUID()
  const onAbort = () => { try { this.notify('cancelOpenQuestion', id) } catch { /* Local cancellation still completes. */ } }
  signal?.addEventListener('abort', onAbort, { once: true })
  if (signal?.aborted) { signal.removeEventListener('abort', onAbort); throw new AdapterError('unavailable') }
  return this.callScoped('answerOpenQuestion', [request, id], signal).finally(() => signal?.removeEventListener('abort', onAbort))
}
