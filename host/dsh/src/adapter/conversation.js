import { randomUUID, createHash } from 'node:crypto'
import { DshHistory } from './history.js'
import { splitUtf8 } from 'whalerein-host/events/preview-split.js'
import { AdapterError } from 'whalerein-host/errors.js'

// This normalization state belongs to the DSH adapter, never the Host or CLI.
export class DshConversation {
  #streams=new Map()
  #origins=new Map()
  #settlements=new Map()
  #interruptSerial=0
  #previewBytes=8192
  constructor(source) { this.source=source;this.history=new DshHistory(source);this.epoch=randomUUID() }
  /** Apply the Host's smallest usable preview text budget on the active route. */
  setPreviewBytes(bytes) {
    if (Number.isSafeInteger(bytes) && bytes > 0) this.#previewBytes = bytes
  }
  async state(sessionId,{signal}={}) {
    if(signal?.aborted)throw new AdapterError('unavailable')
    const sessions=await this.source.listSessions({signal})
    if(signal?.aborted)throw new AdapterError('unavailable')
    const summary=sessions.find(s=>s.sessionId===sessionId)
    const head=await this.history.head(sessionId,{signal})
    if(signal?.aborted)throw new AdapterError('unavailable')
    const approvals=await this.source.listApprovals(sessionId)
    if(signal?.aborted)throw new AdapterError('unavailable')
    const projection=this.history.projection(sessionId)
    return {session:{session_id:sessionId},live_epoch:this.epoch,revision:++this.revision,
      ...(projection?{projection_id:projection}:{}),history_head:head,run_phase:summary?.running?'RUNNING':'IDLE',
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
      stream={sessionId:event.sessionId,seq:0,revision:-1,blocks:new Map(),attempt:{live_epoch:this.epoch,attempt_id:id},ended:false}
      this.#streams.set(id,stream)
    }
    if(stream.ended)return null
    if(!Number.isSafeInteger(data.revision)||data.revision<=stream.revision)return null
    stream.revision=data.revision
    let payload
    if(data.phase==='start') {
      payload={started:{}}
      if(data.reply_id)this.#origins.set(JSON.stringify([event.sessionId,data.reply_id]),stream.attempt)
      if(this.#origins.size>64)this.#origins.delete(this.#origins.keys().next().value)
    } else if(data.phase==='chunk') {
      const block=data.block_id??'text'
      const kind=data.block_kind
      if(kind!=='BLOCK_KIND_TEXT'&&kind!=='BLOCK_KIND_REASONING')return null
      if(typeof data.block_text==='string') {
        const previous=stream.blocks.get(block)
        const hash=createHash('sha256').update(data.block_text)
        if(previous&&previous.copy().digest('hex')===hash.copy().digest('hex'))return null
        if(stream.blocks.size<64||stream.blocks.has(block))stream.blocks.set(block,hash)
        payload={reset:{kind,text:data.block_text}}
      } else {
        if(typeof data.text!=='string'||!data.text)return null
        let hash=stream.blocks.get(block)
        if(!hash&&stream.blocks.size<64){hash=createHash('sha256');stream.blocks.set(block,hash)}
        hash?.update(data.text);payload={delta:{kind,text:data.text}}
      }
    } else {
      if(Number.isSafeInteger(data.native_commit_seq)) {
        this.#settlements.set(JSON.stringify([event.sessionId,data.native_commit_seq]),stream.attempt)
        if(this.#settlements.size>64)this.#settlements.delete(this.#settlements.keys().next().value)
      }
      payload={ended:{outcome:data.outcome==='committed'?'COMMITTED':'ABANDONED'}}
    }
    if(data.phase==='chunk'&&payload.delta) {
      // A large native delta becomes several complete ordered previews before
      // the Host seals them. Each split gets its own normalized sequence, so
      // the emitted intervals stay contiguous with no gap or overlap.
      let parts
      try { parts=splitUtf8(payload.delta.text,this.#previewBytes) }
      catch { parts=[payload.delta.text] }
      if(parts.length>1)return parts.map(text=>{
        const seq=++stream.seq
        return {session:{session_id:event.sessionId},live:{attempt:stream.attempt,block_id:data.block_id??'text',first_seq:seq,last_seq:seq,delta:{...payload.delta,text}}}
      })
    }
    const seq=++stream.seq
    if(data.phase==='end')stream.ended=true
    return {session:{session_id:event.sessionId},live:{attempt:stream.attempt,block_id:data.block_id??'text',first_seq:seq,last_seq:seq,...payload}}
  }

  // Interrupt has no guaranteed native terminal frame. Close every observed
  // attempt for this session, and synthesize one terminal attempt when the
  // caller knows a turn was running but no live stream was observed.
  interrupt(sessionId,{synthetic=true}={}) {
    const updates=[]
    for(const [id,stream] of this.#streams) {
      if(stream.sessionId!==sessionId||stream.ended)continue
      stream.ended=true
      const seq=++stream.seq
      updates.push({session:{session_id:sessionId},live:{attempt:{...stream.attempt},block_id:'text',first_seq:seq,last_seq:seq,ended:{outcome:'INTERRUPTED'}}})
      this.#streams.delete(id)
    }
    if(!updates.length&&synthetic) {
      const attempt={live_epoch:this.epoch,attempt_id:`interrupted-${++this.#interruptSerial}`}
      updates.push({session:{session_id:sessionId},live:{attempt,block_id:'text',first_seq:1,last_seq:1,ended:{outcome:'INTERRUPTED'}}})
    }
    return updates
  }
}
