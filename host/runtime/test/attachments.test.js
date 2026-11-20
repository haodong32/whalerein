import test from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { AttachmentService } from '../src/attachments/service.js'
import {
  effectiveChunkBytes,
  safeSuggestion,
  defaultAttachmentPolicy,
} from '../src/attachments/reader.js'
import { AttachmentTestSource } from './support/attachment-source.js'
import { CommandService } from '../src/commands/service.js'
import { StateStore } from '../src/state/store.js'
import { TestSource } from './support/source.js'
import { codec } from '../src/protocol.js'
import { sha256Bytes } from '../src/security/digest.js'
import { startHarness } from './support/harness.js'

const proto = codec()
const sha256 = (bytes) => createHash('sha256').update(bytes).digest()

function makeService({
  adapterPolicy = {
    max_total_bytes: 64,
    max_chunk_bytes: 3,
    max_concurrent: 2,
    idle_timeout_ms: 30_000,
    overall_timeout_ms: 120_000,
  },
  policy = null,
  sessions = [{ sessionId: 's1', running: false }, { sessionId: 's2', running: false }],
  channels = true,
  receiveEnvelope = 16 * 1024 * 1024,
} = {}) {
  let now = 1000
  const adapter = new AttachmentTestSource({ now: () => now, sessions, policy: adapterPolicy })
  const channelMap = new Map([
    ['channel-1', { deviceId: 'device-1', channelId: 'channel-1' }],
    ['channel-2', { deviceId: 'device-2', channelId: 'channel-2' }],
  ])
  const revoked = new Set()
  const logs = []
  const record = (level) => (event, fields = {}) => logs.push({ level, event, ...fields })
  const security = {
    getChannel: (id) => (channels ? channelMap.get(id) ?? null : null),
    getChannelReceive: () => ({ budget: { envelope_bytes: receiveEnvelope } }),
  }
  const store = { isRevoked: (deviceId) => revoked.has(deviceId) }
  const service = new AttachmentService({
    hostId: 'host-1',
    adapter,
    security,
    store,
    logger: { info: record('info'), warn: record('warn'), error: record('error') },
    now: () => now,
    policy,
    envelopeBytes: 16 * 1024 * 1024,
    timers: false,
  })
  return {
    service, adapter, logs, channelMap, revoked,
    advance: (ms) => { now += ms },
    now: () => now,
  }
}

function openRequest(overrides = {}) {
  return {
    request_id: 'request-1',
    trace_id: 'trace-1',
    host_id: 'host-1',
    ...overrides,
  }
}

function openArgs(overrides = {}) {
  return {
    request: openRequest(),
    deviceId: 'device-1',
    channelId: 'channel-1',
    sessionId: 's1',
    attachmentRef: 'a1',
    ...overrides,
  }
}

test('open resolves authoritative metadata and pull validates terminal integrity', async () => {
  const { service, adapter } = makeService()
  const bytes = Buffer.from('abcdefghij')
  adapter.attach({ ref: 'a1', bytes, name: '../secret/note.bin' })
  const opened = await service.open(openArgs())
  assert.equal(opened.session.host_id, 'host-1')
  assert.equal(opened.session.session_id, 's1')
  assert.equal(opened.attachment_ref, 'a1')
  assert.equal(opened.size, bytes.length)
  assert.equal(opened.name, 'note.bin')
  assert.equal(opened.policy.max_chunk_bytes, 3)
  assert.equal(opened.policy.max_total_bytes, 64)
  assert.equal(opened.expires_at_unix_ms, 1000 + 120_000)
  assert.match(opened.transfer_id, /^[a-f0-9]{32}$/)
  assert.equal(adapter.nativeReads, 0)

  const first = await service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 0, maxBytes: 3,
  })
  assert.equal(Buffer.from(first.data).toString(), 'abc')
  assert.equal(first.final, false)
  assert.equal(first.length, 3)

  const retry = await service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 0, maxBytes: 3,
  })
  assert.deepEqual(Buffer.from(retry.data), Buffer.from(first.data))
  assert.equal(adapter.nativeReads, 1)
  await assert.rejects(service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 0, maxBytes: 3,
  }), (error) => error.code === 'conflict')

  const second = await service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 3, maxBytes: 3,
  })
  assert.equal(Buffer.from(second.data).toString(), 'def')
  const third = await service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 6, maxBytes: 3,
  })
  assert.equal(Buffer.from(third.data).toString(), 'ghi')
  const final = await service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 9, maxBytes: 3,
  })
  assert.equal(final.final, true)
  assert.equal(final.total_size, 10)
  assert.deepEqual(Buffer.from(final.sha256), sha256(bytes))
  assert.equal(adapter.nativeReads, 4)
  assert.equal(adapter.closeCalls.at(-1).cancel, false)

  const finalRetry = await service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 9, maxBytes: 3,
  })
  assert.deepEqual(Buffer.from(finalRetry.sha256), Buffer.from(final.sha256))
  assert.equal(adapter.nativeReads, 4)
  const closed = await service.close({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id,
  })
  assert.deepEqual(closed, { closed: true, cancelled: false })
  assert.equal(service.activeTransfers, 0)
})

test('pull rejects gaps, overlaps, over-budget requests and closed handles without native reads', async () => {
  const { service, adapter } = makeService()
  adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  const opened = await service.open(openArgs())
  const pull = (fields) => service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, ...fields,
  })
  await assert.rejects(pull({ offset: 3, maxBytes: 3 }), (error) => error.code === 'conflict')
  assert.equal(adapter.nativeReads, 0)
  const clamped = await pull({ offset: 0, maxBytes: 4 })
  assert.equal(clamped.length, 3)
  await assert.rejects(pull({ offset: 1, maxBytes: 3 }), (error) => error.code === 'conflict')
  const secondPart = await pull({ offset: 3, maxBytes: 3 })
  const retried = await pull({ offset: 3, maxBytes: 3 })
  assert.deepEqual(Buffer.from(retried.data), Buffer.from(secondPart.data))
  await assert.rejects(pull({ offset: 3, maxBytes: 3 }), (error) => error.code === 'conflict')
  await service.close({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id,
  })
  await assert.rejects(pull({ offset: 6, maxBytes: 3 }), (error) => error.code === 'not_found')
})

test('missing membership and absent or corrupt native objects fail closed before any read', async () => {
  const { service, adapter } = makeService()
  adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  await assert.rejects(service.open(openArgs({ attachmentRef: 'missing' })), (error) => error.code === 'not_found')
  assert.equal(adapter.nativeReads, 0)
  adapter.attach({ ref: 'gone', bytes: Buffer.from('x'), missing: true })
  await assert.rejects(service.open(openArgs({ attachmentRef: 'gone' })), (error) => error.code === 'not_found')
  adapter.openFailure = 'payload_digest_mismatch'
  await assert.rejects(service.open(openArgs()), (error) => error.code === 'payload_digest_mismatch')
  assert.equal(adapter.nativeReads, 0)
  assert.equal(service.activeTransfers, 0)
})

test('open rejects oversize declarations before any native byte read', async () => {
  const { service, adapter } = makeService({ adapterPolicy: {
    max_total_bytes: 5, max_chunk_bytes: 3, max_concurrent: 2, idle_timeout_ms: 30_000, overall_timeout_ms: 120_000,
  } })
  adapter.attach({ ref: 'a1', bytes: Buffer.alloc(10) })
  await assert.rejects(service.open(openArgs()), (error) => error.code === 'payload_too_large')
  assert.equal(adapter.nativeReads, 0)
  assert.equal(adapter.closeCalls.length, 1)
  assert.equal(adapter.closeCalls[0].cancel, true)
  assert.equal(service.activeTransfers, 0)
  assert.equal(service.describe().reservedBytes, 0)
})

test('a native part larger than the effective chunk is rejected and disposed', async () => {
  const { service, adapter } = makeService()
  adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  adapter.oversize = 1
  const opened = await service.open(openArgs())
  await assert.rejects(service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 0, maxBytes: 3,
  }), (error) => error.code === 'payload_too_large')
  assert.equal(service.activeTransfers, 0)
  assert.equal(adapter.closeCalls.at(-1).cancel, true)
  assert.equal(service.describe().reservedBytes, 0)
})

test('a short native stream and a wrong source digest fail terminal verification', async () => {
  const short = makeService()
  short.adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  short.adapter.truncateTo = 4
  const shortOpen = await short.service.open(openArgs())
  await short.service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: shortOpen.transfer_id, offset: 0, maxBytes: 3,
  })
  await assert.rejects(short.service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: shortOpen.transfer_id, offset: 3, maxBytes: 3,
  }), (error) => error.code === 'payload_digest_mismatch')

  const corrupt = makeService({ adapterPolicy: {
    max_total_bytes: 64, max_chunk_bytes: 10, max_concurrent: 2, idle_timeout_ms: 30_000, overall_timeout_ms: 120_000,
  } })
  corrupt.adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij'), sha256: Buffer.alloc(32, 7) })
  const corruptOpen = await corrupt.service.open(openArgs())
  await assert.rejects(corrupt.service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: corruptOpen.transfer_id, offset: 0, maxBytes: 10,
  }), (error) => error.code === 'payload_digest_mismatch')
})

test('an empty attachment completes as a verified zero-byte terminal part', async () => {
  const { service, adapter } = makeService()
  adapter.attach({ ref: 'a1', bytes: Buffer.alloc(0) })
  const opened = await service.open(openArgs())
  const part = await service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 0, maxBytes: 3,
  })
  assert.equal(part.final, true)
  assert.equal(part.length, 0)
  assert.equal(part.total_size, 0)
  assert.deepEqual(Buffer.from(part.sha256), sha256(Buffer.alloc(0)))
})

test('foreign device, channel, host, session, occurrence and handle fail closed', async () => {
  const { service, adapter, channelMap } = makeService()
  adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  const opened = await service.open(openArgs())
  const base = {
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 0, maxBytes: 3,
  }
  await assert.rejects(service.pull({ ...base, deviceId: 'device-2' }), (error) => error.code === 'forbidden')
  await assert.rejects(service.pull({ ...base, channelId: 'channel-2' }), (error) => error.code === 'forbidden')
  await assert.rejects(service.pull({ ...base, request: openRequest({ host_id: 'host-2' }) }), (error) => error.code === 'forbidden')
  await assert.rejects(service.pull({ ...base, sessionId: 's2' }), (error) => error.code === 'forbidden')
  await assert.rejects(service.pull({ ...base, attachmentRef: 'a2' }), (error) => error.code === 'forbidden')
  await assert.rejects(service.pull({ ...base, transferId: 'f'.repeat(32) }), (error) => error.code === 'not_found')
  assert.equal(adapter.nativeReads, 0)
  await assert.rejects(service.close({ ...base, transferId: 'f'.repeat(32) }), (error) => error.code === 'not_found')
  channelMap.delete('channel-1')
  await assert.rejects(service.pull(base), (error) => error.code === 'unauthorized')
  assert.equal(adapter.nativeReads, 0)
})

test('close is scoped and idempotent; cancel disposes the iterator', async () => {
  const { service, adapter } = makeService()
  adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  const opened = await service.open(openArgs())
  const closeArgs = {
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id,
  }
  const cancelled = await service.close({ ...closeArgs, cancel: true })
  assert.deepEqual(cancelled, { closed: true, cancelled: true })
  assert.equal(adapter.closeCalls.at(-1).cancel, true)
  assert.equal(service.activeTransfers, 0)
  assert.deepEqual(await service.close(closeArgs), { closed: true, cancelled: true })
  await assert.rejects(service.pull({ ...closeArgs, offset: 0, maxBytes: 3 }), (error) => error.code === 'not_found')
  await assert.rejects(service.close({ ...closeArgs, deviceId: 'device-2' }), (error) => error.code === 'forbidden')
})

test('idle and overall expiry release reservations and require a fresh transfer', async () => {
  const idle = makeService({ atomPolicy: undefined, policy: {
    max_total_bytes: 64, max_chunk_bytes: 3, max_concurrent: 2, idle_timeout_ms: 100, overall_timeout_ms: 1000,
  } })
  idle.adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  const opened = await idle.service.open(openArgs())
  assert.equal(idle.service.describe().reservedBytes, 10)
  idle.advance(150)
  assert.equal(idle.service.sweep(), 1)
  assert.equal(idle.service.activeTransfers, 0)
  assert.equal(idle.service.describe().reservedBytes, 0)
  await assert.rejects(idle.service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 0, maxBytes: 3,
  }), (error) => error.code === 'expired')

  const deadline = makeService({ policy: {
    max_total_bytes: 64, max_chunk_bytes: 3, max_concurrent: 2, idle_timeout_ms: 1000, overall_timeout_ms: 200,
  } })
  deadline.adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  const second = await deadline.service.open(openArgs())
  deadline.advance(250)
  await assert.rejects(deadline.service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: second.transfer_id, offset: 0, maxBytes: 3,
  }), (error) => error.code === 'expired')
  assert.equal(deadline.service.describe().reservedBytes, 0)

  const fresh = await deadline.service.open(openArgs())
  assert.equal(fresh.attachment_ref, 'a1')
})

test('host stop disposes every open iterator and releases its reservation', async () => {
  const { service, adapter } = makeService()
  adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  const opened = await service.open(openArgs())
  assert.equal(service.activeTransfers, 1)
  assert.equal(service.describe().reservedBytes, 10)
  assert.equal(service.disposeAll('host_stop'), 1)
  assert.equal(service.activeTransfers, 0)
  assert.equal(service.describe().reservedBytes, 0)
  assert.equal(adapter.closeCalls.at(-1).cancel, true)
  await assert.rejects(service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 0, maxBytes: 3,
  }), (error) => error.code === 'unavailable')
})

test('idle timers dispose a transfer without another pull or sweep', async () => {
  const adapter = new AttachmentTestSource({ sessions: [{ sessionId: 's1', running: false }] })
  adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  adapter.policy = {
    max_total_bytes: 64, max_chunk_bytes: 3, max_concurrent: 2,
    idle_timeout_ms: 10, overall_timeout_ms: 5000,
  }
  const service = new AttachmentService({
    hostId: 'host-1', adapter, now: Date.now, timers: true, envelopeBytes: 16 * 1024 * 1024,
  })
  const opened = await service.open({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1',
  })
  await new Promise(resolve => setTimeout(resolve, 40))
  assert.equal(service.activeTransfers, 0)
  await assert.rejects(service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 0, maxBytes: 3,
  }), (error) => error.code === 'expired')
  assert.equal(adapter.closeCalls.at(-1).cancel, true)
})

test('per-device concurrency and declared-byte reservations fail explicitly and release', async () => {
  const concurrency = makeService({ policy: {
    max_total_bytes: 64, max_chunk_bytes: 3, max_concurrent: 1, idle_timeout_ms: 1000, overall_timeout_ms: 5000,
  } })
  concurrency.adapter.attach({ ref: 'a1', bytes: Buffer.from('abc') })
  concurrency.adapter.attach({ ref: 'a2', bytes: Buffer.from('def') })
  const first = await concurrency.service.open(openArgs())
  await assert.rejects(concurrency.service.open(openArgs({ attachmentRef: 'a2' })), (error) => error.code === 'rate_limited')
  assert.equal(concurrency.adapter.closeCalls.at(-1).cancel, true)
  await concurrency.service.close({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: first.transfer_id,
  })
  assert.equal((await concurrency.service.open(openArgs({ attachmentRef: 'a2' }))).size, 3)

  const reservation = makeService({ policy: {
    max_total_bytes: 10, max_chunk_bytes: 4, max_concurrent: 4, idle_timeout_ms: 1000, overall_timeout_ms: 5000,
  } })
  reservation.adapter.policy.max_total_bytes = 10
  reservation.adapter.attach({ ref: 'a1', bytes: Buffer.alloc(8) })
  reservation.adapter.attach({ ref: 'a2', bytes: Buffer.alloc(8) })
  const held = await reservation.service.open(openArgs())
  assert.equal(reservation.service.describe().reservedBytes, 8)
  await assert.rejects(reservation.service.open(openArgs({ attachmentRef: 'a2' })), (error) => error.code === 'rate_limited')
  await reservation.service.close({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: held.transfer_id,
  })
  assert.equal(reservation.service.describe().reservedBytes, 0)
  assert.equal((await reservation.service.open(openArgs({ attachmentRef: 'a2' }))).size, 8)
})

test('a shared blob stays independently usable from two authorized sessions', async () => {
  const { service, adapter } = makeService({ adapterPolicy: {
    max_total_bytes: 64, max_chunk_bytes: 32, max_concurrent: 2, idle_timeout_ms: 30_000, overall_timeout_ms: 120_000,
  } })
  const bytes = Buffer.from('shared-bytes')
  adapter.attach({ sessionId: 's1', ref: 'a1', bytes })
  adapter.attach({ sessionId: 's2', ref: 'a2', bytes })
  const one = await service.open(openArgs())
  const two = await service.open(openArgs({ sessionId: 's2', attachmentRef: 'a2' }))
  assert.notEqual(one.transfer_id, two.transfer_id)
  const partOne = await service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: one.transfer_id, offset: 0, maxBytes: 32,
  })
  const partTwo = await service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's2', attachmentRef: 'a2', transferId: two.transfer_id, offset: 0, maxBytes: 32,
  })
  assert.deepEqual(Buffer.from(partOne.data), bytes)
  assert.deepEqual(Buffer.from(partTwo.data), bytes)
  assert.equal(adapter.nativeReads, 2)
})

test('channel loss and device revocation abort an in-flight read and release resources', async () => {
  async function midRead(cancel) {
    const context = makeService()
    context.adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
    const opened = await context.service.open(openArgs())
    let started
    const startedPromise = new Promise((resolve) => { started = resolve })
    context.adapter.readHook = ({ signal }) => new Promise((resolve, reject) => {
      started()
      if (signal.aborted) { reject(new Error('aborted')); return }
      signal.addEventListener('abort', () => reject(new Error('aborted')), { once: true })
    })
    const pull = context.service.pull({
      request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
      sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 0, maxBytes: 3,
    })
    await startedPromise
    cancel(context)
    await assert.rejects(pull, (error) => ['unavailable', 'forbidden'].includes(error.code))
    assert.equal(context.service.activeTransfers, 0)
    assert.equal(context.service.describe().reservedBytes, 0)
    assert.equal(context.adapter.closeCalls.at(-1).cancel, true)
  }
  await midRead(({ service }) => service.cancelChannel('device-1', 'channel-1', 'channel_closed'))
  await midRead(({ service }) => service.revokeDevice('device-1'))
})

test('diagnostics use only safe correlation, count, limit and stage fields', async () => {
  const { service, adapter, logs } = makeService()
  const name = 'private-transcript-name.txt'
  const payload = 'TOP-SECRET-ATTACHMENT-BYTES'
  adapter.attach({ ref: 'a1', bytes: Buffer.from(payload), name, mediaType: 'application/octet-stream' })
  const opened = await service.open(openArgs())
  await service.pull({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id, offset: 0, maxBytes: 3,
  })
  await service.close({
    request: openRequest(), deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id,
  })
  assert.ok(logs.length > 0)
  const serialized = JSON.stringify(logs)
  assert.equal(serialized.includes(payload), false)
  assert.equal(serialized.includes(name), false)
  const allowed = new Set([
    'level', 'event', 'stage', 'code', 'bytes', 'limit', 'reason', 'final', 'offset',
    'request', 'trace', 'device', 'channel', 'transfer', 'count',
  ])
  for (const record of logs) {
    for (const key of Object.keys(record)) assert.ok(allowed.has(key), `unexpected log field ${key}`)
  }
})

test('request routing exposes open, pull and close typed result payloads', async () => {
  const { adapter } = makeService()
  adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  const store = new StateStore({ path: ':memory:', now: () => 1000 })
  const service = new CommandService({
    store,
    adapter,
    events: { readHistory: async () => ({}) },
    hostId: 'host-1',
    now: () => 1000,
    codec: proto,
    attachments: undefined,
    security: null,
  })
  const request = (overrides = {}) => ({
    request_id: `request-${Math.random().toString(16).slice(2)}`,
    host_id: 'host-1',
    device_id: 'device-1',
    channel_id: 'channel-1',
    kind: 'REQUEST_KIND_QUERY',
    expires_at_unix_ms: 100_000,
    trace_id: 'trace-1',
    protocol_version: 1,
    ...overrides,
  })
  const session = { host_id: 'host-1', session_id: 's1' }
  const opened = await service.handle({
    request: request(),
    payload: { open_attachment: { session, attachment_ref: 'a1' } },
  })
  assert.equal(opened.status, 'succeeded')
  const metadata = opened.response.open_attachment_result.attachment
  assert.equal(metadata.size, 10)

  // A device-declared HTTPS result bound lowers the effective chunk below the
  // source policy and keeps the complete encoded part inside that bound.
  const bounded = await service.handle({
    request: request({ transport: { result_max_bytes: 8192, grant_generation: 1, revocation_generation: 1 } }),
    payload: { open_attachment: { session, attachment_ref: 'a1' } },
  })
  assert.equal(bounded.status, 'succeeded')
  const boundedMetadata = bounded.response.open_attachment_result.attachment
  assert.ok(boundedMetadata.policy.max_chunk_bytes > 0 && boundedMetadata.policy.max_chunk_bytes < 8192)
  const boundedPull = await service.handle({
    request: request({ transport: { result_max_bytes: 8192, grant_generation: 1, revocation_generation: 1 } }),
    payload: {
      pull_attachment: {
        session, attachment_ref: 'a1', transfer_id: boundedMetadata.transfer_id,
        offset: 0, max_bytes: boundedMetadata.policy.max_chunk_bytes,
      },
    },
  })
  assert.equal(boundedPull.status, 'succeeded')
  assert.ok(proto.encode('whalerein.v1.ResponsePayload', boundedPull.response).length <= 8192)
  await service.handle({
    request: request(),
    payload: {
      close_attachment: {
        session, attachment_ref: 'a1', transfer_id: boundedMetadata.transfer_id, cancel: true,
      },
    },
  })

  const pulled = await service.handle({
    request: request(),
    payload: {
      pull_attachment: {
        session, attachment_ref: 'a1', transfer_id: metadata.transfer_id, offset: 0, max_bytes: 3,
      },
    },
  })
  assert.equal(pulled.status, 'succeeded')
  const part = pulled.response.attachment_part
  assert.equal(Buffer.from(part.data).toString(), 'abc')
  const encoded = proto.encode('whalerein.v1.ResponsePayload', pulled.response)
  assert.ok(encoded.length < 16 * 1024 * 1024)

  const closed = await service.handle({
    request: request(),
    payload: {
      close_attachment: {
        session, attachment_ref: 'a1', transfer_id: metadata.transfer_id, cancel: true,
      },
    },
  })
  assert.equal(closed.status, 'succeeded')
  assert.deepEqual(closed.response.close_attachment_result, { closed: true, cancelled: true })

  const foreign = await service.handle({
    request: request(),
    payload: { open_attachment: { session: { host_id: 'host-2', session_id: 's1' }, attachment_ref: 'a1' } },
  })
  assert.equal(foreign.status, 'failed')
  assert.equal(foreign.errorCode, 'forbidden')
})

test('an adapter without attachment support reports unsupported and reads no bytes', async () => {
  const store = new StateStore({ path: ':memory:', now: () => 1000 })
  const adapter = new TestSource({ sessions: [{ sessionId: 's1', running: false }] })
  let reads = 0
  adapter.openAttachment = async () => { reads++; throw Object.assign(new Error('unsupported'), { code: 'unsupported_capability' }) }
  const service = new CommandService({ store, adapter, events: {}, hostId: 'host-1', now: () => 1000, codec: proto })
  const result = await service.handle({
    request: {
      request_id: 'request-1', host_id: 'host-1', device_id: 'device-1', channel_id: 'channel-1',
      kind: 'REQUEST_KIND_QUERY', expires_at_unix_ms: 100_000, protocol_version: 1,
    },
    payload: { open_attachment: { session: { host_id: 'host-1', session_id: 's1' }, attachment_ref: 'a1' } },
  })
  assert.equal(result.status, 'failed')
  assert.equal(result.errorCode, 'unsupported_capability')
  assert.equal(reads, 1)
})

test('a per-pull result bound lowers that part without changing the transfer', async () => {
  const { service, adapter } = makeService({
    adapterPolicy: {
      max_total_bytes: 1 << 20, max_chunk_bytes: 60 * 1024, max_concurrent: 2,
      idle_timeout_ms: 30_000, overall_timeout_ms: 120_000,
    },
  })
  adapter.attach({ ref: 'a1', bytes: Buffer.alloc(5000, 1) })
  const opened = await service.open(openArgs())
  const part = await service.pull({
    request: openRequest({ transport: { result_max_bytes: 8192, grant_generation: 1, revocation_generation: 1 } }),
    deviceId: 'device-1', channelId: 'channel-1', sessionId: 's1', attachmentRef: 'a1',
    transferId: opened.transfer_id, offset: 0,
  })
  assert.ok(part.length > 0 && part.length < 8192)
  assert.equal(part.final, false)
  const next = await service.pull({
    request: openRequest(),
    deviceId: 'device-1', channelId: 'channel-1', sessionId: 's1', attachmentRef: 'a1',
    transferId: opened.transfer_id, offset: part.length,
  })
  assert.equal(next.length, 5000 - part.length)
})

test('a lower device receive envelope caps the effective chunk', async () => {
  const { service, adapter } = makeService({
    adapterPolicy: {
      max_total_bytes: 1 << 20, max_chunk_bytes: 60 * 1024, max_concurrent: 2,
      idle_timeout_ms: 30_000, overall_timeout_ms: 120_000,
    },
    receiveEnvelope: 4096,
  })
  adapter.attach({ ref: 'a1', bytes: Buffer.alloc(9000) })
  const opened = await service.open(openArgs())
  assert.equal(opened.policy.max_chunk_bytes, 4096 - 2048 - 1024)
})

test('chunk allowance derives from encoded result and envelope geometry', () => {
  assert.equal(effectiveChunkBytes({ maxChunkBytes: 60 * 1024, resultMaxBytes: 16 * 1024 * 1024, envelopeBytes: 16 * 1024 * 1024 }), 60 * 1024)
  const small = effectiveChunkBytes({ maxChunkBytes: 60 * 1024, resultMaxBytes: 8192 })
  assert.ok(small > 0 && small < 8192)
  assert.equal(effectiveChunkBytes({ maxChunkBytes: 60 * 1024, resultMaxBytes: 2048 }), 0)
  assert.equal(effectiveChunkBytes({ maxChunkBytes: 60 * 1024, envelopeBytes: 4096 }), 4096 - 2048 - 1024)
  assert.equal(defaultAttachmentPolicy({ fragmentPayloadBytes: 100, bulkConcurrency: 2, fragmentIdleTimeoutMs: 30, transferTimeoutMs: 60 }).max_chunk_bytes, 100)
  assert.equal(safeSuggestion('../../etc/passwd'), 'passwd')
  assert.equal(safeSuggestion('a\u0000b.txt'), 'ab.txt')
})


test('the Host request router carries attachment queries over the encrypted path', async () => {
  const adapter = new AttachmentTestSource({ sessions: [{ sessionId: 's1', running: false }] })
  adapter.policy = {
    max_total_bytes: 1024, max_chunk_bytes: 4, max_concurrent: 2,
    idle_timeout_ms: 30_000, overall_timeout_ms: 120_000,
  }
  adapter.attach({ ref: 'a1', bytes: Buffer.from('router-bytes'), name: 'report.txt' })
  const harness = await startHarness({ adapter })
  try {
    const device = await harness.newPairedDevice('device-1')
    const session = { host_id: 'host-1', session_id: 's1' }
    const opened = await device.query({ open_attachment: { session, attachment_ref: 'a1' } })
    assert.equal(opened.status, 'succeeded')
    const metadata = opened.response.open_attachment_result.attachment
    assert.equal(metadata.size, 12)
    assert.equal(metadata.name, 'report.txt')
    assert.equal(metadata.policy.max_chunk_bytes, 4)

    const first = await device.query({
      pull_attachment: {
        session, attachment_ref: 'a1', transfer_id: metadata.transfer_id, offset: 0, max_bytes: 4,
      },
    })
    assert.equal(first.status, 'succeeded')
    assert.equal(Buffer.from(first.response.attachment_part.data).toString(), 'rout')
    assert.ok(!first.response.attachment_part.final)

    const closed = await device.query({
      close_attachment: {
        session, attachment_ref: 'a1', transfer_id: metadata.transfer_id, cancel: true,
      },
    })
    assert.equal(closed.status, 'succeeded')
    assert.deepEqual(closed.response.close_attachment_result, { closed: true, cancelled: true })

    const after = await device.query({
      pull_attachment: {
        session, attachment_ref: 'a1', transfer_id: metadata.transfer_id, offset: 4, max_bytes: 4,
      },
    })
    assert.equal(after.status, 'failed')
    assert.equal(after.errorCode, 'not_found')
  } finally {
    await harness.stop()
  }
})

test('bulk buffering stays within the derived chunk and reservation budget for two file sizes', async () => {
  // AC-14 measurement shape: configured chunk/concurrency derive the retained
  // buffer bound. The same settings are used for a small and a multi-envelope
  // file so size alone cannot raise the peak.
  const chunk = 1024 * 1024
  const concurrency = 2
  const totalBudget = 64 * 1024 * 1024
  const { service, adapter } = makeService({
    adapterPolicy: {
      max_total_bytes: totalBudget,
      max_chunk_bytes: chunk,
      max_concurrent: concurrency,
      idle_timeout_ms: 30_000,
      overall_timeout_ms: 120_000,
    },
  })
  const sizes = [3 * chunk, 20 * chunk]
  const refs = sizes.map((size, index) => {
    const ref = `bulk-${index}`
    adapter.attach({ ref, bytes: Buffer.alloc(size, index + 1) })
    return ref
  })
  const opened = []
  for (const ref of refs) opened.push(await service.open(openArgs({ attachmentRef: ref })))
  const offsets = [0, 0]
  const done = [false, false]
  let peakBuffered = 0
  let peakReserved = 0
  let maxPart = 0
  while (!done.every(Boolean)) {
    for (let index = 0; index < refs.length; index += 1) {
      if (done[index]) continue
      const part = await service.pull({
        request: openRequest(),
        deviceId: 'device-1',
        channelId: 'channel-1',
        sessionId: 's1',
        attachmentRef: refs[index],
        transferId: opened[index].transfer_id,
        offset: offsets[index],
        maxBytes: chunk,
      })
      maxPart = Math.max(maxPart, part.length)
      offsets[index] += part.length
      done[index] = part.final
      const summary = service.describe()
      peakBuffered = Math.max(peakBuffered, summary.bufferedBytes)
      peakReserved = Math.max(peakReserved, summary.reservedBytes)
    }
  }
  assert.ok(maxPart > 0 && maxPart <= chunk, 'a part must not exceed the configured chunk')
  const effectiveChunk = maxPart
  assert.ok(
    peakBuffered <= concurrency * effectiveChunk,
    `peak buffered ${peakBuffered} exceeds ${concurrency * effectiveChunk}`,
  )
  assert.ok(
    peakReserved <= totalBudget,
    `peak reserved ${peakReserved} exceeds ${totalBudget}`,
  )
  // Encoded overhead cannot raise the retained raw buffer beyond the budget.
  const encodedBound = Math.ceil((effectiveChunk * 4) / 3) + 4096
  assert.ok(encodedBound > effectiveChunk)
  assert.equal(service.describe().active, 0)
})

test('pull combines signals through the compatibility fallback when AbortSignal.any is absent', async () => {
  const { service, adapter } = makeService()
  adapter.attach({ ref: 'a1', bytes: Buffer.from('abc') })
  const opened = await service.open(openArgs())
  const original = AbortSignal.any
  AbortSignal.any = undefined
  try {
    const part = await service.pull({
      request: openRequest(),
      deviceId: 'device-1',
      channelId: 'channel-1',
      sessionId: 's1',
      attachmentRef: 'a1',
      transferId: opened.transfer_id,
      offset: 0,
      maxBytes: 3,
      signal: new AbortController().signal,
    })
    assert.equal(Buffer.from(part.data).toString(), 'abc')
  } finally {
    AbortSignal.any = original
  }
})
