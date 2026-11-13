import { randomUUID, createHash } from 'node:crypto'
import { DshHistory } from './history.js'
import { AdapterError } from 'whalerein-host/errors.js'

// This normalization state belongs to the DSH adapter, never the Host or CLI.
export class DshConversation {
  #streams=new Map()
  #origins=new Map()
  #settlements=new Map()
  constructor(source) { this.source=source;this.history=new DshHistory(source);this.epoch=randomUUID() }
  async state(sessionId,{signal}={}) {
    if(signal?.aborted)throw new AdapterError('unavailable')
    const sessions=await this.source.listSessions({signal})
    if(signal?.aborted)throw new AdapterError('unavailable')
    const summary=sessions.find(s=>s.sessionId===sessionId)
    const head=await this.history.head(sessionId,{signal})
    if(signal?.aborted)throw new AdapterError('unavailable')
    const approvals=await this.source.listApprovals(sessionId)
    if(signal?.aborted)throw new AdapterError('unavailable')
    return {session:{session_id:sessionId},live_epoch:this.epoch,revision:++this.revision,
      history_head:head,run_phase:summary?.running?'RUNNING':'IDLE',
      approvals:approvals.map(a=>({approval_id:a.approvalId,session_id:a.sessionId,tool_name:a.toolName,call_id:a.callId,reason:a.reason,digest:a.digest,version:a.version,expires_at_unix_ms:a.expiresAt,state:`APPROVAL_STATE_${a.state.toUpperCase()}`,decision:a.decision?`APPROVAL_DECISION_${a.decision.toUpperCase()}`:'APPROVAL_DECISION_UNSPECIFIED'}))}
  }
  revision=0
  changeView(){this.epoch=randomUUID();this.#streams.clear();this.#origins.clear();this.#settlements.clear()}
  originFor(event) { return this.#settlements.get(JSON.stringify([event.sessionId,event.dshSeq])) ?? this.#origins.get(JSON.stringify([event.sessionId,event.data?.reply_id])) }
  notification(event) {
    if(!event.ephemeral) {
      const origin=this.originFor(event)
      const history=this.history.notification(event,origin)
      return history?{session:{session_id:event.sessionId},history}:null
    }
    if(event.type!=='assistant/preview')return null
    const data=event.data??{},id=data.stream_id
    if(typeof id!=='string'||!['start','chunk','end'].includes(data.phase))return null
    let stream=this.#streams.get(id)
    if(!stream) {
      if(this.#streams.size>=64)this.#streams.delete(this.#streams.keys().next().value)
      stream={seq:0,revision:-1,blocks:new Map(),attempt:{live_epoch:this.epoch,attempt_id:id}}
      this.#streams.set(id,stream)
    }
    if(!Number.isSafeInteger(data.revision)||data.revision<=stream.revision)return null
    stream.revision=data.revision
    let payload
    if(data.phase==='start') {
      payload={started:{}}
      if(data.reply_id)this.#origins.set(JSON.stringify([event.sessionId,data.reply_id]),stream.attempt)
      if(this.#origins.size>64)this.#origins.delete(this.#origins.keys().next().value)
    } else if(data.phase==='chunk') {
      const block=data.block_id??'text'
      if(typeof data.block_text==='string') {
        const previous=stream.blocks.get(block)
        const hash=createHash('sha256').update(data.block_text)
        if(previous&&previous.copy().digest('hex')===hash.copy().digest('hex'))return null
        if(stream.blocks.size<64||stream.blocks.has(block))stream.blocks.set(block,hash)
        payload={reset:{text:data.block_text}}
      } else {
        if(typeof data.text!=='string'||!data.text)return null
        let hash=stream.blocks.get(block)
        if(!hash&&stream.blocks.size<64){hash=createHash('sha256');stream.blocks.set(block,hash)}
        hash?.update(data.text);payload={delta:{text:data.text}}
      }
    } else {
      if(Number.isSafeInteger(data.native_commit_seq)) {
        this.#settlements.set(JSON.stringify([event.sessionId,data.native_commit_seq]),stream.attempt)
        if(this.#settlements.size>64)this.#settlements.delete(this.#settlements.keys().next().value)
      }
      payload={ended:{outcome:data.outcome==='committed'?'COMMITTED':'ABANDONED'}}
    }
    const seq=++stream.seq
    return {session:{session_id:event.sessionId},live:{attempt:stream.attempt,block_id:data.block_id??'text',first_seq:seq,last_seq:seq,...payload}}
  }
}
