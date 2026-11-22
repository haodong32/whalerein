import test from 'node:test'
import assert from 'node:assert/strict'
import { ed25519 } from '@noble/curves/ed25519.js'
import { randomBytes } from 'node:crypto'
import { RelayObjectClient, relaySignedText, validatePayloadReference } from '../src/access/relay-client.js'
import { authorizationPublicKey } from '../src/security/host-auth.js'
import { hex, sha256Bytes } from '../src/security/digest.js'
import { codec } from '../src/protocol.js'

const proto = codec()
const privateKey = new Uint8Array(randomBytes(32))
const publicKey = authorizationPublicKey(privateKey)
const ORIGIN = 'https://access.example'
const HOST = 'host-1'

function client({ fetch, logger, now = () => 1_700_000_000_000 } = {}) {
  return new RelayObjectClient({
    origin: ORIGIN,
    hostId: HOST,
    privateKey,
    codec: proto,
    fetch,
    now,
    logger,
  })
}

function verify(text, signature) {
  return ed25519.verify(signature, new TextEncoder().encode(text), publicKey)
}

function jsonResponse(value, status = 200, headers = {}) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { 'content-type': 'application/json', ...headers },
  })
}

const ciphertext = new Uint8Array(randomBytes(256))

test('the fixed signed text forms match the wire contract exactly', () => {
  assert.equal(
    relaySignedText('reserve', {
      relayVersion: 4, hostId: 'h', deviceId: 'd', sessionId: 's', channelId: 'c',
      purpose: 'PAYLOAD_PURPOSE_RESULT', partId: '', ciphertextBytes: 12,
      sha256Hex: 'a'.repeat(64), expiresAt: 5, signedAt: 6,
    }),
    `whalerein/v1/relay|reserve|4|h|d|s|c|PAYLOAD_PURPOSE_RESULT||12|${'a'.repeat(64)}|5|6`,
  )
  assert.equal(
    relaySignedText('upload', { relayVersion: 4, objectId: 'o', ciphertextBytes: 1, sha256Hex: 'b'.repeat(64), signedAt: 2 }),
    `whalerein/v1/relay|upload|4|o|1|${'b'.repeat(64)}|2`,
  )
  assert.equal(
    relaySignedText('finish', { relayVersion: 4, objectId: 'o', ciphertextBytes: 1, sha256Hex: 'b'.repeat(64), signedAt: 2 }),
    `whalerein/v1/relay|finish|4|o|1|${'b'.repeat(64)}|2`,
  )
  assert.equal(
    relaySignedText('object', { relayVersion: 4, objectId: 'o', signedAt: 2 }),
    'whalerein/v1/relay|object|4|o|2',
  )
})

test('reserve carries the epoch, exact scope, digest and a valid Host signature', async () => {
  const calls = []
  const reservation = {
    objectId: 'a'.repeat(32),
    ownerNodeId: 'node-1',
    uploadPath: `/v1/relay/objects/${'a'.repeat(32)}`,
    maxBytes: ciphertext.length,
    expiresAtUnixMs: 1_700_000_060_000,
    uploadTimeoutMs: 120_000,
  }
  const relay = client({
    fetch: async (url, options) => {
      calls.push({ url, options })
      return jsonResponse(reservation)
    },
  })
  const result = await relay.reserve({
    deviceId: 'device-1', sessionId: 'session-1', channelId: 'channel-1',
    purpose: 'PAYLOAD_PURPOSE_RESULT', ciphertext, expiresAt: 1_700_000_060_000,
  })
  assert.equal(calls[0].url, `${ORIGIN}/v1/relay/objects`)
  assert.equal(calls[0].options.method, 'POST')
  const body = JSON.parse(calls[0].options.body)
  assert.equal(body.relay_version, 4, 'proto3 JSON keeps the required epoch')
  assert.equal(body.host_id, HOST)
  assert.equal(body.device_id, 'device-1')
  assert.equal(body.purpose, 'PAYLOAD_PURPOSE_RESULT')
  assert.equal(Number(body.ciphertext_bytes), ciphertext.length)
  assert.equal(body.sha256, Buffer.from(sha256Bytes(ciphertext)).toString('base64'))
  assert.equal(body.expires_at_unix_ms, '1700000060000')
  const expected = relaySignedText('reserve', {
    relayVersion: 4, hostId: HOST, deviceId: 'device-1', sessionId: 'session-1',
    channelId: 'channel-1', purpose: 'PAYLOAD_PURPOSE_RESULT', partId: '',
    ciphertextBytes: ciphertext.length, sha256Hex: hex(sha256Bytes(ciphertext)),
    expiresAt: 1_700_000_060_000, signedAt: 1_700_000_000_000,
  })
  assert.equal(verify(expected, new Uint8Array(Buffer.from(body.signature, 'base64'))), true)
  assert.equal(result.object_id, reservation.objectId)
  assert.equal(result.owner_node_id, 'node-1')
  assert.equal(result.max_bytes, ciphertext.length)
})

test('upload uses the reserved upload path with exact length, epoch and signed headers', async () => {
  const calls = []
  const relay = client({
    fetch: async (url, options) => {
      calls.push({ url, options })
      return new Response(null, { status: 204 })
    },
  })
  const reservation = {
    object_id: 'a'.repeat(32), owner_node_id: 'node-1',
    upload_path: `/v1/relay/objects/${'a'.repeat(32)}`,
    max_bytes: ciphertext.length, expires_at_unix_ms: 1_700_000_060_000, upload_timeout_ms: 1000,
  }
  await relay.upload({ reservation, ciphertext })
  assert.equal(calls[0].url, `${ORIGIN}/v1/relay/objects/${'a'.repeat(32)}`)
  assert.equal(calls[0].options.method, 'PUT')
  assert.equal(calls[0].options.headers['WhaleRein-Relay-Epoch'], '4')
  assert.equal(calls[0].options.headers['WhaleRein-Host-Signed-At-Unix-Ms'], '1700000000000')
  assert.equal(calls[0].options.headers['content-length'], String(ciphertext.length))
  const expected = relaySignedText('upload', {
    relayVersion: 4, objectId: reservation.object_id, ciphertextBytes: ciphertext.length,
    sha256Hex: hex(sha256Bytes(ciphertext)), signedAt: 1_700_000_000_000,
  })
  assert.equal(verify(expected, new Uint8Array(Buffer.from(calls[0].options.headers['WhaleRein-Host-Signature'], 'base64url'))), true)
})

test('finish publishes only after the exact ciphertext and returns the object metadata', async () => {
  const calls = []
  const relay = client({
    fetch: async (url, options) => {
      calls.push({ url, options })
      return jsonResponse({
        objectId: 'a'.repeat(32), ownerNodeId: 'node-1', purpose: 'PAYLOAD_PURPOSE_RESULT',
        ciphertextBytes: ciphertext.length, sha256: Buffer.from(sha256Bytes(ciphertext)).toString('base64'),
        expiresAtUnixMs: 1_700_000_060_000,
      })
    },
  })
  const object = await relay.finish({ objectId: 'a'.repeat(32), ciphertext })
  assert.equal(calls[0].url, `${ORIGIN}/v1/relay/objects/${'a'.repeat(32)}/finish`)
  const body = JSON.parse(calls[0].options.body)
  assert.equal(body.relay_version, 4)
  assert.equal(Number(body.ciphertext_bytes), ciphertext.length)
  assert.equal(verify(
    relaySignedText('finish', {
      relayVersion: 4, objectId: 'a'.repeat(32), ciphertextBytes: ciphertext.length,
      sha256Hex: hex(sha256Bytes(ciphertext)), signedAt: 1_700_000_000_000,
    }),
    new Uint8Array(Buffer.from(body.signature, 'base64')),
  ), true)
  assert.equal(object.object_id, 'a'.repeat(32))
})

test('fetch verifies exact length and SHA-256; a truncated, oversized or corrupt body is terminal', async () => {
  const reference = {
    object_id: 'a'.repeat(32),
    owner_node_id: 'node-1',
    ciphertext_bytes: ciphertext.length,
    sha256: sha256Bytes(ciphertext),
    expires_at_unix_ms: 1_700_000_060_000,
    purpose: 'PAYLOAD_PURPOSE_EVENT',
  }
  const get = (body, headers = {}) => client({
    fetch: async () => new Response(body, {
      status: 200,
      headers: { 'content-length': String(body.length), ...headers },
    }),
  })
  const body = await get(ciphertext).fetch({ reference, purpose: 'PAYLOAD_PURPOSE_EVENT' })
  assert.deepEqual(Buffer.from(body), Buffer.from(ciphertext))

  await assert.rejects(
    get(ciphertext.subarray(1)).fetch({ reference, purpose: 'PAYLOAD_PURPOSE_EVENT' }),
    (error) => ['invalid_argument', 'payload_digest_mismatch'].includes(error.code),
  )
  const corrupted = Uint8Array.from(ciphertext)
  corrupted[0] ^= 0xff
  await assert.rejects(
    get(corrupted).fetch({ reference, purpose: 'PAYLOAD_PURPOSE_EVENT' }),
    (error) => error.code === 'payload_digest_mismatch',
  )
  await assert.rejects(
    get(ciphertext).fetch({ reference, purpose: 'PAYLOAD_PURPOSE_RESULT' }),
    (error) => error.code === 'forbidden',
  )
})

test('wrong-owner, expired, quota and storage pressure responses are typed failures', async () => {
  const cases = [
    [409, 'conflict', 'wrong_owner_path'],
    [410, 'expired', ''],
    [429, 'rate_limited', 'quota_exceeded'],
    [503, 'unavailable', 'storage_pressure'],
  ]
  for (const [status, code, reason] of cases) {
    const relay = client({
      fetch: async () => jsonResponse({ code, reason }, status),
    })
    await assert.rejects(
      relay.reserve({
        deviceId: 'device-1', purpose: 'PAYLOAD_PURPOSE_RESULT', ciphertext,
        expiresAt: 1_700_000_060_000,
      }),
      (error) => error.code === code && (reason === '' || error.reason === reason),
      `${status} maps to ${code}`,
    )
  }
})

test('diagnostics never contain ciphertext, filenames or signing material', async () => {
  const records = []
  const logger = { debug: (event, fields) => records.push({ event, ...fields }), info: () => {}, warn: () => {} }
  const relay = client({
    logger,
    fetch: async (url, options) => {
      if (options.method === 'POST' && url.endsWith('/v1/relay/objects')) {
        return jsonResponse({
          objectId: 'a'.repeat(32), ownerNodeId: 'node-1',
          uploadPath: `/v1/relay/objects/${'a'.repeat(32)}`,
          maxBytes: ciphertext.length, expiresAtUnixMs: 1_700_000_060_000, uploadTimeoutMs: 1000,
        })
      }
      if (options.method === 'PUT') return new Response(null, { status: 204 })
      return jsonResponse({
        objectId: 'a'.repeat(32), purpose: 'PAYLOAD_PURPOSE_RESULT',
        ciphertextBytes: ciphertext.length, sha256: Buffer.from(sha256Bytes(ciphertext)).toString('base64'),
      })
    },
  })
  const reservation = await relay.reserve({
    deviceId: 'device-1', purpose: 'PAYLOAD_PURPOSE_RESULT', ciphertext,
    expiresAt: 1_700_000_060_000,
  })
  await relay.upload({ reservation, ciphertext })
  await relay.finish({ objectId: reservation.object_id, ciphertext })
  const text = JSON.stringify(records)
  assert.equal(text.includes(Buffer.from(ciphertext).toString('base64')), false)
  assert.equal(text.includes(hex(ciphertext)), false)
  assert.equal(text.includes(Buffer.from(privateKey).toString('base64')), false)
})

test('validatePayloadReference rejects expired, malformed and oversized references', () => {
  const good = {
    object_id: 'a'.repeat(32), owner_node_id: 'node-1', ciphertext_bytes: 10,
    sha256: new Uint8Array(32), expires_at_unix_ms: 2_000, purpose: 'PAYLOAD_PURPOSE_REQUEST',
  }
  assert.doesNotThrow(() => validatePayloadReference(good, { envelopeBytes: 100, now: () => 1000 }))
  assert.throws(() => validatePayloadReference(good, { envelopeBytes: 9, now: () => 1000 }), (error) => error.code === 'payload_too_large')
  assert.throws(() => validatePayloadReference({ ...good, object_id: 'bad' }, { now: () => 1000 }), (error) => error.code === 'invalid_argument')
  assert.throws(() => validatePayloadReference({ ...good, sha256: new Uint8Array(1) }, { now: () => 1000 }), (error) => error.code === 'invalid_argument')
  assert.throws(() => validatePayloadReference(good, { now: () => 3000 }), (error) => error.code === 'expired')
  assert.throws(
    () => validatePayloadReference(good, { now: () => 1000, purpose: 'PAYLOAD_PURPOSE_RESULT' }),
    (error) => error.code === 'forbidden',
  )
})

test('the relay client validates its configured origin, identity and transport before any request', () => {
  for (const options of [
    { origin: 'not a url', hostId: HOST, privateKey },
    { origin: 'ftp://access.example', hostId: HOST, privateKey },
    { origin: 'https://user:pass@access.example', hostId: HOST, privateKey },
    { origin: 'https://access.example/?q=1', hostId: HOST, privateKey },
    { origin: ORIGIN, hostId: '', privateKey },
    { origin: ORIGIN, hostId: HOST, privateKey: new Uint8Array(1) },
    { origin: ORIGIN, hostId: HOST, privateKey, fetch: null },
  ]) {
    assert.throws(() => new RelayObjectClient({ codec: proto, fetch: async () => new Response('{}'), ...options }), (error) => error.code === 'invalid_argument' || error.code === 'unavailable')
  }
  assert.throws(
    () => relaySignedText('other', {}),
    (error) => error.code === 'invalid_argument',
  )
})

test('an absolute upload path must stay on the configured origin', async () => {
  const calls = []
  const relay = client({
    fetch: async (url, options) => {
      calls.push({ url, options })
      return new Response(null, { status: 204 })
    },
  })
  const reservation = {
    object_id: 'a'.repeat(32), owner_node_id: 'node-1',
    max_bytes: ciphertext.length, expires_at_unix_ms: 1_700_000_060_000, upload_timeout_ms: 1000,
  }
  await relay.upload({ reservation: { ...reservation, upload_path: `${ORIGIN}/v1/relay/objects/${reservation.object_id}` }, ciphertext })
  assert.equal(calls[0].url, `${ORIGIN}/v1/relay/objects/${reservation.object_id}`)
  await assert.rejects(
    relay.upload({ reservation: { ...reservation, upload_path: 'https://evil.example/v1/relay/objects/x' }, ciphertext }),
    (error) => error.code === 'forbidden',
  )
})

test('malformed reserve/finish responses and length mismatches fail explicitly', async () => {
  const invalidCiphertext = client({ fetch: async () => jsonResponse({}) })
  await assert.rejects(
    invalidCiphertext.reserve({ deviceId: 'd', purpose: 'PAYLOAD_PURPOSE_RESULT', ciphertext: new Uint8Array(0), expiresAt: 1_700_000_060_000 }),
    (error) => error.code === 'invalid_argument',
  )

  const nonJson = client({ fetch: async () => new Response('not json', { status: 200 }) })
  await assert.rejects(
    nonJson.reserve({ deviceId: 'd', purpose: 'PAYLOAD_PURPOSE_RESULT', ciphertext, expiresAt: 1_700_000_060_000 }),
    (error) => error.code === 'protocol_version_incompatible',
  )

  const noRouting = client({ fetch: async () => jsonResponse({ objectId: 'bad' }) })
  await assert.rejects(
    noRouting.reserve({ deviceId: 'd', purpose: 'PAYLOAD_PURPOSE_RESULT', ciphertext, expiresAt: 1_700_000_060_000 }),
    (error) => error.code === 'protocol_version_incompatible',
  )

  const wrongLength = client({ fetch: async () => new Response(null, { status: 204 }) })
  await assert.rejects(
    wrongLength.upload({
      reservation: {
        object_id: 'a'.repeat(32), owner_node_id: 'node-1', max_bytes: ciphertext.length + 1,
        expires_at_unix_ms: 1_700_000_060_000, upload_timeout_ms: 1000,
      },
      ciphertext,
    }),
    (error) => error.code === 'invalid_argument',
  )

  const wrongObject = client({
    fetch: async () => jsonResponse({ objectId: 'b'.repeat(32), ciphertextBytes: ciphertext.length }),
  })
  await assert.rejects(
    wrongObject.finish({ objectId: 'a'.repeat(32), ciphertext }),
    (error) => error.code === 'protocol_version_incompatible',
  )
})

test('delete is bounded and an unmapped HTTP status becomes unavailable', async () => {
  const badDelete = client({ fetch: async () => new Response('not json', { status: 200 }) })
  assert.deepEqual(await badDelete.delete({ objectId: 'a'.repeat(32) }), { cancelled: false })

  const unmapped = client({ fetch: async () => jsonResponse({}, 418) })
  await assert.rejects(
    unmapped.delete({ objectId: 'a'.repeat(32) }),
    (error) => error.code === 'unavailable',
  )
})

test('fetch rejects a reference that is already expired before making a request', async () => {
  let calls = 0
  const relay = client({ fetch: async () => { calls++; return new Response(ciphertext) } })
  const reference = {
    object_id: 'a'.repeat(32), owner_node_id: 'node-1', ciphertext_bytes: ciphertext.length,
    sha256: sha256Bytes(ciphertext), expires_at_unix_ms: 1_699_999_999_999,
    purpose: 'PAYLOAD_PURPOSE_REQUEST',
  }
  await assert.rejects(relay.fetch({ reference }), (error) => error.code === 'expired')
  assert.equal(calls, 0)
})

test('malformed reference metadata is rejected before any fetch', () => {
  const good = {
    object_id: 'a'.repeat(32), owner_node_id: 'node-1', ciphertext_bytes: 10,
    sha256: new Uint8Array(32), expires_at_unix_ms: 2_000, purpose: 'PAYLOAD_PURPOSE_REQUEST',
  }
  for (const bad of [
    null,
    { ...good, owner_node_id: '' },
    { ...good, ciphertext_bytes: 0 },
    { ...good, expires_at_unix_ms: 0 },
    { ...good, object_id: 'short' },
  ]) {
    assert.throws(() => validatePayloadReference(bad, { now: () => 1000 }), (error) => error.code === 'invalid_argument')
  }
})

test('invalid upload routing, invalid JSON values and failed streams are typed', async () => {
  const relay = client({ fetch: async () => new Response(null, { status: 204 }) })
  const reservation = {
    object_id: 'a'.repeat(32), owner_node_id: 'node-1', max_bytes: ciphertext.length,
    expires_at_unix_ms: 1_700_000_060_000, upload_timeout_ms: 1000,
  }
  await assert.rejects(
    relay.upload({ reservation: { ...reservation, upload_path: 'https://[' }, ciphertext }),
    (error) => error.code === 'invalid_argument',
  )

  const invalidValue = client({
    fetch: async () => jsonResponse({ objectId: 'a'.repeat(32), maxBytes: 'not-a-number' }),
  })
  await assert.rejects(
    invalidValue.reserve({ deviceId: 'd', purpose: 'PAYLOAD_PURPOSE_RESULT', ciphertext, expiresAt: 1_700_000_060_000 }),
    (error) => error.code === 'protocol_version_incompatible',
  )

  const plainError = client({ fetch: async () => new Response('not json', { status: 500 }) })
  await assert.rejects(
    plainError.delete({ objectId: 'a'.repeat(32) }),
    (error) => error.code === 'unavailable',
  )
})

test('a stream failure or a body without a length header is never a complete object', async () => {
  const reference = {
    object_id: 'a'.repeat(32), owner_node_id: 'node-1', ciphertext_bytes: ciphertext.length,
    sha256: sha256Bytes(ciphertext), expires_at_unix_ms: 1_700_000_060_000,
    purpose: 'PAYLOAD_PURPOSE_REQUEST',
  }
  const streamFailure = client({
    fetch: async () => ({
      status: 200, redirected: false,
      headers: { get: () => String(ciphertext.length) },
      arrayBuffer: async () => { throw new Error('stream reset') },
    }),
  })
  await assert.rejects(streamFailure.fetch({ reference }), (error) => error.code === 'unavailable')

  const shortBody = client({
    fetch: async () => ({
      status: 200, redirected: false,
      headers: { get: () => null },
      arrayBuffer: async () => ciphertext.subarray(1),
    }),
  })
  await assert.rejects(shortBody.fetch({ reference }), (error) => error.code === 'invalid_argument')
})
