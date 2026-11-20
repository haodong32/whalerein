import test from 'node:test'
import assert from 'node:assert/strict'
import { AttachmentService } from '../src/attachments/service.js'
import { AttachmentTestSource } from './support/attachment-source.js'

// Branch-focused coverage for attachment declaration binding and terminal
// failure paths. Every case asserts the externally visible wire code and that
// no reservation survives the rejection.

function fixture({ adapterPolicy, policy = null, receiveEnvelope = 16 * 1024 * 1024 } = {}) {
  let now = 1000
  const adapter = new AttachmentTestSource({ now: () => now, sessions: [{ sessionId: 's1', running: false }], policy: adapterPolicy })
  const services = { getChannel: id => (id === 'channel-1' ? { deviceId: 'device-1', channelId: 'channel-1' } : null), getChannelReceive: () => ({ budget: { envelope_bytes: receiveEnvelope } }) }
  const service = new AttachmentService({
    hostId: 'host-1', adapter, security: services,
    store: { isRevoked: () => false },
    logger: { info() {}, warn() {}, error() {} },
    now: () => now, policy, envelopeBytes: 16 * 1024 * 1024, timers: false,
  })
  return { service, adapter, advance: ms => { now += ms } }
}

const args = overrides => ({
  request: { request_id: 'request-1', trace_id: 'trace-1', host_id: 'host-1' },
  deviceId: 'device-1', channelId: 'channel-1', sessionId: 's1', attachmentRef: 'a1',
  ...overrides,
})

async function openMutated(adapter, service, mutate, request = args().request) {
  const original = adapter.openAttachment.bind(adapter)
  adapter.openAttachment = async (...call) => {
    const opened = await original(...call)
    return mutate(opened) ?? opened
  }
  return service.open(args({ request }))
}

test('open rejects an attachment declaration that does not bind the requested object', async () => {
  const cases = [
    { mutate: opened => { opened.attachment.available = false }, code: 'not_found', name: 'unavailable object' },
    { mutate: opened => { opened.attachment.attachment_ref = 'other' }, code: 'forbidden', name: 'foreign ref' },
    { mutate: opened => { opened.attachment.session.host_id = 'host-2' }, code: 'forbidden', name: 'foreign host' },
    { mutate: opened => { opened.attachment.session.session_id = 's2' }, code: 'forbidden', name: 'foreign session' },
    { mutate: opened => { opened.attachment.size = -1 }, code: 'invalid_argument', name: 'negative size' },
    { mutate: opened => { opened.attachment.size = 1.5 }, code: 'invalid_argument', name: 'fractional size' },
    { mutate: opened => { opened.policy = { max_chunk_bytes: 0 } }, code: 'invalid_argument', name: 'invalid policy' },
  ]
  for (const entry of cases) {
    const { service, adapter } = fixture()
    adapter.attach({ ref: 'a1', bytes: Buffer.from('abc') })
    await assert.rejects(openMutated(adapter, service, entry.mutate), error => error.code === entry.code, entry.name)
    assert.equal(service.activeTransfers, 0, `${entry.name} leaves no transfer`)
    assert.equal(service.describe().reservedBytes, 0, `${entry.name} leaves no reservation`)
  }

  // A wrong authoritative session and an invalid per-request result bound fail
  // before a native open is attempted.
  const wrongSession = fixture()
  wrongSession.adapter.attach({ ref: 'a1', bytes: Buffer.from('abc') })
  wrongSession.adapter.getSession = async () => ({ sessionId: 'other' })
  await assert.rejects(wrongSession.service.open(args()), error => error.code === 'not_found')
  assert.equal(wrongSession.adapter.opens, 0)

  const invalidBound = fixture()
  invalidBound.adapter.attach({ ref: 'a1', bytes: Buffer.from('abc') })
  await assert.rejects(invalidBound.service.open(args({ request: { ...args().request, transport: { result_max_bytes: 'invalid' } } })), error => error.code === 'invalid_argument')
  await assert.rejects(invalidBound.service.open(args({ request: { ...args().request, transport: { result_max_bytes: 2500 } } })), error => error.code === 'payload_too_large')
})

test('open contains a throwing iterator close and unsupported adapters', async () => {
  const throwing = fixture()
  throwing.adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  // Force a pre-read failure after the reader was returned so the catch path
  // has to dispose an iterator it has not wrapped yet.
  const original = throwing.adapter.openAttachment.bind(throwing.adapter)
  throwing.adapter.openAttachment = async (...call) => {
    const opened = await original(...call)
    opened.attachment.size = Number.MAX_SAFE_INTEGER
    opened.reader.close = async () => { throw new Error('iterator close failed') }
    return opened
  }
  await assert.rejects(throwing.service.open(args()), error => error.code === 'payload_too_large')
  assert.equal(throwing.service.activeTransfers, 0)

  const unsupported = fixture()
  unsupported.adapter.attach({ ref: 'a1', bytes: Buffer.from('abc') })
  const withReader = unsupported.adapter.openAttachment.bind(unsupported.adapter)
  unsupported.adapter.openAttachment = async (...call) => {
    const opened = await withReader(...call)
    delete opened.reader
    return opened
  }
  unsupported.adapter.pullAttachment = undefined
  await assert.rejects(unsupported.service.open(args()), error => error.code === 'unsupported_capability')
  assert.equal(unsupported.service.activeTransfers, 0)
})

test('pull and close reject malformed bounds and preserve the retired lifecycle', async () => {
  const { service, adapter, advance } = fixture({ policy: {
    max_total_bytes: 64, max_chunk_bytes: 10, max_concurrent: 2,
    idle_timeout_ms: 100, overall_timeout_ms: 1000,
  } })
  adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  const opened = await service.open(args())
  const base = {
    request: args().request, deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: opened.transfer_id,
  }
  await assert.rejects(service.pull({ ...base, offset: -1, maxBytes: 3 }), error => error.code === 'invalid_argument')
  await assert.rejects(service.pull({ ...base, offset: 0, maxBytes: -1 }), error => error.code === 'invalid_argument')
  await assert.rejects(service.pull({ ...base, offset: 0, maxBytes: 0x1_0000_0000 }), error => error.code === 'invalid_argument')
  await assert.rejects(service.pull({ ...base, offset: 0, maxBytes: 3, transferId: 'unknown-transfer' }), error => error.code === 'not_found')
  await assert.rejects(service.close({ ...base, cancel: 'yes' }), error => error.code === 'invalid_argument')

  // An aborted pull retires the handle instead of leaving a live transfer.
  const aborted = new AbortController()
  aborted.abort('caller_gone')
  await assert.rejects(service.pull({ ...base, offset: 0, maxBytes: 3, signal: aborted.signal }), error => error.code === 'unavailable')
  assert.equal(service.activeTransfers, 0)
  assert.equal((await service.close(base)).closed, true, 'an aborted transfer still closes idempotently')
})

test('a stalled or malformed native read retires the transfer with a stable code', async () => {
  const stalled = fixture()
  stalled.adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  const originalRead = stalled.adapter.openAttachment.bind(stalled.adapter)
  stalled.adapter.openAttachment = async (...call) => {
    const opened = await originalRead(...call)
    opened.reader.read = async () => ({ bytes: new Uint8Array(0), eof: false })
    return opened
  }
  const stalledTransfer = await stalled.service.open(args())
  await assert.rejects(stalled.service.pull({
    request: args().request, deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: stalledTransfer.transfer_id, offset: 0, maxBytes: 4,
  }), error => error.code === 'invalid_argument')
  assert.equal(stalled.service.activeTransfers, 0)

  const malformed = fixture()
  malformed.adapter.attach({ ref: 'a1', bytes: Buffer.from('abcdefghij') })
  const read = malformed.adapter.openAttachment.bind(malformed.adapter)
  malformed.adapter.openAttachment = async (...call) => {
    const opened = await read(...call)
    opened.reader.read = async () => null
    return opened
  }
  const malformedTransfer = await malformed.service.open(args())
  await assert.rejects(malformed.service.pull({
    request: args().request, deviceId: 'device-1', channelId: 'channel-1',
    sessionId: 's1', attachmentRef: 'a1', transferId: malformedTransfer.transfer_id, offset: 0, maxBytes: 4,
  }), error => error.code === 'invalid_argument')
  assert.equal(malformed.service.activeTransfers, 0)
})
