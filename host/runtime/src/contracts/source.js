// Harness-neutral source seam. Implementations own native history and actions;
// neither credentials nor a Host database belongs on this interface.
import { AdapterError, DefinitiveExecutionError } from '../errors.js'
import { validateId } from './ids.js'

function requireId(kind, value, message) {
  if (!validateId(kind, value).ok) throw new AdapterError('invalid_argument', message)
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
  async readHistory() { throw new AdapterError('unsupported_capability') }
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

export function validateSessionUpdate(update) {
  if (!update?.session?.session_id || !validateId('session_id', update.session.session_id).ok) throw new AdapterError('invalid_argument','session identity missing')
  const kinds=['history','live','state','reset','questions'].filter(key=>update[key]!=null)
  if(kinds.length!==1)throw new AdapterError('invalid_argument','exactly one update required')
  if(update.history) {
    const b=update.history
    if(!b.from_cursor||!b.to_cursor||!validateId('cursor', b.from_cursor).ok||!validateId('cursor', b.to_cursor).ok||!validateId('batch_id', b.batch_id).ok)throw new AdapterError('invalid_argument','invalid history interval')
    for(const {message} of b.changes??[]) {
      if(!message?.message_id||!message.role||!Array.isArray(message.blocks))throw new AdapterError('invalid_argument','invalid message')
      requireId('message_id', message.message_id, 'invalid message identity')
      for(const block of message.blocks){requireId('block_id', block.block_id, 'invalid content block identity');if(['text','tool_call','tool_result','notice','attachment'].filter(k=>block[k]!=null).length!==1)throw new AdapterError('invalid_argument','invalid content block')}
    }
  }
  if(update.questions) validateQuestionDescriptor(update.questions)
  if(update.live) {
    const f=update.live
    if(!f.attempt?.live_epoch||!f.attempt.attempt_id||!Number.isSafeInteger(f.first_seq)||!Number.isSafeInteger(f.last_seq)||f.first_seq<1||f.last_seq<f.first_seq||['started','delta','reset','ended'].filter(k=>f[k]!=null).length!==1)throw new AdapterError('invalid_argument','invalid live interval')
    if((f.delta||f.reset)&&!f.block_id)throw new AdapterError('invalid_argument','missing content block identity')
  }
  return update
}
