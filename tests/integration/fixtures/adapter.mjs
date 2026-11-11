// Test-only DSH Adapter proxy. The FakeDSH authority survives plugin crashes.
import net from 'node:net'
import { createInterface } from 'node:readline'
import { DshAdapter } from '../../../host/dsh/src/adapter/base.js'
import { AdapterError } from '../../../host/runtime/src/errors.js'
export const encode = value => JSON.stringify(value, (_, v) => v instanceof Uint8Array ? { $bytes: Array.from(v) } : v?.type === 'Buffer' && Array.isArray(v.data) ? { $bytes: v.data } : v)
export const decode = value => JSON.parse(value, (_, v) => v?.$bytes ? Uint8Array.from(v.$bytes) : v)
export class FixtureAdapter extends DshAdapter {
  pending = new Map()
  events = new Set()
  approvals = new Set()
  next = 0
  async open(options) {
    await super.open(options)
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
      if(value.error) pending?.reject(new AdapterError(value.error))
      else pending?.resolve(value.value)
    })
    this.socket.on('close',()=>{for(const p of this.pending.values())p.reject(new AdapterError('unavailable'));this.pending.clear()})
  }
  async close() { this.socket?.destroy() }
  call(method,...args) {
    const id=++this.next
    return new Promise((resolve,reject)=>{
      this.pending.set(id,{resolve,reject})
      this.socket.write(encode({id,method,args})+'\n')
    })
  }
  subscribeEvents(callback) { this.events.add(callback); return ()=>this.events.delete(callback) }
  subscribeApprovals(callback) { this.approvals.add(callback); return ()=>this.approvals.delete(callback) }
  describe() { return {mode:'fake',available:true} }
}
for(const method of ['nativeHead','listSessions','getSessionSnapshot','listApprovals','sendUserMessage','interrupt','getApproval','resolveApproval']) {
  FixtureAdapter.prototype[method]=function(...args){return this.call(method,...args)}
}
