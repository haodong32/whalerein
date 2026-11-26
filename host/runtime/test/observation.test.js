import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, existsSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  CLOCK_EPOCH, CLOCK_SOURCE, ObservationClockError, createMonotonicClock,
  verifyNodeMonotonicClock,
} from '../src/observation/clock.js'
import {
  ObservationError, assertRecord, loadManifest, loadVectors, validateRecord,
} from '../src/observation/manifest.js'
import {
  createBenchOutlet, createBenchOutletFromEnv, createNullOutlet,
} from '../src/observation/outlet.js'
import { TOKEN_DOMAINS, createTokenizer, tokenizeData } from '../src/observation/tokenizer.js'
import { batchToken, emitFact, emitFactAt } from '../src/observation/index.js'

const CAPTURE_KEY = '0123456789abcdef0123456789abcdef'
const RUN = 'run-test'
const LIFETIME = 'lt-test-1'
const HOST_ENQUEUE = {
  stream: 'raw-stream', block: 'raw-block', seq_start: 0, seq_end: 2, bytes: 42,
  queue_depth: 1, queue_bytes: 42,
}
const DSH_CALLBACK = {
  stream: 'raw-stream', attempt: 'raw-attempt', block: 'raw-block',
  seq_start: 0, seq_end: 2, bytes: 42, native: 'assistant-preview',
}

function tempDir(t) {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-observation-'))
  t.after(() => rmSync(dir, { recursive: true, force: true }))
  return dir
}

function incrementingClock(start = 1000) {
  let value = start
  return {
    source: CLOCK_SOURCE,
    epoch: CLOCK_EPOCH,
    verified: true,
    reason: null,
    nowNs: () => BigInt(value++),
  }
}

function readRecords(path) {
  return readFileSync(path, 'utf8').trim().split('\n').filter(Boolean).map(line => JSON.parse(line))
}

function deferred() {
  const value = {}
  value.promise = new Promise((resolve, reject) => { value.resolve = resolve; value.reject = reject })
  return value
}

test('latency manifests accept every current vector and reject dangerous ones', () => {
  const manifest = loadManifest()
  const vectors = loadVectors()
  assert.equal(manifest.schema, 1)
  assert.ok(vectors.valid.length > 0)
  for (const record of vectors.valid) {
    const result = validateRecord(record, manifest)
    assert.equal(result.ok, true, JSON.stringify(record))
  }
  const reasons = new Set()
  for (const { why, value } of vectors.invalid) {
    const result = validateRecord(value, manifest)
    assert.equal(result.ok, false, why)
    reasons.add(result.reason)
  }
  assert.deepEqual([...reasons].sort(), [
    'forbidden_field', 'invalid_timestamp', 'missing_required_field',
    'unknown_enum_value', 'unknown_event', 'wrong_producer',
  ])
  assert.throws(() => assertRecord(vectors.invalid[0].value, manifest), ObservationError)
})

test('latency/AC-06.privacy rejects content, unknown fields and malformed envelopes', () => {
  const manifest = loadManifest()
  const base = {
    v: 1, run: RUN, producer: 'host', lifetime: LIFETIME, seq: 1, ts: 1,
    event: 'host_enqueue', data: { ...HOST_ENQUEUE },
  }
  assert.equal(validateRecord(base, manifest).ok, true)
  assert.equal(validateRecord({ ...base, data: { ...HOST_ENQUEUE, text: 'secret' } }, manifest).reason, 'forbidden_field')
  assert.equal(validateRecord({ ...base, text: 'secret' }, manifest).reason, 'forbidden_field')
  assert.equal(validateRecord({ ...base, data: { ...HOST_ENQUEUE, extra: 1 } }, manifest).reason, 'unknown_data_field')
  assert.equal(validateRecord({ ...base, data: {} }, manifest).reason, 'missing_required_field')
  assert.equal(validateRecord({ ...base, event: 'core_receipt', data: {} }, manifest).reason, 'wrong_producer')
  assert.equal(validateRecord({ ...base, producer: 'nobody' }, manifest).reason, 'unknown_producer')
  assert.equal(validateRecord({ ...base, v: 2 }, manifest).reason, 'schema_version_mismatch')
  assert.equal(validateRecord({ ...base, run: '' }, manifest).reason, 'invalid_run')
  assert.equal(validateRecord({ ...base, lifetime: '' }, manifest).reason, 'invalid_lifetime')
  assert.equal(validateRecord({ ...base, seq: -1 }, manifest).reason, 'invalid_seq')
  assert.equal(validateRecord({ ...base, ts: -1 }, manifest).reason, 'invalid_timestamp')
  assert.equal(validateRecord({ ...base, data: 'x' }, manifest).reason, 'missing_data')
  assert.equal(validateRecord({ ...base, data: { ...HOST_ENQUEUE, stream: 'x'.repeat(513) } }, manifest).reason, 'oversized_string')
  assert.equal(validateRecord({ ...base, data: { ...HOST_ENQUEUE, queue_depth: -1 } }, manifest).reason, 'invalid_u64')
  assert.equal(validateRecord(null, manifest).reason, 'record_not_object')
  const missing = { ...base }
  delete missing.ts
  assert.equal(validateRecord(missing, manifest).reason, 'missing_header_field')
})

test('manifest validation covers an unknown field type instead of accepting it', () => {
  const manifest = structuredClone(loadManifest())
  manifest.events.host_enqueue.fields.queue_depth = 'mystery'
  const record = {
    v: 1, run: RUN, producer: 'host', lifetime: LIFETIME, seq: 1, ts: 1,
    event: 'host_enqueue', data: { ...HOST_ENQUEUE },
  }
  assert.equal(validateRecord(record, manifest).reason, 'unknown_field_type')
  manifest.events.host_enqueue.fields.queue_depth = 'bool'
  assert.equal(validateRecord(record, manifest).reason, 'invalid_bool')
  manifest.events.host_enqueue.fields.queue_depth = 'str'
  assert.equal(validateRecord(record, manifest).reason, 'invalid_string')
})

test('latency/AC-05.wrong-clock-or-origin rejects unverified clocks and foreign platforms', () => {
  assert.deepEqual(verifyNodeMonotonicClock({ platform: 'darwin' }), {
    ok: false, reason: 'platform_not_linux', source: CLOCK_SOURCE, epoch: CLOCK_EPOCH,
  })
  assert.equal(verifyNodeMonotonicClock({ platform: 'linux', hrtime: () => 1 }).reason, 'not_nanoseconds')
  assert.equal(verifyNodeMonotonicClock({ platform: 'linux', hrtime: () => -1n }).reason, 'not_monotonic')
  assert.equal(verifyNodeMonotonicClock({ platform: 'linux', hrtime: () => { throw new Error('no clock') } }).reason, 'source_unavailable')
  let n = 0
  const proof = verifyNodeMonotonicClock({ platform: 'linux', hrtime: () => BigInt(n++) })
  assert.equal(proof.ok, true)
  const clock = createMonotonicClock({ platform: 'linux', hrtime: () => BigInt(n++) })
  assert.equal(clock.verified, true)
  assert.ok(clock.nowNs() > 0n)
  const rejected = createMonotonicClock({ platform: 'darwin' })
  assert.equal(rejected.verified, false)
  assert.throws(() => rejected.nowNs(), ObservationClockError)
  assert.throws(() => createMonotonicClock({ platform: 'linux', hrtime: () => 1 }).nowNs(), ObservationClockError)
  assert.throws(() => createMonotonicClock({ platform: 'linux', hrtime: () => -1n }).nowNs(), ObservationClockError)
})

test('token derivation is stable, domain-separated and never reveals the raw identity', () => {
  const tokenizer = createTokenizer(CAPTURE_KEY)
  const first = tokenizer.stream('secret-stream')
  assert.equal(first, tokenizer.stream('secret-stream'))
  assert.notEqual(first, tokenizer.watch('secret-stream'))
  assert.notEqual(first, createTokenizer('fedcba9876543210fedcba9876543210').stream('secret-stream'))
  assert.ok(!first.includes('secret-stream'))
  assert.equal(tokenizer.stream(''), '')
  assert.equal(tokenizer.stream(null), '')
  assert.equal(tokenizer.attempt(7), tokenizer.attempt('7'))
  for (const domain of TOKEN_DOMAINS) assert.equal(typeof tokenizer[domain]('x'), 'string')
  assert.ok(TOKEN_DOMAINS.includes('batch'))
  assert.equal(tokenizer.batch('raw-stream:1:3'), tokenizer.token('batch', 'raw-stream:1:3'))
  assert.notEqual(tokenizer.batch('raw-stream:1:3'), tokenizer.batch('raw-stream:1:4'))
  assert.throws(() => tokenizer.token('nope', 'x'), /unknown token domain/)
  assert.throws(() => tokenizer.stream({}), /token identity/)
  assert.throws(() => tokenizer.stream(-1), /token identity/)
  assert.throws(() => createTokenizer('short'), /capture key/)
  assert.throws(() => createTokenizer(new Uint8Array(3)), /capture key/)
  const tokenized = tokenizeData({ stream: 'raw', block: 'raw', seq_start: 1, unknown: 'raw' }, tokenizer)
  assert.notEqual(tokenized.stream, 'raw')
  assert.equal(tokenized.unknown, 'raw')
  assert.equal(tokenized.seq_start, 1)
  // A caller already derived `batch`; the outlet must not hash it again.
  const derived = tokenizer.batch('raw-stream:1:3')
  assert.equal(tokenizeData({ batch: derived }, tokenizer).batch, derived)
  assert.equal(tokenizeData({ batch: 'raw-stream:1:3' }, tokenizer).batch, 'raw-stream:1:3')
})

test('off and aggregate modes create no writer and perform no I/O', async () => {
  for (const mode of ['off', 'aggregate']) {
    let touched = false
    const outlet = createBenchOutlet({
      mode, path: '/never/used', runId: RUN, key: CAPTURE_KEY, lifetime: LIFETIME,
      appendFile: () => { touched = true },
    })
    assert.equal(outlet.enabled, false)
    assert.equal(outlet.mode, mode)
    assert.equal(emitFact(outlet, 'host_enqueue', HOST_ENQUEUE), false)
    assert.equal(outlet.emitAt(1, 'host_enqueue', HOST_ENQUEUE), false)
    assert.equal(outlet.nowNs(), 0)
    assert.equal(outlet.token('batch', 'raw-stream:0:2'), '')
    const view = outlet.forProducer('dsh')
    assert.equal(view.emit('dsh_callback', DSH_CALLBACK), false)
    assert.equal(view.token('batch', 'raw-stream:0:2'), '')
    assert.equal((await outlet.close()).written, 0)
    assert.equal(touched, false)
    assert.equal(outlet.describe().enabled, false)
  }
  const fromEnv = createBenchOutletFromEnv({ env: { WHALEREIN_BENCH_MODE: 'off' } })
  assert.equal(fromEnv.mode, 'off')
  assert.equal(createNullOutlet('aggregate').forProducer('dsh').nowNs(), 0)
})

test('trace creation rejects incomplete or unverified capture parameters', () => {
  const clock = incrementingClock()
  const valid = { mode: 'trace', path: '/tmp/x', runId: RUN, key: CAPTURE_KEY, lifetime: LIFETIME, clock }
  assert.throws(() => createBenchOutlet({ ...valid, mode: 'weird' }), /unsupported observation mode/)
  assert.throws(() => createBenchOutlet({ ...valid, path: '' }), /capture path/)
  assert.throws(() => createBenchOutlet({ ...valid, runId: '' }), /run id/)
  assert.throws(() => createBenchOutlet({ ...valid, lifetime: '' }), /lifetime/)
  assert.throws(() => createBenchOutlet({ ...valid, role: 'core' }), /observation role/)
  assert.throws(() => createBenchOutlet({ ...valid, capacity: 0 }), /capacity/)
  assert.throws(() => createBenchOutlet({ ...valid, byteCap: 0 }), /byte cap/)
  assert.throws(() => createBenchOutlet({ ...valid, clock: { verified: false } }), /verified Linux monotonic clock/)
  assert.throws(() => createBenchOutlet({ ...valid, key: 'short' }), /capture key/)
  assert.throws(() => createBenchOutlet({ ...valid, clock }).forProducer('core'), /unknown producer role/)
})

test('latency/AC-07.shared-process-outlet writes host and dsh roles through one bounded queue', async (t) => {
  const dir = tempDir(t)
  const path = join(dir, 'outlet.ndjson')
  const outlet = createBenchOutlet({
    mode: 'trace', path, runId: RUN, key: CAPTURE_KEY, lifetime: LIFETIME, role: 'host',
    clock: incrementingClock(),
  })
  const dsh = outlet.forProducer('dsh')
  const dshBatch = dsh.token('batch', 'raw-stream:0:2')
  assert.equal(outlet.emit('host_enqueue', HOST_ENQUEUE), true)
  assert.equal(dsh.emit('dsh_callback', { ...DSH_CALLBACK, batch: dshBatch }), true)
  assert.equal(outlet.emitAt(999, 'host_publish', {
    request: 'raw-request', attempt: 1, kind: 'result', bytes: 12, outcome: 'accepted',
  }), true)
  assert.equal(outlet.emit('host_command', {
    request: 'raw-request', attempt: 2, stage: 'finish', outcome: 'ok',
  }), true)
  const stats = await outlet.close()
  assert.equal(stats.written, 4)
  assert.equal(stats.writeErrors, 0)
  const records = readRecords(path)
  assert.deepEqual(records.map(record => record.producer), ['host', 'dsh', 'host', 'host'])
  assert.deepEqual(records.map(record => record.seq), [1, 2, 3, 4])
  assert.equal(records[0].run, RUN)
  assert.equal(records[3].data.attempt, 2, 'a numeric transport attempt stays numeric')
  assert.equal(records[0].lifetime, LIFETIME)
  assert.equal(records[1].data.batch, dshBatch, 'a caller-derived batch is written as-is')
  const text = readFileSync(path, 'utf8')
  for (const raw of ['raw-stream', 'raw-block', 'raw-attempt', 'raw-request']) {
    assert.equal(text.includes(raw), false, `${raw} must never enter a capture`)
  }
})

test('an invalid or closed event is rejected or dropped without throwing', async (t) => {
  const dir = tempDir(t)
  const path = join(dir, 'outlet.ndjson')
  const outlet = createBenchOutlet({
    mode: 'trace', path, runId: RUN, key: CAPTURE_KEY, lifetime: LIFETIME, clock: incrementingClock(),
  })
  assert.equal(outlet.emit('not_an_event', {}), false)
  assert.equal(outlet.emit('host_enqueue', { ...HOST_ENQUEUE, text: 'secret' }), false)
  assert.equal(outlet.emitAt(-1, 'host_enqueue', HOST_ENQUEUE), false)
  assert.equal(outlet.stats().rejected, 3)
  assert.equal(outlet.emit('host_enqueue', HOST_ENQUEUE), true)
  await outlet.close()
  assert.equal(outlet.emit('host_enqueue', HOST_ENQUEUE), false)
  assert.equal(outlet.stats().dropped, 1)
  assert.equal(outlet.stats().closed, true)
  assert.equal(readRecords(path).length, 1)
})

test('latency/AC-06.saturation drops records at the bounded queue and byte cap', async (t) => {
  const dir = tempDir(t)
  const path = join(dir, 'queue.ndjson')
  const blocked = deferred()
  const outlet = createBenchOutlet({
    mode: 'trace', path, runId: RUN, key: CAPTURE_KEY, lifetime: LIFETIME,
    clock: incrementingClock(), capacity: 2, appendFile: () => blocked.promise,
    openFile: async () => ({ sync: async () => {}, close: async () => {} }),
  })
  let accepted = 0
  for (let index = 0; index < 6; index += 1) if (outlet.emit('host_enqueue', HOST_ENQUEUE)) accepted += 1
  assert.equal(accepted, 3)
  assert.ok(outlet.stats().dropped >= 3)
  assert.ok(outlet.stats().queued <= 2)
  const dropped = outlet.stats().dropped
  blocked.resolve()
  const stats = await outlet.close()
  assert.equal(stats.timedOut, false)
  assert.ok(stats.written + dropped >= 1)

  const capped = createBenchOutlet({
    mode: 'trace', path: join(dir, 'cap.ndjson'), runId: RUN, key: CAPTURE_KEY, lifetime: LIFETIME,
    clock: incrementingClock(), byteCap: 120,
    appendFile: () => new Promise(() => {}),
    openFile: async () => ({ sync: async () => {}, close: async () => {} }),
  })
  let capAccepted = 0
  for (let index = 0; index < 6; index += 1) if (capped.emit('host_enqueue', HOST_ENQUEUE)) capAccepted += 1
  assert.ok(capAccepted <= 2)
  assert.ok(capped.stats().dropped >= 4)
})

test('a slow consumer times out shutdown while the product path stays available', async (t) => {
  const dir = tempDir(t)
  const outlet = createBenchOutlet({
    mode: 'trace', path: join(dir, 'slow.ndjson'), runId: RUN, key: CAPTURE_KEY, lifetime: LIFETIME,
    clock: incrementingClock(), shutdownTimeoutMs: 20,
    appendFile: () => new Promise(() => {}),
    openFile: async () => ({ sync: async () => {}, close: async () => {} }),
  })
  assert.equal(outlet.emit('host_enqueue', HOST_ENQUEUE), true)
  assert.equal(outlet.emit('host_enqueue', HOST_ENQUEUE), true)
  const stats = await outlet.close()
  assert.equal(stats.timedOut, true)
  assert.equal(stats.closed, true)
})

test('a write error is counted and never thrown into delivery; close flushes and fsyncs', async (t) => {
  const dir = tempDir(t)
  const failed = createBenchOutlet({
    mode: 'trace', path: join(dir, 'failed.ndjson'), runId: RUN, key: CAPTURE_KEY, lifetime: LIFETIME,
    clock: incrementingClock(), appendFile: async () => { throw new Error('disk full') },
    openFile: async () => ({ sync: async () => {}, close: async () => {} }),
  })
  assert.equal(failed.emit('host_enqueue', HOST_ENQUEUE), true)
  assert.equal(failed.emit('host_enqueue', HOST_ENQUEUE), true)
  const failedStats = await failed.close()
  assert.equal(failedStats.writeErrors >= 1, true)
  assert.equal(failedStats.written, 0)

  const path = join(dir, 'flushed.ndjson')
  const appended = []
  let synced = 0
  const flushed = createBenchOutlet({
    mode: 'trace', path, runId: RUN, key: CAPTURE_KEY, lifetime: LIFETIME,
    clock: incrementingClock(),
    appendFile: async (_path, text) => { appended.push(text) },
    openFile: async () => ({ sync: async () => { synced += 1 }, close: async () => {} }),
  })
  for (let index = 0; index < 3; index += 1) flushed.emit('host_enqueue', HOST_ENQUEUE)
  const flushedStats = await flushed.close()
  assert.equal(flushedStats.written, 3)
  assert.equal(synced, 1)
  assert.equal(appended.join('').trim().split('\n').length, 3)
})

test('emit helpers contain observation failures', () => {
  assert.equal(emitFact(null, 'host_enqueue', HOST_ENQUEUE), false)
  assert.equal(emitFact({ emit() { throw new Error('sink') } }, 'host_enqueue', HOST_ENQUEUE), false)
  assert.equal(emitFactAt(null, 1, 'host_enqueue', HOST_ENQUEUE), false)
  assert.equal(emitFactAt({ emitAt() { throw new Error('sink') } }, 1, 'host_enqueue', HOST_ENQUEUE), false)
})

test('batch derivation is one opaque token over the exact interval', () => {
  const tokenizer = createTokenizer(CAPTURE_KEY)
  const outlet = { token: (domain, raw) => tokenizer.token(domain, raw) }
  const batch = batchToken(outlet, 'raw-stream', 2, 5)
  assert.equal(batch, tokenizer.batch('raw-stream:2:5'))
  assert.equal(batch.length, 32)
  assert.ok(!batch.includes('raw-stream'))
  assert.equal(batchToken(null, 'raw-stream', 2, 5), '')
  assert.equal(batchToken({}, 'raw-stream', 2, 5), '')
  assert.equal(batchToken(outlet, '', 2, 5), '')
  assert.equal(batchToken(outlet, 'raw-stream', -1, 5), '')
  assert.equal(batchToken(outlet, 'raw-stream', 2, Number.MAX_SAFE_INTEGER + 1), '')
  assert.equal(batchToken({ token() { throw new Error('sink') } }, 'raw-stream', 2, 5), '')
})

test('manifest loading rejects a malformed contract file', (t) => {
  const dir = tempDir(t)
  const path = join(dir, 'manifest.json')
  const cases = [
    [null, 'manifest_missing'],
    [{ schema: 2 }, 'manifest_schema'],
    [{ schema: 1 }, 'manifest_header'],
    [{ schema: 1, header: { required: [] } }, 'manifest_header'],
    [{ schema: 1, header: { required: [], v: '1' } }, 'manifest_header'],
    [{ schema: 1, header: { required: [], v: 1 }, producers: [] }, 'manifest_producers'],
    [{ schema: 1, header: { required: [], v: 1 }, producers: ['host'] }, 'manifest_events'],
  ]
  for (const [value, reason] of cases) {
    writeFileSync(path, JSON.stringify(value))
    assert.throws(() => loadManifest(path), error => error instanceof ObservationError && error.reason === reason)
  }
  assert.equal(existsSync(path), true)
})
test('every host and dsh boundary event passes the manifest with tokenized identities', async (t) => {
  const dir = tempDir(t)
  const path = join(dir, 'boundaries.ndjson')
  const outlet = createBenchOutlet({
    mode: 'trace', path, runId: RUN, key: CAPTURE_KEY, lifetime: LIFETIME, clock: incrementingClock(),
  })
  const dsh = outlet.forProducer('dsh')
  const batch = dsh.token('batch', 'raw-stream:0:2')
  const accepted = [
    dsh.emit('dsh_callback', { ...DSH_CALLBACK, batch }),
    dsh.emit('dsh_handoff', {
      stream: 'raw-stream', attempt: 'raw-attempt', block: 'raw-block',
      seq_start: 0, seq_end: 2, bytes: 42, batch,
    }),
    outlet.emit('host_coalesce', {
      stream: 'raw-stream', block: 'raw-block', seq_start: 0, seq_end: 2, frames: 2, bytes: 42, batch,
    }),
    outlet.emit('host_dequeue', {
      stream: 'raw-stream', block: 'raw-block', seq_start: 0, seq_end: 2, bytes: 42, wait_ns: 3, batch,
    }),
    outlet.emit('host_subscribe', { connection: 'raw-connection', generation: 2, outcome: 'subscribed' }),
    outlet.emit('host_command', { request: 'raw-request', attempt: 1, stage: 'start' }),
  ]
  assert.deepEqual(accepted, [true, true, true, true, true, true])
  await outlet.close()
  const records = readRecords(path)
  assert.equal(records.length, 6)
  assert.deepEqual(records.map(record => record.event), [
    'dsh_callback', 'dsh_handoff', 'host_coalesce', 'host_dequeue', 'host_subscribe', 'host_command',
  ])
  const text = readFileSync(path, 'utf8')
  for (const raw of ['raw-stream', 'raw-attempt', 'raw-block', 'raw-connection', 'raw-request']) {
    assert.equal(text.includes(raw), false)
  }
  assert.equal(records[4].data.generation, 2)
  assert.equal(records[2].data.batch, batch)
})

test('a live host_publish accepts the preview interval and queue context', async (t) => {
  const dir = tempDir(t)
  const path = join(dir, 'publish.ndjson')
  const outlet = createBenchOutlet({
    mode: 'trace', path, runId: RUN, key: CAPTURE_KEY, lifetime: LIFETIME, clock: incrementingClock(),
  })
  const batch = batchToken(outlet, 'raw-stream', 4, 6)
  assert.equal(outlet.emit('host_publish', {
    request: '', attempt: 1, kind: 'event', bytes: 512, outcome: 'accepted',
    stream: 'raw-stream', seq_start: 4, seq_end: 6, batch,
    queue_depth: 2, queue_bytes: 1024,
  }), true)
  await outlet.close()
  const [record] = readRecords(path)
  assert.equal(record.event, 'host_publish')
  assert.equal(record.data.batch, batch)
  assert.equal(record.data.outcome, 'accepted')
  assert.equal(record.data.queue_depth, 2)
  assert.ok(!readFileSync(path, 'utf8').includes('raw-stream'))
})

test('off mode does not read the observation contract', () => {
  const previous = process.env.WHALEREIN_BENCH_CONTRACT_PATH
  process.env.WHALEREIN_BENCH_CONTRACT_PATH = '/nonexistent/observation.json'
  try {
    const outlet = createBenchOutlet({ mode: 'off' })
    assert.equal(outlet.enabled, false)
  } finally {
    if (previous === undefined) delete process.env.WHALEREIN_BENCH_CONTRACT_PATH
    else process.env.WHALEREIN_BENCH_CONTRACT_PATH = previous
  }
})
