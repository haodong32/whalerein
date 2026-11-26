export {
  CLOCK_EPOCH, CLOCK_SOURCE, ObservationClockError, createMonotonicClock,
  verifyNodeMonotonicClock,
} from './clock.js'
export {
  OBSERVATION_MANIFEST_PATH, OBSERVATION_VECTORS_PATH, ObservationError, assertRecord,
  loadManifest, loadVectors, validateRecord,
} from './manifest.js'
export {
  DEFAULT_BYTE_CAP, DEFAULT_CAPACITY, DEFAULT_SHUTDOWN_TIMEOUT_MS, OUTLET_MODES,
  OUTLET_ROLES, createBenchOutlet, createBenchOutletFromEnv, createNullOutlet,
} from './outlet.js'
export { createTokenizer, tokenizeData } from './tokenizer.js'

/**
 * Emit a fact without letting an observation failure escape into the product
 * path. Modules call this from hot boundaries; a rejected record is counted by
 * the outlet and remains visible in its stats.
 */
export function emitFact(outlet, event, data) {
  if (!outlet || typeof outlet.emit !== 'function') return false
  try {
    return outlet.emit(event, data)
  } catch {
    return false
  }
}

/** Emit a fact at a timestamp captured earlier at the owning boundary. */
export function emitFactAt(outlet, ts, event, data) {
  if (!outlet || typeof outlet.emitAt !== 'function') return false
  try {
    return outlet.emitAt(ts, event, data)
  } catch {
    return false
  }
}

/**
 * Tokenize the batch raw identity `<stream>:<seq_start>:<seq_end>`.
 *
 * `batch` is not an outlet-tokenized field: the producer holds the raw stream
 * and sequence interval, and the outlet must never guess which event a field
 * belongs to. Passing the result keeps one opaque batch token identical across
 * the adapter, Host queue and transport facts for the same interval. A
 * disabled outlet (or an invalid interval) yields an empty field, which the
 * manifest accepts as an absent batch.
 */
export function batchToken(outlet, stream, seqStart, seqEnd) {
  if (!outlet || typeof outlet.token !== 'function') return ''
  if (typeof stream !== 'string' || stream === '') return ''
  if (!Number.isSafeInteger(seqStart) || !Number.isSafeInteger(seqEnd)) return ''
  if (seqStart < 0 || seqEnd < 0) return ''
  try {
    return outlet.token('batch', `${stream}:${seqStart}:${seqEnd}`)
  } catch {
    return ''
  }
}