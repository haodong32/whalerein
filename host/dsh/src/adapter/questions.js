// Live handles belong to the source adapter. Nothing here installs an answerer
// or reconstructs an open request from a durable projection.
import { createHash, randomBytes } from 'node:crypto'
import { AdapterError, DefinitiveExecutionError } from 'whalerein-host/errors.js'
import { bytesEqual } from 'whalerein-host/security/digest.js'

function invalid() { throw new AdapterError('invalid_argument', 'invalid question answer') }
function aborted(signal) {
  return new Promise((_, reject) => {
    const fail = () => reject(new DefinitiveExecutionError('conflict'))
    if (signal.aborted) return fail()
    signal.addEventListener('abort', fail, { once: true })
  })
}
export function assertQuestionAdmission(context, now = Date.now) {
  const expiresAt = Number(context?.expiresAt ?? 0)
  if (expiresAt > 0 && now() >= expiresAt) throw new DefinitiveExecutionError('expired')
  if (context?.signal?.aborted) throw new DefinitiveExecutionError(context.signal.reason === 'expired' ? 'expired' : 'unavailable')
}
function string(value) { return typeof value === 'string' ? value : '' }
function localized(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return undefined
  return { translations: Object.fromEntries(Object.entries(value).filter(([, text]) => typeof text === 'string')) }
}
function intent(value) {
  if (!value || typeof value !== 'object') return undefined
  const result = { kind: string(value.kind), approve: string(value.approve) }
  for (const [source, target] of [['reject','reject'], ['always','always'], ['callId','call_id']]) {
    if (typeof value[source] === 'string') result[target] = value[source]
  }
  if (value.subject && typeof value.subject === 'object') {
    result.subject = { name: string(value.subject.name) }
    if (typeof value.subject.icon === 'string') result.subject.icon = value.subject.icon
  }
  if (value.display && typeof value.display === 'object') {
    result.display = {}
    for (const key of ['question', 'detail', 'approve']) {
      const entry = localized(value.display[key])
      if (entry) result.display[key] = entry
    }
  }
  return result
}

export function normalizeQuestionItems(questions) {
  if (!Array.isArray(questions)) throw new AdapterError('unavailable', 'question projection unavailable')
  return questions.map(item => {
    if (!item || typeof item.id !== 'string' || typeof item.question !== 'string') {
      throw new AdapterError('unavailable', 'question projection unavailable')
    }
    const normalized = { id: item.id, question: item.question, multi_select: item.multiSelect === true,
      options: Array.isArray(item.options) ? item.options.map(option => {
        if (!option || typeof option.label !== 'string') throw new AdapterError('unavailable', 'question projection unavailable')
        return { label: option.label, ...(typeof option.description === 'string' ? { description: option.description } : {}) }
      }) : [] }
    if (typeof item.detail === 'string') normalized.detail = item.detail
    if (typeof item.header === 'string') normalized.header = item.header
    const presentation = intent(item.intent)
    if (presentation) normalized.intent = presentation
    return normalized
  })
}

// Preserve the native label/custom encoding, including optional empty custom
// strings. Validation never logs the supplied content.
export function validateQuestionAnswers(questions, answers) {
  if (!Array.isArray(answers) || answers.length !== questions.length) invalid()
  const indexed = new Map(questions.map(question => [question.id, question]))
  if (indexed.size !== questions.length) invalid()
  const seen = new Set()
  return answers.map(answer => {
    const question = indexed.get(answer?.id)
    if (!question || seen.has(answer.id) || !Array.isArray(answer.selected) || answer.selected.some(value => typeof value !== 'string')) invalid()
    seen.add(answer.id)
    if (answer.custom !== undefined && typeof answer.custom !== 'string') invalid()
    return { id: answer.id, selected: [...answer.selected], ...(answer.custom !== undefined ? { custom: answer.custom } : {}) }
  })
}

/** Tracks only handles explicitly admitted by a cooperating native provider. */
export class QuestionHandles {
  #handles = new Map()
  #subscribers = new Set()
  #epoch = randomBytes(16).toString('hex')
  #version = 0

  subscribe(callback) {
    this.#subscribers.add(callback)
    return () => this.#subscribers.delete(callback)
  }
  #emit(type, handle) {
    const event = { type, sessionId: handle.sessionId, batchId: handle.batchId }
    for (const callback of this.#subscribers) { try { callback(event) } catch { /* independent observers */ } }
  }
  listActive(sessionId) {
    return [...this.#handles.values()].filter(handle => handle.sessionId === sessionId)
      .map(({ sessionId, batchId }) => ({ sessionId, batchId }))
  }
  register(sessionId, nativeCallId, dispose) {
    if (!sessionId || !nativeCallId || typeof dispose !== 'function') throw new AdapterError('invalid_argument')
    if ([...this.#handles.values()].some(handle => handle.sessionId === sessionId && handle.nativeCallId === nativeCallId)) throw new AdapterError('conflict')
    const batchId = `${this.#epoch}.${randomBytes(16).toString('hex')}`
    const handle = { sessionId, nativeCallId, batchId, dispose, version: ++this.#version }
    this.#handles.set(batchId, handle)
    this.#emit('requested', handle)
    return {
      batchId,
      release: (type = 'released') => {
        if (this.#handles.get(batchId) !== handle) return false
        this.#handles.delete(batchId)
        try { dispose() } catch {
          this.#emit('release_failed', handle)
          throw new AdapterError('unavailable', 'question handle release failed')
        }
        this.#emit(type, handle)
        return true
      },
    }
  }
  close() {
    for (const handle of [...this.#handles.values()]) {
      this.#handles.delete(handle.batchId)
      let outcome = 'released'
      try { handle.dispose() } catch { outcome = 'release_failed' }
      this.#emit(outcome, handle)
    }
  }
}

// A projection is only discovery. It does not own an open Promise or imply
// that a live root can accept a continued answer.
export class QuestionProjection {
  #epoch = randomBytes(16).toString('hex')
  #ids = new Map()
  #nextVersion = 0

  project(sessionId, value, nativeOwner = '') {
    if (!value || !Array.isArray(value.active)) throw new AdapterError('unavailable', 'question projection unavailable')
    const keys = new Set()
    const batches = value.active.map(native => {
      if (!native || typeof native.callId !== 'string' || !['open', 'continued', 'reply_queued'].includes(native.state)) {
        throw new AdapterError('unavailable', 'question projection unavailable')
      }
      const key = JSON.stringify([sessionId, native.callId])
      if (keys.has(key)) throw new AdapterError('unavailable', 'question projection unavailable')
      keys.add(key)
      const questions = normalizeQuestionItems(native.questions)
      const phase = native.state === 'open' ? 'QUESTION_OPEN'
        : native.state === 'reply_queued' ? 'QUESTION_REPLY_QUEUED' : 'QUESTION_CONTINUED'
      const fingerprint = JSON.stringify([phase, questions, nativeOwner])
      let identity = this.#ids.get(key)
      if (!identity) identity = { batchId: randomBytes(16).toString('hex'), fingerprint: '', version: 0 }
      if (identity.fingerprint !== fingerprint) {
        identity.fingerprint = fingerprint
        identity.version = ++this.#nextVersion
      }
      this.#ids.set(key, identity)
      return { session_id: sessionId, batch_id: identity.batchId, source_epoch: this.#epoch,
        version: identity.version, digest: createHash('sha256').update(JSON.stringify([this.#epoch, sessionId, identity.batchId, identity.version, fingerprint])).digest(),
        phase, questions, answerable: false, content_unavailable: false }
    })
    for (const key of this.#ids.keys()) {
      if (JSON.parse(key)[0] === sessionId && !keys.has(key)) this.#ids.delete(key)
    }
    return batches
  }
  nativeCall(sessionId, batchId) {
    for (const [key, identity] of this.#ids) {
      const [owner, nativeCallId] = JSON.parse(key)
      if (owner === sessionId && identity.batchId === batchId) return nativeCallId
    }
    return null
  }
  clear() { this.#ids.clear(); this.#epoch = randomBytes(16).toString('hex') }
}

// Semantic stamps for the normalized pending-question inventory. An epoch is
// minted per adapter incarnation and again whenever the exact native owner
// object changes; the revision only advances when the normalized observable
// inventory changes. Reads never mint a revision by themselves.
export class QuestionStampTracker {
  #incarnation = randomBytes(16).toString('hex')
  #states = new Map()

  #sameOwner(state, owner) {
    if (!state) return false
    if (Array.isArray(state.owner) || Array.isArray(owner)) {
      return Array.isArray(state.owner) && Array.isArray(owner)
        && state.owner.length === owner.length && state.owner.every((value, index) => value === owner[index])
    }
    return state.owner === owner
  }

  sample(sessionId, owner, inventory) {
    const fingerprint = JSON.stringify(inventory)
    let state = this.#states.get(sessionId)
    if (!this.#sameOwner(state, owner)) {
      state = {
        epoch: `${this.#incarnation}:${randomBytes(8).toString('hex')}`,
        revision: 1,
        fingerprint,
        owner: Array.isArray(owner) ? [...owner] : owner,
      }
      this.#states.set(sessionId, state)
    } else if (state.fingerprint !== fingerprint) {
      state.revision += 1
      state.fingerprint = fingerprint
    }
    return { observation_epoch: state.epoch, revision: state.revision }
  }

  reset() {
    this.#incarnation = randomBytes(16).toString('hex')
    this.#states.clear()
  }
}

// Inventory fields covered by the stamp: ordered membership, content, guards,
// phase, answerability and content-unavailable metadata.
export function questionInventoryFingerprint(batches) {
  return (batches ?? []).map(batch => [
    batch.batch_id ?? '', batch.session_id ?? '', batch.source_epoch ?? '', batch.version ?? 0,
    Buffer.from(batch.digest ?? []).toString('hex'), batch.phase ?? '',
    batch.answerable === true, batch.content_unavailable === true, batch.questions ?? [],
  ])
}

// Observes a live native waterfall while allowing the next provider to run
// immediately. Construction alone installs no listener and claims no wait.
export class OpenQuestionBroker {
  #records = new Map()
  #projection = new QuestionProjection()
  #handles
  #agents
  #service
  #now
  #onChange
  #closed = false
  #nextPosition = 0

  constructor({ agents, userQuestions, handles, onChange = null, now = Date.now }) {
    this.#agents = agents
    this.#service = userQuestions
    this.#handles = handles
    this.#onChange = onChange
    this.#now = now
  }

  #changed(sessionId) {
    try { this.#onChange?.(sessionId) } catch { /* observers are independent */ }
  }

  #isRoot(agent) {
    return Boolean(agent && this.#agents?.get?.(agent.id) === agent && this.#agents?.roots?.().includes(agent))
  }

  async capture(request, next) {
    const agent = request?.agent
    const sourceSignal = request?.signal
    if (this.#closed || !this.#isRoot(agent) || sourceSignal?.aborted) return next()
    const suppliedCallId = request?.wait?.callId
    if (request.wait && (typeof suppliedCallId !== 'string' || !suppliedCallId)) return next()
    // Blocking native requests (notably approval intent) deliberately have no
    // timed wait or call ID. Identity exists only for this exact live request.
    const callId = suppliedCallId ?? `live:${randomBytes(16).toString('hex')}`
    const key = JSON.stringify([agent.id, callId])
    if (this.#records.has(key)) return next()
    let questions
    try { questions = normalizeQuestionItems(request.questions) }
    catch { return next() }
    const localAbort = new AbortController()
    try { request.signal = sourceSignal ? AbortSignal.any([sourceSignal, localAbort.signal]) : localAbort.signal }
    catch { return next() }
    const remote = Promise.withResolvers()
    const ended = Promise.withResolvers()
    const completed = Promise.withResolvers()
    const seq = agent.session?.seq
    const record = { agent, ownerSession: agent.session, callId, questions, request, sourceSignal, remote, ended, completed, busy: false, claim: null,
      position: Number.isSafeInteger(seq) ? seq : null, ordinal: ++this.#nextPosition }
    this.#records.set(key, record)
    this.#changed(agent.id)
    const onAbort = () => ended.resolve({ kind: 'abort', error: sourceSignal.reason })
    sourceSignal?.addEventListener('abort', onAbort, { once: true })
    // Start the next native provider now, not after a remote response. Its
    // completion and the remote response compete within this one waterfall.
    const local = Promise.resolve().then(next)
    const observedLocal = local.then(
      value => ({ kind: 'local', value }),
      error => {
        if (error?.code === 'NO_PROVIDER' && request.wait?.timed === true) return new Promise(() => {})
        throw error
      },
    )
    let winner = 'error'
    try {
      const outcome = await Promise.race([observedLocal, remote.promise, ended.promise])
      winner = outcome.kind
      if (outcome.kind === 'remote' && sourceSignal?.aborted) {
        winner = 'abort'
        throw sourceSignal.reason
      }
      if (outcome.kind === 'abort') throw outcome.error
      if (outcome.kind === 'delegate') return await local
      return outcome.value
    } finally {
      completed.resolve(winner)
      sourceSignal?.removeEventListener('abort', onAbort)
      localAbort.abort()
      if (this.#records.get(key) === record) this.#records.delete(key)
      if (record.claim) { try { record.claim.release() } catch { /* Native cancellation remains authoritative. */ } }
      if (!this.#closed) this.#projection.project(agent.id, this.#view(agent.id))
      if (!this.#closed) this.#changed(agent.id)
    }
  }

  // A record only owns native scope while its exact agent object is still the
  // current root and its capture-time session is unchanged. Replacing either
  // object invalidates the open handle even when the session ID is identical.
  #ownedRecords(sessionId) {
    return [...this.#records.values()].filter(record =>
      record.agent?.id === sessionId && this.#isRoot(record.agent) && record.agent.session === record.ownerSession)
  }

  #view(sessionId, records = this.#ownedRecords(sessionId)) {
    return { active: records.map(record => ({ callId: record.callId, state: 'open', questions: record.request.questions })) }
  }

  list(sessionId) {
    if (this.#closed) return []
    return this.#projection.project(sessionId, this.#view(sessionId)).map(batch => ({ ...batch, answerable: true }))
  }

  nativeCall(sessionId, batchId) { return this.#projection.nativeCall(sessionId, batchId) }

  position(sessionId, batchId) {
    const callId = this.nativeCall(sessionId, batchId)
    const record = this.#records.get(JSON.stringify([sessionId, callId]))
    return record ? { position: record.position, ordinal: record.ordinal } : null
  }

  async answer(request, signal, context = {}) {
    assertQuestionAdmission({ ...context, signal }, this.#now)
    if (this.#closed || signal?.aborted) throw new DefinitiveExecutionError('unavailable')
    const sessionId = request?.session_id
    const batch = this.list(sessionId).find(item => item.batch_id === request?.batch_id)
    if (!batch) throw new DefinitiveExecutionError('conflict')
    if (batch.phase !== request.expected_phase || batch.source_epoch !== request.source_epoch ||
        batch.version !== Number(request.expected_version) || !bytesEqual(batch.digest, request.expected_digest)) throw new DefinitiveExecutionError('conflict')
    const callId = this.#projection.nativeCall(sessionId, batch.batch_id)
    const record = this.#records.get(JSON.stringify([sessionId, callId]))
    if (!record || record.busy || !this.#isRoot(record.agent) || record.agent.session !== record.ownerSession || record.sourceSignal?.aborted) throw new DefinitiveExecutionError('conflict')
    let answers
    try { answers = validateQuestionAnswers(batch.questions, request.answers) }
    catch { throw new DefinitiveExecutionError('invalid_argument') }
    record.busy = true
    const claimAbort = new AbortController()
    const attempt = new AbortController()
    record.answerAbort = attempt
    const claimSignal = AbortSignal.any([signal, claimAbort.signal, record.sourceSignal, attempt.signal].filter(Boolean))
    let iterator
    try {
      if (record.request.wait?.timed) {
        if (typeof this.#service?.attachWait !== 'function') throw new DefinitiveExecutionError('unsupported_capability')
        iterator = this.#service.attachWait(record.agent, callId, claimSignal)
        const opening = await iterator.next()
        assertQuestionAdmission({ ...context, signal }, this.#now)
        if (opening.done) throw new DefinitiveExecutionError('conflict')
        if (attempt.signal.aborted || this.#records.get(JSON.stringify([sessionId, callId])) !== record || signal?.aborted || record.sourceSignal?.aborted || !this.#isRoot(record.agent) || record.agent.session !== record.ownerSession) {
          throw new DefinitiveExecutionError('conflict')
        }
        record.claim = this.#handles.register(sessionId, callId, () => claimAbort.abort())
      }
      if (attempt.signal.aborted || this.#records.get(JSON.stringify([sessionId, callId])) !== record || signal?.aborted || record.sourceSignal?.aborted || !this.#isRoot(record.agent) || record.agent.session !== record.ownerSession) throw new DefinitiveExecutionError('conflict')
      // This is the last synchronous boundary before native settlement.
      assertQuestionAdmission({ ...context, signal }, this.#now)
      record.remote.resolve({ kind: 'remote', value: { answers } })
      const winner = await Promise.race([record.completed.promise, aborted(attempt.signal)])
      if (winner !== 'remote') throw new DefinitiveExecutionError('conflict')
      return { batch_id: batch.batch_id, phase: 'QUESTION_SETTLED' }
    } finally {
      claimAbort.abort()
      attempt.abort()
      // A later interrupt may have released the claim and allowed a new answer
      // to start. Only this attempt may clear the busy flag and claim it owns.
      if (record.answerAbort === attempt) {
        record.answerAbort = null
        if (record.claim) { try { record.claim.release() } catch { /* Return path does not fabricate settlement. */ }; record.claim = null }
        record.busy = false
      }
      if (iterator) await iterator.return()
    }
  }

  // A session interrupt cancels the source turn that owns the claim. Release the
  // claim and settle only this adapter's in-flight answer so a later question is
  // not permanently rejected by a leaked `busy` flag or handle.
  interruptSession(sessionId) {
    for (const record of this.#records.values()) {
      if (record.agent?.id !== sessionId) continue
      try { record.answerAbort?.abort() } catch { /* teardown continues */ }
      if (record.claim) {
        try { record.claim.release('interrupted') } catch { /* the answer finally also resets busy */ }
        record.claim = null
      }
      record.busy = false
    }
  }

  close() {
    this.#closed = true
    for (const record of this.#records.values()) {
      if (record.claim) { try { record.claim.release() } catch { /* Continue detaching other requests. */ } }
      record.ended.resolve({ kind: 'delegate' })
    }
    this.#records.clear()
    this.#projection.clear()
  }
}
