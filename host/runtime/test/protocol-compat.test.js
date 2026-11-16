import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import {
  ProtoCodec,
  DEFAULT_PROTO_PATH,
  PROTOCOL_WINDOW,
  protocolVersionCompatibility,
  toBigInt,
} from '../src/protocol.js'
import {
  ERROR_ACTIONS,
  ERROR_CODES,
  ERROR_EXIT_CODES,
  clientAction,
  errorExitCode,
} from '../src/errors.js'
import { ID_LIMITS, assertRequestIdentity, validateId } from '../src/contracts/ids.js'
import {
  CLOCK_SKEW_TOLERANCE_MS,
  PAIRING_SIGNATURE_FUTURE_MS,
  PAIRING_SIGNATURE_PAST_MS,
  clockSkewLabel,
  classifyTimestamp,
  isExpired,
  validatePairingSignatureTime,
} from '../src/contracts/time.js'
import { validateSessionUpdate } from '../src/contracts/source.js'

const root = fileURLToPath(new URL('../../../', import.meta.url))
const codec = new ProtoCodec()
const descriptor = JSON.parse(readFileSync(DEFAULT_PROTO_PATH, 'utf8'))
const errorCodes = JSON.parse(readFileSync(`${root}proto/v1/error_codes.json`, 'utf8'))
const vectors = JSON.parse(readFileSync(`${root}proto/v1/compat/vectors.json`, 'utf8'))

function vector(id) {
  const found = vectors.vectors.find((entry) => entry.id === id)
  assert.ok(found, `missing vector ${id}`)
  return found
}

test('when a 64-bit value exceeds 2^53-1, decoding keeps it exact instead of narrowing to an unsafe Number', () => {
  const bytes = codec.encode('whalerein.v1.HostRequest', {
    request_id: 'r-1',
    expires_at_unix_ms: '9007199254740993',
  })
  const decoded = codec.decode('whalerein.v1.HostRequest', bytes)
  assert.equal(decoded.expires_at_unix_ms, '9007199254740993')
  assert.equal(typeof decoded.expires_at_unix_ms, 'string')
  assert.equal(toBigInt(decoded.expires_at_unix_ms), 9007199254740993n)

  const safe = codec.decode(
    'whalerein.v1.HostRequest',
    codec.encode('whalerein.v1.HostRequest', { expires_at_unix_ms: '1700000000000' }),
  )
  assert.equal(safe.expires_at_unix_ms, 1700000000000)
  assert.equal(typeof safe.expires_at_unix_ms, 'number')
})

test('when a decoded 64-bit value is not a safe integer, toBigInt refuses to guess', () => {
  assert.equal(toBigInt(42), 42n)
  assert.equal(toBigInt('42'), 42n)
  assert.equal(toBigInt(4n), 4n)
  assert.throws(() => toBigInt(2 ** 53), (error) => error.code === 'invalid_argument')
  assert.throws(() => toBigInt('not-a-number'), (error) => error.code === 'invalid_argument')
})

test('when the schema adds a field, a binary reader skips the unknown tag and keeps known fields', () => {
  const known = vector('binary-unknown-tag-is-ignored')
  const decoded = codec.decode('whalerein.v1.HostRequest', Buffer.from(known.hex, 'hex'))
  assert.equal(decoded.request_id, 'req-1')
})

test('when an unknown enum number arrives, decoding never renders it as the zero value and validation fails explicitly', () => {
  // HostRequest.kind is field 4 (tag 0x20); 99 is not a RequestKind value.
  const decoded = codec.decode('whalerein.v1.HostRequest', Uint8Array.from([0x20, 99]))
  assert.equal(decoded.kind, 99)
  const problems = codec.unknownEnums('whalerein.v1.HostRequest', decoded)
  assert.equal(problems.length, 1)
  assert.equal(problems[0].enum, '.whalerein.v1.RequestKind')
  assert.throws(
    () => codec.assertKnownEnums('whalerein.v1.HostRequest', decoded),
    (error) => error.code === 'protocol_version_incompatible' && /RequestKind/.test(error.message),
  )
})

test('when an enum is absent or null, validation does not invent an unknown value', () => {
  assert.deepEqual(codec.unknownEnums('whalerein.v1.QuestionBatch', { batch_id: 'b', phase: undefined }), [])
  assert.deepEqual(codec.unknownEnums('whalerein.v1.QuestionBatch', { batch_id: 'b', phase: null }), [])
  assert.deepEqual(codec.unknownEnums('whalerein.v1.QuestionBatch', { batch_id: 'b' }), [])
})

test('when a oneof carries no known variant, it is an error instead of an empty success', () => {
  const empty = codec.decode('whalerein.v1.RequestPayload', new Uint8Array(0))
  assert.deepEqual(empty, {})
  assert.equal(codec.hasOneof('whalerein.v1.RequestPayload', empty, 'payload'), false)
  assert.throws(
    () => codec.requireOneof('whalerein.v1.RequestPayload', empty, 'payload'),
    (error) => error.code === 'invalid_argument' && /no known payload variant/.test(error.message),
  )
  const known = codec.decode(
    'whalerein.v1.RequestPayload',
    codec.encode('whalerein.v1.RequestPayload', { list_sessions: {} }),
  )
  assert.equal(codec.hasOneof('whalerein.v1.RequestPayload', known, 'payload'), true)
  assert.equal(codec.requireOneof('whalerein.v1.RequestPayload', known, 'payload'), known)
  assert.throws(() => codec.hasOneof('whalerein.v1.RequestPayload', known, 'nope'), (error) => error.code === 'internal')
})

test('when peers disagree on a protocol version, the window names which side upgrades', () => {
  assert.deepEqual(PROTOCOL_WINDOW, { min: 1, max: 1 })
  assert.equal(protocolVersionCompatibility(1).compatible, true)
  assert.equal(protocolVersionCompatibility(1).side, 'none')
  const missing = protocolVersionCompatibility(0)
  assert.equal(missing.compatible, false)
  assert.equal(missing.side, 'peer')
  assert.match(missing.message, /upgrade the older peer/)
  const newer = protocolVersionCompatibility(2)
  assert.equal(newer.side, 'local')
  assert.match(newer.message, /upgrade this client/)
  assert.equal(protocolVersionCompatibility('2').side, 'local')
  assert.equal(protocolVersionCompatibility('x').side, 'peer')
})

test('when the golden error table changes, the host vocabulary, exit code and action stay identical', () => {
  const enumValues = descriptor.nested.whalerein.nested.v1.nested.ErrorCode.values
  assert.deepEqual([...ERROR_CODES].sort(), Object.keys(enumValues).sort())
  assert.equal(ERROR_CODES.length, errorCodes.codes.length)
  for (const entry of errorCodes.codes) {
    assert.equal(ERROR_CODES.includes(entry.name), true, `${entry.name} missing`)
    assert.equal(ERROR_EXIT_CODES[entry.name], entry.exitCode, `${entry.name} exit`)
    assert.equal(ERROR_ACTIONS[entry.name], entry.action, `${entry.name} action`)
    assert.equal(errorExitCode(entry.name), entry.exitCode)
    assert.equal(clientAction(entry.name), entry.action)
  }
  assert.equal(errorExitCode('not_a_code'), 1)
  assert.equal(clientAction('not_a_code'), 'retry')
  assert.equal(errorExitCode('protocol_version_incompatible'), 9)
  assert.equal(clientAction('protocol_version_incompatible'), 'upgrade')
  assert.equal(clientAction('pairing_fenced'), 'restart_pairing')
  assert.equal(clientAction('pairing_committed_unconfirmed'), 'review_pairing')
})

test('when an identifier violates the wire bounds, validation rejects it before logging or forwarding', () => {
  assert.deepEqual(validateId('request_id', 'abc-DEF_123'), { ok: true })
  assert.equal(validateId('request_id', '').ok, false)
  assert.equal(validateId('command_id', '').ok, true)
  assert.equal(validateId('request_id', 'a|b').reason, 'AAD separator')
  assert.equal(validateId('request_id', 'a\nb').reason, 'control character')
  assert.equal(validateId('request_id', 'héllo').reason, 'not printable ASCII')
  assert.equal(validateId('session_id', 'sess/ü').ok, true)
  assert.equal(validateId('request_id', 'a'.repeat(ID_LIMITS.request_id.max + 1)).reason, 'too long')
  assert.equal(validateId('session_id', 'a'.repeat(ID_LIMITS.session_id.max + 1)).reason, 'too long')
  assert.throws(() => validateId('nope', 'x'), (error) => error.code === 'internal')
  assert.throws(
    () => assertRequestIdentity({ request_id: 'r', host_id: 'h', device_id: 'd', channel_id: 'c', command_id: 'bad|id', trace_id: 't' }),
    (error) => error.code === 'invalid_argument' && /command_id/.test(error.message),
  )
  assert.equal(assertRequestIdentity({ request_id: 'r', command_id: '' }).request_id, 'r')
})

test('when a session update carries a malformed identity, the source contract rejects it', () => {
  const valid = {
    session: { host_id: 'h', session_id: 's-1' },
    history: {
      from_cursor: 'a',
      to_cursor: 'b',
      batch_id: 'batch',
      changes: [{ message: { message_id: 'm', role: 'ASSISTANT', blocks: [{ block_id: 'b', text: { text: 'x' } }] } }],
    },
  }
  assert.equal(validateSessionUpdate(valid).session.session_id, 's-1')
  assert.throws(
    () => validateSessionUpdate({ ...valid, session: { host_id: 'h', session_id: 'bad|id' } }),
    (error) => error.code === 'invalid_argument',
  )
  assert.throws(
    () => validateSessionUpdate({ ...valid, history: { ...valid.history, batch_id: 'bad|id' } }),
    (error) => error.code === 'invalid_argument',
  )
  assert.throws(
    () => validateSessionUpdate({ ...valid, history: { ...valid.history, changes: [{ message: { message_id: 'bad\nid', role: 'ASSISTANT', blocks: [{ block_id: 'b', text: { text: 'x' } }] } }] } }),
    (error) => error.code === 'invalid_argument',
  )
})

test('when timestamps differ, expiry is exact and only the pairing window accepts bounded skew', () => {
  assert.equal(isExpired(1000, 1000), true)
  assert.equal(isExpired(1001, 1000), false)
  assert.equal(isExpired(999, 1000), true)
  assert.deepEqual(classifyTimestamp(900, 1000), { order: 'past', byMs: 100 })
  assert.deepEqual(classifyTimestamp(1100, 1000), { order: 'future', byMs: 100 })
  assert.deepEqual(classifyTimestamp(1000, 1000), { order: 'now', byMs: 0 })
  assert.equal(clockSkewLabel(1000, 1000), null)
  assert.equal(clockSkewLabel(1000 + CLOCK_SKEW_TOLERANCE_MS + 1, 1000), 'clock_skew')
  const now = 1_000_000
  assert.deepEqual(validatePairingSignatureTime(now, now), { ok: true })
  assert.equal(validatePairingSignatureTime(now - PAIRING_SIGNATURE_PAST_MS, now).ok, true)
  assert.equal(validatePairingSignatureTime(now + PAIRING_SIGNATURE_FUTURE_MS, now).ok, true)
  assert.equal(validatePairingSignatureTime(now - PAIRING_SIGNATURE_PAST_MS - 1, now).reason, 'too_old')
  assert.equal(validatePairingSignatureTime(now + PAIRING_SIGNATURE_FUTURE_MS + 1, now).reason, 'too_far_ahead')
})

test('when a codec is constructed for a custom descriptor, it does not share the default root', () => {
  const other = new ProtoCodec(DEFAULT_PROTO_PATH)
  assert.equal(other.decode('whalerein.v1.Pong', codec.encode('whalerein.v1.Pong', { host_time_unix_ms: 1 })).host_time_unix_ms, 1)
  assert.throws(() => other.type('whalerein.v1.NotAType'), (error) => error.code === 'internal')
  assert.equal(other.enumName('whalerein.v1.RequestKind', 1), 'REQUEST_KIND_HANDSHAKE')
  assert.equal(other.enumValue('whalerein.v1.RequestKind', 'REQUEST_KIND_HANDSHAKE'), 1)
  assert.throws(() => new ProtoCodec(`${root}proto/v1/compat/missing.json`), (error) => error.code === 'internal')
})
