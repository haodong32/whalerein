import { MemoryHostCredentials } from '../src/security/credentials.js'
import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { HostRuntime } from '../src/host.js'
import { StateStore } from '../src/state/store.js'
import { TestSource } from './support/source.js'
import { codec } from '../src/protocol.js'
import { testConfig } from './support/config.js'
import { silentLogger } from '../src/log.js'
import { generateKeypair, open, requestAad, seal } from '../src/security/noise.js'
import { sha256Bytes } from '../src/security/digest.js'
import { establishChannel } from './support/channel.js'

const proto = codec()

class FakeHostAccess {
  constructor() {
    this.connected = true
    this.handlers = null
    this.results = []
    this.failPublish = false
  }

  start(handlers) {
    this.handlers = handlers
    handlers.onConnected?.()
    return this
  }

  async stop() {
    this.connected = false
  }

  async publishResult(payload) {
    if (this.failPublish) throw new Error('offline')
    this.results.push(proto.decode('whalerein.v1.HostResult', payload))
  }

  async publishEvent() {}

  describe() {
    return { connected: this.connected, reconnects: 0 }
  }

  lastResult() {
    return this.results[this.results.length - 1]
  }
}

function setup(logger = silentLogger) {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-router-'))
  const config = testConfig({
    adapter: 'fake',
    mqtt_url: 'mqtts://broker.test',
    host_id: 'host-1',
    host_token: 'token',
    state_dir: dir,
    event_batch_ms: 0,
  }, {})
  const store = new StateStore({ path: ':memory:' })
  const adapter = new TestSource({ sessions: [{ sessionId: 's1', running: true }] })
  const access = new FakeHostAccess()
  const host = new HostRuntime({ credentials: new MemoryHostCredentials(), config, adapter, store, access, logger, codec: proto })
  return { dir, config, store, adapter, access, host }
}

async function send(access, request) {
  await access.handlers.onRequest(request)
  return access.lastResult()
}

function baseRequest(overrides = {}) {
  return {
    request_id: 'r1',
    host_id: 'host-1',
    device_id: 'd1',
    kind: 'REQUEST_KIND_QUERY',
    protocol_version: 1,
    relay_transport_version: 2,
    expires_at_unix_ms: Date.now() + 60_000,
    channel_id: 'channel-1',
    body: new Uint8Array(0),
    ...overrides,
  }
}

function encryptedQuery({ security, store, channel, payload, overrides = {}, kind = 'REQUEST_KIND_QUERY', commandId = '' }) {
  const requestId = overrides.request_id ?? `r-${Math.random()}`
  const expiresAt = overrides.expires_at_unix_ms ?? Date.now() + 60_000
  const channelId = channel.channelId
  const aad = requestAad({
    requestId,
    hostId: 'host-1',
    deviceId: channel.deviceId,
    kind,
    commandId,
    expiresAt,
  })
  const plaintext = proto.encode('whalerein.v1.RequestPayload', payload)
  const sealed = seal(channel.c2h, aad, plaintext)
  const frame = proto.encode('whalerein.v1.EncryptedFrame', {
    channel_id: channelId,
    nonce: sealed.nonce,
    ciphertext: sealed.ciphertext,
  })
  return {
    request_id: requestId,
    host_id: 'host-1',
    device_id: channel.deviceId,
    kind,
    command_id: commandId,
    payload_digest: commandId === '' ? new Uint8Array(0) : sha256Bytes(frame),
    expires_at_unix_ms: expiresAt,
    protocol_version: 1,
    channel_id: channelId,
    body: frame,
  }
}

test('when the host router receives an invalid envelope, it rejects it before calling the Source', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    assert.equal(ctx.host.started, true)
    let result = await send(ctx.access, baseRequest({ kind: 'REQUEST_KIND_UNSPECIFIED', request_id: 'r-unsupported' }))
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'invalid_argument')

    const before = ctx.access.results.length
    await ctx.access.handlers.onRequest({ ...baseRequest({ request_id: '' }) })
    assert.equal(ctx.access.results.length, before)

    result = await send(ctx.access, baseRequest({ request_id: 'r-host', host_id: 'other' }))
    assert.equal(result.error_code, 'forbidden')

    result = await send(ctx.access, baseRequest({ request_id: 'r-version', protocol_version: 2 }))
    assert.equal(result.error_code, 'protocol_version_incompatible')

    result = await send(ctx.access, baseRequest({ request_id: 'r-expired', expires_at_unix_ms: 1 }))
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'expired')

    result = await send(ctx.access, baseRequest({ request_id: 'r-nochannel', channel_id: 'nope' }))
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'unauthorized')

    result = await send(ctx.access, baseRequest({ request_id: 'r-pairing', kind: 'REQUEST_KIND_PAIRING_CLAIM', body: new Uint8Array([1, 2]) }))
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'invalid_argument')

    ctx.host.security.setRevocationsPending(false)
    const badClaim = proto.encode('whalerein.v1.PairingClaim', {
      claim_id: 'claim-bad',
      token_hash: 'zz',
      phase: 'PAIRING_PHASE_HELLO',
      noise_message: new Uint8Array([1]),
      device_id: 'd1',
    })
    result = await send(ctx.access, baseRequest({ request_id: 'r-pairing-2', kind: 'REQUEST_KIND_PAIRING_CLAIM', body: badClaim }))
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'invalid_argument')

    result = await send(ctx.access, baseRequest({ request_id: 'r-handshake', kind: 'REQUEST_KIND_HANDSHAKE', body: new Uint8Array([1, 2, 3]) }))
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'unauthorized')

    const authorizeBody = proto.encode('whalerein.v1.PairingClaim', {
      claim_id: 'c',
      phase: 'PAIRING_PHASE_HELLO',
      device_id: 'd1',
    })
    result = await send(ctx.access, baseRequest({
      request_id: 'r-authorize',
      kind: 'REQUEST_KIND_PAIRING_AUTHORIZE',
      body: authorizeBody,
    }))
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'invalid_argument')

    const countBeforeFail = ctx.access.results.length
    ctx.access.failPublish = true
    await ctx.access.handlers.onRequest(baseRequest({ request_id: 'r-fail-publish', host_id: 'other' }))
    assert.equal(ctx.access.results.length, countBeforeFail)
    ctx.access.failPublish = false

    const diagnostics = ctx.host.diagnostics()
    assert.equal(diagnostics.commands.unknown, 0)
    ctx.store.listSessionCursors = () => { throw new Error('db') }
    ctx.store.countCommands = () => { throw new Error('db') }
    assert.equal(ctx.host.diagnostics().cursors.sessions, 0)
  } finally {
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('when the host router receives an encrypted query, it maps adapter errors and seals the response', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    const request = encryptedQuery({ security: ctx.host.security, store: ctx.store, channel, payload: { list_sessions: {} } })
    await ctx.access.handlers.onRequest(request)
    const result = ctx.access.lastResult()
    assert.equal(result.status, 'succeeded')
    const aad = requestAad({
      requestId: request.request_id,
      hostId: 'host-1',
      deviceId: 'd1',
      kind: request.kind,
      commandId: '',
      expiresAt: request.expires_at_unix_ms,
    })
    const frame = proto.decode('whalerein.v1.EncryptedFrame', result.result_body)
    const plaintext = open(channel.h2c, aad, frame.nonce, frame.ciphertext)
    const response = proto.decode('whalerein.v1.ResponsePayload', plaintext)
    assert.equal(response.session_list.sessions.length, 1)

    ctx.adapter.failNext('listSessions', 'unavailable')
    await ctx.access.handlers.onRequest(encryptedQuery({ security: ctx.host.security, store: ctx.store, channel, payload: { list_sessions: {} } }))
    assert.equal(ctx.access.lastResult().status, 'failed')
    assert.equal(ctx.access.lastResult().error_code, 'unavailable')

    // Valid frame/AAD but undecodable RequestPayload.
    const invalid = encryptedQuery({ security: ctx.host.security, store: ctx.store, channel, payload: { ping: {} } })
    const invalidFrame = proto.decode('whalerein.v1.EncryptedFrame', invalid.body)
    void invalidFrame
    await ctx.access.handlers.onRequest({ ...invalid, body: new Uint8Array([9, 9, 9]) })
    assert.equal(ctx.access.lastResult().status, 'rejected')

    const mismatch = encryptedQuery({
      security: ctx.host.security,
      store: ctx.store,
      channel,
      payload: { interrupt: { session_id: 's1' } },
      kind: 'REQUEST_KIND_COMMAND',
      commandId: 'cmd-1',
    })
    mismatch.payload_digest = sha256Bytes(new Uint8Array([0]))
    await ctx.access.handlers.onRequest(mismatch)
    assert.equal(ctx.access.lastResult().status, 'rejected')
    assert.equal(ctx.access.lastResult().error_code, 'payload_digest_mismatch')
  } finally {
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('when the host router receives a revocation notice, it applies and acknowledges it', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.store.upsertPeerKey({ deviceId: 'd1', publicKey: generateKeypair().publicKey, fingerprint: 'f' })
    const request = baseRequest({
      request_id: 'rev-1',
      kind: 'REQUEST_KIND_REVOCATION',
      body: proto.encode('whalerein.v1.RevocationNotice', {
        replay_id: 'replay-1',
        device_ids: ['d1'],
        final: true,
        revoked_at_unix_ms: 5,
      }),
    })
    await ctx.access.handlers.onRequest(request)
    const result = ctx.access.lastResult()
    assert.equal(result.status, 'succeeded')
    const ack = proto.decode('whalerein.v1.RevocationAck', result.result_body)
    assert.deepEqual(ack.device_ids, ['d1'])
    assert.equal(ctx.store.getPeerKey('d1'), null)
    assert.equal(ctx.host.security.revocationsPending, false)

    const invalid = await send(ctx.access, baseRequest({ request_id: 'rev-2', kind: 'REQUEST_KIND_REVOCATION', body: new Uint8Array([1]) }))
    assert.equal(invalid.status, 'rejected')
    assert.equal(invalid.error_code, 'invalid_argument')
  } finally {
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('when adapter or Access teardown fails during stop, the host contains the failure', async () => {
  const ctx = setup()
  await ctx.host.start()
  ctx.access.stop = async () => { throw new Error('access stop failed') }
  ctx.adapter.close = async () => { throw new Error('adapter close failed') }
  await ctx.host.stop()
  assert.equal(ctx.host.started, false)
  assert.equal(ctx.host.diagnostics().access.connected, false)
  ctx.store.close()
  rmSync(ctx.dir, { recursive: true, force: true })
})

test('when host startup fails, resources are released and repeated stop calls are safe', async () => {
  const ctx = setup()
  const broken = {
    async open() { throw new Error('source down') },
    async close() {},
  }
  const host = new HostRuntime({ credentials: new MemoryHostCredentials(),
    config: ctx.config,
    adapter: broken,
    store: ctx.store,
    access: ctx.access,
    logger: silentLogger,
    codec: proto,
  })
  await assert.rejects(host.start(), (error) => error.code === 'internal')
  assert.equal(host.started, false)
  await host.stop()
  await host.stop()
  ctx.store.close()
  rmSync(ctx.dir, { recursive: true, force: true })
})

test('when host configuration changes on reload, overrides apply and injected Access reconnects', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    await ctx.host.reload({ eventBatchMs: 1 })
    assert.equal(ctx.host.config.eventBatchMs, 1)
    assert.equal(ctx.host.started, true)
    assert.equal(ctx.access.handlers !== null, true)
  } finally {
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('when an envelope references a missing channel, rejection logs retain trace correlation and channel_missing without copying its payload', async () => {
  const records = []
  const { createLogger } = await import('../src/log.js')
  const logger = createLogger({ salt: 'aa'.repeat(16), sink: (record) => records.push(record) })
  const ctx = setup(logger)
  const host = ctx.host
  try {
    await host.start()
    const privateBody = Buffer.from('synthetic-private-request-body')
    const result = await send(ctx.access, baseRequest({ trace_id: 'operation-123', body: privateBody }))
    assert.equal(result.trace_id, 'operation-123')
    const rejected = records.find((record) => record.event === 'request_frame_rejected')
    assert.equal(rejected.trace_id, 'operation-123')
    assert.equal(rejected.scope, 'channel')
    assert.equal(rejected.reason, 'channel_missing')
    assert.equal(records.find((record) => record.event === 'host_result').trace_id, rejected.trace_id)
    assert.equal(JSON.stringify(records).includes(privateBody.toString()), false)
    assert.equal(JSON.stringify(records).includes(privateBody.toString('base64')), false)
  } finally {
    await host.stop()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})
