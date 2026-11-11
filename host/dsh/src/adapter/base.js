import { mapAdapterError } from './errors.js'
// The DSH adapter boundary. Every DSH conversation API call, projection,
// and subscription lives behind this interface; the rest of the plugin only
// sees these plugin-owned types. Implementations throw AdapterError with a
// stable wire ErrorCode and never expose raw DSH error text.

import { SourceAdapter } from 'whalerein-host/contracts/source.js'
import { DshConversation } from './conversation.js'
import { AdapterError, DefinitiveExecutionError, PluginError } from 'whalerein-host/errors.js'

export * from './native.js'
export { summaryToProto } from 'whalerein-host/contracts/serialization.js'

export class DshAdapter extends SourceAdapter {
  #watchers = new Map()
  #committed = []
  #scheduled = false
  /**
   * @param {object} options
   * @param {() => number} [options.now]
   * @param {object} [options.logger]
   */
  constructor({ now = Date.now, logger = null } = {}) {
    super()
    this.now = now
    this.logger = logger
    this.opened = false
    this.conversation = new DshConversation(this)
  }

  async open({ metadata } = {}) {
    this.metadata = metadata
    this.conversation = new DshConversation(this)
    this.opened = true
  }

  async close() {
    this.opened = false
    this.#watchers.clear()
    this.#committed=[]
    this.#scheduled=false
  }

  async listSessions() {
    const source = this.describe().capabilities ?? []
    const capabilities = {history:source.includes('snapshot'),resumable_history:source.includes('sync'),live_text:source.includes('events'),interrupt:source.includes('interrupt'),approvals:source.includes('approvals')&&this.approvalMode!=='delegate',delivery_modes:['START_IF_IDLE']}
    return (await this.listNativeSessions()).map(({cursor,...session})=>({...session,historyCursor:'',sourceCapabilities:capabilities}))
  }

  async getSession(sessionId) {
    const sessions = await this.listSessions()
    const summary = sessions.find(session => session.sessionId === sessionId)
    if (!summary) throw new AdapterError('not_found', 'session unavailable')
    return summary
  }

  async execute(sessionId, command) {
    if (command.send_message) {
      const mode = command.send_message.delivery_mode || 'START_IF_IDLE'
      if (mode !== 'START_IF_IDLE') throw new DefinitiveExecutionError('unsupported_capability', 'delivery mode unavailable')
      let session
      try { session = await this.getSession(sessionId) }
      catch (error) { throw new DefinitiveExecutionError(mapAdapterError(error)) }
      if (session.running) throw new DefinitiveExecutionError('conflict', 'session busy')
      const result = await this.sendUserMessage(sessionId, command.send_message.text)
      return {command_accepted:{message_id:result?.messageId??''}}
    }
    if (command.interrupt) { await this.interrupt(sessionId); return {} }
    if (command.resolve_approval) {
      const approval = command.resolve_approval
      await this.resolveApproval(approval.approval_id, approval.decision === 'APPROVAL_DECISION_APPROVE'?'approve':'reject', approval.expected_digest, Number(approval.expected_version))
      return {}
    }
    throw new DefinitiveExecutionError('unsupported_capability')
  }

  watch(sessionId, sink) {
    const sinks=this.#watchers.get(sessionId)??new Set()
    sinks.add(sink);this.#watchers.set(sessionId,sinks)
    return { close:()=>{sinks.delete(sink);if(!sinks.size&&this.#watchers.get(sessionId)===sinks)this.#watchers.delete(sessionId)} }
  }

  #publish(sessionId,update) {
    if(update)for(const sink of this.#watchers.get(sessionId)??[]) {
      try{sink(update)}catch{this.logger?.warn('source_observer_failed',{code:'internal'})}
    }
  }
  observeApproval(event) {
    const id=event.approval?.sessionId
    if(this.opened&&id)this.#publish(id,{session:{session_id:id},state_changed:true})
  }
  observeNative(event) {
    if(!this.opened)return
    if(event.ephemeral){this.#publish(event.sessionId,this.conversation.notification(event));return}
    // DSH emits native settlement immediately after its durable append. Defer
    // the ordered commit projection one microtask so the native sequence can
    // bind a mid-generation Attempt even when its start predated plugin load.
    if(this.#committed.length>=1000){this.#committed=[];this.#publish(event.sessionId,{session:{session_id:event.sessionId},reset:{reason:'source_observer_overflow'}})}
    this.#committed.push(event)
    if(!this.#scheduled){
      this.#scheduled=true
      queueMicrotask(()=>{
        this.#scheduled=false
        for(const record of this.#committed.splice(0)) {
          if(!this.opened)break
          try{this.#publish(record.sessionId,this.conversation.notification(record))}
          catch{this.#publish(record.sessionId,{session:{session_id:record.sessionId},reset:{reason:'source_projection_failed'}})}
          if(record.kind==='session_state')this.#publish(record.sessionId,{session:{session_id:record.sessionId},state_changed:true})
        }
      })
    }
  }

  readHistory(sessionId, read) { return this.conversation.history.read(sessionId, read) }
  readState(sessionId) { return this.conversation.state(sessionId) }

  async listNativeSessions() {
    throw new AdapterError('unsupported_capability', 'sessions unavailable')
  }

  async getSessionSnapshot() {
    throw new AdapterError('unsupported_capability', 'snapshot unavailable')
  }

  /** Unified session feed: committed facts and memory-only live previews. */
  subscribeEvents() {
    return () => {}
  }

  /** Adapter-owned approval lifecycle feed (requested/resolved). */
  subscribeApprovals() {
    return () => {}
  }

  async listApprovals() {
    throw new AdapterError('unsupported_capability', 'approvals unavailable')
  }

  async sendUserMessage() {
    throw new AdapterError('unsupported_capability', 'messages unavailable')
  }

  async interrupt() {
    throw new AdapterError('unsupported_capability', 'interrupt unavailable')
  }

  async getApproval() {
    return null
  }

  async resolveApproval() {
    throw new AdapterError('approval_unavailable', 'approval unavailable')
  }

  describe() {
    return { mode: 'abstract', available: false }
  }
}

/** Wrap an arbitrary adapter rejection into a stable wire error. */
export function adapterError(error) {
  if (error instanceof AdapterError || error instanceof PluginError) return error
  return new AdapterError(mapAdapterError(error))
}
