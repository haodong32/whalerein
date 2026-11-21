// Preview splitting for oversized typed live deltas.
//
// Only complete, individually displayable updates are produced. Every split
// stays on a valid UTF-8 boundary, keeps the exact text prefix once, and tiles
// the original normalized sequence interval with contiguous sub-intervals.

import { AdapterError } from '../errors.js'

export const MAX_SPLIT_PARTS = 64
// Per-tick amplification bound. A larger delta is carried by the next updates
// or recovered from committed history; it is never expanded without bound.
export const MAX_SPLIT_BYTES = 256 * 1024

export function utf8ByteLength(text) {
  return Buffer.byteLength(String(text ?? ''), 'utf8')
}

/**
 * Split text into parts of at most `maxBytes` UTF-8 bytes without cutting a
 * code point (and therefore never a surrogate pair or a multi-byte sequence).
 * The concatenation of the parts is exactly `text`.
 */
export function splitUtf8(text, maxBytes) {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 1) {
    throw new AdapterError('invalid_argument', 'preview split byte budget invalid')
  }
  const parts = []
  let current = ''
  let currentBytes = 0
  for (const character of String(text ?? '')) {
    const size = Buffer.byteLength(character, 'utf8')
    if (size > maxBytes) {
      // A single code point that cannot fit any packet is unrepresentable, not
      // a boundary-splitting opportunity.
      throw new AdapterError('payload_too_large', 'one code point exceeds the preview packet budget')
    }
    if (currentBytes + size > maxBytes) {
      parts.push(current)
      current = character
      currentBytes = size
    } else {
      current += character
      currentBytes += size
    }
  }
  if (current !== '') parts.push(current)
  return parts
}

/** Contiguous, gap-free and overlap-free sub-intervals of one interval. */
export function partitionInterval(firstSeq, lastSeq, count) {
  const total = lastSeq - firstSeq + 1
  if (!Number.isSafeInteger(total) || total < 1 || !Number.isSafeInteger(count) || count < 1 || count > total) {
    throw new AdapterError('invalid_argument', 'preview interval cannot be partitioned')
  }
  const intervals = []
  for (let index = 0; index < count; index++) {
    const start = firstSeq + Math.floor((index * total) / count)
    const end = firstSeq + Math.floor(((index + 1) * total) / count) - 1
    intervals.push([start, end])
  }
  return intervals
}

/**
 * Split one live delta update.
 *
 * `maxTextBytes` is the largest delta text byte length whose complete encoded
 * EventEnvelope still fits the smallest usable packet budget on the route.
 * Returns an array of updates, or null when the interval cannot be tiled (for
 * example one huge token with a one-unit interval); callers then use the
 * complete-message reference carrier instead of a partial or reset event.
 */
export function splitLivePreview(update, { maxTextBytes, maxParts = MAX_SPLIT_PARTS, maxBytes = MAX_SPLIT_BYTES } = {}) {
  const live = update?.live
  const text = live?.delta?.text
  if (typeof text !== 'string') return null
  if (!Number.isSafeInteger(maxTextBytes) || maxTextBytes < 1) return null
  if (utf8ByteLength(text) <= maxTextBytes) return [update]
  const firstSeq = Number(live.first_seq)
  const lastSeq = Number(live.last_seq)
  const range = lastSeq - firstSeq + 1
  if (!Number.isSafeInteger(range) || range < 2) return null

  let parts
  try {
    parts = splitUtf8(text, maxTextBytes)
  } catch {
    return null
  }
  if (parts.length <= 1) return null
  // Amplification is bounded by refusing the split, not by dropping text: the
  // complete-message reference carrier then delivers the same bytes once.
  if (parts.length > maxParts || parts.length > range) return null
  const total = parts.reduce((sum, part) => sum + utf8ByteLength(part), 0)
  if (total > maxBytes) return null

  const intervals = partitionInterval(firstSeq, lastSeq, parts.length)
  return parts.map((part, index) => {
    const [start, end] = intervals[index]
    return {
      ...update,
      live: {
        ...live,
        first_seq: start,
        last_seq: end,
        delta: { ...live.delta, text: part },
      },
    }
  })
}
