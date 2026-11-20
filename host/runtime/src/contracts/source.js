// Harness-neutral source seam. Implementations own native history and actions;
// neither credentials nor a Host database belongs on this interface.
//
// This module is also the executable normalized contract. It mirrors the wire
// invariants in proto/v1/whalerein.proto so a Source adapter cannot invent a
// partial shape that the Host later serializes as an empty success.
import { AdapterError, DefinitiveExecutionError } from '../errors.js'
import { validateId } from './ids.js'

function requireId(kind, value, message) {
  if (!validateId(kind, value).ok) throw new AdapterError('invalid_argument', message)
  return value
}

function requireString(value, message, { allowEmpty = false } = {}) {
  if (typeof value !== 'string' || (!allowEmpty && value === '')) {
    throw new AdapterError('invalid_argument', message)
  }
  return value
}

function optionalString(value, message, maxBytes = 4096) {
  if (value === undefined) return undefined
  if (typeof value !== 'string' || Buffer.byteLength(value, 'utf8') > maxBytes) {
    throw new AdapterError('invalid_argument', message)
  }
  return value
}

function safeInteger(value, message, { min = 0 } = {}) {
  if (!Number.isSafeInteger(value) || value < min) throw new AdapterError('invalid_argument', message)
  return value
}

function knownEnum(value, allowed, message) {
  if (!allowed.has(value)) throw new AdapterError('invalid_argument', message)
  return value
}

export class SourceAdapter {
  async open({metadata}={}) { this.metadata=metadata }
  async close() {}
  async describeHostCapabilities() { return {} }
  async executeHost() { throw new DefinitiveExecutionError('unsupported_capability') }
  async listSessions() { throw new AdapterError('unsupported_capability') }
  async getSession() { throw new AdapterError('unsupported_capability') }
  watch() { throw new AdapterError('unsupported_capability') }
  // Forward synchronization reads `after_cursor`; an independent interactive
  // read selects `page` (latest-first) or `exact` (one committed message).
  async readHistory() { throw new AdapterError('unsupported_capability') }
  // Typed send follow-up. A committed answer returns a scoped
  // CommittedMessageRef; bodies stay in history.
  async readMessageReply() { throw new AdapterError('unsupported_capability') }
  async listModels() { throw new AdapterError('unsupported_capability') }
  async readSessionModel() { throw new AdapterError('unsupported_capability') }
  async readPendingQueue() { throw new AdapterError('unsupported_capability') }
  async readState() { throw new AdapterError('unsupported_capability') }
  async readWaitState() { throw new AdapterError('unsupported_capability') }
  // Wait observes execution only. Aborting the signal must not interrupt it.
  async waitSession() { throw new AdapterError('unsupported_capability') }
  async listApprovals() { throw new AdapterError('unsupported_capability') }
  async getApproval() { throw new AdapterError('unsupported_capability') }
  // Authoritative pending-question inventory. A successful read resolves
  // { batches, next_cursor: '', stamp } where `stamp` certifies the complete
  // normalized collection sampled, not this wire page.
  async listQuestionPage() { throw new AdapterError('unsupported_capability') }
  // Read the same authoritative inventory stamp without publishing a page.
  // Unsupported is an explicit result so a Host watch stays usable for
  // Conversation; it is not an empty available collection.
  async readQuestionStamp() { return { availability: 'unsupported', code: 'unsupported_capability' } }
  async getQuestion() { throw new AdapterError('unsupported_capability') }
  // Open one authorized attachment occurrence. Resolves:
  //   {
  //     attachment: {
  //       media_type: string,
  //       name?: string,        // display suggestion; Host sanitizes it
  //       size: number,         // authoritative declared length in bytes
  //       sha256?: Uint8Array   // only when the Source can prove it
  //     },
  //     policy?: {              // the Source's own stricter limits, if any
  //       max_total_bytes, max_chunk_bytes, max_concurrent,
  //       idle_timeout_ms, overall_timeout_ms
  //     },
  //     reader: {
  //       read(maxBytes) -> { data: Uint8Array, final: boolean },
  //       close({cancel}) -> void | Promise<void>
  //     }
  //   }
  // The adapter resolves the exact committed occurrence inside the target
  // session and proves positive membership before opening any native byte
  // read. `transfer_id` and `expires_at_unix_ms` are Host-owned; a Source must
  // not supply them. Unsupported capabilities throw `unsupported_capability`;
  // an absent occurrence throws `not_found` before any native byte read.
  async openAttachment() { throw new AdapterError('unsupported_capability') }
  // Optional fallback when a Source cannot return a reader from openAttachment:
  // read the next contiguous part. `maxBytes` is a request bound; the adapter
  // returns at most the effective chunk budget as { data, final }.
  async pullAttachment() { throw new AdapterError('unsupported_capability') }
  // Dispose a reader. `cancel: true` is an explicit cancellation; false closes
  // a completed or idle transfer. Close is idempotent.
  async closeAttachment() { throw new AdapterError('unsupported_capability') }
  // Resolve with a confirmed native receipt. Only DefinitiveExecutionError
  // establishes failure; any other exception can follow an executed action.
  // An optional third argument carries an opaque operationId stable for the
  // journaled Host/device/command identity. It contains no command payload.
  // Implementations must check context.signal after asynchronous prechecks and
  // before native mutation. Cancellation after native entry does not prove
  // that the mutation failed.
  async execute() { throw new DefinitiveExecutionError('unsupported_capability') }
}

const QUESTION_AVAILABILITIES = new Set(['available','unavailable','unsupported'])

// A Source-owned collection stamp. Revisions are opaque positive values; the
// Host only compares identity, so an exactly representable JavaScript integer
// or its unsigned decimal string form is accepted.
export function validateQuestionStamp(stamp) {
  if (!stamp || typeof stamp !== 'object' || Array.isArray(stamp)) throw new AdapterError('invalid_argument','question stamp missing')
  if (typeof stamp.observation_epoch !== 'string' || stamp.observation_epoch === '') throw new AdapterError('invalid_argument','question epoch missing')
  const revision = stamp.revision
  const valid = typeof revision === 'number'
    ? Number.isSafeInteger(revision) && revision >= 1
    : typeof revision === 'string' && /^[1-9][0-9]*$/.test(revision)
  if (!valid) throw new AdapterError('invalid_argument','question revision invalid')
  return { observation_epoch: stamp.observation_epoch, revision }
}

export function validateQuestionDescriptor(descriptor) {
  if (!descriptor || typeof descriptor !== 'object' || !QUESTION_AVAILABILITIES.has(descriptor.availability)) {
    throw new AdapterError('invalid_argument','question availability invalid')
  }
  if (descriptor.availability === 'available') {
    return { availability: 'available', stamp: validateQuestionStamp(descriptor.stamp) }
  }
  const code = descriptor.code
  if (code !== undefined && (typeof code !== 'string' || code.length > 64)) throw new AdapterError('invalid_argument','question unavailable code invalid')
  return { availability: descriptor.availability, ...(code ? { code } : {}) }
}

const QUESTION_AVAILABILITY_TO_PROTO = Object.freeze({
  available: 'QUESTION_AVAILABILITY_AVAILABLE',
  unavailable: 'QUESTION_AVAILABILITY_UNAVAILABLE',
  unsupported: 'QUESTION_AVAILABILITY_UNSUPPORTED',
})

/** Wire descriptor; UNSPECIFIED is never emitted. */
export function questionDescriptorToProto(descriptor) {
  const normalized = validateQuestionDescriptor(descriptor)
  return {
    availability: QUESTION_AVAILABILITY_TO_PROTO[normalized.availability],
    ...(normalized.code ? { unavailable_code: normalized.code } : {}),
    ...(normalized.stamp ? { stamp: normalized.stamp } : {}),
  }
}

export function sameQuestionDescriptor(left, right) {
  if (!left || !right || left.availability !== right.availability) return false
  if (left.availability !== 'available') return (left.code ?? '') === (right.code ?? '')
  return left.stamp.observation_epoch === right.stamp.observation_epoch && String(left.stamp.revision) === String(right.stamp.revision)
}

// ---------------------------------------------------------------------------
// Conversation content
// ---------------------------------------------------------------------------

export const MESSAGE_ROLES = Object.freeze(new Set([
  'USER', 'ASSISTANT', 'TOOL', 'NOTICE', 'MESSAGE_ROLE_CONTEXT', 'MESSAGE_ROLE_UNKNOWN',
]))
export const LIVE_BLOCK_KINDS = Object.freeze(new Set(['BLOCK_KIND_TEXT', 'BLOCK_KIND_REASONING']))
export const TOOL_STAGES = Object.freeze(new Set([
  'TOOL_STAGE_PREPARATION', 'TOOL_STAGE_EXECUTION', 'TOOL_STAGE_PROGRESS', 'TOOL_STAGE_SETTLEMENT',
]))
export const TOOL_OUTCOMES = Object.freeze(new Set([
  'TOOL_OUTCOME_UNSPECIFIED', 'TOOL_OUTCOME_SUCCEEDED', 'TOOL_OUTCOME_FAILED',
  'TOOL_OUTCOME_INTERRUPTED', 'TOOL_OUTCOME_UNKNOWN',
]))
export const NOTICE_CATEGORIES = Object.freeze(new Set([
  'NOTICE_CATEGORY_RETRY', 'NOTICE_CATEGORY_MAX_OUTPUT', 'NOTICE_CATEGORY_COMPACTION',
  'NOTICE_CATEGORY_COMMAND', 'NOTICE_CATEGORY_CONTEXT', 'NOTICE_CATEGORY_OMISSION',
]))
export const NOTICE_STATES = Object.freeze(new Set([
  'NOTICE_STATE_SCHEDULED', 'NOTICE_STATE_STARTED', 'NOTICE_STATE_CANCELLED',
  'NOTICE_STATE_COMPLETED', 'NOTICE_STATE_FAILED', 'NOTICE_STATE_REACHED',
]))

const CONTEXT_ROLES = new Set(['MESSAGE_ROLE_CONTEXT', 'MESSAGE_ROLE_UNKNOWN'])
const CONTEXT_CATEGORIES = new Set(['NOTICE_CATEGORY_CONTEXT', 'NOTICE_CATEGORY_OMISSION'])
const CONTENT_KINDS = ['text', 'reasoning', 'tool_activity', 'notice', 'attachment']

/** Validate an opaque attachment occurrence reference in a content block. */
export function validateAttachmentContent(attachment) {
  if (!attachment || typeof attachment !== 'object' || Array.isArray(attachment)) {
    throw new AdapterError('invalid_argument', 'attachment content missing')
  }
  optionalString(attachment.media_type, 'attachment media type invalid', 256)
  optionalString(attachment.name, 'attachment name invalid', 512)
  safeInteger(attachment.size, 'attachment size invalid')
  if (typeof attachment.available !== 'boolean') {
    throw new AdapterError('invalid_argument', 'attachment availability missing')
  }
  if (attachment.available) {
    requireId('attachment_ref', attachment.attachment_ref, 'attachment occurrence reference invalid')
  } else if (attachment.attachment_ref !== undefined && attachment.attachment_ref !== '') {
    requireId('attachment_ref', attachment.attachment_ref, 'attachment occurrence reference invalid')
  }
  return attachment
}

export function validateNotice(notice) {
  if (!notice || typeof notice !== 'object' || Array.isArray(notice)) {
    throw new AdapterError('invalid_argument', 'notice content missing')
  }
  knownEnum(notice.category, NOTICE_CATEGORIES, 'notice category invalid or unspecified')
  knownEnum(notice.state, NOTICE_STATES, 'notice state invalid or unspecified')
  // The hard cut removes the old free-text notice body. A raw text field could
  // carry command arguments or compaction input, so it is never accepted.
  if (Object.hasOwn(notice, 'text')) throw new AdapterError('invalid_argument', 'notice must not carry a raw text body')
  optionalString(notice.summary, 'notice summary invalid', 4096)
  return notice
}

export function validateToolActivity(activity) {
  if (!activity || typeof activity !== 'object' || Array.isArray(activity)) {
    throw new AdapterError('invalid_argument', 'tool activity missing')
  }
  requireId('call_id', activity.call_id, 'tool call identity invalid')
  requireString(activity.name, 'tool name missing')
  optionalString(activity.name, 'tool name invalid', 256)
  optionalString(activity.summary, 'tool summary invalid', 1024)
  optionalString(activity.progress, 'tool progress invalid', 1024)
  optionalString(activity.failure_code, 'tool failure code invalid', 128)
  if (!Array.isArray(activity.stages) || activity.stages.length === 0) {
    throw new AdapterError('invalid_argument', 'tool stages missing')
  }
  let previous = 0
  for (const stage of activity.stages) {
    const value = knownEnum(stage, TOOL_STAGES, 'tool stage invalid or unspecified')
    const order = [...TOOL_STAGES].indexOf(value) + 1
    if (order <= previous) throw new AdapterError('invalid_argument', 'tool stages must be unique and ascending')
    previous = order
  }
  knownEnum(activity.outcome, TOOL_OUTCOMES, 'tool outcome invalid')
  if (activity.outcome !== 'TOOL_OUTCOME_UNSPECIFIED' && !activity.stages.includes('TOOL_STAGE_SETTLEMENT')) {
    throw new AdapterError('invalid_argument', 'tool outcome requires a settlement stage')
  }
  if (activity.outcome === 'TOOL_OUTCOME_FAILED' && !activity.failure_code) {
    throw new AdapterError('invalid_argument', 'failed tool activity needs a stable failure code')
  }
  if (activity.result !== undefined) {
    if (!Array.isArray(activity.result)) throw new AdapterError('invalid_argument', 'tool result content invalid')
    for (const part of activity.result) {
      if (!part || typeof part !== 'object' || ['text', 'attachment'].filter(k => part[k] != null).length !== 1) {
        throw new AdapterError('invalid_argument', 'tool result part invalid')
      }
      if (part.text != null) requireString(part.text.text, 'tool result text missing', { allowEmpty: true })
      if (part.attachment != null) validateAttachmentContent(part.attachment)
    }
  }
  return activity
}

/** Validate one committed or live content block. */
export function validateContentBlock(block) {
  requireId('block_id', block?.block_id, 'invalid content block identity')
  const kinds = CONTENT_KINDS.filter(k => block[k] != null)
  if (kinds.length !== 1) throw new AdapterError('invalid_argument', 'invalid content block')
  switch (kinds[0]) {
    case 'text':
      requireString(block.text.text, 'text content missing', { allowEmpty: true })
      break
    case 'reasoning':
      requireString(block.reasoning.text, 'reasoning content missing', { allowEmpty: true })
      break
    case 'tool_activity':
      validateToolActivity(block.tool_activity)
      break
    case 'notice':
      validateNotice(block.notice)
      break
    case 'attachment':
      validateAttachmentContent(block.attachment)
      break
  }
  return block
}

function validateConversationMessage(message) {
  if (!message || typeof message !== 'object' || Array.isArray(message)) {
    throw new AdapterError('invalid_argument', 'invalid message')
  }
  requireId('message_id', message.message_id, 'invalid message identity')
  knownEnum(message.role, MESSAGE_ROLES, 'invalid message role')
  if (!Array.isArray(message.blocks)) throw new AdapterError('invalid_argument', 'invalid message')
  for (const block of message.blocks) validateContentBlock(block)
  if (CONTEXT_ROLES.has(message.role)) {
    // A context or unknown body must never enter the user lane. Only a safe
    // notice or omission is permitted.
    for (const block of message.blocks) {
      if (!block.notice || !CONTEXT_CATEGORIES.has(block.notice.category)) {
        throw new AdapterError('invalid_argument', 'context message must be a safe notice or omission')
      }
    }
  } else {
    // A context/omission notice belongs only to context authorship; it can
    // never be used to smuggle a context body into the user lane.
    for (const block of message.blocks) {
      if (block.notice && CONTEXT_CATEGORIES.has(block.notice.category)) {
        throw new AdapterError('invalid_argument', 'context notice requires context authorship')
      }
    }
  }
  return message
}

// ---------------------------------------------------------------------------
// History selection
// ---------------------------------------------------------------------------

export const HISTORY_SOURCES = Object.freeze(new Set([
  'HISTORY_SOURCE_ONLINE', 'HISTORY_SOURCE_CACHE',
]))

export function validateCommittedMessageRef(reference) {
  if (!reference || typeof reference !== 'object' || Array.isArray(reference)) {
    throw new AdapterError('invalid_argument', 'committed message reference missing')
  }
  if (!reference.session || typeof reference.session !== 'object') {
    throw new AdapterError('invalid_argument', 'committed message session missing')
  }
  requireId('host_id', reference.session.host_id, 'committed message host invalid')
  requireId('session_id', reference.session.session_id, 'committed message session invalid')
  requireId('message_id', reference.message_id, 'committed message identity invalid')
  requireId('source_locator', reference.source_locator, 'committed message locator invalid')
  requireId('projection_id', reference.projection_id, 'committed message projection invalid')
  return reference
}

/** Validate an independent history request and its mutually exclusive modes. */
export function validateHistoryRead(read) {
  if (!read || typeof read !== 'object' || Array.isArray(read)) {
    throw new AdapterError('invalid_argument', 'history request missing')
  }
  requireId('session_id', read.session_id, 'history session invalid')
  if (read.after_cursor !== undefined && read.after_cursor !== '') {
    requireId('cursor', read.after_cursor, 'history after cursor invalid')
  }
  if (read.cut_cursor !== undefined && read.cut_cursor !== '') {
    requireId('cursor', read.cut_cursor, 'history cut cursor invalid')
  }
  if (read.continuation !== undefined && read.continuation !== '') {
    requireId('continuation', read.continuation, 'history continuation invalid')
  }
  const forward = read.after_cursor !== undefined
  const page = read.page != null
  const exact = read.exact != null
  const continuation = read.continuation !== undefined && read.continuation !== ''
  if ([forward, page, exact, continuation].filter(Boolean).length > 1) {
    throw new AdapterError('invalid_argument', 'history selection is not exclusive')
  }
  if (page) {
    if (!read.page || typeof read.page !== 'object' || Array.isArray(read.page)) {
      throw new AdapterError('invalid_argument', 'history page selection invalid')
    }
    if (read.page.before_cursor !== undefined && read.page.before_cursor !== '') {
      requireId('cursor', read.page.before_cursor, 'history page cursor invalid')
    }
  }
  if (exact) validateCommittedMessageRef(read.exact)
  if (read.source !== undefined) knownEnum(read.source, HISTORY_SOURCES, 'history source invalid')
  if (read.snapshot !== undefined && typeof read.snapshot !== 'boolean') {
    throw new AdapterError('invalid_argument', 'history snapshot flag invalid')
  }
  for (const key of ['max_bytes', 'max_messages']) {
    if (read[key] !== undefined && (!Number.isSafeInteger(read[key]) || read[key] < 1)) {
      throw new AdapterError('invalid_argument', `history ${key} invalid`)
    }
  }
  return read
}


// ---------------------------------------------------------------------------
// Typed send follow-up
// ---------------------------------------------------------------------------

export const MESSAGE_REPLY_STATES = Object.freeze(new Set([
  'MESSAGE_REPLY_PENDING', 'MESSAGE_REPLY_COMMITTED',
  'MESSAGE_REPLY_INPUT_REQUIRED', 'MESSAGE_REPLY_UNCERTAIN',
]))
export const INPUT_KINDS = Object.freeze(new Set([
  'INPUT_APPROVAL', 'INPUT_QUESTION', 'INPUT_OTHER',
]))

export function validateMessageReply(reply) {
  if (!reply || typeof reply !== 'object' || Array.isArray(reply)) {
    throw new AdapterError('invalid_argument', 'message reply missing')
  }
  requireId('session_id', reply.session_id, 'message reply session invalid')
  requireId('message_id', reply.submitted_message_id, 'message reply submission invalid')
  knownEnum(reply.state, MESSAGE_REPLY_STATES, 'message reply state invalid or unspecified')
  const pending = reply.pending_input ?? []
  if (!Array.isArray(pending)) throw new AdapterError('invalid_argument', 'pending input invalid')
  for (const kind of pending) knownEnum(kind, INPUT_KINDS, 'pending input kind invalid')
  if (new Set(pending).size !== pending.length) {
    throw new AdapterError('invalid_argument', 'duplicate pending input kind')
  }
  if (reply.state === 'MESSAGE_REPLY_COMMITTED') {
    validateCommittedMessageRef(reply.committed)
    if (pending.length > 0) throw new AdapterError('invalid_argument', 'committed reply cannot claim pending input')
  } else if (reply.state === 'MESSAGE_REPLY_INPUT_REQUIRED') {
    if (pending.length === 0) throw new AdapterError('invalid_argument', 'input-required reply needs an evidenced kind')
    if (reply.committed != null) throw new AdapterError('invalid_argument', 'input-required reply cannot carry a committed reference')
  } else if (reply.committed != null || pending.length > 0) {
    throw new AdapterError('invalid_argument', 'unconfirmed reply cannot claim a committed reference or input')
  }
  return reply
}

// ---------------------------------------------------------------------------
// Attachment read results
// ---------------------------------------------------------------------------

export function validateAttachmentPolicy(policy) {
  if (!policy || typeof policy !== 'object' || Array.isArray(policy)) {
    throw new AdapterError('invalid_argument', 'attachment policy missing')
  }
  safeInteger(policy.max_total_bytes, 'attachment total limit invalid')
  for (const key of ['max_chunk_bytes', 'max_concurrent', 'idle_timeout_ms', 'overall_timeout_ms']) {
    safeInteger(policy[key], `attachment ${key} invalid`, { min: 1 })
  }
  if (policy.max_total_bytes > 0 && policy.max_chunk_bytes > policy.max_total_bytes) {
    throw new AdapterError('invalid_argument', 'attachment chunk exceeds the total policy')
  }
  return policy
}

export function validateAttachmentMetadata(metadata) {
  if (!metadata || typeof metadata !== 'object' || Array.isArray(metadata)) {
    throw new AdapterError('invalid_argument', 'attachment metadata missing')
  }
  if (!metadata.session || typeof metadata.session !== 'object') {
    throw new AdapterError('invalid_argument', 'attachment session missing')
  }
  requireId('host_id', metadata.session.host_id, 'attachment host invalid')
  requireId('session_id', metadata.session.session_id, 'attachment session invalid')
  requireId('attachment_ref', metadata.attachment_ref, 'attachment occurrence invalid')
  requireString(metadata.media_type, 'attachment media type missing')
  optionalString(metadata.media_type, 'attachment media type invalid', 256)
  optionalString(metadata.name, 'attachment name invalid', 512)
  safeInteger(metadata.size, 'attachment size invalid')
  validateAttachmentPolicy(metadata.policy)
  if (metadata.size > metadata.policy.max_total_bytes && metadata.policy.max_total_bytes !== 0) {
    throw new AdapterError('payload_too_large', 'attachment exceeds the total policy')
  }
  requireId('transfer_id', metadata.transfer_id, 'attachment transfer invalid')
  safeInteger(metadata.expires_at_unix_ms, 'attachment expiry invalid', { min: 1 })
  return metadata
}

/** Verify a contiguous attachment part and its terminal integrity metadata. */
export function validateAttachmentPart(part, metadata) {
  if (!part || typeof part !== 'object' || Array.isArray(part)) {
    throw new AdapterError('invalid_argument', 'attachment part missing')
  }
  if (!part.session || typeof part.session !== 'object') {
    throw new AdapterError('invalid_argument', 'attachment part session missing')
  }
  requireId('host_id', part.session.host_id, 'attachment part host invalid')
  requireId('session_id', part.session.session_id, 'attachment part session invalid')
  requireId('attachment_ref', part.attachment_ref, 'attachment part occurrence invalid')
  requireId('transfer_id', part.transfer_id, 'attachment part transfer invalid')
  safeInteger(part.offset, 'attachment part offset invalid')
  safeInteger(part.length, 'attachment part length invalid')
  if (!(part.data instanceof Uint8Array) || part.data.length !== part.length) {
    throw new AdapterError('invalid_argument', 'attachment part length mismatch')
  }
  if (typeof part.final !== 'boolean') throw new AdapterError('invalid_argument', 'attachment final flag missing')
  if (part.final) {
    safeInteger(part.total_size, 'attachment terminal size missing')
    if (part.total_size !== part.offset + part.length) {
      throw new AdapterError('invalid_argument', 'attachment terminal size mismatch')
    }
    if (!(part.sha256 instanceof Uint8Array) || part.sha256.length !== 32) {
      throw new AdapterError('invalid_argument', 'attachment terminal digest missing')
    }
  } else if (part.total_size !== undefined || part.sha256 !== undefined) {
    throw new AdapterError('invalid_argument', 'non-terminal part carries terminal integrity metadata')
  }
  if (metadata) {
    if (part.session.host_id !== metadata.session.host_id || part.session.session_id !== metadata.session.session_id) {
      throw new AdapterError('invalid_argument', 'attachment part scope mismatch')
    }
    if (part.attachment_ref !== metadata.attachment_ref || part.transfer_id !== metadata.transfer_id) {
      throw new AdapterError('invalid_argument', 'attachment part transfer mismatch')
    }
    if (part.offset + part.length > metadata.size) {
      throw new AdapterError('invalid_argument', 'attachment part exceeds declared size')
    }
  }
  return part
}

// ---------------------------------------------------------------------------
// Session updates
// ---------------------------------------------------------------------------

export function validateSessionUpdate(update) {
  if (!update?.session?.session_id || !validateId('session_id', update.session.session_id).ok) throw new AdapterError('invalid_argument','session identity missing')
  const kinds=['history','live','state','reset','questions'].filter(key=>update[key]!=null)
  if(kinds.length!==1)throw new AdapterError('invalid_argument','exactly one update required')
  if(update.history) {
    const b=update.history
    if(!b.from_cursor||!b.to_cursor||!validateId('cursor', b.from_cursor).ok||!validateId('cursor', b.to_cursor).ok||!validateId('batch_id', b.batch_id).ok)throw new AdapterError('invalid_argument','invalid history interval')
    if(b.projection_id !== undefined) requireId('projection_id', b.projection_id, 'invalid history projection')
    for(const {message} of b.changes??[]) validateConversationMessage(message)
  }
  if(update.questions) validateQuestionDescriptor(update.questions)
  if(update.live) {
    const f=update.live
    if(!f.attempt?.live_epoch||!f.attempt.attempt_id||!Number.isSafeInteger(f.first_seq)||!Number.isSafeInteger(f.last_seq)||f.first_seq<1||f.last_seq<f.first_seq||['started','delta','reset','ended'].filter(k=>f[k]!=null).length!==1)throw new AdapterError('invalid_argument','invalid live interval')
    if(f.delta||f.reset){
      if(!f.block_id)throw new AdapterError('invalid_argument','missing content block identity')
      const payload=f.delta??f.reset
      knownEnum(payload.kind, LIVE_BLOCK_KINDS, 'live block kind invalid or unspecified')
      requireString(payload.text, 'live block text missing', { allowEmpty: true })
    }
  }
  if(update.state?.projection_id !== undefined) requireId('projection_id', update.state.projection_id, 'invalid state projection')
  return update
}
