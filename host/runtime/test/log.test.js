import test from 'node:test'
import assert from 'node:assert/strict'
import { createLogger, createLogSalt, redactId, silentLogger } from '../src/log.js'

test('when logging structured fields, identifiers are redacted and unapproved fields are omitted', () => {
  const records = []
  const logger = createLogger({ salt: 'ab'.repeat(16), level: 'debug', sink: (record) => records.push(record), now: () => 7 })
  logger.debug('sample', {
    request: 'request-id',
    host: 'host-id',
    status: 'succeeded',
    count: 3,
    token: 'must-not-appear',
    body: 'message body',
    ciphertext: 'cipher',
    nested: { secret: 'x' },
    invalidNumber: Number.NaN,
  })
  assert.equal(records.length, 1)
  const record = records[0]
  assert.equal(record.time, 7)
  assert.equal(record.event, 'sample')
  assert.equal(record.status, 'succeeded')
  assert.equal(record.count, 3)
  assert.equal(record.request, redactId('ab'.repeat(16), 'request-id'))
  assert.notEqual(record.request, 'request-id')
  assert.equal(record.token, undefined)
  assert.equal(record.body, undefined)
  assert.equal(record.ciphertext, undefined)
  assert.equal(record.nested, undefined)
  assert.equal(record.invalidNumber, undefined)
  assert.equal(JSON.stringify(record).includes('must-not-appear'), false)
})

test('when log levels or silent mode are set, the logger filters events and cleans scalar fields', () => {
  const records = []
  const info = createLogger({ salt: '0'.repeat(32), level: 'info', sink: (record) => records.push(record) })
  info.debug('hidden')
  info.info('shown', { status: 'ok\nnext', ms: 1, online: true, pending: false, mode: 'x' })
  info.warn('warn', { code: 'unavailable' })
  info.error('error', { code: 'internal' })
  assert.deepEqual(records.map((record) => record.event), ['shown', 'warn', 'error'])
  assert.equal(records[0].status, 'oknext')
  records.length = 0
  silentLogger.error('hidden')
  assert.equal(records.length, 0)
  assert.equal(redactId('00'.repeat(16), ''), '')
  assert.equal(createLogSalt().length, 32)
  records.length = 0
  const nan = createLogger({ salt: '0'.repeat(32), level: 'info', sink: (record) => records.push(record) })
  nan.info('nan', { ms: Number.NaN, count: Number.POSITIVE_INFINITY })
  assert.equal(records[0].ms, 0)
  assert.equal(records[0].count, 0)
  records.length = 0
  nan.info('unsupported-scalars', { status: { nested: true }, code: null, kind: undefined })
  assert.equal(records[0].status, undefined)
  assert.equal(records[0].code, undefined)
  assert.equal(records[0].kind, undefined)
})

test('when local log salts differ, operation trace IDs remain comparable', () => {
  const records = []
  for (const salt of ['ab'.repeat(16), 'cd'.repeat(16)]) {
    createLogger({ salt, sink: (record) => records.push(record) }).info('result', { trace: 'operation-123', device: 'device-1', detail: 'private message', reason: 'channel_missing' })
  }
  assert.equal(records[0].trace_id, 'operation-123')
  assert.equal(records[1].trace_id, records[0].trace_id)
  assert.notEqual(records[0].device, records[1].device)
  assert.equal(records[0].detail, undefined)
  const rejected = []
  createLogger({ salt: '00'.repeat(16), sink: (record) => rejected.push(record) }).info('invalid', { trace: 'bad\ntrace' })
  assert.equal(rejected[0].trace_id, undefined)
})

test('the default sink writes one JSON line and uses the default clock', () => {
  const chunks = []
  const original = process.stderr.write
  process.stderr.write = (chunk) => {
    chunks.push(String(chunk))
    return true
  }
  try {
    createLogger({ salt: '00'.repeat(16) }).info('default-sink', { status: 'ok' })
  } finally {
    process.stderr.write = original
  }
  assert.equal(chunks.length, 1)
  const record = JSON.parse(chunks[0])
  assert.equal(record.event, 'default-sink')
  assert.equal(record.status, 'ok')
  assert.equal(typeof record.time, 'number')
})
