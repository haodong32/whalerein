// All native DSH payload interpretation stays at this seam. A normalized block
// carries only display-safe data: no provider replay state, private prompts,
// raw tool arguments, compaction input or unmapped payload bodies.
import { createHash } from 'node:crypto'

// Committed events that carry native lifecycle or replay data and never belong
// in visible history. System/developer prompts are private model instructions.
const INTERNAL_TYPES = new Set([
  'agent/inbox/spliced',
  'request/header',
  'request/context',
  'ignore',
  'system/message',
  'developer/message',
  'assistant/attempt',
  'approval/asked',
  'approval/decided',
  'model/selection',
  'sandbox/mode',
  'agent-preset/selected',
  'session/end-seed',
  'compaction/prune',
  'image/offload',
  'session/title',
  'todo/write',
  'working-directory/change',
  'job/registered',
  'job/settled',
  'feedback/record',
  'team/member',
  'team/task',
  'team/message/queued',
  'team/message/delivered',
  'goal/change',
  'plan/mode',
  'schedule/change',
  'workspace/changes',
])

// DSH marks a genuine human turn with source.kind === 'user'. Every other
// durable user-role message is producer-supplied context; recognized producers
// get a fixed safe label, unknown/missing authorship gets an omission. A raw
// context body never enters the user lane.
const CONTEXT_SOURCE_LABELS = Object.freeze({
  'runtime-context': 'Runtime context updated',
  'skill-invocation': 'Skill context updated',
  'system-prompt': 'System prompt context updated',
  'model-selection': 'Model selection context updated',
  'tool-registry': 'Tool catalog updated',
  'action-cards': 'Action cards updated',
  'working-directory': 'Working directory context updated',
  'user-approval': 'Approval context updated',
  'agent-message': 'Agent context updated',
  'subagent-settled': 'Subagent context updated',
  'plan-mode': 'Plan mode context updated',
  'schedule': 'Schedule context updated',
  'ptc-mode': 'Tool execution mode context updated',
  'compact-checkpoint': 'Compaction checkpoint updated',
  'cordis-host-runner': 'Host runner context updated',
  'user-question-reply': 'Question reply context updated',
  'team-message': 'Team context updated',
  'goal': 'Goal context updated',
  'session-reference': 'Session reference context updated',
  'webhook': 'Webhook context updated',
  'agent-instructions': 'Agent instructions updated',
  'time-context': 'Time context updated',
  'tmux-context': 'Terminal context updated',
  'repeat-tool-reminder': 'Tool reminder updated',
  'tool-goal': 'Goal context updated',
  'tool-jobs': 'Job context updated',
  'session-title-llm': 'Session title context updated',
  'dsh-session-title-llm': 'Session title context updated',
  'migration-required': 'Migration context updated',
  'subagent': 'Subagent context updated',
})

const TOOL_NAME_MAX = 256
const TOOL_SUMMARY_MAX = 1024
const FAILURE_CODE_MAX = 128
const NOTICE_SUMMARY_MAX = 4096
const ATTACHMENT_NAME_MAX = 512

export function messageId(sessionId, view, seq) {
  return createHash('sha256').update(JSON.stringify([sessionId, view, seq])).digest('hex')
}

function bounded(value, max = 256) {
  if (typeof value !== 'string') return ''
  // Keep identifiers and display labels single-line and bounded; no native path
  // separators enter a normalized block.
  return value.replace(/[\u0000-\u001f\u007f]/g, ' ').trim().slice(0, max)
}

function boundedSummary(value, max = NOTICE_SUMMARY_MAX) {
  if (typeof value !== 'string') return ''
  const clean = value.replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, ' ')
  const points = Array.from(clean)
  let out = ''
  for (const point of points) {
    if (Buffer.byteLength(out + point, 'utf8') > max) break
    out += point
  }
  return out
}

function nonEmpty(value) {
  return typeof value === 'string' && value !== ''
}

function contentBlocks(message) {
  return Array.isArray(message) ? message : []
}

function attachmentMetadata(sessionId, view, seq, index, attachment, fallbackMediaType = '', available = false) {
  const mediaValue = typeof attachment?.mediaType === 'string' ? attachment.mediaType : fallbackMediaType
  const mediaType = /^[\w.+-]+\/[\w.+-]+$/.test(mediaValue) && mediaValue.length <= 128 ? mediaValue : ''
  const rawName = typeof attachment?.name === 'string' && !/[\\/]/.test(attachment.name) ? attachment.name : ''
  const name = bounded(rawName, ATTACHMENT_NAME_MAX)
  const size = Number.isSafeInteger(attachment?.bytes) && attachment.bytes >= 0 ? attachment.bytes : 0
  const attachmentId = typeof attachment?.attachmentId === 'string' ? attachment.attachmentId : ''
  // Opaque, deterministic occurrence identity scoped to this session and exact
  // committed position. Bytes and storage paths never travel with it.
  const attachmentRef = attachmentId === ''
    ? ''
    : createHash('sha256').update(JSON.stringify([
      sessionId, view, seq, index, attachmentId,
    ])).digest('hex')
  return {
    media_type: mediaType,
    name,
    size,
    // Availability is a source-level byte-seam property. The occurrence and
    // its deterministic reference are canonical either way; only a Source that
    // can prove and open the stored object advertises `available: true`.
    available: Boolean(available && attachmentRef !== ''),
    ...(attachmentRef === '' ? {} : { attachment_ref: attachmentRef }),
  }
}

function blocksFromUserContent(sessionId, view, event, content, attachments) {
  const blocks = []
  for (const [index, block] of contentBlocks(content).entries()) {
    if (block?.type === 'text' && typeof block.text === 'string') {
      blocks.push({ block_id: `text-${index}`, text: { text: block.text } })
    } else if (['image', 'file'].includes(block?.type) && block.attachment && typeof block.attachment === 'object') {
      blocks.push({ block_id: `attachment-${index}`, attachment: attachmentMetadata(sessionId, view, event.dshSeq, index, block.attachment, '', attachments) })
    }
    // Unknown user content blocks are dropped rather than carrying a payload.
  }
  return blocks
}

function blocksFromAssistantContent(sessionId, view, event, content, attachments) {
  const blocks = []
  for (const [index, block] of contentBlocks(content).entries()) {
    if (block?.type === 'text' && typeof block.text === 'string') {
      blocks.push({ block_id: `text-${index}`, text: { text: block.text } })
    } else if (block?.type === 'reasoning' && typeof block.text === 'string') {
      blocks.push({ block_id: `reasoning-${index}`, reasoning: { text: block.text } })
    } else if (block?.type === 'tool-call') {
      const callId = bounded(block.id, 128)
      if (!nonEmpty(callId)) continue
      const name = bounded(block.name, TOOL_NAME_MAX) || 'Tool'
      blocks.push({
        block_id: `tool-${index}`,
        tool_activity: {
          call_id: callId,
          stages: ['TOOL_STAGE_PREPARATION'],
          name,
          summary: name,
          outcome: 'TOOL_OUTCOME_UNSPECIFIED',
        },
      })
    } else if (['image', 'file'].includes(block?.type) && block.attachment && typeof block.attachment === 'object') {
      blocks.push({ block_id: `attachment-${index}`, attachment: attachmentMetadata(sessionId, view, event.dshSeq, index, block.attachment, '', attachments) })
    }
    // Unknown assistant blocks are dropped; never an opaque passthrough.
  }
  return blocks
}

function resultParts(sessionId, view, event, content, attachments) {
  const parts = []
  for (const [index, block] of contentBlocks(content).entries()) {
    if (block?.type === 'text' && typeof block.text === 'string') {
      parts.push({ text: { text: block.text } })
    } else if (['image', 'file'].includes(block?.type) && block.attachment && typeof block.attachment === 'object') {
      parts.push({ attachment: attachmentMetadata(sessionId, view, event.dshSeq, `result-${index}`, block.attachment, '', attachments) })
    }
  }
  return parts
}

function stableFailureCode(error) {
  const code = typeof error?.code === 'string' ? bounded(error.code, FAILURE_CODE_MAX) : ''
  const stable = code.replace(/[^A-Za-z0-9_.-]/g, '_').replace(/^_+|_+$/g, '')
  return stable || 'tool_error'
}

function toolCallBlock(name) {
  return bounded(name, TOOL_NAME_MAX) || 'Tool'
}

function toolResultActivity(sessionId, view, event, data, attachments) {
  const message = data?.message
  const callId = bounded(message?.source?.callId ?? data?.callId ?? data?.call_id, 128)
  if (!nonEmpty(callId)) return null
  const failed = message?.isError === true || data?.error != null
  const activity = {
    call_id: callId,
    stages: ['TOOL_STAGE_SETTLEMENT'],
    name: 'Tool',
    summary: failed ? 'Tool failed' : 'Tool completed',
    outcome: failed ? 'TOOL_OUTCOME_FAILED' : 'TOOL_OUTCOME_SUCCEEDED',
    ...(failed ? { failure_code: stableFailureCode(data?.error) } : {}),
  }
  const parts = resultParts(sessionId, view, event, message?.content, attachments)
  if (parts.length > 0) activity.result = parts
  return activity
}

function notice(category, state, summary) {
  const text = typeof summary === 'string' ? boundedSummary(summary) : ''
  return { category, state, ...(text === '' ? {} : { summary: text }) }
}

function noticeMessage(sessionId, view, event, blocks) {
  return {
    message_id: messageId(sessionId, view, event.dshSeq),
    role: 'NOTICE',
    blocks,
    authored_at_unix_ms: event.createdAt ?? 0,
  }
}

function contextMessage(sessionId, view, event, role, category, summary) {
  return {
    message_id: messageId(sessionId, view, event.dshSeq),
    role,
    blocks: [{ block_id: 'notice-context', notice: notice(category, 'NOTICE_STATE_COMPLETED', summary) }],
    authored_at_unix_ms: event.createdAt ?? 0,
  }
}

function compactionSummaryText(event) {
  const summary = event.data?.summary
  if (!Array.isArray(summary)) return ''
  const text = summary.filter(block => block?.type === 'text' && typeof block.text === 'string')
    .map(block => block.text).join('')
  return boundedSummary(text.trim())
}

function turnEndNotices(event) {
  const blocks = []
  if (event.data?.reason?.kind === 'max-tokens') {
    blocks.push({ block_id: 'notice-max-output', notice: notice('NOTICE_CATEGORY_MAX_OUTPUT', 'NOTICE_STATE_REACHED', 'Output limit reached') })
  }
  return blocks
}

/**
 * Stateful native-event projector. It is deterministic for one ordered event
 * sequence: runtime-lifetime retries, compaction summaries and command names
 * are derived only from events observed (or staged) before the projection.
 * A view reset discards the state.
 */
export class DshProjection {
  #compactionSummaries = new Map()
  #commands = new Map()
  #retries = new Map()
  #attachments

  constructor({ attachments = false } = {}) {
    this.#attachments = attachments === true
  }

  reset() {
    this.#compactionSummaries.clear()
    this.#commands.clear()
    this.#retries.clear()
  }

  project(sessionId, view, event) {
    const type = event?.type ?? ''
    if (!Number.isSafeInteger(event?.dshSeq)) return null
    if (type.startsWith('session/') || type.startsWith('turn/') || type.startsWith('step/') ||
        type.startsWith('approval/') || type === 'notice' || type.includes('error') ||
        INTERNAL_TYPES.has(type)) {
      // Turn/step boundaries still reconcile derived retry cancellation.
      if (type === 'step/end' || type === 'turn/end') {
        const blocks = turnEndNotices(event)
        blocks.push(...this.#cancelRetries(type, event))
        if (blocks.length > 0) return noticeMessage(sessionId, view, event, blocks)
      }
      return null
    }

    if (type === 'llm/retry') {
      const retryId = bounded(event.data?.retryId, 128)
      if (!nonEmpty(retryId)) return null
      const retry = Number.isSafeInteger(event.data?.retry) ? event.data.retry : 0
      const maxRetries = Number.isSafeInteger(event.data?.maxRetries) ? event.data.maxRetries : 0
      this.#retries.set(retryId, {
        turn: event.data?.turn,
        step: event.data?.step,
        state: 'scheduled',
      })
      const summary = maxRetries > 0
        ? `Model request retry ${retry} of ${maxRetries} scheduled`
        : 'Model request retry scheduled'
      return noticeMessage(sessionId, view, event, [
        { block_id: 'notice-retry', notice: notice('NOTICE_CATEGORY_RETRY', 'NOTICE_STATE_SCHEDULED', summary) },
      ])
    }

    if (type === 'llm/retry-started') {
      const retryId = bounded(event.data?.retryId, 128)
      const entry = this.#retries.get(retryId)
      if (entry) entry.state = 'started'
      return noticeMessage(sessionId, view, event, [
        { block_id: 'notice-retry', notice: notice('NOTICE_CATEGORY_RETRY', 'NOTICE_STATE_STARTED', 'Model request retry started') },
      ])
    }

    if (type === 'compaction/start') {
      return noticeMessage(sessionId, view, event, [
        { block_id: 'notice-compaction', notice: notice('NOTICE_CATEGORY_COMPACTION', 'NOTICE_STATE_STARTED', 'Compaction started') },
      ])
    }

    if (type === 'compaction/summary') {
      const compactionId = bounded(event.data?.compactionId, 128)
      if (nonEmpty(compactionId)) this.#compactionSummaries.set(compactionId, compactionSummaryText(event))
      return null
    }

    if (type === 'compaction/end') {
      if (event.data?.error === undefined) return null
      return noticeMessage(sessionId, view, event, [
        { block_id: 'notice-compaction', notice: notice('NOTICE_CATEGORY_COMPACTION', 'NOTICE_STATE_FAILED', 'Compaction failed') },
      ])
    }

    if (type === 'command/run') {
      const commandId = bounded(event.data?.commandId, 128)
      const name = bounded(event.data?.name, 128)
      if (nonEmpty(commandId)) this.#commands.set(commandId, name)
      // Command arguments are never read or carried.
      const summary = name === '' ? 'Command started' : `Command ${name} started`
      return noticeMessage(sessionId, view, event, [
        { block_id: 'notice-command', notice: notice('NOTICE_CATEGORY_COMMAND', 'NOTICE_STATE_STARTED', summary) },
      ])
    }

    if (type === 'command/done') {
      const commandId = bounded(event.data?.commandId, 128)
      const name = this.#commands.get(commandId) ?? ''
      this.#commands.delete(commandId)
      const failed = event.data?.kind === 'error'
      const summary = name === '' ? (failed ? 'Command failed' : 'Command completed') : `Command ${name} ${failed ? 'failed' : 'completed'}`
      return noticeMessage(sessionId, view, event, [{
        block_id: 'notice-command',
        notice: notice('NOTICE_CATEGORY_COMMAND', failed ? 'NOTICE_STATE_FAILED' : 'NOTICE_STATE_COMPLETED', summary),
      }])
    }

    if (type === 'tool/call') {
      const callId = bounded(event.data?.callId, 128)
      if (!nonEmpty(callId)) return null
      const name = toolCallBlock(event.data?.name)
      return {
        message_id: messageId(sessionId, view, event.dshSeq),
        role: 'TOOL',
        blocks: [{
          block_id: 'tool-execution',
          tool_activity: {
            call_id: callId,
            stages: ['TOOL_STAGE_EXECUTION'],
            name,
            summary: name,
            outcome: 'TOOL_OUTCOME_UNSPECIFIED',
          },
        }],
        authored_at_unix_ms: event.createdAt ?? 0,
      }
    }

    if (type === 'tool/result') {
      const activity = toolResultActivity(sessionId, view, event, event.data, this.#attachments)
      if (!activity) return null
      return {
        message_id: messageId(sessionId, view, event.dshSeq),
        role: 'TOOL',
        blocks: [{ block_id: 'tool-settlement', tool_activity: activity }],
        authored_at_unix_ms: event.createdAt ?? 0,
      }
    }

    if (type === 'tool/ptc-dispatch-start') {
      const callId = bounded(event.data?.subCallId, 128)
      if (!nonEmpty(callId)) return null
      const name = toolCallBlock(event.data?.name)
      return {
        message_id: messageId(sessionId, view, event.dshSeq),
        role: 'TOOL',
        blocks: [{
          block_id: 'tool-execution',
          tool_activity: {
            call_id: callId,
            stages: ['TOOL_STAGE_EXECUTION'],
            name,
            summary: name,
            outcome: 'TOOL_OUTCOME_UNSPECIFIED',
          },
        }],
        authored_at_unix_ms: event.createdAt ?? 0,
      }
    }

    if (type === 'tool/ptc-dispatch') {
      const callId = bounded(event.data?.subCallId, 128)
      if (!nonEmpty(callId)) return null
      const failed = event.data?.isError === true
      const activity = {
        call_id: callId,
        stages: ['TOOL_STAGE_SETTLEMENT'],
        name: toolCallBlock(event.data?.name),
        summary: failed ? 'Tool failed' : 'Tool completed',
        outcome: failed ? 'TOOL_OUTCOME_FAILED' : 'TOOL_OUTCOME_SUCCEEDED',
        ...(failed ? { failure_code: stableFailureCode(event.data?.error) } : {}),
      }
      const parts = resultParts(sessionId, view, event, event.data?.content, this.#attachments)
      if (parts.length > 0) activity.result = parts
      return {
        message_id: messageId(sessionId, view, event.dshSeq),
        role: 'TOOL',
        blocks: [{ block_id: 'tool-settlement', tool_activity: activity }],
        authored_at_unix_ms: event.createdAt ?? 0,
      }
    }

    if (type === 'assistant/message') {
      const content = event.data?.message?.content
      let blocks = Array.isArray(content) ? blocksFromAssistantContent(sessionId, view, event, content, this.#attachments) : []
      if (blocks.length === 0 && typeof event.data?.text === 'string') {
        blocks = [{ block_id: 'text-0', text: { text: event.data.text } }]
      }
      if (blocks.length === 0) return null
      return {
        message_id: messageId(sessionId, view, event.dshSeq),
        role: 'ASSISTANT',
        blocks,
        authored_at_unix_ms: event.createdAt ?? 0,
      }
    }

    if (type === 'user/message') {
      const source = event.data?.source ?? event.data?.message?.source
      const kind = bounded(source?.kind, 64)
      if (kind === 'user') {
        const blocks = blocksFromUserContent(sessionId, view, event, event.data?.content ?? event.data?.message?.content, this.#attachments)
        if (blocks.length === 0) return null
        const nativeId = event.data?.id ?? event.data?.message?.id
        const rpcId = source?.rpcId
        // Older native events carry the operation ID in the user message ID.
        // Only use that fallback for the Host's canonical operation ID shape.
        const deliveryId = typeof rpcId === 'string' && /^[a-zA-Z0-9_.:-]{1,128}$/.test(rpcId)
          ? rpcId
          : typeof nativeId === 'string' && /^[a-f0-9]{64}$/.test(nativeId) ? nativeId : undefined
        return {
          message_id: messageId(sessionId, view, event.dshSeq),
          ...(deliveryId ? { delivery_id: deliveryId } : {}),
          role: 'USER',
          blocks,
          authored_at_unix_ms: event.createdAt ?? 0,
        }
      }
      if (kind === 'compact-checkpoint') {
        const compactionId = bounded(source?.compactionId, 128)
        const summary = this.#compactionSummaries.get(compactionId) ?? ''
        return noticeMessage(sessionId, view, event, [{
          block_id: 'notice-compaction',
          notice: notice('NOTICE_CATEGORY_COMPACTION', 'NOTICE_STATE_COMPLETED', summary),
        }])
      }
      if (kind !== '' && Object.hasOwn(CONTEXT_SOURCE_LABELS, kind)) {
        // A producer-declared `notice` form carries its own bounded summary;
        // otherwise only the fixed adapter label is exposed.
        const supplied = source?.form === 'notice' && typeof source?.summary === 'string'
          ? boundedSummary(source.summary)
          : ''
        const summary = supplied || CONTEXT_SOURCE_LABELS[kind]
        return contextMessage(sessionId, view, event, 'MESSAGE_ROLE_CONTEXT', 'NOTICE_CATEGORY_CONTEXT', summary)
      }
      // Missing, unknown or plugin-specific authorship: no body, no guessing.
      return contextMessage(sessionId, view, event, 'MESSAGE_ROLE_UNKNOWN', 'NOTICE_CATEGORY_OMISSION', 'Source context is unavailable')
    }

    // Unknown committed events never become a raw notice or user row.
    return null
  }

  #cancelRetries(type, event) {
    const blocks = []
    for (const [retryId, entry] of this.#retries) {
      if (entry.state !== 'scheduled') continue
      const owned = type === 'step/end'
        ? event.data?.turn === entry.turn && event.data?.step === entry.step
        : event.data?.turn === entry.turn
      if (!owned) continue
      this.#retries.delete(retryId)
      blocks.push({
        block_id: `notice-retry-cancelled-${createHash('sha256').update(retryId).digest('hex').slice(0, 16)}`,
        notice: notice('NOTICE_CATEGORY_RETRY', 'NOTICE_STATE_CANCELLED', 'Model request retry cancelled'),
      })
    }
    return blocks
  }
}

/** Project one event with no cross-event state (deterministic and window-free). */
export function projectMessage(sessionId, view, event) {
  return new DshProjection().project(sessionId, view, event)
}

const OCCURRENCE_GROUPS = Object.freeze({
  'user/message': { prefix: '', paths: ['data.content', 'data.message.content'] },
  'assistant/message': { prefix: '', paths: ['data.message.content'] },
  'tool/result': { prefix: 'result-', paths: ['data.message.content'] },
  'tool/ptc-dispatch': { prefix: 'result-', paths: ['data.content'] },
})

function pathValue(value, path) {
  let current = value
  for (const key of path.split('.')) {
    if (!current || typeof current !== 'object') return undefined
    current = current[key]
  }
  return current
}

/**
 * Prove one attachment occurrence from already-observed committed events.
 * Returns the native attachment reference only when the canonical occurrence
 * reference for this session/view/position matches exactly. Callers scan hot or
 * cold history before touching the native attachment store.
 */
export function attachmentOccurrence(sessionId, view, events, attachmentRef) {
  if (typeof attachmentRef !== 'string' || attachmentRef === '' || !Array.isArray(events)) return null
  for (const event of events) {
    const group = OCCURRENCE_GROUPS[event?.type]
    if (!group || !Number.isSafeInteger(event.dshSeq)) continue
    for (const path of group.paths) {
      const content = pathValue(event, path)
      if (!Array.isArray(content)) continue
      for (const [index, block] of content.entries()) {
        if (!['image', 'file'].includes(block?.type) || !block.attachment || typeof block.attachment !== 'object') continue
        const position = group.prefix === '' ? index : `${group.prefix}${index}`
        const metadata = attachmentMetadata(sessionId, view, event.dshSeq, position, block.attachment)
        if (metadata.attachment_ref !== '' && metadata.attachment_ref === attachmentRef) {
          return { attachment: block.attachment, kind: block.type, seq: event.dshSeq }
        }
      }
    }
  }
  return null
}
