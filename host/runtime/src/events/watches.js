// Channel-scoped observation. Source data is already normalized; this module
// owns only watch lifetime, recipients, encryption and bounded publication.
import { LiveBuffer } from './live-buffer.js'
import { randomUUID } from 'node:crypto'
import { ERROR_CODES, PluginError } from '../errors.js'
import { MESSAGE_BYTES } from '../access/transport.js'
import { codec as defaultCodec, resolveCodec } from '../protocol.js'
import {
  questionDescriptorToProto,
  sameQuestionDescriptor,
  validateQuestionDescriptor,
  validateSessionUpdate,
} from '../contracts/source.js'
import { transportProfileBudget } from '../contracts/transport.js'

const LEASE = 30000
// Bounded replay/settlement memory per shared Source. It suppresses exact
// duplicate live frames and committed batches and retires an Attempt once its
// end or authoritative commit was observed.
const SETTLED_LIMIT = 128
const LIVE_SEEN_LIMIT = 128
const BATCH_SEEN_LIMIT = 512
// Per-channel live publication lane. It bounds latency, not canonical content.
const LIVE_LANE_PUBLICATIONS = 64
const LIVE_LANE_BYTES = 256 * 1024

function attemptKey(attempt, projectionId) {
  return `${projectionId ?? ''}\u0000${attempt?.live_epoch ?? ''}\u0000${attempt?.attempt_id ?? ''}`
}

function blockKey(frame, projectionId) {
  return `${attemptKey(frame.attempt, projectionId)}|${frame.block_id ?? ''}|${frame.delta?.kind ?? frame.reset?.kind ?? ''}`
}

function safeCode(value) {
  return typeof value === 'string' && ERROR_CODES.includes(value) ? value : 'invalid_argument'
}

function boundedSetEntry(map, key, value, limit) {
  if (map.has(key)) map.delete(key)
  map.set(key, value)
  if (map.size > limit) map.delete(map.keys().next().value)
  return value
}
// A questions facet failure is contained unless it disproves authority over the
// whole session. These codes stop every selected facet.
const QUESTION_FATAL_CODES = new Set(['forbidden', 'unauthorized', 'not_found', 'expired', 'invalid_argument', 'protocol_version_incompatible'])
// A source error that proves the session is gone ends every observation of it
// with an explicit Host-authored close instead of leaving the client to time
// out. Transient source failures stay warnings and are retried.
const SESSION_FATAL_CODES = new Set(['forbidden', 'unauthorized', 'not_found', 'expired'])

function sessionCloseReason(code) {
  if (code === 'not_found') return 'session_deleted'
  if (code === 'expired') return 'session_expired'
  return 'unauthorized'
}

function validInterests(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value)
    && typeof value.conversation === 'boolean' && typeof value.questions === 'boolean'
    && (value.conversation || value.questions)
}

function interestsOf(value) {
  return { conversation: value.conversation === true, questions: value.questions === true }
}

function mergeInterests(left, right) {
  return { conversation: left.conversation || right.conversation, questions: left.questions || right.questions }
}

function errorCode(error) {
  return typeof error?.code === 'string' ? error.code : 'unavailable'
}

export class WatchedEvents {
  #watches = new Map()
  #sources = new Map()
  #lanes = new Map()
  #timer = null
  #outboxDrops = 0
  #snapshotRequired = new Map()
  constructor({ hostId, store, security, adapter, access, codec = defaultCodec, now = Date.now, config = {}, logger }) {
    Object.assign(this, { hostId, store, security, adapter, access, now, config, logger }); this.codec = resolveCodec(codec)
  }
  start() { this.#timer = setInterval(() => this.#expire(), 1000); this.#timer.unref?.(); return this }
  async stop() {
    clearInterval(this.#timer); this.#timer = null
    for (const id of [...this.#watches.keys()]) this.#remove(id)
    await Promise.all([...this.#lanes.values()].flatMap(l => [l.live, l.committed]))
    this.#lanes.clear()
  }
  #expire() { for (const [id, w] of this.#watches) if (w.expires <= this.now() || !this.security.getChannel(w.channelId)) this.#remove(id) }
  #remove(id) {
    const watch = this.#watches.get(id); if (!watch) return
    this.#watches.delete(id)
    const source = this.#sources.get(watch.sessionId)
    source?.recipients.delete(id)
    if (source && !source.recipients.size) {
      source.controller.abort()
      source.buffer?.flush()
      source.subscription?.close()
      this.#sources.delete(watch.sessionId)
    }
  }
  // End every observation of one Source with a Host-authored terminal event.
  // The event is always written to the outbox and also published live when
  // possible, so a disconnected client receives it on reconnect. A stale
  // watch_id is ignored by a new observation.
  #closeSession(sessionId, reason) {
    const source = this.#sources.get(sessionId)
    if (!source) return
    for (const id of [...source.recipients]) {
      const watch = this.#watches.get(id)
      if (watch) this.#enqueueClosed(watch, reason)
    }
  }

  // A source error code that proves the session is gone ends every
  // observation of it; transient failures stay with the caller to report or
  // retry.
  #closeIfGone(sessionId, code) {
    if (!SESSION_FATAL_CODES.has(code)) return false
    this.#closeSession(sessionId, sessionCloseReason(code))
    return true
  }

  // Seal one watch event and wrap it for delivery. A missing key means the
  // event cannot be delivered, so the caller drops it.
  #envelope(watch, plaintext, at) {
    const sealed = this.security.sealEvent({
      deviceId: watch.deviceId, channelId: watch.channelId, plaintext, createdAt: at,
    })
    if (!sealed) return null
    return this.codec.encode('whalerein.v1.EventEnvelope', {
      host_id: this.hostId, device_id: watch.deviceId, channel_id: watch.channelId,
      nonce: sealed.nonce, ciphertext: sealed.ciphertext, created_at_unix_ms: at,
    })
  }

  // The outbox row is the durable copy of one watch event.
  #addOutbox(watch, envelope, at) {
    return this.store.addOutbox({
      deviceId: watch.deviceId, channelId: watch.channelId, payload: envelope, now: at,
      perDeviceLimit: this.config.outboxPerDeviceLimit ?? 100,
      totalLimit: this.config.outboxTotalLimit ?? 1000,
    })
  }

  #enqueueClosed(watch, reason) {
    const plaintext = this.codec.encode('whalerein.v1.EventPayload', {
      watch_closed: {
        watch_id: watch.id,
        session: { host_id: this.hostId, session_id: watch.sessionId },
        reason,
      },
    })
    const at = this.now()
    const envelope = this.#envelope(watch, plaintext, at)
    if (!envelope) return
    // The durable copy is the delivery guarantee; the live copy is latency.
    const row = this.#addOutbox(watch, envelope, at)
    if (row.overflow && !this.#snapshotRequired.has(watch.channelId)) {
      this.#snapshotRequired.set(watch.channelId, { deviceId: watch.deviceId, droppedUpdate: false })
    }
    if (this.access?.connected) this.access.publishEvent(envelope).catch(() => {})
    this.#remove(watch.id)
  }

  #owned(deviceId, channelId, id) {
    this.#expire(); const watch = this.#watches.get(id)
    if (!watch || watch.deviceId !== deviceId || watch.channelId !== channelId) throw new PluginError('expired', 'watch unavailable')
    return watch
  }
  #conversationRecipients(sessionId) {
    const source = this.#sources.get(sessionId)
    if (!source) return false
    for (const id of source.recipients) if (this.#watches.get(id)?.interests.conversation) return true
    return false
  }
  #questionRecipients(sessionId) {
    const source = this.#sources.get(sessionId)
    if (!source) return false
    for (const id of source.recipients) if (this.#watches.get(id)?.interests.questions) return true
    return false
  }
  async watch(deviceId, channelId, sessionId, signal, interests) {
    if (!validInterests(interests)) throw new PluginError('invalid_argument', 'watch interests required')
    if (signal?.aborted) throw new PluginError('unavailable')
    this.#expire()
    const channel = this.security.getChannel(channelId)
    if (!channel || channel.deviceId !== deviceId) throw new PluginError('unauthorized', 'channel unavailable')
    const requested = interestsOf(interests)
    let watch = [...this.#watches.values()].find(w => w.deviceId === deviceId && w.channelId === channelId && w.sessionId === sessionId)
    if (!watch) {
      if (this.#watches.size >= 512 || [...this.#watches.values()].filter(w => w.deviceId === deviceId).length >= 8) throw new PluginError('rate_limited', 'watch budget exceeded')
      if (!this.#sources.has(sessionId) && this.#sources.size >= 64) throw new PluginError('rate_limited', 'source watch budget exceeded')
      watch = { id: randomUUID(), deviceId, channelId, sessionId, interests: requested, expires: this.now() + LEASE }
      this.#watches.set(watch.id, watch)
      let source = this.#sources.get(sessionId)
      if (!source) {
        source = {
          recipients: new Set(), controller: new AbortController(),
          buffer: new LiveBuffer(update => this.#fanout(sessionId, update), this.config.eventBatchMs ?? 20),
          registration: null, subscription: null, stateTask: null,
          questionTask: null, questionDirty: false, questionGeneration: 0, questionsAccepted: null,
          projectionId: null, settled: new Map(), liveSeen: new Map(), batches: new Map(),
        }
        this.#sources.set(sessionId, source)
        source.recipients.add(watch.id)
        // Registration is shared. Every observer that joins while it is
        // pending waits for the same outcome, so a rejected registration
        // cannot leave a later observer holding a live-looking but
        // subscription-less source.
        source.registration = new Promise((resolve, reject) => {
          try { resolve(this.adapter.watch(sessionId, update => this.#receive(sessionId, update))) }
          catch (error) { reject(error) }
        })
        try {
          const subscription = await source.registration
          if (signal?.aborted || this.#sources.get(sessionId) !== source || !source.recipients.size) {
            subscription.close()
            throw new PluginError('expired', 'watch cancelled during source registration')
          }
          source.subscription = subscription
        }
        catch (error) {
          // No observer may keep a watch whose source never registered.
          if (this.#sources.get(sessionId) === source) {
            this.#sources.delete(sessionId)
            source.controller.abort()
          }
          this.#remove(watch.id)
          throw error
        }
      } else {
        source.recipients.add(watch.id)
        if (!source.subscription) {
          try { await source.registration }
          catch (error) { this.#remove(watch.id); throw error }
          if (this.#sources.get(sessionId) !== source || !source.subscription) {
            this.#remove(watch.id)
            throw new PluginError('unavailable', 'source subscription unavailable')
          }
        }
      }
    } else {
      // A second observer may select a facet the first did not. Interest is a
      // union for the shared Source observation; delivery stays per recipient.
      watch.interests = mergeInterests(watch.interests, requested)
    }
    try {
      const ready = await this.#ready(watch, signal)
      watch.observing = true
      return ready
    } catch (error) {
      this.#remove(watch.id)
      throw error
    }
  }
  async #readQuestionDescriptor(sessionId, signal) {
    let raw
    try { raw = await this.adapter.readQuestionStamp(sessionId, { signal }) }
    catch (error) {
      if (signal?.aborted) throw new PluginError('unavailable')
      const code = errorCode(error)
      if (QUESTION_FATAL_CODES.has(code)) throw error
      return { availability: 'unavailable', code }
    }
    if (signal?.aborted) throw new PluginError('unavailable')
    try { return validateQuestionDescriptor(raw) }
    catch { return { availability: 'unavailable', code: 'unavailable' } }
  }
  async #ready(watch, signal) {
    if (signal?.aborted) throw new PluginError('unavailable')
    const interests = watch.interests
    const source = this.#sources.get(watch.sessionId)
    const questionGeneration = source?.questionGeneration ?? 0
    const stateRead = interests.conversation
      ? this.adapter.readState(watch.sessionId, { signal }).then(state => {
        if (signal?.aborted) throw new PluginError('unavailable')
        if (state.session?.session_id !== watch.sessionId || !state.history_head || !state.live_epoch) throw new PluginError('unavailable', 'source state unavailable')
        return state
      }).catch(error => {
        // An authoritative read proved the session is gone. Close every
        // observation of it with a Host-authored event before the client's
        // own lease or timeout hides the reason.
        if (this.#closeIfGone(watch.sessionId, errorCode(error))) throw new PluginError('not_found', 'session ended')
        throw error
      })
      : null
    const questionsRead = interests.questions ? this.#readQuestionDescriptor(watch.sessionId, signal) : null
    const [state, questions] = await Promise.all([stateRead, questionsRead])
    if (!this.#watches.has(watch.id) || !this.security.getChannel(watch.channelId)) throw new PluginError('expired', 'watch expired during state read')
    watch.expires = this.now() + LEASE
    const ready = { watch_id: watch.id, lease_ms: LEASE }
    if (state) {
      ready.state = { ...state, session: { host_id: this.hostId, session_id: watch.sessionId } }
      if (questions) ready.state.questions = questionDescriptorToProto(questions)
    }
    if (questions) {
      ready.questions = questionDescriptorToProto(questions)
      // A concurrent invalidation read owns the newer accepted value. Only the
      // unchanged generation may install this baseline.
      if (source && this.#sources.get(watch.sessionId) === source && source.questionGeneration === questionGeneration) {
        source.questionsAccepted = questions
      }
    }
    return ready
  }
  async refresh(deviceId, channelId, ids, signal) {
    if (!Array.isArray(ids) || ids.length > 8 || new Set(ids).size !== ids.length) throw new PluginError('invalid_argument', 'invalid watch selection')
    return { watches: await Promise.all(ids.map(id => this.#ready(this.#owned(deviceId, channelId, id), signal))) }
  }
  unwatch(deviceId, channelId, id) { this.#owned(deviceId, channelId, id); this.#remove(id) }
  async readHistory(sessionId, read, signal) {
    const page = await this.adapter.readHistory(sessionId, read, { signal })
    if (page.session?.session_id !== sessionId) throw new PluginError('invalid_argument', 'source history scope mismatch')
    page.session = { host_id: this.hostId, session_id: sessionId }
    validateSessionUpdate({ session: page.session, history: page.batch })
    return page
  }
  // Authoritative question reconciliation. Only one read is in flight per
  // Source; an invalidation during that read advances a generation so the
  // completion is discarded and one more read is guaranteed. This is not the
  // swallowed state-task pattern.
  #reconcileQuestions(sessionId) {
    const source = this.#sources.get(sessionId)
    if (!source) return
    // Conversation-only observation never performs question inventory reads.
    if (!this.#questionRecipients(sessionId)) return
    source.questionGeneration += 1
    if (source.questionTask) { source.questionDirty = true; return }
    const run = async () => {
      do {
        source.questionDirty = false
        const generation = source.questionGeneration
        let descriptor
        try { descriptor = await this.#readQuestionDescriptor(sessionId, source.controller.signal) }
        catch (error) {
          if (source.controller.signal.aborted || this.#sources.get(sessionId) !== source) return
          const code = errorCode(error)
          if (this.#closeIfGone(sessionId, code)) return
          descriptor = { availability: 'unavailable', code }
        }
        if (this.#sources.get(sessionId) !== source || source.controller.signal.aborted) return
        // A late old read can never regress the accepted observation.
        if (generation !== source.questionGeneration) continue
        if (sameQuestionDescriptor(source.questionsAccepted, descriptor)) continue
        source.questionsAccepted = descriptor
        this.#fanout(sessionId, {
          session: { host_id: this.hostId, session_id: sessionId },
          questions: questionDescriptorToProto(descriptor),
        })
      } while (source.questionDirty && this.#sources.get(sessionId) === source && !source.controller.signal.aborted)
    }
    source.questionTask = run().finally(() => {
      source.questionTask = null
      if (source.questionDirty) this.#reconcileQuestions(sessionId)
    })
  }
  #receive(sessionId, notification) {
    if (!notification || typeof notification !== 'object') return
    if (notification.session?.session_id !== sessionId) {
      this.logger?.warn('source_update_rejected', { code: 'invalid_argument', reason: 'scope' })
      return
    }
    if (notification.questions_changed === true) {
      this.#reconcileQuestions(sessionId)
      if (!notification.state_changed) return
    }
    if (notification.state_changed) {
      // Notifications are hints. Native state is read authoritatively and also
      // reconciled on RefreshWatch if a final notification was lost. A
      // questions-only observation never performs conversation reads.
      if (!this.#conversationRecipients(sessionId)) return
      const source = this.#sources.get(sessionId)
      if (!source) return
      if (!source.stateTask) {
        source.stateTask = this.adapter.readState(sessionId, { signal: source.controller.signal }).then(state => {
          if (this.#sources.get(sessionId) !== source) return
          if (state.session?.session_id !== sessionId) throw new PluginError('invalid_argument')
          state.session = { host_id: this.hostId, session_id: sessionId }
          this.#receive(sessionId, { session: { session_id: sessionId }, state })
        }).catch((error) => {
          const code = errorCode(error)
          if (!this.#closeIfGone(sessionId, code)) this.logger?.warn('source_state_unavailable', { code })
        }).finally(() => { source.stateTask = null })
      }
      return
    }
    let update
    try {
      update = validateSessionUpdate({ ...notification, session: { host_id: this.hostId, session_id: sessionId } })
    } catch (error) {
      this.logger?.warn('source_update_rejected', { code: safeCode(error?.code), reason: 'shape' })
      return
    }
    this.#expire()
    const source = this.#sources.get(sessionId)
    if (!source) return
    try {
      if (update.live) {
        if (!this.#admitLive(source, update.live)) return
      } else if (update.history) {
        if (!this.#admitHistory(source, update.history)) return
      } else if (update.state?.projection_id !== undefined) {
        this.#adoptProjection(source, update.state.projection_id)
      }
    } catch (error) {
      this.logger?.warn('source_update_rejected', { code: safeCode(error?.code), reason: 'stream' })
      return
    }
    source.buffer.push(update)
  }

  /**
   * Projection is part of preview identity. A changed projection invalidates
   * every remembered attempt; old previews cannot survive the switch.
   */
  #adoptProjection(source, projectionId) {
    if (source.projectionId === projectionId) return
    source.buffer?.flush()
    source.projectionId = projectionId
    source.settled.clear()
    source.liveSeen.clear()
  }

  #markSettled(source, attempt) {
    if (!attempt) return
    boundedSetEntry(source.settled, attemptKey(attempt, source.projectionId), true, SETTLED_LIMIT)
    for (const key of [...source.liveSeen.keys()]) {
      if (key.startsWith(`${attemptKey(attempt, source.projectionId)}|`)) source.liveSeen.delete(key)
    }
  }

  /** Reject duplicate/retired previews; accept valid intervals for fanout. */
  #admitLive(source, frame) {
    const settled = source.settled.has(attemptKey(frame.attempt, source.projectionId))
    if (frame.started) {
      // A replayed start for a settled Attempt cannot reopen preview output.
      if (settled) {
        this.logger?.warn('source_update_rejected', { code: 'expired', reason: 'attempt_settled' })
        return false
      }
      const startKey = `${attemptKey(frame.attempt, source.projectionId)}|started`
      const started = source.liveSeen.get(startKey)
      if (started !== undefined && frame.last_seq <= started) {
        this.logger?.warn('source_update_rejected', { code: 'conflict', reason: 'duplicate_start' })
        return false
      }
      boundedSetEntry(source.liveSeen, startKey, frame.last_seq, LIVE_SEEN_LIMIT)
      return true
    }
    if (frame.ended) {
      // The pending preview belongs before its settlement. LiveBuffer flush is
      // triggered by the missing delta payload before this update is published.
      this.#markSettled(source, frame.attempt)
      return true
    }
    if (frame.delta || frame.reset) {
      if (settled) {
        this.logger?.warn('source_update_rejected', { code: 'expired', reason: 'preview_retired' })
        return false
      }
      const key = blockKey(frame, source.projectionId)
      const previous = source.liveSeen.get(key)
      if (previous !== undefined && frame.last_seq <= previous) {
        this.logger?.warn('source_update_rejected', { code: 'conflict', reason: 'duplicate_preview' })
        return false
      }
      boundedSetEntry(source.liveSeen, key, frame.last_seq, LIVE_SEEN_LIMIT)
    }
    return true
  }

  /** Drop exact committed replays and remember the Attempt settled by a commit. */
  #admitHistory(source, batch) {
    const batchId = batch.batch_id
    if (source.batches.has(batchId)) {
      this.logger?.warn('source_update_rejected', { code: 'conflict', reason: 'duplicate_batch' })
      return false
    }
    boundedSetEntry(source.batches, batchId, true, BATCH_SEEN_LIMIT)
    if (batch.projection_id !== undefined) this.#adoptProjection(source, batch.projection_id)
    for (const change of batch.changes ?? []) {
      if (change?.origin) this.#markSettled(source, change.origin)
    }
    // A commit settles before its preview lane can publish; the pending
    // preview is flushed by the non-delta update in LiveBuffer.
    return true
  }

  #fanout(sessionId, update) {
    const source = this.#sources.get(sessionId)
    if (!source) return
    const questionsOnly = update.questions != null
    // One shared event route is bounded by its smallest authenticated receiver
    // and by local Host policy. A larger receiver cannot raise it.
    const budget = this.#eventBudget(source)
    for (const id of source.recipients) {
      const watch = this.#watches.get(id); if (!watch) continue
      if (questionsOnly ? !watch.interests.questions : !watch.interests.conversation) continue
      this.#enqueue(watch, { ...update, watch_id: id }, budget)
    }
  }

  #eventBudget(source) {
    let limit = this.config.transportBudget?.envelope_bytes
    if (!Number.isSafeInteger(limit) || limit < 1) {
      try { limit = transportProfileBudget(this.config.transportProfile ?? 'default').envelope_bytes }
      catch { limit = MESSAGE_BYTES }
    }
    for (const id of source.recipients) {
      const watch = this.#watches.get(id)
      if (!watch) continue
      const receive = this.security.getChannelReceive?.(watch.channelId)
      const advertised = receive?.budget?.envelope_bytes
      if (Number.isSafeInteger(advertised) && advertised > 0) limit = Math.min(limit, advertised)
    }
    return limit
  }

  /**
   * An oversized complete event is not delivered partially. The durable outbox
   * receives a tiny HistoryReset instead and the channel is marked for snapshot
   * repair; canonical content is repaired from committed history.
   */
  #eventTooLarge(watch, kind, bytes, limit) {
    const previous = this.#snapshotRequired.get(watch.channelId)
    this.#snapshotRequired.set(watch.channelId, {
      deviceId: watch.deviceId, droppedUpdate: true, resetSent: previous?.resetSent ?? false,
    })
    this.logger?.warn('event_oversize', { code: 'payload_too_large', kind, bytes, limit })
    // One reset per channel is enough; repeated oversized updates cannot
    // amplify the outbox while the client repairs from committed history.
    if (previous?.resetSent) return
    const plaintext = this.codec.encode('whalerein.v1.EventPayload', {
      update: {
        session: { host_id: this.hostId, session_id: watch.sessionId },
        reset: { reason: 'snapshot_required' },
        watch_id: watch.id,
      },
    })
    const at = this.now()
    const envelope = this.#envelope(watch, plaintext, at)
    if (!envelope || envelope.length > limit) return
    const row = this.#addOutbox(watch, envelope, at)
    if (row.overflow) {
      this.#outboxDrops++
      this.logger?.warn('event_outbox_full', { code: 'snapshot_required', reason: row.reason })
      return
    }
    this.#snapshotRequired.get(watch.channelId).resetSent = true
    if (this.access?.connected) this.access.publishEvent(envelope).catch(() => {})
  }

  #enqueue(watch, update, budget = MESSAGE_BYTES) {
    const plaintext = this.codec.encode('whalerein.v1.EventPayload', { update })
    const at = this.now()
    const envelope = this.#envelope(watch, plaintext, at)
    if (!envelope) return
    if (envelope.length > budget) {
      this.#eventTooLarge(watch, update.live ? 'live' : update.history ? 'committed' : 'control', envelope.length, budget)
      return
    }
    let lane = this.#lanes.get(watch.channelId)
    if (!lane) {
      lane = { live: Promise.resolve(), committed: Promise.resolve(), bytes: 0, count: 0 }
      this.#lanes.set(watch.channelId, lane)
    }
    if (update.live) {
      if (!this.access?.connected || lane.count >= LIVE_LANE_PUBLICATIONS || lane.bytes + envelope.length > LIVE_LANE_BYTES) return
      lane.count++
      lane.bytes += envelope.length
      lane.live = lane.live.then(() => {
        if (this.now() - at <= 1000 && this.#watches.has(watch.id) && this.security.getChannel(watch.channelId) && this.access.connected) {
          return this.access.publishEvent(envelope)
        }
      }).catch(() => {}).finally(() => {
        lane.count--
        lane.bytes -= envelope.length
      })
      return
    }

    const row = this.#addOutbox(watch, envelope, at)
    if (row.overflow) {
      this.#outboxDrops++
      this.#snapshotRequired.set(watch.channelId, { deviceId: watch.deviceId, droppedUpdate: true })
      this.logger?.warn('event_outbox_full', { code: 'snapshot_required', reason: row.reason })
      return
    }
    if (!this.access?.connected) return
    lane.committed = lane.committed.then(async () => {
      if (!this.#watches.has(watch.id)) return
      await this.access.publishEvent(envelope)
      this.store.removeOutbox(row.id)
    }).catch(() => this.logger?.warn('event_publish_failed', { code: 'unavailable' }))
  }

  async flushOutbox() {
    if (!this.access?.connected) return 0
    let count = 0
    for (const row of this.store.listOutbox(this.config.outboxTotalLimit ?? 1000)) {
      if (!this.security.getChannel(row.channelId)) { this.store.removeOutbox(row.id); continue }
      try { await this.access.publishEvent(row.payload); this.store.removeOutbox(row.id); count++ } catch { break }
    }
    return count
  }
  #clearChannelEvents(deviceId, channelId) {
    for (const [id, w] of this.#watches) if (w.deviceId === deviceId && w.channelId === channelId) this.#remove(id)
    this.store.deleteOutboxForChannel(deviceId, channelId)
    this.#lanes.delete(channelId)
    this.#snapshotRequired.delete(channelId)
  }

  onChannelsCleared(channels) {
    for (const { deviceId, channelId } of channels) this.#clearChannelEvents(deviceId, channelId)
  }

  onDeviceOffline(deviceId) {
    const observedChannels = new Set()
    for (const watch of this.#watches.values()) {
      if (watch.deviceId === deviceId) observedChannels.add(watch.channelId)
    }
    for (const channelId of observedChannels) this.#clearChannelEvents(deviceId, channelId)
  }

  observingDeviceIds() {
    this.#expire()
    const devices = new Set()
    for (const watch of this.#watches.values()) {
      if (watch.observing) devices.add(watch.deviceId)
    }
    return [...devices].sort()
  }

  describe() {
    const devicesNeedingSnapshot = new Set()
    const devicesWithDroppedUpdates = new Set()
    for (const { deviceId, droppedUpdate } of this.#snapshotRequired.values()) {
      devicesNeedingSnapshot.add(deviceId)
      if (droppedUpdate) devicesWithDroppedUpdates.add(deviceId)
    }
    return {
      watches: this.#watches.size, sessions: this.#sources.size, pending: this.store.countOutbox(),
      outboxDrops: this.#outboxDrops,
      snapshotRequired: devicesNeedingSnapshot.size,
      droppedDevices: devicesWithDroppedUpdates.size,
    }
  }
}
