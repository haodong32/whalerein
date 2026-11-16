// All native DSH payload interpretation stays at this seam.
import { createHash } from 'node:crypto'

// Committed events that carry native lifecycle or replay data and never belong
// in visible history.
const INTERNAL_TYPES = new Set([
  'agent/inbox/spliced',
  'request/header',
  'request/context',
  'ignore',
  'system/message',
  'developer/message',
])

// DSH labels every visible user turn with source.kind === 'user'. Other user
// message sources are runtime context, skill catalogs, prompts and tooling
// metadata. They stay out of the user lane so remote viewers see the user's
// own words, not local paths or injected catalogs.
const CONTEXT_SOURCE_LABELS = Object.freeze({
  'runtime-context': 'Runtime context updated',
  'skill-invocation': 'Skill catalog updated',
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
})

export function messageId(sessionId, view, seq) {
  return createHash('sha256').update(JSON.stringify([sessionId, view, seq])).digest('hex')
}

function bounded(value, max = 256) {
  if (typeof value !== 'string') return ''
  // Keep identifiers single-line and bounded; no native path separators.
  return value.replace(/[\u0000-\u001f\u007f]/g, ' ').trim().slice(0, max)
}

function attachmentContent(block) {
  const attachment = block.attachment && typeof block.attachment === 'object' ? block.attachment : block
  const mediaValue = typeof attachment.mediaType === 'string' ? attachment.mediaType : ''
  const mediaType = /^[\w.+-]+\/[\w.+-]+$/.test(mediaValue) && mediaValue.length <= 128 ? mediaValue : ''
  const rawName = typeof attachment.name === 'string' && !/[\\/]/.test(attachment.name) ? attachment.name : ''
  const name = bounded(rawName, 128)
  const size = Number.isSafeInteger(attachment.bytes) && attachment.bytes >= 0 ? attachment.bytes : 0
  return { media_type: mediaType, name, size, available: false }
}

function toolCallContent(block, index) {
  const callId = bounded(block.id ?? block.callId ?? block.call_id ?? '', 256)
  const name = bounded(block.name ?? '', 256)
  const summary = bounded(block.summary ?? '', 512) || name || 'Tool call'
  return { block_id: `tool-${index}`, tool_call: { call_id: callId, name, summary } }
}

function unsupportedContent(block, index) {
  const type = bounded(block?.type ?? '', 64) || 'unknown'
  return {
    block_id: `unsupported-${index}`,
    notice: { category: 'unsupported_content', text: `Unsupported content block: ${type}` },
  }
}

function contextNotice(event, role) {
  const source = event.data?.message?.source
  const kind = bounded(source?.kind ?? '', 64)
  if (!kind || kind === 'user') return null
  const text = CONTEXT_SOURCE_LABELS[kind] ?? 'Source context updated'
  return {
    message_id: messageId(event.sessionId, event.view, event.dshSeq),
    role: 'NOTICE',
    blocks: [{ block_id: 'context', notice: { category: 'source_context', text } }],
    authored_at_unix_ms: event.createdAt ?? 0,
  }
}

export function projectMessage(sessionId, view, event) {
  const type = event.type ?? ''
  if (type.startsWith('session/') || type.startsWith('turn/') || type.startsWith('step/') ||
      type.startsWith('approval/') || INTERNAL_TYPES.has(type)) return null
  const role = type === 'assistant/message' ? 'ASSISTANT' : type === 'user/message' ? 'USER'
    : type === 'tool/result' || type === 'tool/call' ? 'TOOL' : 'NOTICE'
  if (role === 'NOTICE') {
    // Unknown committed events are not user-visible activity. Only explicit
    // notices and error events earn a bounded notice row (PLUG-17).
    if (type !== 'notice' && !type.includes('error')) return null
    return {
      message_id: messageId(sessionId, view, event.dshSeq),
      role,
      blocks: [{ block_id: 'notice', notice: {
        category: type.includes('error') ? 'source_error' : 'source_notice',
        text: type.includes('error') ? 'Source reported an error' : 'Source activity',
      } }],
      authored_at_unix_ms: event.createdAt ?? 0,
    }
  }
  if (role === 'USER') {
    const context = contextNotice({ ...event, sessionId, view }, role)
    if (context) return context
  }
  const data = event.data ?? {}
  const content = data.message?.content ?? data.content
  const blocks = []
  if (type === 'tool/call') {
    blocks.push(toolCallContent(data, 0))
  }
  const text = typeof data.text === 'string' ? data.text : typeof data.message?.content === 'string' ? data.message.content : null
  if (text !== null) blocks.push({ block_id: 'text-0', text: { text } })
  if (Array.isArray(content)) for (const [index, block] of content.entries()) {
    if (block?.type === 'text' && typeof block.text === 'string') blocks.push({ block_id: `text-${index}`, text: { text: block.text } })
    else if (block?.type === 'tool-call' || block?.type === 'toolCall') blocks.push(toolCallContent(block, index))
    else if (block?.type === 'image' || block?.type === 'file' || block?.type === 'attachment') {
      blocks.push({ block_id: `attachment-${index}`, attachment: attachmentContent(block) })
    } else blocks.push(unsupportedContent(block, index))
  }
  if (role === 'TOOL' && blocks.length === 0) {
    const result = typeof data.result === 'string' ? data.result : 'Tool result'
    blocks.push({ block_id: 'result', tool_result: {
      call_id: bounded(data.callId ?? data.call_id ?? '', 256),
      text: result,
      is_error: data.isError === true,
    } })
  }
  // An empty but real user/assistant message must stay a visible row; an
  // unknown block type is already represented by its typed fallback notice.
  if (!blocks.length) blocks.push({ block_id: 'notice', notice: { category: 'source_notice', text: 'Source activity' } })
  return { message_id: messageId(sessionId, view, event.dshSeq), role, blocks, authored_at_unix_ms: event.createdAt ?? 0 }
}
