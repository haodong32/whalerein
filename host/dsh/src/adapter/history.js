import { randomUUID, createHash } from 'node:crypto'
import { AdapterError } from 'whalerein-host/errors.js'
import { codec } from 'whalerein-host/protocol.js'
import { projectMessage } from './projection.js'

// Tokens are source/session/view positions. Clients never do integer arithmetic.
export class DshHistory {
  #views = new Map()
  constructor(source) { this.source=source }
  #view(sessionId, head, authoritative) {
    let view=this.#views.get(sessionId)
    if(!view) {
      let saved=this.source.metadata?.get(`history-view:${sessionId}`)
      try { saved=JSON.parse(saved) } catch { saved=null }
      view=saved&&typeof saved.id==='string'&&Number.isSafeInteger(saved.head)?saved:{id:randomUUID(),head:0}
      this.#views.set(sessionId,view)
    }
    if(authoritative&&head<view.head) {view.id=randomUUID();this.source.conversation.changeView()}
    view.head=authoritative?head:Math.max(head,view.head)
    this.source.metadata?.set(`history-view:${sessionId}`,JSON.stringify(view))
    return view
  }
  #cursor(sessionId,view,seq) { return Buffer.from(JSON.stringify([sessionId,view,seq])).toString('base64url') }
  #position(token,sessionId,view) {
    if(!token||token.length>4096) throw new AdapterError('snapshot_required','history cursor unavailable')
    let value
    try {value=JSON.parse(Buffer.from(token,'base64url').toString('utf8'))}catch{throw new AdapterError('snapshot_required','history cursor invalid')}
    if(!Array.isArray(value)||value.length!==3||value[0]!==sessionId||value[1]!==view||!Number.isSafeInteger(value[2])||value[2]<-1)throw new AdapterError('snapshot_required','history view changed')
    return value[2]
  }
  #batch(sessionId,view,from,to,changes) {
    const from_cursor=this.#cursor(sessionId,view,from),to_cursor=this.#cursor(sessionId,view,to)
    return {from_cursor,to_cursor,batch_id:createHash('sha256').update(JSON.stringify([from_cursor,to_cursor])).digest('hex'),changes}
  }
  async head(sessionId,{signal}={}) {
    if(signal?.aborted)throw new AdapterError('unavailable')
    const head=typeof this.source.nativeHead==='function' ? await this.source.nativeHead(sessionId,{signal}) : (await this.source.getSessionSnapshot(sessionId,{signal})).cursor
    if(signal?.aborted)throw new AdapterError('unavailable')
    const view=this.#view(sessionId,head,true)
    return this.#cursor(sessionId,view.id,head)
  }
  notification(event,origin) {
    if(!Number.isSafeInteger(event.dshSeq)) return null
    const view=this.#view(event.sessionId,event.dshSeq,false)
    const message=projectMessage(event.sessionId,view.id,event)
    return this.#batch(event.sessionId,view.id,Math.max(-1,event.dshSeq-1),event.dshSeq,message?[{message,...(origin?{origin}:{})}]:[])
  }
  async read(sessionId,read,{signal}={}) {
    if(signal?.aborted)throw new AdapterError('unavailable')
    const source=await this.source.getSessionSnapshot(sessionId,{signal})
    if(signal?.aborted)throw new AdapterError('unavailable')
    const head=source.cursor??source.events.at(-1)?.dshSeq??-1
    const view=this.#view(sessionId,head,true)
    const after=read.after_cursor?this.#position(read.after_cursor,sessionId,view.id):-1
    if(source.complete===false) {
      if(read.snapshot)throw new AdapterError('unavailable','complete source history unavailable')
      const retained=source.retained_from??source.events[0]?.dshSeq??head+1
      if(after<retained-1)throw new AdapterError('snapshot_required','source range no longer retained')
    }
    const cut=read.cut_cursor?this.#position(read.cut_cursor,sessionId,view.id):head
    if(after>cut||cut>head)throw new AdapterError('snapshot_required','source range unavailable')
    const limit=Math.max(1,Math.min(read.max_messages||100,1000))
    const budget=Math.max(1024,Math.min(read.max_bytes||256*1024,256*1024))
    const selected=source.events.filter(e=>e.dshSeq>after&&e.dshSeq<=cut)
    const changes=[]
    let inspected=after,bytes=0,count=0
    for(const event of selected) {
      const message=projectMessage(sessionId,view.id,event)
      const size=message?codec().encode('whalerein.v1.ConversationMessage',message).length+32:0
      if(count&&(bytes+size>budget||count>=limit))break
      inspected=event.dshSeq;count++;bytes+=size
      if(message) { const origin=this.source.conversation.originFor(event); changes.push({message,...(origin?{origin}:{})}) }
    }
    const complete=count===selected.length
    if(complete)inspected=cut
    return {session:{session_id:sessionId},batch:this.#batch(sessionId,view.id,after,inspected,changes),cut_cursor:this.#cursor(sessionId,view.id,cut),complete,snapshot:read.snapshot===true}
  }
}
