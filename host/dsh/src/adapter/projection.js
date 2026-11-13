// All native DSH payload interpretation stays at this seam.
import { createHash } from 'node:crypto'

export function messageId(sessionId, view, seq) {
  return createHash('sha256').update(JSON.stringify([sessionId, view, seq])).digest('hex')
}

export function projectMessage(sessionId, view, event) {
  const type = event.type ?? ''
  if (type.startsWith('session/') || type.startsWith('turn/') || type.startsWith('step/') ||
      type.startsWith('approval/') || ['agent/inbox/spliced','request/header','request/context','ignore','system/message','developer/message'].includes(type)) return null
  const role = type === 'assistant/message' ? 'ASSISTANT' : type === 'user/message' ? 'USER' : type === 'tool/result' ? 'TOOL' : 'NOTICE'
  if (role === 'NOTICE') return {message_id:messageId(sessionId,view,event.dshSeq),role,
    blocks:[{block_id:'notice',notice:{category:type.includes('error')?'source_error':'source_notice',text:type.includes('error')?'Source reported an error':'Source activity'}}],authored_at_unix_ms:event.createdAt??0}
  const data = event.data ?? {}
  const content = data.message?.content ?? data.content
  const blocks = []
  const text = typeof data.text === 'string' ? data.text : typeof data.message?.content === 'string' ? data.message.content : null
  if (text !== null) blocks.push({ block_id: 'text-0', text: {text} })
  if (Array.isArray(content)) for (const [index, block] of content.entries()) {
    if (block.type === 'text' && typeof block.text === 'string') blocks.push({block_id:`text-${index}`,text:{text:block.text}})
    else if (block.type === 'tool-call' || block.type === 'toolCall') blocks.push({block_id:`tool-${index}`,tool_call:{call_id:String(block.id??''),name:String(block.name??''),summary:''}})
    else blocks.push({block_id:`unsupported-${index}`,notice:{category:'unsupported_content',text:'Content is unavailable in this projection'}})
  }
  if (role === 'TOOL' && blocks.length === 0) blocks.push({block_id:'result',tool_result:{call_id:String(data.callId??''),text:typeof data.result === 'string'?data.result:'Tool result',is_error:data.isError===true}})
  if (!blocks.length) blocks.push({block_id:'notice',notice:{category:'source_notice',text:'Source activity'}})
  return {message_id:messageId(sessionId,view,event.dshSeq),role,blocks,authored_at_unix_ms:event.createdAt??0}
}
