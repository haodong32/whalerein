import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { ProtoCodec, codec, DEFAULT_PROTO_PATH } from '../src/protocol.js'
import { PluginError } from '../src/errors.js'

const fixtures = fileURLToPath(new URL('../../../proto/v1/fixtures/', import.meta.url))

test('when HostRequest is encoded from an object, its bytes match the cross-language fixture', () => {
  const proto = codec()
  const json = JSON.parse(readFileSync(`${fixtures}host_request.json`, 'utf8'))
  const expected = readFileSync(`${fixtures}host_request.bin`)
  // The descriptor is generated with --keep-case; the fixture JSON is camelCase
  // because it represents the HTTPS side. The plugin never parses that JSON.
  const explicit = {
    request_id: 'req-1',
    host_id: 'host-1',
    device_id: 'device-1',
    kind: 'REQUEST_KIND_QUERY',
    command_id: '',
    payload_digest: new Uint8Array(0),
    expires_at_unix_ms: Number(json.expiresAtUnixMs),
    trace_id: 'trace-1',
    protocol_version: 1,
    channel_id: 'channel-1',
    body: new Uint8Array(Buffer.from('hello')),
  }
  const encoded = proto.encode('whalerein.v1.HostRequest', explicit)
  assert.deepEqual(Buffer.from(encoded), expected)
  const decoded = proto.decodeJson('whalerein.v1.HostRequest', expected)
  assert.equal(decoded.request_id, 'req-1')
  assert.equal(decoded.host_id, 'host-1')
  assert.equal(decoded.kind, 'REQUEST_KIND_QUERY')
  assert.equal(decoded.expires_at_unix_ms, '1893456000000')
  assert.equal(decoded.body, 'aGVsbG8=')
})

test('when canonical proto3 JSON values are encoded, decoding preserves every supplied field including base64 bytes and 64-bit decimal strings', () => {
  const proto = codec()
  const json = {
    request_id: 'r',
    host_id: 'h',
    device_id: 'd',
    kind: 'REQUEST_KIND_COMMAND',
    command_id: 'c',
    payload_digest: Buffer.alloc(32, 7).toString('base64'),
    expires_at_unix_ms: '1893456000000',
    trace_id: 't',
    protocol_version: 1,
    channel_id: 'ch',
    body: Buffer.from('body').toString('base64'),
  }
  const bytes = proto.encodeJsonValue('whalerein.v1.HostRequest', json)
  const roundtrip = proto.decodeJson('whalerein.v1.HostRequest', bytes)
  for (const [field, expected] of Object.entries(json)) assert.equal(roundtrip[field], expected, field)
  assert.equal(roundtrip.request_id, 'r')
  assert.equal(roundtrip.command_id, 'c')
  assert.equal(roundtrip.payload_digest, json.payload_digest)
  assert.equal(roundtrip.expires_at_unix_ms, '1893456000000')
  assert.equal(roundtrip.body, json.body)
})

test('when EventEnvelope and PairingClaim messages are encoded, decoding restores their fields', () => {
  const proto = codec()
  const envelope = {
    host_id: 'host-1',
    device_id: 'device-1',
    channel_id: 'channel-1',
    nonce: new Uint8Array(12),
    ciphertext: new Uint8Array([1, 2, 3]),
    created_at_unix_ms: 123,
    trace_id: 'trace',
  }
  const bytes = proto.encode('whalerein.v1.EventEnvelope', envelope)
  const decoded = proto.decode('whalerein.v1.EventEnvelope', bytes)
  assert.equal(decoded.host_id, 'host-1')
  assert.deepEqual([...decoded.ciphertext], [1, 2, 3])
  assert.equal(decoded.created_at_unix_ms, 123)

  const claim = {
    claim_id: 'claim-1',
    token_hash: 'a'.repeat(64),
    phase: 'PAIRING_PHASE_HELLO',
    noise_message: new Uint8Array([9, 9]),
    device_id: 'device-1',
    expires_at_unix_ms: 456,
  }
  const claimBytes = proto.encode('whalerein.v1.PairingClaim', claim)
  const claimDecoded = proto.decode('whalerein.v1.PairingClaim', claimBytes)
  assert.equal(claimDecoded.phase, 'PAIRING_PHASE_HELLO')
  assert.deepEqual([...claimDecoded.noise_message], [9, 9])
})

test('when queried, enum helpers return the protobuf names and numbers', () => {
  const proto = codec()
  assert.equal(proto.enumValue('whalerein.v1.RequestKind', 'REQUEST_KIND_COMMAND'), 3)
  assert.equal(proto.enumName('whalerein.v1.ErrorCode', 9), 'payload_digest_mismatch')
  assert.throws(() => proto.enumName('whalerein.v1.HostRequest', 1), PluginError)
  assert.equal(proto.enumName('whalerein.v1.RequestKind', 3), 'REQUEST_KIND_COMMAND')
})

test('when encoding uses an unknown type or invalid bytes, or decoding sees malformed wire data, the codec rejects at the correct boundary', () => {
  const proto = codec()
  assert.throws(() => proto.encode('whalerein.v1.DoesNotExist', {}), (error) => error.code === 'internal')
  assert.throws(() => proto.decode('whalerein.v1.HostRequest', new Uint8Array([255, 255, 255])), (error) => error.code === 'invalid_argument')
  assert.throws(() => proto.encode('whalerein.v1.HostRequest', { body: '!!!' }), (error) => error.code === 'invalid_argument')
  assert.throws(() => new ProtoCodec('/nonexistent/proto.json'), (error) => error.code === 'internal')
  assert.equal(DEFAULT_PROTO_PATH.endsWith('host/runtime/src/generated/whalerein.json'), true)
})

test('when a custom descriptor path is supplied, the codec loads it independently', () => {
  const custom = new ProtoCodec(DEFAULT_PROTO_PATH)
  assert.equal(custom.decode('whalerein.v1.Ping', custom.encode('whalerein.v1.Ping', { client_time_unix_ms: 5 })).client_time_unix_ms, 5)
})

test('model selection preserves absent effort, receipts and absent capability defaults', () => {
  const proto = codec()
  const request = { select_session_model: { session_id: 's', provider: 'p', model: 'm' } }
  const decoded = proto.decode('whalerein.v1.RequestPayload', proto.encode('whalerein.v1.RequestPayload', request))
  assert.equal(decoded.select_session_model.model, 'm')
  assert.equal(Object.hasOwn(decoded.select_session_model, 'effort'), false)
  const receipt = { model_selection_receipt: { selected: { provider: 'p', model: 'm', effort: 'high' }, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' } }
  assert.deepEqual(proto.decode('whalerein.v1.ResponsePayload', proto.encode('whalerein.v1.ResponsePayload', receipt)), receipt)
  const caps = proto.decode('whalerein.v1.SourceCapabilities', proto.encode('whalerein.v1.SourceCapabilities', {}))
  assert.equal(Boolean(caps.model_selection), false)
})

test('old source capability bytes leave all question operations disabled', () => {
  // Fields 1 and 5 are history and approvals in the older wire message.
  const old = codec().decode('whalerein.v1.SourceCapabilities', Buffer.from([8, 1, 40, 1]))
  assert.equal(old.history, true)
  assert.equal(old.approvals, true)
  for (const key of ['questions_read', 'questions_answer_open', 'questions_answer_continued']) {
    assert.equal(Boolean(old[key]), false)
  }
})
