// One bounded, content-free observation outlet per participating process.
//
// `trace` mode owns an NDJSON file and a background writer. Files are appended
// in acquisition order, a fixed byte cap bounds the capture, and a full or
// stalled queue drops facts instead of delaying product delivery. `off` and
// `aggregate` create no writer and perform no I/O; existing component metrics
// stay on their own sinks.
import { appendFile as fsAppendFile, open as fsOpen } from 'node:fs/promises'
import { createMonotonicClock } from './clock.js'
import { createTokenizer, tokenizeData } from './tokenizer.js'
import { loadManifest, validateRecord } from './manifest.js'

export const DEFAULT_CAPACITY = 4096
export const DEFAULT_BYTE_CAP = 32 * 1024 * 1024
export const DEFAULT_SHUTDOWN_TIMEOUT_MS = 5000
export const OUTLET_MODES = Object.freeze(['off', 'aggregate', 'trace'])
export const OUTLET_ROLES = Object.freeze(['host', 'dsh'])
const DRAIN_BATCH = 128

function nullStats(mode) {
  return {
    mode, enabled: mode === 'trace', written: 0, dropped: 0, rejected: 0,
    writeErrors: 0, bytes: 0, queued: 0, queuedBytes: 0, closed: mode !== 'trace', timedOut: false,
  }
}

/** A shared no-op sink. It allocates no file handle and never schedules work. */
export function createNullOutlet(mode = 'off') {
  const stats = () => nullStats(mode)
  const sink = {
    mode,
    enabled: false,
    emit() { return false },
    emitAt() { return false },
    token() { return '' },
    nowNs() { return 0 },
    stats,
    describe: stats,
    async close() { return stats() },
  }
  sink.forProducer = producer => ({
    producer,
    role: producer,
    mode,
    enabled: false,
    emit() { return false },
    emitAt() { return false },
    token() { return '' },
    nowNs() { return 0 },
    stats,
    describe: stats,
  })
  return sink
}

/**
 * Create the process outlet.
 *
 * @param {object} options
 * @param {'off'|'aggregate'|'trace'} [options.mode]
 * @param {string} [options.path] NDJSON capture file (trace only).
 * @param {string} [options.runId]
 * @param {string|Uint8Array} [options.key] ephemeral capture key (trace only).
 * @param {string} [options.lifetime] opaque run-local process lifetime.
 * @param {'host'|'dsh'} [options.role]
 * @param {object} [options.clock] createMonotonicClock-compatible clock.
 * @param {number} [options.capacity] bounded queue length.
 * @param {number} [options.byteCap] bounded accepted bytes.
 * @param {number} [options.shutdownTimeoutMs]
 * @param {Function} [options.appendFile] test seam for the append.
 * @param {Function} [options.openFile] test seam for close-time fsync.
 * @param {object} [options.manifest]
 */
export function createBenchOutlet({
  mode = 'off',
  path = '',
  runId = '',
  key = null,
  lifetime = '',
  role = 'host',
  clock = createMonotonicClock(),
  capacity = DEFAULT_CAPACITY,
  byteCap = DEFAULT_BYTE_CAP,
  shutdownTimeoutMs = DEFAULT_SHUTDOWN_TIMEOUT_MS,
  appendFile = fsAppendFile,
  openFile = fsOpen,
  manifest = loadManifest(),
} = {}) {
  if (!OUTLET_MODES.includes(mode)) throw new TypeError(`unsupported observation mode ${String(mode)}`)
  if (mode !== 'trace') return createNullOutlet(mode)
  if (typeof path !== 'string' || path === '') throw new TypeError('trace observation requires a capture path')
  if (typeof runId !== 'string' || runId === '') throw new TypeError('trace observation requires a run id')
  if (typeof lifetime !== 'string' || lifetime === '') throw new TypeError('trace observation requires a lifetime')
  if (!OUTLET_ROLES.includes(role)) throw new TypeError(`unsupported observation role ${String(role)}`)
  if (clock.verified !== true) throw new TypeError('trace observation requires a verified Linux monotonic clock')
  if (!Number.isSafeInteger(capacity) || capacity <= 0) throw new TypeError('observation capacity must be a positive integer')
  if (!Number.isSafeInteger(byteCap) || byteCap <= 0) throw new TypeError('observation byte cap must be a positive integer')
  const tokenizer = createTokenizer(key)
  return new TraceOutlet({
    path, runId, lifetime, role, clock, capacity, byteCap, shutdownTimeoutMs,
    appendFile, openFile, manifest, tokenizer,
  })
}

/** Build one outlet from the fixture-provided environment. */
export function createBenchOutletFromEnv({ role = 'host', env = process.env, ...overrides } = {}) {
  return createBenchOutlet({
    mode: env.WHALEREIN_BENCH_MODE ?? 'off',
    path: env.WHALEREIN_BENCH_OUTLET ?? '',
    runId: env.WHALEREIN_BENCH_RUN_ID ?? '',
    key: env.WHALEREIN_BENCH_CAPTURE_KEY ?? null,
    lifetime: env.WHALEREIN_BENCH_LIFETIME ?? role,
    role,
    ...overrides,
  })
}

class TraceOutlet {
  #queue = []
  #queuedBytes = 0
  #acceptedBytes = 0
  #draining = false
  #closed = false
  #closing = null
  #seq = 0
  #written = 0
  #dropped = 0
  #rejected = 0
  #writeErrors = 0
  #timedOut = false
  #idleWaiters = []

  constructor({ path, runId, lifetime, role, clock, capacity, byteCap, shutdownTimeoutMs, appendFile, openFile, manifest, tokenizer }) {
    Object.assign(this, { path, runId, lifetime, role, clock, capacity, byteCap, shutdownTimeoutMs, appendFile, openFile, manifest, tokenizer })
    this.mode = 'trace'
    this.enabled = true
  }

  nowNs() { return this.clock.nowNs() }

  /**
   * Derive one run-local token for an identity a caller already holds in raw
   * form. The facade's domain methods are not exported: a producer that needs
   * a derived composite (such as a batch) uses this seam instead.
   */
  token(domain, raw) { return this.tokenizer.token(domain, raw) }

  emit(event, data) {
    return this.#emitAt(this.clock.nowNs(), event, data, this.role)
  }

  emitAt(ts, event, data) {
    return this.#emitAt(ts, event, data, this.role)
  }

  /** A view that stamps the same shared queue with another producer role. */
  forProducer(producer) {
    if (!OUTLET_ROLES.includes(producer)) throw new TypeError(`unknown producer role ${String(producer)}`)
    return {
      producer,
      role: producer,
      mode: this.mode,
      enabled: true,
      emit: (event, data) => this.#emitAt(this.nowNs(), event, data, producer),
      emitAt: (ts, event, data) => this.#emitAt(ts, event, data, producer),
      token: (domain, raw) => this.tokenizer.token(domain, raw),
      nowNs: () => this.nowNs(),
      stats: () => this.stats(),
      describe: () => this.stats(),
    }
  }

  #emitAt(ts, event, data, producer) {
    if (this.#closed) { this.#dropped++; return false }
    const at = typeof ts === 'bigint' ? Number(ts) : Number(ts)
    if (!Number.isInteger(at) || at < 0) { this.#rejected++; return false }
    let record
    try {
      record = {
        v: this.manifest.header.v,
        run: this.runId,
        producer,
        lifetime: this.lifetime,
        seq: ++this.#seq,
        ts: at,
        event,
        data: tokenizeData(data, this.tokenizer),
      }
    } catch {
      this.#rejected++
      return false
    }
    const valid = validateRecord(record, this.manifest)
    if (!valid.ok) { this.#rejected++; return false }
    const line = `${JSON.stringify(record)}\n`
    const size = Buffer.byteLength(line, 'utf8')
    if (this.#queue.length >= this.capacity || this.#acceptedBytes + size > this.byteCap) {
      this.#dropped++
      return false
    }
    this.#queue.push(line)
    this.#queuedBytes += size
    this.#acceptedBytes += size
    this.#schedule()
    return true
  }

  #schedule() {
    if (this.#draining) return
    this.#draining = true
    void this.#drain()
  }

  async #drain() {
    try {
      while (this.#queue.length > 0) {
        const batch = this.#queue.splice(0, DRAIN_BATCH)
        let size = 0
        for (const line of batch) size += Buffer.byteLength(line, 'utf8')
        this.#queuedBytes -= size
        try {
          await this.appendFile(this.path, batch.join(''), 'utf8')
          this.#written += batch.length
        } catch {
          // A sink failure is evidence, never a product failure. The batch is
          // lost and counted; delivery keeps its own path.
          this.#writeErrors++
        }
      }
    } finally {
      this.#draining = false
      this.#notifyIdle()
    }
  }

  #notifyIdle() {
    if (this.#draining || this.#queue.length > 0) return
    for (const waiter of this.#idleWaiters.splice(0)) waiter(true)
  }

  #waitForDrain() {
    if (!this.#draining && this.#queue.length === 0) return Promise.resolve(true)
    return new Promise((resolve) => {
      const timer = setTimeout(() => resolve(false), this.shutdownTimeoutMs)
      timer.unref?.()
      this.#idleWaiters.push((value) => { clearTimeout(timer); resolve(value) })
    })
  }

  stats() {
    return {
      mode: this.mode,
      enabled: true,
      written: this.#written,
      dropped: this.#dropped,
      rejected: this.#rejected,
      writeErrors: this.#writeErrors,
      bytes: this.#acceptedBytes,
      queued: this.#queue.length,
      queuedBytes: this.#queuedBytes,
      closed: this.#closed,
      timedOut: this.#timedOut,
    }
  }

  describe() { return this.stats() }

  async close() {
    if (this.#closing) return this.#closing
    this.#closed = true
    this.#closing = this.#close()
    return this.#closing
  }

  async #close() {
    if (!this.#draining && this.#queue.length > 0) this.#schedule()
    const drained = await this.#waitForDrain()
    if (!drained) {
      this.#timedOut = true
      return this.stats()
    }
    if (this.#written > 0) {
      try {
        const handle = await this.openFile(this.path, 'a')
        try { await handle.sync() } finally { await handle.close() }
      } catch {
        this.#writeErrors++
      }
    }
    return this.stats()
  }
}