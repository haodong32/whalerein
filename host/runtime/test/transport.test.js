import test from 'node:test'
import assert from 'node:assert/strict'
import { randomBytes } from 'node:crypto'
import {
  RelayTransport,
  assertEventRepresentation,
  assertRelayEpoch,
  assertRequestRepresentation,
  assertResultRepresentation,
} from '../src/access/transport.js'
import {
  effectivePacketBytes,
  maxPlaintextBytes,
  publishPacketBytes,
  varintBytes,
} from '../src/access/packet.js'
import { RELAY_VERSION } from '../src/contracts/transport.js'
import { codec } from '../src/protocol.js'

const proto = codec()
const HOST = 'host-1'
const DEVICE = 'device-1'
const CHANNEL = 'channel-1'
const RESULT_TOPIC = `whalerein/v1/h/${HOST}/result`
const EVENT_TOPIC = `whalerein/v1/h/${HOST}/event`

const budget = (packet = 64 * 1024, envelope = 16 * 1024 * 1024) => ({
  packet_bytes: packet,
  envelope_bytes: envelope,
  bulk_concurrency: 4,
  reserved_bytes: Math.max(envelope, 32 * 1024 * 1024),
  transfer_timeout_ms: 120_000,
})

function hostResult({ body = new Uint8Array([1, 2, 3, 4]), status = 'succeeded', reference } = {}) {
  return proto.encode('whalerein.v1.HostResult', {
    request_id: 'request-1',
    host_id: HOST,
    device_id: DEVICE,
    status,
    result_body: reference ? new Uint8Array(0) : body,
    ...(reference ? { result_reference: reference } : {}),
    channel_id: CHANNEL,
    protocol_version: 1,
    relay_version: RELAY_VERSION,
  })
}

function eventEnvelope({ ciphertext = new Uint8Array([1, 2, 3]), reference } = {}) {
  return proto.encode('whalerein.v1.EventEnvelope', {
    host_id: HOST,
    device_id: DEVICE,
    channel_id: CHANNEL,
    nonce: randomBytes(12),
    ...(reference ? { payload_reference: reference } : { ciphertext }),
    created_at_unix_ms: 1,
    relay_version: RELAY_VERSION,
  })
}

class FakeRelay {
  constructor() {
    this.objects = new Map()
    this.calls = []
    this.events = []
    this.fail = null
  }

  async reserve({ ciphertext, expiresAt, purpose }) {
    this.calls.push('reserve')
    if (this.fail === 'reserve') throw Object.assign(new Error('quota'), { code: 'rate_limited' })
    const objectId = randomBytes(16).toString('hex')
    this.objects.set(objectId, { ciphertext, purpose })
    const reservation = {
      object_id: objectId,
      owner_node_id: 'node-1',
      upload_path: `/v1/relay/objects/${objectId}`,
      max_bytes: ciphertext.length,
      expires_at_unix_ms: expiresAt,
      upload_timeout_ms: 1000,
    }
    this.events.push('reserve')
    return reservation
  }

  async upload({ reservation, ciphertext }) {
    this.calls.push('upload')
    if (this.fail === 'upload') throw Object.assign(new Error('upload'), { code: 'unavailable' })
    this.events.push('upload')
    this.objects.get(reservation.object_id).uploaded = ciphertext
  }

  async finish({ objectId }) {
    this.calls.push('finish')
    if (this.fail === 'finish') throw Object.assign(new Error('finish'), { code: 'unavailable' })
    const object = this.objects.get(objectId)
    this.events.push('finish')
    return { object_id: objectId, purpose: object.purpose, ciphertext_bytes: object.ciphertext.length }
  }

  async fetch({ reference }) {
    this.calls.push('fetch')
    if (this.fail === 'fetch') throw Object.assign(new Error('corrupt'), { code: 'payload_digest_mismatch' })
    return this.objects.get(reference.object_id)?.ciphertext ?? new Uint8Array(0)
  }

  async delete({ objectId }) {
    this.calls.push('delete')
    this.events.push('delete')
    this.objects.delete(objectId)
    return { cancelled: true }
  }
}

function transport({ relay = null, packet = 64 * 1024, envelope = 16 * 1024 * 1024, publish } = {}) {
  return new RelayTransport({
    budget: budget(packet, envelope),
    relay,
    publish: publish ?? (async () => {}),
    codec: proto,
  })
}

test('exact MQTT PUBLISH arithmetic counts header, varints, topic, packet id, properties and payload', () => {
  assert.equal(varintBytes(0), 1)
  assert.equal(varintBytes(127), 1)
  assert.equal(varintBytes(128), 2)
  assert.equal(varintBytes(16_384), 3)
  // remaining = 2 (topic length) + 4 (topic) + 2 (packet id) + 1 (properties) + 0
  assert.equal(publishPacketBytes('data', 0), 1 + 1 + 9)
  assert.equal(publishPacketBytes('data', 0, { propertiesBytes: 5 }), 1 + 1 + 14)
  assert.equal(publishPacketBytes('data', 0, { qos: 0 }), 1 + 1 + 7)
  // The remaining-length varint pushes the packet size at its 127/128 edge.
  assert.equal(publishPacketBytes('data', 118), 1 + 1 + 127)
  assert.equal(publishPacketBytes('data', 119), 1 + 2 + 128)
  // Topic length is measured in encoded bytes and counts fully.
  assert.equal(publishPacketBytes('da\u00e9ta', 0), 1 + 1 + (2 + 6 + 2 + 1))
})

test('the effective hop limit is the minimum of local, broker and receiver limits and is never clamped up', () => {
  assert.equal(effectivePacketBytes(64 * 1024, 1024 * 1024, 32 * 1024), 32 * 1024)
  assert.equal(effectivePacketBytes(64 * 1024, 1024 * 1024), 64 * 1024)
  assert.equal(effectivePacketBytes(64 * 1024, 4096, undefined), 4096)
  assert.throws(() => effectivePacketBytes(0), (error) => error.code === 'invalid_argument')
})

test('maxPlaintextBytes finds the largest body that still fits the complete packet', () => {
  const envelopeSize = (textBytes) => publishPacketBytes('topic', textBytes + 32)
  const limit = envelopeSize(100)
  assert.equal(maxPlaintextBytes(envelopeSize, limit), 100)
  assert.equal(maxPlaintextBytes(envelopeSize, limit - 1), 99)
  assert.equal(maxPlaintextBytes(() => 1000, 10), 0)
})

test('representation validation rejects both, neither and the wrong epoch', () => {
  const result = proto.decode('whalerein.v1.HostResult', hostResult())
  assert.doesNotThrow(() => assertResultRepresentation(result))
  assert.doesNotThrow(() => assertResultRepresentation({ ...result, result_body: new Uint8Array(0) }))
  assert.throws(
    () => assertResultRepresentation({ ...result, status: 'online_status_unspecified', result_body: new Uint8Array(0) }),
    (error) => error.code === 'invalid_argument',
  )
  assert.throws(
    () => assertResultRepresentation({ ...result, result_reference: { object_id: 'a'.repeat(32) } }),
    (error) => error.code === 'invalid_argument',
  )
  assert.throws(() => assertRelayEpoch({ relay_version: 2 }), (error) => error.code === 'protocol_version_incompatible')
  assert.throws(() => assertRelayEpoch({}), (error) => error.code === 'protocol_version_incompatible')
  assert.throws(
    () => assertEventRepresentation({ nonce: new Uint8Array(12) }),
    (error) => error.code === 'invalid_argument',
  )
  assert.throws(
    () => assertRequestRepresentation({ kind: 'REQUEST_KIND_QUERY' }),
    (error) => error.code === 'invalid_argument',
  )
  assert.throws(
    () => assertRequestRepresentation({
      kind: 'REQUEST_KIND_QUERY',
      body: new Uint8Array([1]),
      body_reference: { object_id: 'a'.repeat(32) },
    }),
    (error) => error.code === 'invalid_argument',
  )
  assert.doesNotThrow(() => assertRequestRepresentation({ kind: 'REQUEST_KIND_HANDSHAKE', body: new Uint8Array([1]) }))
  assert.doesNotThrow(() => assertRequestRepresentation({ kind: 'REQUEST_KIND_HOST_CLIENTS' }))
  assert.throws(
    () => assertRequestRepresentation({ kind: 'REQUEST_KIND_HOST_CLIENTS', body_reference: { object_id: 'a'.repeat(32) } }),
    (error) => error.code === 'invalid_argument',
  )
  assert.throws(
    () => assertRequestRepresentation({ kind: 'REQUEST_KIND_HANDSHAKE', body_reference: { object_id: 'a'.repeat(32) } }),
    (error) => error.code === 'invalid_argument',
  )
})

test('a complete message below, at and above the encoded boundary is inline, inline and a ready reference', async () => {
  const envelope = hostResult({ body: new Uint8Array(1024) })
  const limit = publishPacketBytes(RESULT_TOPIC, envelope.length)
  for (const [label, receiverLimit, representation] of [
    ['below', limit + 1, 'inline'],
    ['at', limit, 'inline'],
    ['above', limit - 1, 'reference'],
  ]) {
    const relay = new FakeRelay()
    const published = []
    const sender = transport({ relay, publish: async (topic, bytes) => published.push({ topic, bytes }) })
    const outcome = await sender.sendComplete(RESULT_TOPIC, envelope, { kind: 'result', receiverLimit })
    assert.equal(outcome.representation, representation, label)
    assert.equal(published.length, 1)
    if (label === 'above') {
      const decoded = proto.decode('whalerein.v1.HostResult', published[0].bytes)
      assert.equal((decoded.result_body ?? new Uint8Array(0)).length, 0, 'inline body is cleared')
      assert.equal(decoded.result_reference.ciphertext_bytes, 1024)
      assert.equal(decoded.result_reference.purpose, 'PAYLOAD_PURPOSE_RESULT')
      assert.equal(decoded.request_id, 'request-1', 'message identity is unchanged')
    } else {
      assert.deepEqual(published[0].bytes, Buffer.from(envelope))
    }
    assert.equal(published[0].topic, RESULT_TOPIC)
  }
})

test('no reference is published before the exact ciphertext is reserved, uploaded and finished', async () => {
  const relay = new FakeRelay()
  const published = []
  const order = []
  const sender = new RelayTransport({
    budget: budget(),
    relay,
    codec: proto,
    publish: async (topic, bytes) => {
      order.push('publish')
      published.push(bytes)
    },
  })
  const originalReserve = relay.reserve.bind(relay)
  relay.reserve = async (options) => { order.push('reserve'); return originalReserve(options) }
  const originalUpload = relay.upload.bind(relay)
  relay.upload = async (options) => { order.push('upload'); return originalUpload(options) }
  const originalFinish = relay.finish.bind(relay)
  relay.finish = async (options) => { order.push('finish'); return originalFinish(options) }

  await sender.sendComplete(RESULT_TOPIC, hostResult({ body: new Uint8Array(1024) }), { kind: 'result', receiverLimit: 400 })
  assert.deepEqual(order, ['reserve', 'upload', 'finish', 'publish'])
  const decoded = proto.decode('whalerein.v1.HostResult', published[0])
  assert.equal((decoded.result_body ?? new Uint8Array(0)).length, 0)
  assert.equal(decoded.result_reference.object_id, [...relay.objects.keys()][0])
})

test('a failed upload never publishes a reference and cancels the reservation', async () => {
  const relay = new FakeRelay()
  relay.fail = 'upload'
  let published = 0
  const sender = transport({ relay, publish: async () => { published++ } })
  await assert.rejects(
    sender.sendComplete(RESULT_TOPIC, hostResult({ body: new Uint8Array(1024) }), { kind: 'result', receiverLimit: 400 }),
    (error) => error.code === 'unavailable',
  )
  assert.equal(published, 0)
  assert.deepEqual(relay.calls, ['reserve', 'upload', 'delete'])
})

test('a reference packet that itself exceeds the hop limit fails explicitly before publication', async () => {
  const relay = new FakeRelay()
  let published = 0
  const sender = transport({ relay, publish: async () => { published++ } })
  await assert.rejects(
    sender.sendComplete(RESULT_TOPIC, hostResult({ body: new Uint8Array(1024) }), { kind: 'result', receiverLimit: 100 }),
    (error) => error.code === 'payload_too_large',
  )
  assert.equal(published, 0)
  assert.ok(relay.calls.includes('delete'))
})

test('a result above the receiver envelope limit is staged and published as a reference', async () => {
  // The receiver advertised a smaller inline bound than the Host's logical
  // envelope. Content stays deliverable: it travels by reference, not by
  // failing the query.
  const relay = new FakeRelay()
  const published = []
  const sender = transport({ relay, packet: 512, envelope: 4096, publish: async (topic, bytes) => published.push(bytes) })
  await sender.sendComplete(RESULT_TOPIC, hostResult({ body: new Uint8Array(1024) }), {
    kind: 'result', receiverLimit: 400, envelopeLimit: 256,
  })
  assert.deepEqual(relay.calls.filter(call => call === 'reserve' || call === 'upload' || call === 'finish'),
    ['reserve', 'upload', 'finish'])
  const decoded = proto.decode('whalerein.v1.HostResult', published[0])
  assert.equal((decoded.result_body ?? new Uint8Array(0)).length, 0)
  assert.ok(decoded.result_reference)
})

test('a message above envelope_bytes fails with payload_too_large and no relay traffic', async () => {
  const relay = new FakeRelay()
  const sender = transport({ relay, packet: 512, envelope: 1024 })
  const large = hostResult({ body: new Uint8Array(4096) })
  await assert.rejects(
    sender.sendComplete(RESULT_TOPIC, large, { kind: 'result', receiverLimit: 64 }),
    (error) => error.code === 'payload_too_large',
  )
  assert.equal(relay.calls.length, 0)
})

test('per-topic publication is ordered even when a later message is ready first', async () => {
  const published = []
  const gates = []
  const sender = transport({
    publish: async (topic, bytes) => {
      const id = proto.decode('whalerein.v1.HostResult', bytes).request_id
      published.push(id)
      if (id === 'one') await new Promise((resolve) => gates.push(resolve))
    },
  })
  const make = (requestId) => proto.encode('whalerein.v1.HostResult', {
    request_id: requestId, host_id: HOST, device_id: DEVICE, status: 'succeeded',
    result_body: new Uint8Array([1]), relay_version: RELAY_VERSION,
  })
  const first = sender.sendComplete(RESULT_TOPIC, make('one'), { kind: 'result' })
  const second = sender.sendComplete(RESULT_TOPIC, make('two'), { kind: 'result' })
  await new Promise((resolve) => setImmediate(resolve))
  assert.deepEqual(published, ['one'])
  gates.forEach((resolve) => resolve())
  await Promise.all([first, second])
  assert.deepEqual(published, ['one', 'two'])
})

test('receiveRequest resolves an inline request body directly and validates its representation and epoch', async () => {
  const sender = transport()
  const request = proto.encode('whalerein.v1.HostRequest', {
    request_id: 'r1', host_id: HOST, device_id: DEVICE, kind: 'REQUEST_KIND_QUERY',
    body: new Uint8Array([9, 9]), protocol_version: 1, relay_version: RELAY_VERSION,
  })
  const resolved = await sender.receiveRequest(request)
  assert.equal(resolved.kind, 'REQUEST_KIND_QUERY')
  assert.deepEqual(Buffer.from(resolved.body), Buffer.from([9, 9]))

  const old = proto.encode('whalerein.v1.HostRequest', {
    request_id: 'r1', host_id: HOST, device_id: DEVICE, kind: 'REQUEST_KIND_QUERY',
    body: new Uint8Array([9]), protocol_version: 1,
  })
  await assert.rejects(sender.receiveRequest(old), (error) => error.code === 'protocol_version_incompatible')
  await assert.rejects(
    sender.receiveRequest(proto.encode('whalerein.v1.HostRequest', {
      request_id: 'r1', host_id: HOST, device_id: DEVICE, kind: 'REQUEST_KIND_COMMAND',
      protocol_version: 1, relay_version: RELAY_VERSION,
    })),
    (error) => error.code === 'invalid_argument',
  )
  await assert.rejects(sender.receiveRequest(Buffer.from('WRMQTT2legacy')), (error) => error.code === 'invalid_argument')
})

test('receiveRequest resolves a reference to the same complete body', async () => {
  const body = new Uint8Array([4, 5, 6])
  const relay = new FakeRelay()
  const objectId = 'b'.repeat(32)
  relay.objects.set(objectId, { ciphertext: body, purpose: 'PAYLOAD_PURPOSE_REQUEST' })
  const sender = transport({ relay })
  const reference = {
    object_id: objectId,
    owner_node_id: 'node-1',
    ciphertext_bytes: body.length,
    sha256: new Uint8Array(32),
    expires_at_unix_ms: Date.now() + 60_000,
    purpose: 'PAYLOAD_PURPOSE_REQUEST',
  }
  const request = proto.encode('whalerein.v1.HostRequest', {
    request_id: 'r1', host_id: HOST, device_id: DEVICE, kind: 'REQUEST_KIND_COMMAND',
    body_reference: reference, protocol_version: 1, relay_version: RELAY_VERSION,
  })
  await assert.rejects(transport().receiveRequest(request), (error) => error.code === 'unavailable')
  const resolved = await sender.receiveRequest(request)
  assert.deepEqual(Buffer.from(resolved.body), Buffer.from(body))
  assert.equal(resolved.body_reference, undefined)
  assert.deepEqual(relay.calls, ['fetch'])

  // A corrupt or partial fetch is terminal and never becomes a message.
  relay.fail = 'fetch'
  await assert.rejects(
    sender.receiveRequest(request),
    (error) => error.code === 'payload_digest_mismatch',
  )
})

test('a reference that cannot fit the envelope budget is rejected before any fetch', async () => {
  const relay = new FakeRelay()
  const sender = transport({ relay, packet: 256, envelope: 512 })
  const request = proto.encode('whalerein.v1.HostRequest', {
    request_id: 'r1', host_id: HOST, device_id: DEVICE, kind: 'REQUEST_KIND_QUERY',
    body_reference: {
      object_id: 'c'.repeat(32), owner_node_id: 'node-1', ciphertext_bytes: 1024,
      sha256: new Uint8Array(32), expires_at_unix_ms: Date.now() + 60_000,
      purpose: 'PAYLOAD_PURPOSE_REQUEST',
    },
    protocol_version: 1, relay_version: RELAY_VERSION,
  })
  await assert.rejects(sender.receiveRequest(request), (error) => error.code === 'payload_too_large')
  assert.equal(relay.calls.length, 0)
})

test('events use the same complete-message contract and stage only their ciphertext', async () => {
  const relay = new FakeRelay()
  const published = []
  const sender = transport({ relay, publish: async (topic, bytes) => published.push({ topic, bytes }) })
  await sender.sendComplete(EVENT_TOPIC, eventEnvelope({ ciphertext: new Uint8Array(1024) }), { kind: 'event', receiverLimit: 400 })
  const decoded = proto.decode('whalerein.v1.EventEnvelope', published[0].bytes)
  assert.equal((decoded.ciphertext ?? new Uint8Array(0)).length, 0)
  assert.equal(decoded.payload_reference.purpose, 'PAYLOAD_PURPOSE_EVENT')
  assert.equal(decoded.nonce.length, 12, 'the nonce is preserved in either representation')
  assert.equal([...relay.objects.values()][0].ciphertext.length, 1024)
})

test('invalid send and receive shapes fail before publication', async () => {
  const sender = transport()
  await assert.rejects(sender.sendComplete(RESULT_TOPIC, new Uint8Array(0)), (error) => error.code === 'invalid_argument')
  await assert.rejects(
    sender.sendComplete('other/topic', hostResult(), { kind: 'request' }),
    (error) => error.code === 'invalid_argument',
  )
  const envelope = hostResult({ body: new Uint8Array(1024) })
  await assert.rejects(sender.receiveRequest(new Uint8Array(0)), (error) => error.code === 'invalid_argument')
  await assert.rejects(
    transport({ relay: null }).sendComplete(RESULT_TOPIC, envelope, { kind: 'result', receiverLimit: 100 }),
    (error) => error.code === 'payload_too_large',
  )
  assert.throws(() => new RelayTransport({ budget: budget(), codec: proto }), (error) => error.code === 'invalid_argument')
})

test('already referenced envelopes publish as one packet or fail before publish', async () => {
  const reference = {
    object_id: 'd'.repeat(32), owner_node_id: 'node-1', ciphertext_bytes: 100,
    sha256: new Uint8Array(32), expires_at_unix_ms: Date.now() + 60_000,
    purpose: 'PAYLOAD_PURPOSE_RESULT',
  }
  const referenced = hostResult({ reference })
  const limit = publishPacketBytes(RESULT_TOPIC, referenced.length)
  const published = []
  const sender = transport({ publish: async (topic, bytes) => published.push(bytes) })
  const outcome = await sender.sendComplete(RESULT_TOPIC, referenced, { kind: 'result', receiverLimit: limit })
  assert.equal(outcome.representation, 'reference')
  assert.equal(published.length, 1)
  await assert.rejects(
    sender.sendComplete(RESULT_TOPIC, referenced, { kind: 'result', receiverLimit: limit - 1 }),
    (error) => error.code === 'payload_too_large',
  )
  assert.equal(published.length, 1)
})

test('relay-object scope is validated before reservation and an expired scope is terminal', async () => {
  const relay = new FakeRelay()
  const sender = transport({ relay })
  const noDevice = proto.encode('whalerein.v1.HostResult', {
    request_id: 'r', host_id: HOST, status: 'succeeded',
    result_body: new Uint8Array(1024), relay_version: RELAY_VERSION,
  })
  await assert.rejects(
    sender.sendComplete(RESULT_TOPIC, noDevice, { kind: 'result', receiverLimit: 100 }),
    (error) => error.code === 'invalid_argument',
  )
  const noChannel = hostResult({ body: new Uint8Array(1024) })
  const decodedNoChannel = proto.decode('whalerein.v1.HostResult', noChannel)
  decodedNoChannel.device_id = DEVICE
  delete decodedNoChannel.channel_id
  await assert.rejects(
    sender.sendComplete(RESULT_TOPIC, proto.encode('whalerein.v1.HostResult', decodedNoChannel), {
      kind: 'result', receiverLimit: 100,
    }),
    (error) => error.code === 'invalid_argument',
  )
  await assert.rejects(
    sender.sendComplete(RESULT_TOPIC, hostResult({ body: new Uint8Array(1024) }), {
      kind: 'result', receiverLimit: 100, expiresAt: 1,
    }),
    (error) => error.code === 'expired',
  )
  assert.equal(relay.calls.length, 0)
})

test('a reservation with no owner and an interrupted relay both cancel before publication', async () => {
  const missingOwner = new FakeRelay()
  missingOwner.reserve = async () => ({
    object_id: 'e'.repeat(32), owner_node_id: '',
    upload_path: `/v1/relay/objects/${'e'.repeat(32)}`,
    max_bytes: 1024, expires_at_unix_ms: Date.now() + 60_000, upload_timeout_ms: 1000,
  })
  let published = 0
  const sender = transport({ relay: missingOwner, publish: async () => { published++ } })
  await assert.rejects(
    sender.sendComplete(RESULT_TOPIC, hostResult({ body: new Uint8Array(1024) }), { kind: 'result', receiverLimit: 400 }),
    (error) => error.code === 'protocol_version_incompatible',
  )
  assert.equal(published, 0)
  assert.ok(missingOwner.calls.includes('delete'))

  const interrupted = new FakeRelay()
  let interruptedSender
  const originalReserve = interrupted.reserve.bind(interrupted)
  interrupted.reserve = async (options) => { const value = await originalReserve(options); interruptedSender.reset(); return value }
  interruptedSender = transport({ relay: interrupted, publish: async () => { published++ } })
  await assert.rejects(
    interruptedSender.sendComplete(RESULT_TOPIC, hostResult({ body: new Uint8Array(1024) }), { kind: 'result', receiverLimit: 400 }),
    (error) => error.code === 'unavailable',
  )
  assert.equal(published, 0)
})

test('packet arithmetic rejects invalid lengths and topic names', () => {
  assert.throws(() => varintBytes(-1), (error) => error.code === 'invalid_argument')
  assert.throws(() => varintBytes(1.5), (error) => error.code === 'invalid_argument')
  assert.throws(() => publishPacketBytes('', 1), (error) => error.code === 'invalid_argument')
  assert.throws(() => publishPacketBytes('t', -1), (error) => error.code === 'invalid_argument')
  assert.throws(() => publishPacketBytes('t', 1, { propertiesBytes: -1 }), (error) => error.code === 'invalid_argument')
  assert.equal(maxPlaintextBytes(() => 1, 1), 0)
})

const LIVE_TOPIC = `whalerein/v1/h/${HOST}/live`

test('live publication is one inline QoS 0 packet on the live topic with no chain, reference or retry', async () => {
  const relay = new FakeRelay()
  const published = []
  const options = []
  const sender = transport({
    relay,
    publish: async (topic, bytes, publishOptions) => {
      options.push(publishOptions)
      published.push({ topic, bytes })
    },
  })
  const envelope = eventEnvelope({ ciphertext: new Uint8Array(1024) })
  // The same envelope would become a reference on the control class; live must
  // stay inline and is dropped if it cannot fit.
  assert.equal(await sender.sendLive(LIVE_TOPIC, envelope, { receiverLimit: 400 }), false)
  assert.equal(published.length, 0)
  assert.equal(relay.calls.length, 0)

  assert.equal(await sender.sendLive(LIVE_TOPIC, envelope), true)
  assert.equal(published.length, 1)
  assert.equal(published[0].topic, LIVE_TOPIC)
  assert.deepEqual(options, [{ qos: 0 }])
  assert.equal(relay.calls.length, 0, 'live never stages an HTTPS reference')
  const decoded = proto.decode('whalerein.v1.EventEnvelope', published[0].bytes)
  assert.equal(decoded.payload_reference, undefined)
  assert.equal(decoded.ciphertext.length, 1024)
})

test('live publication drops a wrong epoch, a reference, an empty or an oversized packet', async () => {
  const published = []
  const sender = transport({ publish: async (topic, bytes) => published.push(bytes) })
  const old = proto.encode('whalerein.v1.EventEnvelope', {
    host_id: HOST, device_id: DEVICE, channel_id: CHANNEL, nonce: new Uint8Array(12),
    ciphertext: new Uint8Array([1]), created_at_unix_ms: 1, relay_version: 3,
  })
  assert.equal(await sender.sendLive(LIVE_TOPIC, old), false)
  assert.equal(await sender.sendLive(LIVE_TOPIC, new Uint8Array(0)), false)
  const reference = {
    object_id: 'a'.repeat(32), owner_node_id: 'node-1', ciphertext_bytes: 100,
    sha256: new Uint8Array(32), expires_at_unix_ms: Date.now() + 60_000,
    purpose: 'PAYLOAD_PURPOSE_EVENT',
  }
  assert.equal(await sender.sendLive(LIVE_TOPIC, eventEnvelope({ reference })), false)
  const tooLarge = transport({ packet: 400 })
  assert.equal(await tooLarge.sendLive(LIVE_TOPIC, eventEnvelope({ ciphertext: new Uint8Array(1024) })), false)
  assert.equal(published.length, 0)
})

test('two live previews are in flight at once while control stays ordered on QoS 1', async () => {
  const gates = []
  const topics = []
  const controlOptions = []
  const sender = transport({
    publish: async (topic, bytes, options) => {
      topics.push(topic)
      if (options?.qos === 0) {
        await new Promise((resolve) => gates.push(resolve))
        return
      }
      controlOptions.push(options)
    },
  })
  const first = sender.sendLive(LIVE_TOPIC, eventEnvelope({ ciphertext: new Uint8Array([1]) }))
  const second = sender.sendLive(LIVE_TOPIC, eventEnvelope({ ciphertext: new Uint8Array([2]) }))
  await new Promise((resolve) => setImmediate(resolve))
  assert.equal(gates.length, 2, 'both previews reach the publish seam before either resolves')
  gates.forEach((resolve) => resolve())
  assert.deepEqual(await Promise.all([first, second]), [true, true])

  await sender.sendComplete(EVENT_TOPIC, eventEnvelope({ ciphertext: new Uint8Array([3]) }), { kind: 'event' })
  assert.deepEqual(controlOptions, [undefined], 'control keeps the default QoS 1 publication')
  assert.equal(topics.filter((topic) => topic === LIVE_TOPIC).length, 2)
})
