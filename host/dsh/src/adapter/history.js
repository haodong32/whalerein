import { randomUUID, createHash } from 'node:crypto'
import { AdapterError } from 'whalerein-host/errors.js'
import { codec } from 'whalerein-host/protocol.js'
import { DshProjection, attachmentOccurrence } from './projection.js'

// A page is a soft target for canonical packing; one canonical Message may
// always occupy a page alone. These bound a page request, not message content:
// the Host owns the hard encoded envelope. An exact read has no page boundary,
// so one Message larger than its own read bound fails explicitly.
const MAX_PAGE_MESSAGES = 1000
const MAX_PAGE_BYTES = 1024 * 1024
// A read walks a bounded number of native pages. If it cannot prove the older
// boundary within this budget the page reports `complete: false` with a real,
// progressing continuation instead of materializing the full native log.
const MAX_NATIVE_PAGES = 64
const NATIVE_PAGE_MESSAGES = 200
const EXACT_WINDOW_EVENTS = 256

function encodedSize(message) {
  return message ? codec().encode('whalerein.v1.ConversationMessage', message).length + 32 : 0
}

function token(value) {
  return Buffer.from(JSON.stringify(value)).toString('base64url')
}

function decodeToken(value) {
  if (typeof value !== 'string' || value === '' || value.length > 4096) {
    throw new AdapterError('snapshot_required', 'history cursor unavailable')
  }
  try {
    return JSON.parse(Buffer.from(value, 'base64url').toString('utf8'))
  } catch {
    throw new AdapterError('snapshot_required', 'history cursor invalid')
  }
}

// Tokens are source/session/view positions. Clients never do integer arithmetic.
export class DshHistory {
  #views = new Map()
  // Live notification projection keeps runtime-lifetime state (retry/compaction/
  // command correlation) per session view. History reads build a fresh projector
  // over the observed event window so the same event sequence projects
  // canonically without retaining the native log.
  #projectors = new Map()
  constructor(source) { this.source = source }
  #attachments() {
    return typeof this.source.attachmentsAvailable === 'function' && this.source.attachmentsAvailable()
  }
  #newProjector() {
    return new DshProjection({ attachments: this.#attachments() })
  }
  // Public factory for other adapter seams (reply projection) that must use the
  // same canonical block mapper and attachment availability.
  newProjector() {
    return this.#newProjector()
  }
  #view(sessionId, head, authoritative) {
    let view=this.#views.get(sessionId)
    if(!view) {
      let saved=this.source.metadata?.get(`history-view:${sessionId}`)
      try { saved=JSON.parse(saved) } catch { saved=null }
      view=saved&&typeof saved.id==='string'&&Number.isSafeInteger(saved.head)?saved:{id:randomUUID(),head:0}
      this.#views.set(sessionId,view)
    }
    if(authoritative&&head<view.head) {
      view.id=randomUUID()
      this.#projectors.delete(sessionId)
      this.source.conversation.changeView()
    }
    // `head` is the committed fence used for page cuts and rewind detection.
    // A live notification reports observed progress and may run ahead of the
    // committed fence while an Attempt streams; it must not advance `head`, or
    // the next page would look like a rewind and rotate the projection.
    if(authoritative)view.head=head
    this.source.metadata?.set(`history-view:${sessionId}`,JSON.stringify(view))
    return view
  }
  #projector(sessionId, view) {
    let record=this.#projectors.get(sessionId)
    if(!record||record.view!==view) {
      record={view,projector:this.#newProjector()}
      this.#projectors.set(sessionId,record)
    }
    return record.projector
  }
  projection(sessionId) { return this.#views.get(sessionId)?.id }
  // Resolve the pinned view without consulting a materialized log. A first
  // read establishes it from the native head; continuations reuse it.
  async ensureView(sessionId, {signal}={}) {
    if(signal?.aborted)throw new AdapterError('unavailable')
    // Always pin the committed head, even for an existing view created by a
    // live notification: `head` is the fence used by later page cuts and
    // rewind detection, and only an authoritative read may establish it.
    const head=typeof this.source.nativeHead==='function' ? await this.source.nativeHead(sessionId,{signal}) : (await this.source.getSessionSnapshot(sessionId,{signal})).cursor
    if(signal?.aborted)throw new AdapterError('unavailable')
    return this.#view(sessionId,head,true)
  }
  #cursor(sessionId,view,seq) { return token([sessionId,view,seq]) }
  #position(token,sessionId,view) {
    const value=decodeToken(token)
    if(!Array.isArray(value)||value.length!==3||value[0]!==sessionId||value[1]!==view||!Number.isSafeInteger(value[2])||value[2]<-1)throw new AdapterError('snapshot_required','history view changed')
    return value[2]
  }
  // Fixed-cut token. `cut` is inclusive: every page for this traversal only
  // observes events at or below it even while the source keeps appending.
  #cut(sessionId,view,cutSeq) { return token([sessionId,view,'cut',cutSeq]) }
  #parseCut(value,sessionId,view) {
    const parsed=decodeToken(value)
    if(!Array.isArray(parsed)||parsed.length!==4||parsed[0]!==sessionId||parsed[1]!==view||parsed[2]!=='cut'||!Number.isSafeInteger(parsed[3])||parsed[3]<-1)throw new AdapterError('snapshot_required','history cut changed')
    return parsed[3]
  }
  // Continuation position. `before` is exclusive: the next page observes events
  // with seq < before. This is the value Core returns as page.before_cursor.
  #boundary(sessionId,view,cutSeq,before) { return token([sessionId,view,'page',cutSeq,before]) }
  #parseBoundary(value,sessionId) {
    const parsed=decodeToken(value)
    if(!Array.isArray(parsed)||parsed.length!==5||parsed[0]!==sessionId||parsed[2]!=='page'||!Number.isSafeInteger(parsed[3])||!Number.isSafeInteger(parsed[4]))throw new AdapterError('snapshot_required','history continuation invalid')
    return { view: parsed[1], cutSeq: parsed[3], before: parsed[4] }
  }
  #exact(sessionId,view,seq) { return token([sessionId,view,'exact',seq]) }
  #parseExact(value,sessionId) {
    const parsed=decodeToken(value)
    if(!Array.isArray(parsed)||parsed.length!==4||parsed[0]!==sessionId||parsed[2]!=='exact'||!Number.isSafeInteger(parsed[3])||parsed[3]<-1)throw new AdapterError('snapshot_required','committed message locator invalid')
    return { view: parsed[1], seq: parsed[3] }
  }
  #batch(sessionId,view,from,to,changes) {
    const from_cursor=this.#cursor(sessionId,view,from),to_cursor=this.#cursor(sessionId,view,to)
    return {from_cursor,to_cursor,batch_id:createHash('sha256').update(JSON.stringify([from_cursor,to_cursor])).digest('hex'),projection_id:view,changes}
  }
  async head(sessionId,{signal}={}) {
    if(signal?.aborted)throw new AdapterError('unavailable')
    const head=typeof this.source.nativeHead==='function' ? await this.source.nativeHead(sessionId,{signal}) : (await this.source.getSessionSnapshot(sessionId,{signal})).cursor
    if(signal?.aborted)throw new AdapterError('unavailable')
    const view=this.#view(sessionId,head,true)
    return this.#cursor(sessionId,view.id,head)
  }
  // Opaque exact locator for one canonical committed Message. The send
  // follow-up path and exact history read share this single position scheme.
  locatorFor(sessionId,view,seq) {
    return { message_id: createHash('sha256').update(JSON.stringify([sessionId,view,seq])).digest('hex'), source_locator: this.#exact(sessionId,view,seq), projection_id: view }
  }
  notification(event,origin) {
    if(!Number.isSafeInteger(event.dshSeq)) return null
    const view=this.#view(event.sessionId,event.dshSeq,false)
    const message=this.#projector(event.sessionId,view.id).project(event.sessionId,view.id,event)
    return this.#batch(event.sessionId,view.id,Math.max(-1,event.dshSeq-1),event.dshSeq,message?[{message,...(origin?{origin}:{})}]:[])
  }
  async read(sessionId,read,{signal}={}) {
    if(signal?.aborted)throw new AdapterError('unavailable')
    if(read?.exact!=null)return this.#readExact(sessionId,read,{signal})
    if(read?.page!=null)return this.#readPage(sessionId,read,{signal})
    // Core resolves its own continuation into page.before_cursor; a raw
    // continuation reaching a Source is never interpreted here.
    if(read?.continuation!==undefined&&read?.continuation!=='') {
      throw new AdapterError('snapshot_required','history continuation unavailable')
    }
    const source=await this.source.getSessionSnapshot(sessionId,{signal})
    if(signal?.aborted)throw new AdapterError('unavailable')
    // One head definition for state and pages. The passive snapshot cursor and
    // the live session sequence can disagree while an Attempt streams, and a
    // page that pins a different fence than the watch state would rotate the
    // projection for no reason.
    const head=typeof this.source.nativeHead==='function'
      ? await this.source.nativeHead(sessionId,{signal})
      : (source.cursor??source.events.at(-1)?.dshSeq??-1)
    if(signal?.aborted)throw new AdapterError('unavailable')
    const view=this.#view(sessionId,head,true)
    const after=read.after_cursor?this.#position(read.after_cursor,sessionId,view.id):-1
    if(source.complete===false) {
      if(read.snapshot)throw new AdapterError('unavailable','complete source history unavailable')
      const retained=source.retained_from??source.events[0]?.dshSeq??head+1
      if(after<retained-1)throw new AdapterError('snapshot_required','source range no longer retained')
    }
    const cut=read.cut_cursor?this.#position(read.cut_cursor,sessionId,view.id):head
    if(after>cut||cut>head)throw new AdapterError('snapshot_required','source range unavailable')
    const limit=Math.max(1,Math.min(read.max_messages||100,MAX_PAGE_MESSAGES))
    // `max_bytes` is a soft page target. One canonical Message may exceed it
    // alone; the Host decides the hard encoded envelope and reports
    // payload_too_large without advancing the cursor. Never truncate a block.
    const budget=Math.max(1024,Math.min(read.max_bytes||256*1024,MAX_PAGE_BYTES))
    // Project the retained sequence in order, including events before the
    // selection, so cross-event notice state (retry/compaction/command) is the
    // same as when those events arrived live.
    const projector=this.#newProjector()
    const changes=[]
    let inspected=after,bytes=0,count=0,stopped=false
    for(const event of source.events) {
      if(!Number.isSafeInteger(event.dshSeq)||event.dshSeq>cut)break
      const message=projector.project(sessionId,view.id,event)
      if(event.dshSeq<=after)continue
      const size=encodedSize(message)
      if(count&&(bytes+size>budget||count>=limit)){stopped=true;break}
      inspected=event.dshSeq;count++;bytes+=size
      if(message) { const origin=this.source.conversation.originFor(event); changes.push({message,...(origin?{origin}:{})}) }
    }
    if(!stopped)inspected=cut
    return {session:{session_id:sessionId},batch:this.#batch(sessionId,view.id,after,inspected,changes),cut_cursor:this.#cursor(sessionId,view.id,cut),projection_id:view.id,source:'HISTORY_SOURCE_ONLINE',complete:!stopped,snapshot:read.snapshot===true}
  }
  // Latest-first fixed-cut page. A first page pins the native head; every later
  // page carries the same cut and steps toward older history. Messages within a
  // page are chronological Source order. Native observation is bounded: a page
  // walks at most MAX_NATIVE_PAGES passive native pages and never materializes
  // the full log per read.
  async #readPage(sessionId,read,{signal}) {
    const beforeToken=read.page?.before_cursor
    let viewId,cutSeq,beforeStart
    if(typeof beforeToken==='string'&&beforeToken!=='') {
      const parsed=this.#parseBoundary(beforeToken,sessionId)
      viewId=parsed.view;cutSeq=parsed.cutSeq;beforeStart=parsed.before
      const current=this.#views.get(sessionId)
      if(current&&current.id!==viewId)throw new AdapterError('snapshot_required','history projection changed')
      if(!current) {
        const view=await this.ensureView(sessionId,{signal})
        if(view.id!==viewId)throw new AdapterError('snapshot_required','history projection changed')
      }
      if(read.cut_cursor!==undefined&&read.cut_cursor!=='') {
        const cut=this.#parseCut(read.cut_cursor,sessionId,viewId)
        if(cut!==cutSeq)throw new AdapterError('snapshot_required','history cut changed')
      }
    } else if(read.cut_cursor!==undefined&&read.cut_cursor!=='') {
      // A first page may pin an already-established cut explicitly (for
      // example a bounded retry). The token still binds scope, view and cut.
      const parsed=decodeToken(read.cut_cursor)
      if(!Array.isArray(parsed)||parsed.length!==4||parsed[0]!==sessionId||parsed[2]!=='cut'||!Number.isSafeInteger(parsed[3])||parsed[3]<-1)throw new AdapterError('snapshot_required','history cut invalid')
      viewId=parsed[1];cutSeq=parsed[3];beforeStart=undefined
      const current=this.#views.get(sessionId)
      if(current&&current.id!==viewId)throw new AdapterError('snapshot_required','history projection changed')
      if(!current) {
        const view=await this.ensureView(sessionId,{signal})
        if(view.id!==viewId)throw new AdapterError('snapshot_required','history projection changed')
      }
    } else {
      const head=await this.source.nativeHead(sessionId,{signal})
      if(signal?.aborted)throw new AdapterError('unavailable')
      const view=this.#view(sessionId,head,true)
      viewId=view.id;cutSeq=head;beforeStart=undefined
    }
    const limit=Math.max(1,Math.min(read.max_messages||100,MAX_PAGE_MESSAGES))
    const budget=Math.max(1024,Math.min(read.max_bytes||256*1024,MAX_PAGE_BYTES))
    const collected=[] // newest-first canonical messages
    let bytes=0,complete=false,before=beforeStart,oldestIncluded=null,lastScanned=null
    for(let page=0;page<MAX_NATIVE_PAGES;page++) {
      const value=await this.source.nativeEventsPage(sessionId,{throughSeq:cutSeq,beforeSeq:before,maxMessages:NATIVE_PAGE_MESSAGES,signal})
      if(signal?.aborted)throw new AdapterError('unavailable')
      if(!value||!Array.isArray(value.events))throw new AdapterError('unavailable','native history page unavailable')
      const events=value.events
      if(events.length===0&&value.hasMore===true)throw new AdapterError('snapshot_required','native history page made no progress')
      // Project this native window in Source order so within-window notice state
      // is canonical, then select newest-first for the latest-first page.
      const projector=this.#newProjector()
      const projected=new Map()
      for(const event of events) {
        if(!Number.isSafeInteger(event.dshSeq)||event.dshSeq>cutSeq)continue
        const message=projector.project(sessionId,viewId,event)
        if(message)projected.set(event.dshSeq,{event,message})
      }
      const seqs=[...projected.keys()].sort((a,b)=>b-a)
      let stopped=false,exhausted=false
      for(let index=0;index<seqs.length;index++) {
        const seq=seqs[index]
        const item=projected.get(seq)
        const size=encodedSize(item.message)
        if(collected.length&&(bytes+size>budget||collected.length>=limit)){stopped=true;break}
        collected.push(item);bytes+=size;oldestIncluded=seq
        if(collected.length>=limit){
          stopped=true
          // Reaching the message/byte limit on the oldest event of the oldest
          // native page is a complete read, not a fabricated continuation.
          exhausted=index===seqs.length-1&&value.hasMore!==true
          break
        }
      }
      const first=events[0]?.dshSeq
      if(Number.isSafeInteger(first))lastScanned=first
      if(stopped) {
        // Continue below the oldest included Message so no visible Message is
        // skipped or repeated across pages.
        before=oldestIncluded
        complete=exhausted
        break
      }
      if(!value.hasMore){complete=true;break}
      if(!Number.isSafeInteger(first)||first===before)break
      before=first
      if(collected.length===0)oldestIncluded=null
    }
    const changes=collected.reverse().map(({event,message})=>{
      const origin=this.source.conversation.originFor(event)
      return {message,...(origin?{origin}:{})}
    })
    let boundary
    if(complete) {
      boundary=Number.isSafeInteger(oldestIncluded)?oldestIncluded:(Number.isSafeInteger(lastScanned)?lastScanned:(Number.isSafeInteger(beforeStart)?beforeStart:cutSeq+1))
    } else {
      boundary=Number.isSafeInteger(before)?before:(Number.isSafeInteger(oldestIncluded)?oldestIncluded:cutSeq+1)
    }
    const cutToken=this.#cut(sessionId,viewId,cutSeq)
    // A complete, empty page may legitimately have no older position; use the
    // cut token so the batch is still well formed and never looks like a
    // non-progressing continuation.
    const fromCursor=changes.length===0&&complete ? cutToken : this.#boundary(sessionId,viewId,cutSeq,boundary)
    const toCursor=cutToken
    const batch={from_cursor:fromCursor,to_cursor:toCursor,batch_id:createHash('sha256').update(JSON.stringify([fromCursor,toCursor])).digest('hex'),projection_id:viewId,changes}
    return {session:{session_id:sessionId},batch,cut_cursor:cutToken,projection_id:viewId,source:'HISTORY_SOURCE_ONLINE',complete,snapshot:false,continuation:complete?'':fromCursor}
  }
  // Exact committed-message read through the same projection. The opaque
  // locator carries the adapter position; a bounded native window proves the
  // projected identity, scope and projection without a second index or scan.
  async #readExact(sessionId,read,{signal}) {
    const exact=read.exact
    if(!exact||typeof exact!=='object'||exact.session?.session_id!==sessionId)throw new AdapterError('forbidden','committed message scope mismatch')
    if(this.source.hostId&&exact.session?.host_id!==this.source.hostId)throw new AdapterError('forbidden','committed message host mismatch')
    const position=this.#parseExact(exact.source_locator,sessionId)
    const view=await this.ensureView(sessionId,{signal})
    if(view.id!==exact.projection_id||view.id!==position.view)throw new AdapterError('snapshot_required','committed message projection changed')
    if(position.seq<0)throw new AdapterError('not_found','committed message unavailable')
    const window=await this.source.nativeEventWindow(sessionId,position.seq,{signal,before:EXACT_WINDOW_EVENTS})
    if(signal?.aborted)throw new AdapterError('unavailable')
    if(!Array.isArray(window))throw new AdapterError('unavailable','native history window unavailable')
    const projector=this.#newProjector()
    let message=null
    for(const event of window) {
      if(!Number.isSafeInteger(event.dshSeq)||event.dshSeq>position.seq)continue
      const projected=projector.project(sessionId,view.id,event)
      if(event.dshSeq===position.seq)message=projected
    }
    if(!message||message.message_id!==exact.message_id)throw new AdapterError('not_found','committed message no longer available')
    const size=encodedSize(message)
    const exactBound=Math.max(1024,Math.min(read.max_bytes||MAX_PAGE_BYTES,MAX_PAGE_BYTES))
    if(size>exactBound)throw new AdapterError('payload_too_large','canonical message exceeds the exact-read bound')
    const fromCursor=this.#cursor(sessionId,view.id,Math.max(-1,position.seq-1))
    const toCursor=this.#cursor(sessionId,view.id,position.seq)
    return {session:{session_id:sessionId},batch:{from_cursor:fromCursor,to_cursor:toCursor,batch_id:createHash('sha256').update(JSON.stringify([fromCursor,toCursor])).digest('hex'),projection_id:view.id,changes:[{message}]},cut_cursor:toCursor,projection_id:view.id,source:'HISTORY_SOURCE_ONLINE',complete:true,snapshot:false,continuation:''}
  }
}
