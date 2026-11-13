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
    this.deliveryModes = ['START_IF_IDLE']
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

  async describeHostCapabilities() {
    const source = this.describe().capabilities ?? []
    return { model_catalog: source.includes('model-catalog'), model_read: source.includes('model-read') }
  }

  async listSessions(options = {}) {
    const source = this.describe().capabilities ?? []
    const capabilities = {history:source.includes('snapshot'),resumable_history:source.includes('sync'),live_text:source.includes('events'),interrupt:source.includes('interrupt'),approvals:source.includes('approvals')&&this.approvalMode!=='delegate',model_selection:source.includes('model-select'),delivery_modes:this.supportedDeliveryModes(),pending_queue_read:source.includes('pending-queue-read'),pending_queue_edit:source.includes('pending-queue-edit'),pending_queue_remove:source.includes('pending-queue-remove'),pending_queue_steer:source.includes('pending-queue-steer'),questions_read:source.includes('questions-read'),questions_answer_open:source.includes('questions-answer-open'),questions_answer_continued:source.includes('questions-answer-continued'),session_fork:source.includes('session-fork')}
    const sessions=await this.listNativeSessions(options)
    if(options.signal?.aborted) throw new AdapterError('unavailable')
    return sessions.map(({cursor,...session})=>({...session,historyCursor:'',sourceCapabilities:capabilities}))
  }

  supportedDeliveryModes() { return this.deliveryModes }

  async getSession(sessionId, options) {
    const sessions = await this.listSessions(options)
    const summary = sessions.find(session => session.sessionId === sessionId)
    if (!summary) throw new AdapterError('not_found', 'session unavailable')
    return summary
  }

  assertCommandFresh(context = {}) {
    if (context.signal?.aborted) throw new DefinitiveExecutionError(context.signal.reason === 'expired' ? 'expired' : 'unavailable')
    if (context.expiresAt > 0 && context.expiresAt <= this.now()) throw new DefinitiveExecutionError('expired')
  }

  async execute(sessionId, command, context = {}) {
    this.assertCommandFresh(context)
    if (command.fork_session) return this.forkSession(sessionId)
    if (command.send_message) {
      const raw = command.send_message.delivery_mode
      const mode = raw == null || raw === 0 || raw === 'DELIVERY_MODE_UNSPECIFIED' ? 'ENQUEUE_AFTER_RUN' : raw
      if (!['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER'].includes(mode)) {
        throw new DefinitiveExecutionError('invalid_argument', 'delivery mode invalid')
      }
      if (!this.supportedDeliveryModes().includes(mode)) throw new DefinitiveExecutionError('unsupported_capability', 'delivery mode unavailable')
      if (mode === 'START_IF_IDLE') {
        let session
        try { session = await this.getSession(sessionId, {signal:context.signal}) }
        catch (error) { throw new DefinitiveExecutionError(mapAdapterError(error)) }
        if (session.running) throw new DefinitiveExecutionError('conflict', 'session busy')
      }
      this.assertCommandFresh(context)
      if (!this.supportedDeliveryModes().includes(mode)) throw new DefinitiveExecutionError('unsupported_capability', 'delivery mode unavailable')
      const result = await this.sendUserMessage(sessionId, command.send_message.text, mode === 'STEER' ? 'steer' : 'queue', context.operationId)
      return {command_accepted:{message_id:result?.messageId??'', delivery_id:result?.deliveryId??''}}
    }
    if (command.select_session_model) {
      const receipt = await this.selectSessionModel(sessionId, command.select_session_model)
      return { model_selection_receipt: receipt }
    }
    if (command.update_pending_queue) return this.updatePendingQueue(sessionId, command.update_pending_queue, context)
    if (command.interrupt) { await this.interrupt(sessionId); return {} }
    if (command.answer_question) {
      const request = command.answer_question
      if (request.session_id !== sessionId) throw new DefinitiveExecutionError('invalid_argument')
      if (request.expected_phase === 'QUESTION_OPEN' && this.describe().capabilities?.includes('questions-answer-open')) {
        return { question_receipt: await this.answerOpenQuestion(request, context.signal, context) }
      }
      if (request.expected_phase !== 'QUESTION_CONTINUED' || !this.describe().capabilities?.includes('questions-answer-continued')) {
        throw new DefinitiveExecutionError('unsupported_capability')
      }
      return { question_receipt: await this.answerContinuedQuestion(request, context) }
    }
    if (command.resolve_approval) {
      const approval = command.resolve_approval
      await this.resolveApproval(approval.approval_id, approval.decision === 'APPROVAL_DECISION_APPROVE'?'approve':'reject', approval.expected_digest, Number(approval.expected_version), context)
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

  readHistory(sessionId, read, options) { return this.conversation.history.read(sessionId, read, options) }
  readState(sessionId, options) { return this.conversation.state(sessionId, options) }

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

  async forkSession() {
    throw new DefinitiveExecutionError('unsupported_capability')
  }

  async sendUserMessage() {
    throw new AdapterError('unsupported_capability', 'messages unavailable')
  }

  async updatePendingQueue() {
    throw new DefinitiveExecutionError('unsupported_capability')
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
