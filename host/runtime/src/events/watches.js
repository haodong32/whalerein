// Channel-scoped observation. Source data is already normalized; this module
// owns only watch lifetime, recipients, encryption and bounded publication.
import { LiveBuffer } from './live-buffer.js'
import { randomUUID } from 'node:crypto'
import { PluginError } from '../errors.js'
import { codec as defaultCodec, resolveCodec } from '../protocol.js'
import {
  questionDescriptorToProto,
  sameQuestionDescriptor,
  validateQuestionDescriptor,
  validateSessionUpdate,
} from '../contracts/source.js'

const LEASE = 30000
// A questions facet failure is contained unless it disproves authority over the
// whole session. These codes stop every selected facet.
const QUESTION_FATAL_CODES = new Set(['forbidden', 'unauthorized', 'not_found', 'expired', 'invalid_argument', 'protocol_version_incompatible'])

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
  outboxDrops = 0
  droppedByDevice = new Map()
  outboxSnapshotRequired = new Set()
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
    try { return await this.#ready(watch, signal) } catch (error) { this.#remove(watch.id); throw error }
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
          descriptor = { availability: 'unavailable', code: errorCode(error) }
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
      this.logger?.warn('source_update_rejected', { code: 'invalid_argument' })
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
        }).catch(() => this.logger?.warn('source_state_unavailable', { code: 'unavailable' })).finally(() => { source.stateTask = null })
      }
      return
    }
    try {
      const update = validateSessionUpdate({ ...notification, session: { host_id: this.hostId, session_id: sessionId } })
      this.#expire()
      this.#sources.get(sessionId)?.buffer.push(update)
    } catch { this.logger?.warn('source_update_rejected', { code: 'invalid_argument' }) }
  }
  #fanout(sessionId, update) {
    const questionsOnly = update.questions != null
    for (const id of this.#sources.get(sessionId)?.recipients ?? []) {
      const watch = this.#watches.get(id); if (!watch) continue
      if (questionsOnly ? !watch.interests.questions : !watch.interests.conversation) continue
      this.#enqueue(watch, { ...update, watch_id: id })
    }
  }
  #enqueue(watch, update) {
    const channel = this.security.getChannel(watch.channelId); if (!channel) return
    const plaintext = this.codec.encode('whalerein.v1.EventPayload', { update })
    if (update.live && plaintext.length > 256 * 1024) return
    const at = this.now()
    const sealed = this.security.sealEvent({ deviceId: watch.deviceId, channelId: watch.channelId, plaintext, createdAt: at })
    if (!sealed) return
    const envelope = this.codec.encode('whalerein.v1.EventEnvelope', { host_id: this.hostId, device_id: watch.deviceId, channel_id: watch.channelId, nonce: sealed.nonce, ciphertext: sealed.ciphertext, created_at_unix_ms: at })
    let lane = this.#lanes.get(watch.deviceId)
    if (!lane) { lane = { live: Promise.resolve(), committed: Promise.resolve(), bytes: 0, count: 0 }; this.#lanes.set(watch.deviceId, lane) }
    if (update.live) {
      if (!this.access?.connected || lane.count >= 64 || lane.bytes + envelope.length > 256 * 1024) return
      lane.count++; lane.bytes += envelope.length
      lane.live = lane.live.then(() => { if (this.now() - at <= 1000 && this.#watches.has(watch.id) && this.security.getChannel(watch.channelId) && this.access.connected) return this.access.publishEvent(envelope) }).catch(() => { }).finally(() => { lane.count--; lane.bytes -= envelope.length })
    } else {
      const row = this.store.addOutbox({ deviceId: watch.deviceId, channelId: watch.channelId, payload: envelope, now: at, perDeviceLimit: this.config.outboxPerDeviceLimit ?? 100, totalLimit: this.config.outboxTotalLimit ?? 1000 })
      if (row.overflow) {
        // A dropped committed envelope is a cursor gap. Keep bounded drop
        // accounting and require a snapshot instead of silently continuing.
        this.outboxDrops += 1
        this.droppedByDevice.set(watch.deviceId, (this.droppedByDevice.get(watch.deviceId) ?? 0) + 1)
        this.outboxSnapshotRequired.add(watch.deviceId)
        this.logger?.warn('event_outbox_full', { code: 'snapshot_required', reason: row.reason })
        return
      }
      if (this.access?.connected) lane.committed = lane.committed.then(async () => { if (!this.#watches.has(watch.id)) return; await this.access.publishEvent(envelope); this.store.removeOutbox(row.id) }).catch(() => this.logger?.warn('event_publish_failed', { code: 'unavailable' }))
    }
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
  onChannelReplaced(deviceId, channelId) {
    for (const [id, w] of this.#watches) if (w.deviceId === deviceId && w.channelId === channelId) this.#remove(id)
    this.store.deleteOutboxForChannel(deviceId, channelId)
    this.#lanes.delete(deviceId)
    // A replaced/cleared channel starts a new client view; the resync demand
    // belongs to the previous channel and must not leak across identities.
    this.outboxSnapshotRequired.delete(deviceId)
    this.droppedByDevice.delete(deviceId)
  }
  onChannelsCleared(channels) { for (const { deviceId, channelId } of channels ?? []) this.onChannelReplaced(deviceId, channelId) }
  describe() {
    return {
      watches: this.#watches.size, sessions: this.#sources.size, pending: this.store.countOutbox(),
      outboxDrops: this.outboxDrops, snapshotRequired: this.outboxSnapshotRequired.size, droppedDevices: this.droppedByDevice.size,
    }
  }
}
