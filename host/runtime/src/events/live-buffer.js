// Coalescing normalized live previews. A pending preview may absorb an adjacent
// delta only when the Attempt, block, checked block kind and normalized sequence
// stay identical. Settlement/reset/state/history updates always flush first so a
// preview can never overtake the authoritative update that retires it.

// Conservative default until the active route's smallest authenticated packet
// budget is known. The Host sets the effective value per Source.
export const MAX_PREVIEW_BYTES = 8 * 1024

function bytes(value) {
  return typeof value === 'string' ? Buffer.byteLength(value, 'utf8') : 0
}

function sameIdentity(previous, frame) {
  return previous.attempt?.live_epoch === frame.attempt?.live_epoch
    && previous.attempt?.attempt_id === frame.attempt?.attempt_id
    && previous.block_id === frame.block_id
    && previous.delta?.kind === frame.delta?.kind
    && previous.last_seq + 1 === frame.first_seq
}

export class LiveBuffer {
  #pending = null
  #lastDeltaAt = -Infinity
  #frames = 0
  #timer = null
  #maxBytes
  #observe
  constructor(publish, delayMs = 20, maxBytes = MAX_PREVIEW_BYTES, observe = null) {
    this.publish = publish
    this.delayMs = delayMs
    this.#maxBytes = Number.isSafeInteger(maxBytes) && maxBytes > 0 ? maxBytes : MAX_PREVIEW_BYTES
    // Optional observer for the coalescing boundary. It receives raw identities;
    // the process outlet is the only component that tokenizes them.
    this.#observe = typeof observe === 'function' ? observe : null
  }

  #notify(frame, frames, bytes) {
    if (!this.#observe) return
    try {
      this.#observe({
        stream: frame.attempt?.attempt_id,
        block: frame.block_id,
        seq_start: frame.first_seq,
        seq_end: frame.last_seq,
        frames,
        bytes,
      })
    } catch {
      // Observation never changes preview delivery.
    }
  }

  get maxBytes() { return this.#maxBytes }

  /** Apply the smallest usable preview budget for the active route. */
  setMaxBytes(maxBytes) {
    if (Number.isSafeInteger(maxBytes) && maxBytes > 0) this.#maxBytes = maxBytes
  }

  /** Whether a coalesced preview is waiting for its short hold window. */
  get pending() { return this.#pending !== null }

  push(update) {
    const frame = update.live
    if (!frame?.delta) {
      // started/ended/reset/history/state/questions settle before the preview.
      this.flush()
      this.publish(update)
      return
    }
    const now = performance.now()
    const idle = now - this.#lastDeltaAt >= this.delayMs
    this.#lastDeltaAt = now
    const previous = this.#pending?.live
    if (previous && (idle || !sameIdentity(previous, frame))) this.flush()
    if (previous && bytes(previous.delta.text) + bytes(frame.delta.text) > this.#maxBytes) this.flush()
    if (!this.#pending) {
      this.#pending = { ...update, live: { ...frame, delta: { ...frame.delta } } }
      this.#frames = 1
      if (idle) {
        this.flush()
        return
      }
      this.#timer = setTimeout(() => this.flush(), this.delayMs)
      this.#timer.unref?.()
    } else {
      this.#pending.live.delta.text += frame.delta.text
      this.#pending.live.last_seq = frame.last_seq
      this.#frames += 1
    }
    if (bytes(this.#pending.live.delta.text) >= this.#maxBytes) this.flush()
  }

  flush() {
    clearTimeout(this.#timer)
    this.#timer = null
    const update = this.#pending
    const frames = this.#frames
    this.#pending = null
    this.#frames = 0
    if (!update) return
    // One fact per published batch, stamped when the batch is settled. A
    // singleton batch is reported too: its interval is the join key the
    // adapter handoff and the Host transport facts share, and omitting it
    // would leave the common uncoalesced preview without a batch boundary.
    this.#notify(update.live, frames, bytes(update.live.delta.text))
    this.publish(update)
  }
}
