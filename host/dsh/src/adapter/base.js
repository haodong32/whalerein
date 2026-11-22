import { mapAdapterError } from './errors.js'
// The DSH adapter boundary. Every DSH conversation API call, projection,
// and subscription lives behind this interface; the rest of the plugin only
// sees these plugin-owned types. Implementations throw AdapterError with a
// stable wire ErrorCode and never expose raw DSH error text.

import { SourceAdapter } from 'whalerein-host/contracts/source.js'
import { DshConversation } from './conversation.js'
import { attachmentOccurrence } from './projection.js'
import { AdapterError, DefinitiveExecutionError, PluginError } from 'whalerein-host/errors.js'

export * from './native.js'
export { summaryToProto } from 'whalerein-host/contracts/serialization.js'

export class DshAdapter extends SourceAdapter {
  #watchers = new Map()
  #committed = []
  #scheduled = false
  #replyEvidence = new Map()
  #openAttachments = new Map()
  /**
   * @param {object} options
   * @param {() => number} [options.now]
   * @param {object} [options.logger]
   * @param {string} [options.hostId]
   */
  constructor({ now = Date.now, logger = null, hostId = '' } = {}) {
    super()
    this.now = now
    this.logger = logger
    this.hostId = typeof hostId === 'string' ? hostId : ''
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
    this.#replyEvidence.clear()
    this.#replyOutcomes.clear()
    for (const reader of this.#openAttachments.values()) {
      try { await reader.close({ cancel: true }) } catch { /* cleanup is best effort */ }
    }
    this.#openAttachments.clear()
  }

  async describeHostCapabilities() {
    const source = this.describe().capabilities ?? []
    return { model_catalog: source.includes('model-catalog'), model_read: source.includes('model-read') }
  }

  async listSessions(options = {}) {
    const source = this.describe().capabilities ?? []
    const snapshot = source.includes('snapshot')
    const events = source.includes('events')
    // Optional expanded-conversation capabilities are advertised only where the
    // adapter really produces the typed data. Each Source declares its own
    // native support; an absent marker stays explicitly unavailable rather than
    // being simulated or silently downgraded.
    const capabilities = {history:snapshot,resumable_history:source.includes('sync'),live_text:events,live_reasoning:events,tool_activity:snapshot,conversation_notices:snapshot,attachments:source.includes('attachments'),history_page:source.includes('history-page'),history_exact:source.includes('history-exact'),send_followup:source.includes('send-followup'),interrupt:source.includes('interrupt'),approvals:source.includes('approvals')&&this.approvalMode!=='delegate',model_selection:source.includes('model-select'),delivery_modes:this.supportedDeliveryModes(),pending_queue_read:source.includes('pending-queue-read'),pending_queue_edit:source.includes('pending-queue-edit'),pending_queue_remove:source.includes('pending-queue-remove'),pending_queue_steer:source.includes('pending-queue-steer'),questions_read:source.includes('questions-read'),questions_answer_open:source.includes('questions-answer-open'),questions_answer_continued:source.includes('questions-answer-continued'),session_fork:source.includes('session-fork')}
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
      const receipt = await this.selectSessionModel(sessionId, command.select_session_model, context)
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
    if(!update)return
    const updates=Array.isArray(update)?update:[update]
    for(const item of updates)for(const sink of this.#watchers.get(sessionId)??[]) {
      try{sink(item)}catch{this.logger?.warn('source_observer_failed',{code:'internal'})}
    }
  }

  /** Forward the Host's active preview text budget to native delta splitting. */
  setPreviewBytes(bytes) { this.conversation.setPreviewBytes(bytes) }
  // Question discovery is independent of committed history: broker capture or
  // removal, phase/local resolution and root replacement publish a lightweight
  // invalidation. The Host reconciles it authoritatively and coalesces bursts.
  notifyQuestionsChanged(sessionId) {
    if (!this.opened || typeof sessionId !== 'string' || sessionId === '') return
    this.#publish(sessionId, { session: { session_id: sessionId }, questions_changed: true })
  }
  observeApproval(event) {
    const id=event.approval?.sessionId
    if(this.opened&&id)this.#publish(id,{session:{session_id:id},state_changed:true})
  }
  // Publish one terminal Attempt frame for a turn interrupted before the
  // source supplied its own end frame.
  publishInterruptedAttempt(sessionId, options = {}) {
    for (const update of this.conversation.interrupt(sessionId, options)) this.#publish(sessionId, update)
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
          if(record.kind==='session_state') {
            this.#publish(record.sessionId,{session:{session_id:record.sessionId},state_changed:true})
            this.notifyQuestionsChanged(record.sessionId)
          }
        }
      })
    }
  }

  readHistory(sessionId, read, options) { return this.conversation.history.read(sessionId, read, options) }
  readState(sessionId, options) { return this.conversation.state(sessionId, options) }

  // --- bounded native observation ----------------------------------------
  // Every native history/attachment/reply path consumes these hooks. The base
  // implementation is the deterministic snapshot used by FakeDSH; a real
  // Source overrides them with the passive native page API and exact ranges.
  // No hook may materialize the full native log for one bounded read.
  attachmentsAvailable() { return false }

  async nativeEventsPage(sessionId, { throughSeq, beforeSeq, maxMessages = 200 } = {}) {
    const snapshot = await this.getSessionSnapshot(sessionId)
    const events = Array.isArray(snapshot?.events) ? snapshot.events : []
    const eligible = events.filter(event => Number.isSafeInteger(event.dshSeq) &&
      (throughSeq === undefined || event.dshSeq <= throughSeq) &&
      (beforeSeq === undefined || event.dshSeq < beforeSeq))
    const page = eligible.slice(-Math.max(1, maxMessages))
    return { events: page, hasMore: eligible.length > page.length, cursor: page.at(-1)?.dshSeq ?? null }
  }

  async nativeEventWindow(sessionId, seq, { before = 256 } = {}) {
    const snapshot = await this.getSessionSnapshot(sessionId)
    const events = Array.isArray(snapshot?.events) ? snapshot.events : []
    return events.filter(event => Number.isSafeInteger(event.dshSeq) && event.dshSeq <= seq).slice(-Math.max(1, before))
  }

  async nativeEventsRange(sessionId, fromSeq, toSeqExclusive) {
    const snapshot = await this.getSessionSnapshot(sessionId)
    const events = Array.isArray(snapshot?.events) ? snapshot.events : []
    return events.filter(event => Number.isSafeInteger(event.dshSeq) && event.dshSeq >= fromSeq && event.dshSeq < toSeqExclusive)
  }

  async nativeReadImage() { throw new AdapterError('unsupported_capability', 'image reads unavailable') }
  async nativeReadFileStream() { throw new AdapterError('unsupported_capability', 'file reads unavailable') }

  // --- typed send follow-up -----------------------------------------------
  // Evidence comes from the original submitted identity and the bounded
  // observation around it. Bodies stay in canonical history; only a scoped
  // reference or evidenced input kinds cross the seam.
  #replyOutcomes = new Map()

  async readMessageReply(sessionId, submittedMessageId, signal) {
    if (typeof sessionId !== 'string' || sessionId === '' ||
        typeof submittedMessageId !== 'string' || submittedMessageId === '' || submittedMessageId.length > 512) {
      throw new AdapterError('invalid_argument', 'submission identity invalid')
    }
    if (signal?.aborted) throw new AdapterError('unavailable')
    const pending = (state, pendingInput = []) => ({
      session_id: sessionId,
      submitted_message_id: submittedMessageId,
      state,
      pending_input: pendingInput,
    })
    const view = await this.conversation.history.ensureView(sessionId, { signal })
    const outcomeKey = `${sessionId}:${submittedMessageId}:${view.id}`
    const cached = this.#replyOutcomes.get(outcomeKey)
    if (cached) return { ...cached, session_id: sessionId, submitted_message_id: submittedMessageId }
    const submitted = await this.#submissionEvidence(sessionId, submittedMessageId, view.id, { signal })
    if (submitted.ambiguous) return pending('MESSAGE_REPLY_UNCERTAIN')
    if (!submitted.seq && submitted.seq !== 0) {
      const wait = await this.#safeWaitState(sessionId, signal)
      return pending(wait?.running === true ? 'MESSAGE_REPLY_PENDING' : 'MESSAGE_REPLY_UNCERTAIN')
    }
    const head = typeof this.nativeHead === 'function' ? await this.nativeHead(sessionId, { signal }) : submitted.seq
    if (signal?.aborted) throw new AdapterError('unavailable')
    if (!Number.isSafeInteger(head) || head < submitted.seq) return pending('MESSAGE_REPLY_PENDING')
    const events = await this.nativeEventsRange(sessionId, submitted.seq, head + 1, { signal })
    if (signal?.aborted) throw new AdapterError('unavailable')
    if (!Array.isArray(events)) return pending('MESSAGE_REPLY_UNCERTAIN')
    const start = events.findIndex(event => event.dshSeq === submitted.seq && isSubmissionEvent(event, submittedMessageId))
    if (start < 0) return pending('MESSAGE_REPLY_PENDING')
    let finalAssistant = null
    let ended = false
    let competing = false
    for (const event of events.slice(start + 1)) {
      if (event.type === 'user/message') { competing = true; break }
      if (event.type === 'assistant/message') finalAssistant = event
      if (event.type === 'turn/end' || event.type === 'session/interrupted') {
        // Any terminal turn outcome settles the submitted operation; an
        // interrupted prefix still commits a valid visible Message.
        ended = true
        if (finalAssistant) break
      }
    }
    const wait = await this.#safeWaitState(sessionId, signal)
    if (!ended && wait?.running === true && !competing) {
      const kinds = await this.pendingInputKinds(sessionId, signal)
      if (kinds.length > 0) return pending('MESSAGE_REPLY_INPUT_REQUIRED', kinds)
    }
    if (competing && !ended) return pending('MESSAGE_REPLY_UNCERTAIN')
    if (!ended) return pending(wait?.running === true ? 'MESSAGE_REPLY_PENDING' : 'MESSAGE_REPLY_UNCERTAIN')
    if (!finalAssistant) return pending('MESSAGE_REPLY_UNCERTAIN')
    const projected = this.#projectEvent(sessionId, view.id, finalAssistant)
    if (!projected) return pending('MESSAGE_REPLY_UNCERTAIN')
    if (!this.hostId) throw new AdapterError('unavailable', 'host identity unavailable for a committed reference')
    const reference = this.conversation.history.locatorFor(sessionId, view.id, finalAssistant.dshSeq)
    if (!reference || !projected.message_id) return pending('MESSAGE_REPLY_UNCERTAIN')
    const committed = {
      session_id: sessionId,
      submitted_message_id: submittedMessageId,
      state: 'MESSAGE_REPLY_COMMITTED',
      committed: {
        session: { host_id: this.hostId, session_id: sessionId },
        message_id: projected.message_id,
        source_locator: reference.source_locator,
        projection_id: reference.projection_id,
      },
      pending_input: [],
    }
    // A settled reply is terminal for its operation/view; subsequent polls reuse
    // this scoped evidence instead of re-reading the turn each time.
    if (this.#replyOutcomes.size >= 128) this.#replyOutcomes.delete(this.#replyOutcomes.keys().next().value)
    this.#replyOutcomes.set(outcomeKey, committed)
    return committed
  }

  #projectEvent(sessionId, view, event) {
    const projector = this.conversation.history.newProjector()
    return projector.project(sessionId, view, event)
  }

  async pendingInputKinds(sessionId, signal) {
    let state = null
    try { state = await this.readWaitState(sessionId, signal) } catch { state = null }
    const kinds = new Set()
    if (state && Array.isArray(state.pendingInput)) {
      for (const kind of state.pendingInput) {
        if (kind === 'approval') kinds.add('INPUT_APPROVAL')
        else if (kind === 'question') kinds.add('INPUT_QUESTION')
        else kinds.add('INPUT_OTHER')
      }
    }
    if (state && state.running === false && kinds.size === 0) return []
    if (kinds.size === 0 && typeof this.listApprovals === 'function') {
      try {
        const approvals = await this.listApprovals(sessionId)
        if (Array.isArray(approvals) && approvals.length > 0) kinds.add('INPUT_APPROVAL')
      } catch { /* evidence stays absent */ }
    }
    return [...kinds]
  }

  async #safeWaitState(sessionId, signal) {
    try { return await this.readWaitState(sessionId, signal) } catch { return null }
  }

  async #submissionEvidence(sessionId, submittedMessageId, view, { signal } = {}) {
    const key = `${sessionId}:${submittedMessageId}`
    const cached = this.#replyEvidence.get(key)
    if (cached && cached.view === view) return cached
    const matches = []
    let beforeSeq
    // A queued or just-started submission is near the tail. This scan is
    // bounded and reused across polls; an older submission still resolves from
    // the cached locator without re-reading the whole log.
    for (let page = 0; page < 8; page++) {
      const value = await this.nativeEventsPage(sessionId, { beforeSeq, maxMessages: 200, signal })
      if (signal?.aborted) throw new AdapterError('unavailable')
      for (const event of value?.events ?? []) {
        if (isSubmissionEvent(event, submittedMessageId)) matches.push(event.dshSeq)
      }
      if (matches.length > 1) {
        const result = { view, seq: matches[0], ambiguous: true }
        this.#rememberSubmission(key, result)
        return result
      }
      if (!value?.hasMore) break
      const first = value.events?.[0]?.dshSeq
      if (!Number.isSafeInteger(first) || first === beforeSeq) break
      beforeSeq = first
    }
    if (matches.length === 1) {
      const result = { view, seq: matches[0], ambiguous: false }
      this.#rememberSubmission(key, result)
      return result
    }
    // A queued submission has not started yet: do not cache the absence, so a
    // later poll can observe its first committed user Message.
    this.#replyEvidence.delete(key)
    return { view, seq: null, ambiguous: false }
  }

  #rememberSubmission(key, result) {
    if (this.#replyEvidence.size >= 128) this.#replyEvidence.delete(this.#replyEvidence.keys().next().value)
    this.#replyEvidence.set(key, result)
  }

  // --- attachment occurrence reads ---------------------------------------
  // Resolution proves committed membership first; no native byte is read until
  // the returned reader is advanced. The reader is cancellable and idempotent.
  async openAttachment(sessionId, attachmentRef, { signal } = {}) {
    if (signal?.aborted) throw new AdapterError('unavailable')
    if (typeof sessionId !== 'string' || sessionId === '') throw new AdapterError('invalid_argument', 'attachment session invalid')
    if (typeof attachmentRef !== 'string' || attachmentRef === '' || attachmentRef.length > 4096) throw new AdapterError('invalid_argument', 'attachment occurrence invalid')
    const occurrence = await this.resolveAttachment(sessionId, attachmentRef, { signal })
    if (signal?.aborted) throw new AdapterError('unavailable')
    if (!occurrence) throw new AdapterError('not_found', 'attachment occurrence unavailable')
    const native = occurrence.attachment
    const size = Number.isSafeInteger(native?.bytes) && native.bytes >= 0 ? native.bytes : null
    if (size === null) throw new AdapterError('invalid_argument', 'attachment size invalid')
    const mediaType = typeof native?.mediaType === 'string' && /^[\w.+-]+\/[\w.+-]+$/.test(native.mediaType) ? native.mediaType : 'application/octet-stream'
    const name = typeof native?.name === 'string' && !/[\\/]/.test(native.name) ? native.name : ''
    const digest = occurrence.kind === 'file' ? sha256FromAttachmentId(native?.attachmentId) : null
    const controller = new AbortController()
    const onAbort = () => controller.abort(signal?.reason)
    if (signal) {
      if (signal.aborted) controller.abort(signal.reason)
      else signal.addEventListener('abort', onAbort, { once: true })
    }
    const reader = occurrence.kind === 'image'
      ? this.#imageReader(native, size, controller)
      : this.#fileReader(native, size, controller)
    const close = async ({ cancel = false } = {}) => {
      try { await reader.close({ cancel }) } finally {
        signal?.removeEventListener?.('abort', onAbort)
        this.#openAttachments.delete(`${sessionId}:${attachmentRef}`)
      }
    }
    const wrapped = { read: (maxBytes, options) => reader.read(maxBytes, options), close }
    this.#openAttachments.set(`${sessionId}:${attachmentRef}`, wrapped)
    return {
      attachment: {
        ...(this.hostId ? { session: { host_id: this.hostId, session_id: sessionId } } : {}),
        attachment_ref: attachmentRef,
        media_type: mediaType,
        ...(name === '' ? {} : { name }),
        size,
        ...(digest ? { sha256: digest } : {}),
      },
      reader: wrapped,
    }
  }

  async closeAttachment(sessionId, attachmentRef, { cancel = false } = {}) {
    const reader = this.#openAttachments.get(`${sessionId}:${attachmentRef}`)
    if (!reader) return
    await reader.close({ cancel })
  }

  // Prove committed occurrence membership from bounded hot/cold observation
  // before any native byte read. A Source that cannot read bytes never scans.
  async resolveAttachment(sessionId, attachmentRef, { signal } = {}) {
    if (!this.attachmentsAvailable()) throw new AdapterError('unsupported_capability', 'attachment occurrence reads unavailable')
    const view = await this.conversation.history.ensureView(sessionId, { signal })
    let beforeSeq
    for (let page = 0; page < 64; page++) {
      const value = await this.nativeEventsPage(sessionId, { beforeSeq, maxMessages: 500, signal })
      if (signal?.aborted) throw new AdapterError('unavailable')
      const occurrence = attachmentOccurrence(sessionId, view.id, value?.events ?? [], attachmentRef)
      if (occurrence) return occurrence
      if (!value?.hasMore) return null
      const first = value.events?.[0]?.dshSeq
      if (!Number.isSafeInteger(first) || first === beforeSeq) return null
      beforeSeq = first
    }
    return null
  }

  #imageReader(native, size, controller) {
    let buffer = null
    let offset = 0
    let closed = false
    return {
      read: async (maxBytes) => {
        if (closed) throw new AdapterError('not_found')
        if (controller.signal.aborted) throw new AdapterError('unavailable')
        if (!Number.isSafeInteger(maxBytes) || maxBytes < 1) throw new AdapterError('invalid_argument')
        if (buffer === null) {
          const data = await this.nativeReadImage(native, controller.signal)
          if (!(data instanceof Uint8Array) || data.length !== size) throw new AdapterError('payload_digest_mismatch', 'image size mismatch')
          buffer = data
        }
        const end = Math.min(size, offset + maxBytes)
        const bytes = buffer.subarray(offset, end)
        offset = end
        return { bytes, eof: offset >= size }
      },
      close: async () => {
        closed = true
        buffer = null
        if (!controller.signal.aborted) controller.abort()
      },
    }
  }

  #fileReader(native, size, controller) {
    let iterator = null
    let pending = null
    let offset = 0
    let closed = false
    const pull = async () => {
      if (iterator === null) iterator = await this.nativeReadFileStream(native, controller.signal)
      return iterator
    }
    return {
      read: async (maxBytes) => {
        if (closed) throw new AdapterError('not_found')
        if (controller.signal.aborted) throw new AdapterError('unavailable')
        if (!Number.isSafeInteger(maxBytes) || maxBytes < 1) throw new AdapterError('invalid_argument')
        if (size === 0) return { bytes: new Uint8Array(0), eof: true }
        const chunks = []
        let produced = 0
        let eof = false
        for (;;) {
          if (produced >= maxBytes) break
          if (!pending) {
            const stream = await pull()
            let next
            try { next = await stream.next() } catch (error) {
              throw new AdapterError(mapAdapterError(error))
            }
            if (next.done) { eof = true; break }
            pending = next.value instanceof Uint8Array ? next.value : Uint8Array.from(next.value ?? [])
            if (pending.length === 0) { pending = null; continue }
          }
          const take = Math.min(maxBytes - produced, pending.length)
          chunks.push(pending.subarray(0, take))
          produced += take
          pending = take === pending.length ? null : pending.subarray(take)
        }
        const bytes = concatBytes(chunks, produced)
        offset += produced
        if (eof && offset !== size) throw new AdapterError('payload_digest_mismatch', 'file size mismatch')
        return { bytes, eof }
      },
      close: async () => {
        closed = true
        pending = null
        if (!controller.signal.aborted) controller.abort()
        try { await iterator?.return?.() } catch { /* native cleanup is best effort */ }
      },
    }
  }

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

function isSubmissionEvent(event, submittedMessageId) {
  if (event?.type !== 'user/message') return false
  const data = event.data ?? {}
  const source = data.source ?? data.message?.source
  return data.id === submittedMessageId ||
    data.message?.id === submittedMessageId ||
    source?.rpcId === submittedMessageId
}

function sha256FromAttachmentId(attachmentId) {
  if (typeof attachmentId !== 'string') return null
  const match = /^sha256:([0-9a-f]{64})$/.exec(attachmentId)
  if (!match) return null
  const bytes = new Uint8Array(32)
  for (let index = 0; index < 32; index++) bytes[index] = Number.parseInt(match[1].slice(index * 2, index * 2 + 2), 16)
  return bytes
}

function concatBytes(chunks, length) {
  const out = new Uint8Array(length)
  let offset = 0
  for (const chunk of chunks) { out.set(chunk, offset); offset += chunk.length }
  return out
}

/** Wrap an arbitrary adapter rejection into a stable wire error. */
export function adapterError(error) {
  if (error instanceof AdapterError || error instanceof PluginError) return error
  return new AdapterError(mapAdapterError(error))
}
