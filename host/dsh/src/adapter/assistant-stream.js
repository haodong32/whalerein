// Translate DSH's process-local frames at the adapter seam. Only visible
// reasoning/text and ordering/lifecycle metadata cross it; provider/tool
// argument chunks do not.
//
// Native StreamChunk vocabulary (dsh-llm/lib/types/types.d.ts):
//   { type: 'text-delta', index, text }
//   { type: 'reasoning-delta', index, text }
//   { type: 'block-end', index, block: TextBlock | ReasoningBlock | ToolCallBlock | ... }
// The block index is the only stable native block identity, so the adapter
// derives its own `block_id` from it and keeps reasoning and answer blocks in
// separate namespaces. Unsupported chunks are never coerced into text.
export function assistantStreamEvent({ agent, frame } = {}, now = Date.now) {
  const sessionId = agent?.session?.id
  if (typeof sessionId !== 'string' || !sessionId || !frame ||
      typeof frame.attemptId !== 'string' || frame.attemptId.length > 512 ||
      !Number.isSafeInteger(frame.revision) || frame.revision < 0) return null
  const data = { stream_id: frame.attemptId, revision: frame.revision, phase: frame.type }
  if (frame.type === 'start') {
    if (!Number.isSafeInteger(frame.turn) || !Number.isSafeInteger(frame.step)) return null
    data.reply_id = `${frame.turn}:${frame.step}`
  } else if (frame.type === 'chunk') {
    if (!Number.isSafeInteger(frame.index) || frame.index < 0) return null
    const chunk = frame.chunk
    if (!chunk || typeof chunk !== 'object') return null
    // `chunk.index` is the provider block index; `frame.index` is the dense
    // chunk-frame position and is only a compatibility fallback for fixtures.
    const blockIndex = Number.isSafeInteger(chunk.index) && chunk.index >= 0 ? chunk.index : frame.index
    data.index = blockIndex
    if (chunk.type === 'text-delta') {
      if (typeof chunk.text !== 'string') return null
      data.block_id = `text-${blockIndex}`
      data.block_kind = 'BLOCK_KIND_TEXT'
      data.text = chunk.text
    } else if (chunk.type === 'reasoning-delta') {
      if (typeof chunk.text !== 'string') return null
      data.block_id = `reasoning-${blockIndex}`
      data.block_kind = 'BLOCK_KIND_REASONING'
      data.text = chunk.text
    } else if (chunk.type === 'block-end') {
      const block = chunk.block
      if (block?.type === 'text' && typeof block.text === 'string') {
        data.block_id = `text-${blockIndex}`
        data.block_kind = 'BLOCK_KIND_TEXT'
        data.block_text = block.text
      } else if (block?.type === 'reasoning' && typeof block.text === 'string') {
        data.block_id = `reasoning-${blockIndex}`
        data.block_kind = 'BLOCK_KIND_REASONING'
        data.block_text = block.text
      } else {
        // A durable tool-call block or an unmapped provider block is not live
        // visible text. It settles through committed history instead.
        return null
      }
    } else {
      return null
    }
  } else if (frame.type === 'end') {
    data.outcome = frame.outcome?.kind === 'committed' ? 'committed' : 'abandoned'
    if (Number.isSafeInteger(frame.outcome?.seq)) data.native_commit_seq = frame.outcome.seq
  } else return null
  return { sessionId, type: 'assistant/preview', kind: 'output', data,
    createdAt: now(), dshSeq: null, ephemeral: true }
}

// Reply coordinates are interpreted once, beside native stream translation.
// Consumers correlate an opaque reply ID rather than decoding DSH turn fields.
export function assistantReplyData(type, data) {
  if (type !== 'assistant/message' || !Number.isSafeInteger(data?.turn) || data.turn < 1 || !Number.isSafeInteger(data?.step)) return data
  return { ...data, reply_id: `${data.turn}:${data.step}` }
}
