// Harness-neutral source seam. Implementations own native history and actions;
// neither credentials nor a Host database belongs on this interface.
import { AdapterError, DefinitiveExecutionError } from '../errors.js'

export class SourceAdapter {
  async open({metadata}={}) { this.metadata=metadata }
  async close() {}
  async listSessions() { throw new AdapterError('unsupported_capability') }
  async getSession() { throw new AdapterError('unsupported_capability') }
  watch() { throw new AdapterError('unsupported_capability') }
  async readHistory() { throw new AdapterError('unsupported_capability') }
  async readState() { throw new AdapterError('unsupported_capability') }
  async listApprovals() { throw new AdapterError('unsupported_capability') }
  async getApproval() { throw new AdapterError('unsupported_capability') }
  // Resolve with a confirmed native receipt. Only DefinitiveExecutionError
  // establishes failure; any other exception can follow an executed action.
  async execute() { throw new DefinitiveExecutionError('unsupported_capability') }
}

export function validateSessionUpdate(update) {
  if (!update?.session?.session_id || update.session.session_id.length>512) throw new AdapterError('invalid_argument','session identity missing')
  const kinds=['history','live','state','reset'].filter(key=>update[key]!=null)
  if(kinds.length!==1)throw new AdapterError('invalid_argument','exactly one update required')
  if(update.history) {
    const b=update.history
    if(!b.from_cursor||!b.to_cursor||!b.batch_id||b.from_cursor.length>4096||b.to_cursor.length>4096)throw new AdapterError('invalid_argument','invalid history interval')
    for(const {message} of b.changes??[]) {
      if(!message?.message_id||!message.role||!Array.isArray(message.blocks))throw new AdapterError('invalid_argument','invalid message')
      for(const block of message.blocks)if(!block.block_id||['text','tool_call','tool_result','notice','attachment'].filter(k=>block[k]!=null).length!==1)throw new AdapterError('invalid_argument','invalid content block')
    }
  }
  if(update.live) {
    const f=update.live
    if(!f.attempt?.live_epoch||!f.attempt.attempt_id||!Number.isSafeInteger(f.first_seq)||!Number.isSafeInteger(f.last_seq)||f.first_seq<1||f.last_seq<f.first_seq||['started','delta','reset','ended'].filter(k=>f[k]!=null).length!==1)throw new AdapterError('invalid_argument','invalid live interval')
    if((f.delta||f.reset)&&!f.block_id)throw new AdapterError('invalid_argument','missing content block identity')
  }
  return update
}
