// Translate DSH's process-local frames at the adapter seam. Only visible text
// and ordering/lifecycle metadata cross it; provider/tool argument chunks do not.
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
    data.index = frame.index
    data.block_id = `text-${frame.chunk?.index ?? 0}`
    if (frame.chunk?.type === 'text-delta' && typeof frame.chunk.text === 'string') {
      data.text = frame.chunk.text
    } else if (frame.chunk?.type === 'block-end' && frame.chunk.block?.type === 'text' && typeof frame.chunk.block.text === 'string') {
      data.block_text = frame.chunk.block.text
    }
  } else if (frame.type === 'end') {
    data.outcome = frame.outcome?.kind === 'committed' ? 'committed' : 'abandoned'
    if(Number.isSafeInteger(frame.outcome?.seq))data.native_commit_seq=frame.outcome.seq
  } else return null
  return { sessionId, type: 'assistant/preview', kind: 'output', data,
    createdAt: now(), dshSeq: null, ephemeral: true }
}

// Reply coordinates are interpreted once, beside native stream translation.
// Consumers correlate an opaque reply ID rather than decoding DSH turn fields.
export function assistantReplyData(type, data) {
  if (type !== 'assistant/message' || !Number.isSafeInteger(data?.turn) || !Number.isSafeInteger(data?.step)) return data
  return { ...data, reply_id: `${data.turn}:${data.step}` }
}
