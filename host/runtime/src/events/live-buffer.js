// Coalescing normalized live previews. A pending preview may absorb an adjacent
// delta only when the Attempt, block, checked block kind and normalized sequence
// stay identical. Settlement/reset/state/history updates always flush first so a
// preview can never overtake the authoritative update that retires it.

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
  #timer = null
  constructor(publish, delayMs = 20) { this.publish = publish; this.delayMs = delayMs }

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
    const previous = this.#pending?.live
    if (previous && !sameIdentity(previous, frame)) this.flush()
    if (previous && bytes(previous.delta.text) + bytes(frame.delta.text) > MAX_PREVIEW_BYTES) this.flush()
    if (!this.#pending) {
      this.#pending = { ...update, live: { ...frame, delta: { ...frame.delta } } }
      this.#timer = setTimeout(() => this.flush(), this.delayMs)
      this.#timer.unref?.()
    } else {
      this.#pending.live.delta.text += frame.delta.text
      this.#pending.live.last_seq = frame.last_seq
    }
    if (bytes(this.#pending.live.delta.text) >= MAX_PREVIEW_BYTES) this.flush()
  }

  flush() {
    clearTimeout(this.#timer)
    this.#timer = null
    const update = this.#pending
    this.#pending = null
    if (update) this.publish(update)
  }
}
