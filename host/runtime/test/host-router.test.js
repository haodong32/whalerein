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

  // Mirrors the real transport: an inline result that exceeds the receiver
  // budget is published as a reference, never truncated and never failed.
  async publishResult(payload, options = {}) {
    if (this.failPublish) throw new Error('offline')
    const limit = Number.isSafeInteger(options?.envelopeLimit) && options.envelopeLimit > 0
      ? options.envelopeLimit
      : 16 * 1024 * 1024
    const result = proto.decode('whalerein.v1.HostResult', payload)
    if (payload.length > limit && result.result_body?.length) {
      result.result_reference = {
        object_id: 'object-1', owner_node_id: 'node-1',
        ciphertext_bytes: result.result_body.length, sha256: new Uint8Array(32),
        expires_at_unix_ms: Date.now() + 60_000, purpose: 'PAYLOAD_PURPOSE_RESULT',
      }
      result.result_body = new Uint8Array(0)
    }
    this.results.push(result)
  }

  async publishEvent() {}

  describe() {
    return { connected: this.connected, reconnects: 0 }
  }

  lastResult() {
    return this.results[this.results.length - 1]
  }
}

function setup(logger = silentLogger, now = Date.now) {
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
  const host = new HostRuntime({ credentials: new MemoryHostCredentials(), config, adapter, store, access, logger, now, codec: proto })
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
    relay_version: 4,
    expires_at_unix_ms: Date.now() + 60_000,
    channel_id: 'channel-1',
    body: new Uint8Array(0),
    ...overrides,
  }
}

function encryptedQuery({ security, store, channel, payload, plaintext: rawPayload, overrides = {}, kind = 'REQUEST_KIND_QUERY', commandId = '' }) {
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
  const plaintext = rawPayload ?? proto.encode('whalerein.v1.RequestPayload', payload)
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
    relay_version: 4,
    channel_id: channelId,
    body: frame,
    ...overrides,
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

test('the Host reports observation, not a cached channel, for an internal client read', async (t) => {
  const ctx = setup()
  t.after(async () => {
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  })
  await ctx.host.start()
  const read = (overrides = {}) => send(ctx.access, baseRequest({
    kind: 'REQUEST_KIND_HOST_CLIENTS', device_id: '', channel_id: '', ...overrides,
  }))
  assert.equal((await read({ device_id: 'd1' })).error_code, 'forbidden')
  assert.equal((await read({ host_id: '' })).error_code, 'forbidden')
  assert.equal((await read({ relay_version: 2 })).error_code, 'protocol_version_incompatible')
  assert.equal((await read()).error_code, 'unavailable')
  ctx.host.security.setRevocationsPending(false)
  const channel = establishChannel({ hostId: 'host-1', deviceId: 'd1', security: ctx.host.security, store: ctx.store })
  assert.deepEqual(ctx.host.clientDevices().map(({ deviceId, connected }) => [deviceId, connected]), [['d1', false]])
  const watch = await ctx.host.events.watch('d1', channel.channelId, 's1', undefined, { conversation: true, questions: false })
  const active = await read()
  assert.equal(active.status, 'succeeded')
  assert.deepEqual(proto.decode('whalerein.v1.HostClients', active.result_body).observing_device_ids, ['d1'])
  assert.deepEqual(ctx.host.clientDevices().map(({ deviceId, connected }) => [deviceId, connected]), [['d1', true]])
  ctx.host.events.unwatch('d1', channel.channelId, watch.watch_id)
  const ended = await read()
  assert.equal(ended.status, 'succeeded')
  assert.deepEqual(proto.decode('whalerein.v1.HostClients', ended.result_body ?? new Uint8Array()).observing_device_ids ?? [], [])
})

test('a revoked Device watch ends before its lease and leaves the other Device observing', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const first = establishChannel({ hostId: 'host-1', deviceId: 'd1', security: ctx.host.security, store: ctx.store })
    const sibling = establishChannel({ hostId: 'host-1', deviceId: 'd1', identity: first.identity, security: ctx.host.security, store: ctx.store })
    const second = establishChannel({ hostId: 'host-1', deviceId: 'd2', security: ctx.host.security, store: ctx.store })
    const watch = await ctx.host.events.watch('d1', first.channelId, 's1', undefined, { conversation: true, questions: false })
    const siblingWatch = await ctx.host.events.watch('d1', sibling.channelId, 's1', undefined, { conversation: true, questions: false })
    await ctx.host.events.watch('d2', second.channelId, 's1', undefined, { conversation: true, questions: false })
    assert.deepEqual(ctx.host.events.observingDeviceIds(), ['d1', 'd2'])

    const revoked = await send(ctx.access, baseRequest({
      request_id: 'rev-watch',
      kind: 'REQUEST_KIND_REVOCATION',
      body: proto.encode('whalerein.v1.RevocationNotice', {
        replay_id: 'replay-watch',
        device_ids: ['d1'],
        final: true,
        revoked_at_unix_ms: 5,
      }),
    }))
    assert.equal(revoked.status, 'succeeded')
    assert.equal(ctx.store.getPeerKey('d1'), null)
    assert.equal(ctx.store.getPeerKey('d2') !== null, true, 'the sibling Device keeps its grant')
    assert.deepEqual(ctx.host.events.observingDeviceIds(), ['d2'], 'revocation ends only the revoked watch')
    await assert.rejects(ctx.host.events.refresh('d1', first.channelId, [watch.watch_id]),
      (error) => error.code === 'expired', 'the revoked watch is already ended')
    await assert.rejects(ctx.host.events.refresh('d1', sibling.channelId, [siblingWatch.watch_id]),
      (error) => error.code === 'expired', 'revocation also ends the sibling watch')
    assert.equal(ctx.host.security.getChannel(sibling.channelId), null)
    await assert.rejects(ctx.host.events.watch('d1', first.channelId, 's1', undefined, { conversation: true, questions: false }),
      (error) => error.code === 'unauthorized', 'a revoked channel cannot observe again')
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

test('model read response enforces the full encrypted HostResult transport budget', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    ctx.adapter.describeHostCapabilities = async () => ({ model_catalog: true })
    let length = 16 * 1024 * 1024 - 1024
    ctx.adapter.listModels = async () => ({ groups: [{ id: 'p', name: 'x'.repeat(length), models: [] }] })
    const query = () => send(ctx.access, encryptedQuery({ security: ctx.host.security, store: ctx.store, channel, payload: { list_models: {} }, overrides: { request_id: 'budget-case' } }))
    const below = await query()
    assert.equal(below.status, 'succeeded')
    const belowBytes = proto.encode('whalerein.v1.HostResult', below).length
    assert.ok(belowBytes < 16 * 1024 * 1024)
    length += 16 * 1024 * 1024 - belowBytes
    const exact = await query()
    assert.equal(exact.status, 'succeeded')
    assert.equal(proto.encode('whalerein.v1.HostResult', exact).length, 16 * 1024 * 1024)
    length++
    const above = await query()
    assert.equal(above.status, 'succeeded')
    assert.ok(above.result_reference, 'an oversized result is delivered by reference')
    assert.equal(above.result_body?.length ?? 0, 0, 'no inline body above the receiver bound')
  } finally {
    await ctx.host.stop()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('empty sealed frames cannot establish query or command success', async () => {
  const ctx=setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel=establishChannel({security:ctx.host.security,store:ctx.store,hostId:'host-1',deviceId:'d1',codec:proto})
    ctx.adapter.sessions.get('s1').sourceCapabilities={pending_queue_read:true,pending_queue_remove:true}
    ctx.adapter.readPendingQueue=async()=>({session_id:'s1',available:true,items:[]})
    let calls=0
    ctx.adapter.execute=async()=>{calls++;return {}}
    ctx.host.security.sealResult=()=>new Uint8Array(0)
    await ctx.access.handlers.onRequest(encryptedQuery({security:ctx.host.security,store:ctx.store,channel,payload:{read_pending_queue:{session_id:'s1'}}}))
    assert.equal(ctx.access.lastResult().status,'failed')
    assert.equal(ctx.access.lastResult().error_code,'internal')
    const request=encryptedQuery({security:ctx.host.security,store:ctx.store,channel,kind:'REQUEST_KIND_COMMAND',commandId:'empty-seal',payload:{update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}})
    await ctx.access.handlers.onRequest(request)
    assert.equal(ctx.access.lastResult().status,'unknown')
    assert.equal(ctx.access.lastResult().error_code,'internal')
    assert.equal(calls,1)
  } finally {await ctx.host.stop();ctx.store.close();rmSync(ctx.dir,{recursive:true,force:true})}
})

test('successful query sealing failures publish a sanitized failure rather than false success', async () => {
  const ctx=setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel=establishChannel({security:ctx.host.security,store:ctx.store,hostId:'host-1',deviceId:'d1',codec:proto})
    ctx.adapter.sessions.get('s1').sourceCapabilities={pending_queue_read:true}
    ctx.adapter.readPendingQueue=async()=>({session_id:'s1',available:true,items:[]})
    ctx.host.security.sealResult=()=>{throw new Error('synthetic sealing failure')}
    for(const payload of [{read_pending_queue:{session_id:'s1'}},{list_sessions:{}},{ping:{}}]) {
      await ctx.access.handlers.onRequest(encryptedQuery({security:ctx.host.security,store:ctx.store,channel,payload}))
      const result=ctx.access.lastResult()
      assert.equal(result.status,'failed')
      assert.equal(result.error_code,'internal')
      assert.equal(result.result_body?.length ?? 0,0)
      assert.equal(JSON.stringify(result).includes('synthetic sealing failure'),false)
    }
  } finally {await ctx.host.stop();ctx.store.close();rmSync(ctx.dir,{recursive:true,force:true})}
})

test('a command response sealing failure reports unknown and replays the durable receipt without re-executing', async () => {
  const ctx=setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel=establishChannel({security:ctx.host.security,store:ctx.store,hostId:'host-1',deviceId:'d1',codec:proto})
    ctx.adapter.sessions.get('s1').sourceCapabilities={pending_queue_remove:true}
    let calls=0
    ctx.adapter.execute=async()=>{calls++;return {}}
    const request=encryptedQuery({security:ctx.host.security,store:ctx.store,channel,kind:'REQUEST_KIND_COMMAND',commandId:'queue-seal',payload:{update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}})
    const original=ctx.host.security.sealResult.bind(ctx.host.security)
    ctx.host.security.sealResult=()=>{throw new Error('synthetic sealing failure')}
    await ctx.access.handlers.onRequest(request)
    let result=ctx.access.lastResult()
    assert.equal(result.status,'unknown')
    assert.equal(result.error_code,'internal')
    assert.equal(result.result_body?.length ?? 0,0)
    assert.equal(JSON.stringify(result).includes('synthetic sealing failure'),false)
    assert.equal(calls,1)
    ctx.host.security.sealResult=original
    await ctx.access.handlers.onRequest(request)
    result=ctx.access.lastResult()
    assert.equal(result.status,'succeeded')
    assert.ok(result.result_body?.length)
    assert.equal(calls,1)
  } finally {await ctx.host.stop();ctx.store.close();rmSync(ctx.dir,{recursive:true,force:true})}
})

test('an oversized complete pending queue is published by reference, never partially', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security:ctx.host.security, store:ctx.store, hostId:'host-1', deviceId:'d1', codec:proto })
    ctx.adapter.sessions.get('s1').sourceCapabilities = {pending_queue_read:true}
    ctx.adapter.readPendingQueue = async () => ({session_id:'s1',available:true,items:[{item_id:'q1',placement:'QUEUED',blocks:[{block_id:'t',text:{text:'x'.repeat(16*1024*1024)}}]}]})
    const request = encryptedQuery({security:ctx.host.security,store:ctx.store,channel,payload:{read_pending_queue:{session_id:'s1'}}})
    await ctx.access.handlers.onRequest(request)
    assert.equal(ctx.access.lastResult().status,'succeeded')
    assert.ok(ctx.access.lastResult().result_reference, 'the complete observation travels by reference')
    assert.equal(ctx.access.lastResult().result_body?.length ?? 0,0)
  } finally {
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir,{recursive:true,force:true})
  }
})

test('pending queue complete encrypted HostResult uses exact transport boundary', async () => {
  const ctx=setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel=establishChannel({security:ctx.host.security,store:ctx.store,hostId:'host-1',deviceId:'d1',codec:proto})
    let textSize=16*1024*1024-4096
    ctx.adapter.sessions.get('s1').sourceCapabilities = {pending_queue_read:true}
    ctx.adapter.readPendingQueue=async()=>({session_id:'s1',available:true,items:[{item_id:'q1',placement:'QUEUED',blocks:[{block_id:'t',text:{text:'x'.repeat(textSize)}}]}]})
    const publish=ctx.access.publishResult.bind(ctx.access)
    let wireSize=0
    ctx.access.publishResult=async bytes=>{wireSize=bytes.length;await publish(bytes)}
    const observe=async()=>{
      const request=encryptedQuery({security:ctx.host.security,store:ctx.store,channel,payload:{read_pending_queue:{session_id:'s1'}},overrides:{request_id:'queue-boundary'}})
      await ctx.access.handlers.onRequest(request)
      return ctx.access.lastResult()
    }
    assert.equal((await observe()).status,'succeeded')
    const limit=16*1024*1024
    const exactText=limit-(wireSize-textSize)
    for(const delta of [-1,0,1]) {
      textSize=exactText+delta
      const result=await observe()
      if(delta<=0) { assert.equal(result.status,'succeeded'); assert.equal(wireSize,limit+delta) }
      else {
        assert.equal(result.status,'succeeded')
        assert.ok(result.result_reference, 'above the receiver bound the result travels by reference')
        assert.equal(result.result_body?.length ?? 0, 0)
      }
    }
  } finally { await ctx.host.stop();ctx.store.close();rmSync(ctx.dir,{recursive:true,force:true}) }
})

test('wait state rejects an oversized encrypted HostResult for a valid request', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({security:ctx.host.security,store:ctx.store,hostId:'host-1',deviceId:'d1',codec:proto})
    const describe = ctx.adapter.describe.bind(ctx.adapter)
    ctx.adapter.describe = () => ({...describe(), capabilities:['wait']})
    ctx.adapter.readWaitState = async () => ({active:true,running:false,queueAvailable:true,pendingInput:[]})
    ctx.adapter.waitSession = async () => ({reason:'idle',state:{active:true,running:false,queueAvailable:true,pendingInput:[]}})
    let wireSize = 0
    const publish = ctx.access.publishResult.bind(ctx.access)
    ctx.access.publishResult = async bytes => { wireSize=bytes.length; await publish(bytes) }
    const query = async (size, waiting) => {
      const payload = waiting ? {wait_session:{session_id:'x'.repeat(size),wait_id:'w',timeout_ms:1000}} : {read_wait_state:{session_id:'x'.repeat(size)}}
      const request=encryptedQuery({security:ctx.host.security,store:ctx.store,channel,payload,overrides:{request_id:'wait-boundary'}})
      const requestSize=proto.encode('whalerein.v1.HostRequest',request).length
      await ctx.access.handlers.onRequest(request)
      return {result:ctx.access.lastResult(),requestSize}
    }
    const limit=16*1024*1024
    for (const waiting of [false, true]) {
      let size=limit-4096
      assert.equal((await query(size, waiting)).result.status,'succeeded')
      const exact=limit-(wireSize-size)
      for (const delta of [-1,0,1]) {
        const observed=await query(exact+delta, waiting)
        assert.ok(observed.requestSize <= limit, `request size ${observed.requestSize}`)
        if (delta <= 0) {
          assert.equal(observed.result.status,'succeeded')
          assert.equal(wireSize,limit+delta)
        } else {
          assert.equal(observed.result.status,'succeeded')
          assert.ok(observed.result.result_reference, 'an oversized result travels by reference')
          assert.equal(observed.result.result_body?.length ?? 0, 0)
        }
      }
      const recovered = await query(2, waiting)
      assert.equal(recovered.result.status, 'succeeded')
      assert.ok(recovered.result.result_body?.length)
    }
  } finally {await ctx.host.stop();ctx.store.close();rmSync(ctx.dir,{recursive:true,force:true})}
})

test('question reads enforce the complete encrypted HostResult budget without leaking content', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    ctx.adapter.getSession = async id => ({ sessionId: id, sourceCapabilities: { questions_read: true } })
    let length = 16 * 1024 * 1024 - 4096
    ctx.adapter.listQuestionPage = async () => ({ batches: [{ session_id: 's1', batch_id: 'b1', questions: [{ id: 'q1', question: 'x'.repeat(length) }] }] })
    ctx.adapter.getQuestion = async () => ({ session_id: 's1', batch_id: 'b1', questions: [{ id: 'q1', question: 'x'.repeat(length) }] })
    let wireSize = 0
    const publish = ctx.access.publishResult.bind(ctx.access)
    ctx.access.publishResult = async bytes => { wireSize = bytes.length; await publish(bytes) }
    for (const payload of [{ list_questions: { session_id: 's1' } }, { get_question: { session_id: 's1', batch_id: 'b1' } }]) {
      length = 16 * 1024 * 1024 - 4096
      let lastRequest
      const query = () => {
        lastRequest = encryptedQuery({ security: ctx.host.security, store: ctx.store, channel, payload, overrides: { request_id: 'question-budget' } })
        return send(ctx.access, lastRequest)
      }
      assert.equal((await query()).status, 'succeeded')
      const exact = 16 * 1024 * 1024 - (wireSize - length)
      for (const offset of [-1, 0, 1]) {
        length = exact + offset
        const result = await query()
        if (offset <= 0) { assert.equal(result.status, 'succeeded'); assert.equal(wireSize, 16 * 1024 * 1024 + offset) }
        else if (payload.list_questions) {
          assert.equal(result.status, 'succeeded')
          const aad = requestAad({ requestId: lastRequest.request_id, hostId: 'host-1', deviceId: 'd1',
            kind: lastRequest.kind, commandId: '', expiresAt: lastRequest.expires_at_unix_ms })
          const frame = proto.decode('whalerein.v1.EncryptedFrame', result.result_body)
          const response = proto.decode('whalerein.v1.ResponsePayload', open(channel.h2c, aad, frame.nonce, frame.ciphertext))
          assert.equal(response.question_page.batches[0].content_unavailable, true)
          assert.equal(response.question_page.batches[0].questions.length, 0)
        } else {
          assert.equal(result.status, 'succeeded')
          assert.ok(result.result_reference, 'an oversized question read travels by reference')
          assert.equal(result.result_body?.length ?? 0, 0)
        }
      }
    }
  } finally { await ctx.host.stop(); ctx.store.close(); rmSync(ctx.dir, { recursive: true, force: true }) }
})

test('question pages use authenticated device cursors that resume across a source change', async () => {
  const ctx = setup()
  try {
    await ctx.host.start(); ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    ctx.adapter.getSession = async id => ({ sessionId: id, sourceCapabilities: { questions_read: true } })
    let revision = 1
    const huge = 'x'.repeat(9 * 1024 * 1024)
    ctx.adapter.listQuestionPage = async () => ({ batches: ['one', 'two'].map(id => ({ session_id: 's1', batch_id: id,
      source_epoch: 'epoch', version: revision, digest: new Uint8Array(32), phase: 'QUESTION_OPEN',
      questions: [{ id: 'q', question: huge }] })) })
    const query = async cursor => {
      const request = encryptedQuery({ security: ctx.host.security, store: ctx.store, channel,
        payload: { list_questions: { session_id: 's1', cursor } } })
      const result = await send(ctx.access, request)
      if (result.status !== 'succeeded') return { result }
      const aad = requestAad({ requestId: request.request_id, hostId: 'host-1', deviceId: 'd1', kind: request.kind,
        commandId: '', expiresAt: request.expires_at_unix_ms })
      const frame = proto.decode('whalerein.v1.EncryptedFrame', result.result_body)
      return { result, page: proto.decode('whalerein.v1.ResponsePayload', open(channel.h2c, aad, frame.nonce, frame.ciphertext)).question_page }
    }
    const first = (await query('')).page
    assert.deepEqual(first.batches.map(batch => batch.batch_id), ['one'])
    assert.ok(first.next_cursor)
    const second = (await query(first.next_cursor)).page
    assert.deepEqual(second.batches.map(batch => batch.batch_id), ['two'])
    assert.equal(second.next_cursor ?? '', '')
    revision++
    // The source changed between pages (a batch was added). The authenticated
    // cursor still resumes at the recorded successor instead of conflicting.
    const changed = id => ({ session_id: 's1', batch_id: id, source_epoch: 'epoch', version: revision,
      digest: new Uint8Array(32), phase: 'QUESTION_OPEN', questions: [{ id: 'q', question: huge }] })
    ctx.adapter.listQuestionPage = async () => ({ batches: [changed('one'), changed('two'), changed('three')] })
    const resumed = await query(first.next_cursor)
    assert.equal(resumed.result.status, 'succeeded')
    assert.deepEqual(resumed.page.batches.map(batch => batch.batch_id), ['two'])
    assert.equal((await query(first.next_cursor + 'x')).result.error_code, 'conflict')
  } finally { await ctx.host.stop(); ctx.store.close(); rmSync(ctx.dir, { recursive: true, force: true }) }
})


test('an authenticated delivery with an omitted envelope host is scoped to the configured Host', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto})
    let identity
    ctx.adapter.execute = async (_session, _command, context) => {
      identity = context.operationId
      return {command_accepted: {delivery_id: identity}}
    }
    const request = encryptedQuery({security: ctx.host.security, store: ctx.store, channel,
      payload: {send_message: {session_id: 's1', text: 'fixture'}}, kind: 'REQUEST_KIND_COMMAND', commandId: 'same-command'})
    request.host_id = ''
    await ctx.access.handlers.onRequest(request)
    assert.equal(ctx.access.lastResult().status, 'succeeded')
    const {sha256Text, hex} = await import('../src/security/digest.js')
    const expected = hex(sha256Text(JSON.stringify(['whalerein/v1/operation', 'host-1', 'd1', 'same-command'])))
    assert.equal(identity, expected)
  } finally {
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, {recursive: true, force: true})
  }
})

test('local stop fences new read and watch admission before Access presence changes', async () => {
  const ctx = setup()
  const gate = Promise.withResolvers()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    const originalStop = ctx.access.stop.bind(ctx.access)
    ctx.access.stop = async () => { await gate.promise; await originalStop() }
    const stopping = ctx.host.stop()
    await new Promise((resolve) => setImmediate(resolve))
    const request = encryptedQuery({ security: ctx.host.security, store: ctx.store, channel, payload: { list_models: {} } })
    const result = await send(ctx.access, request)
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'unavailable')
    gate.resolve()
    await stopping
    assert.equal(ctx.host.clientDevices(), null)
  } finally {
    gate.resolve()
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('local stop fences a command before the Source executes it', async () => {
  const ctx = setup()
  const gate = Promise.withResolvers()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    let executed = false
    ctx.adapter.execute = async () => { executed = true; return { command_accepted: { delivery_id: 'late' } } }
    const originalStop = ctx.access.stop.bind(ctx.access)
    ctx.access.stop = async () => { await gate.promise; await originalStop() }
    const stopping = ctx.host.stop()
    await new Promise((resolve) => setImmediate(resolve))
    const request = encryptedQuery({ security: ctx.host.security, store: ctx.store, channel,
      payload: { send_message: { session_id: 's1', text: 'during stop' } }, kind: 'REQUEST_KIND_COMMAND', commandId: 'stop-fenced-command' })
    const result = await send(ctx.access, request)
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'unavailable')
    assert.equal(executed, false)
    gate.resolve()
    await stopping
  } finally {
    gate.resolve()
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('disconnect releases a stalled model query and discards its late result', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    ctx.adapter.describeHostCapabilities = async () => ({ model_catalog: true })
    let release
    let captured
    ctx.adapter.listModels = ({ signal }) => { captured = signal; return new Promise(resolve => { release = resolve }) }
    const request = encryptedQuery({ security: ctx.host.security, store: ctx.store, channel, payload: { list_models: {} } })
    const pending = ctx.access.handlers.onRequest(request)
    while (!release) await new Promise(resolve => setImmediate(resolve))
    ctx.access.handlers.onDisconnected()
    await pending
    const result = ctx.access.lastResult()
    assert.equal(result.status, 'failed')
    assert.equal(result.error_code, 'unavailable')
    assert.equal(captured.aborted, true)
    release({ groups: [{ id: 'fixture-private-sentinel', models: [] }] })
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(JSON.stringify(ctx.access.lastResult()).includes('fixture-private-sentinel'), false)
  } finally {
    await ctx.host.stop()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})


test('idle channel cleanup preserves a sibling channel with an authenticated request in flight', async (t) => {
  let now = Date.now()
  const ctx = setup(silentLogger, () => now)
  const gate = Promise.withResolvers()
  t.after(async () => {
    gate.resolve({ groups: [] })
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  })
  await ctx.host.start()
  ctx.host.security.setRevocationsPending(false)
  const options = {
    security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', identity: generateKeypair(), codec: proto,
  }
  const idle = establishChannel(options)
  const busy = establishChannel(options)
  for (const { channelId } of [idle, busy]) {
    ctx.store.addOutbox({ deviceId: 'd1', channelId, payload: new Uint8Array([1]) })
  }
  const entered = Promise.withResolvers()
  ctx.adapter.describeHostCapabilities = async () => ({ model_catalog: true })
  ctx.adapter.listModels = () => { entered.resolve(); return gate.promise }
  const request = encryptedQuery({
    ...options, channel: busy, payload: { list_models: {} }, overrides: { expires_at_unix_ms: now + 180_000 },
  })
  const pending = send(ctx.access, request)
  await entered.promise
  now += 120_000
  ctx.host.security.pruneIdleChannels()
  assert.equal(ctx.host.security.getChannel(idle.channelId), null)
  assert.ok(ctx.host.security.getChannel(busy.channelId))
  assert.deepEqual(ctx.store.listOutbox().map(row => row.channelId), [busy.channelId])
  gate.resolve({ groups: [] })
  assert.equal((await pending).status, 'succeeded')
  assert.ok(ctx.host.security.getChannel(busy.channelId))
})

test('device_offline ends the Watch while independent HTTP channels stay usable', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    const sender = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', identity: channel.identity, codec: proto,
    })
    const watch = await ctx.host.events.watch('d1', channel.channelId, 's1', undefined, { conversation: true, questions: false })
    assert.deepEqual(ctx.host.events.observingDeviceIds(), ['d1'])
    ctx.access.handlers.onPresence({ event: 'device_offline', device_id: 'd1', reason: 'will' })
    assert.deepEqual(ctx.host.events.observingDeviceIds(), [])
    await assert.rejects(ctx.host.events.refresh('d1', channel.channelId, [watch.watch_id]), error => error.code === 'expired')
    assert.equal(ctx.host.security.listChannels().length, 2)
    const ping = encryptedQuery({ security: ctx.host.security, store: ctx.store, channel: sender, payload: { ping: {} } })
    assert.equal((await send(ctx.access, ping)).status, 'succeeded')
  } finally {
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})


function startLiveAnswer(ctx, channel, { commandId = 'answer-command', requestId = 'answer-request' } = {}) {
  const gate = Promise.withResolvers()
  let entered = false
  ctx.adapter.getSession = async id => ({ sessionId: id, sourceCapabilities: { questions_answer_continued: true } })
  ctx.adapter.execute = async () => {
    entered = true
    await gate.promise
    return { question_receipt: { batch_id: 'b', phase: 'QUESTION_REPLY_QUEUED' } }
  }
  const request = encryptedQuery({
    security: ctx.host.security, store: ctx.store, channel, kind: 'REQUEST_KIND_COMMAND', commandId,
    payload: { answer_question: { session_id: 's1', batch_id: 'b', expected_phase: 'QUESTION_CONTINUED' } },
    overrides: { request_id: requestId, expires_at_unix_ms: Date.now() + 60_000 },
  })
  const pending = ctx.access.handlers.onRequest(request)
  return { gate, request, pending, entered: () => entered }
}

test('session-observation/AC-25.device-offline preserves an independently live HTTP answer', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    const live = startLiveAnswer(ctx, channel)
    while (!live.entered()) await new Promise(resolve => setImmediate(resolve))
    ctx.access.handlers.onPresence({ event: 'device_offline', device_id: 'd1', reason: 'will' })
    assert.equal(ctx.host.security.listChannels().length, 1)
    live.gate.resolve()
    await live.pending
    assert.equal(ctx.access.lastResult().status, 'succeeded')
    assert.equal(ctx.store.getCommand('d1', live.request.command_id).status, 'succeeded')
  } finally {
    await ctx.host.stop(); ctx.store.close(); rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('session-observation/AC-31.cancellation-before-admission fences the delayed exact request', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    ctx.adapter.getSession = async id => ({ sessionId: id, sourceCapabilities: { questions_answer_continued: true } })
    let calls = 0
    ctx.adapter.execute = async () => { calls++; return { question_receipt: { batch_id: 'b', phase: 'QUESTION_REPLY_QUEUED' } } }
    const request = encryptedQuery({
      security: ctx.host.security, store: ctx.store, channel, kind: 'REQUEST_KIND_COMMAND', commandId: 'fenced-command',
      payload: { answer_question: { session_id: 's1', batch_id: 'b', expected_phase: 'QUESTION_CONTINUED' } },
      overrides: { request_id: 'fenced-request', expires_at_unix_ms: Date.now() + 60_000 },
    })
    const notice = {
      event: 'request_stale', request_id: 'fenced-request', host_id: 'host-1', device_id: 'd1',
      command_id: 'fenced-command', digest: Buffer.from(request.payload_digest).toString('base64'),
      deadline_unix_ms: request.expires_at_unix_ms, reason: 'client_poll_expired',
    }
    ctx.host.notifyPresence(notice)
    ctx.host.notifyPresence(notice)
    const fenced = await send(ctx.access, request)
    assert.equal(fenced.status, 'failed')
    assert.equal(fenced.error_code, 'unavailable')
    assert.equal(calls, 0, 'a delayed fenced request never invokes the source')
    assert.equal(ctx.store.getCommand('d1', 'fenced-command').status, 'failed')
    // A fresh request identity remains independent.
    const fresh = encryptedQuery({
      security: ctx.host.security, store: ctx.store, channel, kind: 'REQUEST_KIND_COMMAND', commandId: 'fresh-command',
      payload: { answer_question: { session_id: 's1', batch_id: 'b', expected_phase: 'QUESTION_CONTINUED' } },
      overrides: { request_id: 'fresh-request', expires_at_unix_ms: Date.now() + 60_000 },
    })
    assert.equal((await send(ctx.access, fresh)).status, 'succeeded')
    assert.equal(calls, 1)
    // Cancellation after an accepted result cannot overwrite it.
    ctx.host.notifyPresence({ ...notice, request_id: 'fresh-request', command_id: 'fresh-command',
      digest: Buffer.from(fresh.payload_digest).toString('base64'), deadline_unix_ms: fresh.expires_at_unix_ms })
    assert.equal(ctx.store.getCommand('d1', 'fresh-command').status, 'succeeded')
  } finally {
    await ctx.host.stop(); ctx.store.close(); rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('session-observation/AC-34.spoofed or foreign cancellation is rejected without cancelling the scope', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    const live = startLiveAnswer(ctx, channel)
    while (!live.entered()) await new Promise(resolve => setImmediate(resolve))
    const valid = {
      event: 'request_stale', request_id: live.request.request_id, host_id: 'host-1', device_id: 'd1',
      command_id: live.request.command_id, digest: Buffer.from(live.request.payload_digest).toString('base64'),
      deadline_unix_ms: live.request.expires_at_unix_ms,
    }
    // Foreign Host, unknown device, mismatched command and mismatched digest
    // are each rejected before the exact scope is touched.
    ctx.host.notifyPresence({ ...valid, host_id: 'other-host' })
    ctx.host.notifyPresence({ ...valid, device_id: 'foreign-device' })
    ctx.host.notifyPresence({ ...valid, command_id: 'other-command' })
    ctx.host.notifyPresence({ ...valid, digest: Buffer.alloc(32).toString('base64') })
    ctx.host.notifyPresence({ ...valid, reason: 'cloud_session_ended', host_id: undefined })
    ctx.host.notifyPresence({ ...valid, reason: 'cloud_session_ended', device_id: undefined })
    live.gate.resolve()
    await live.pending
    assert.equal(ctx.access.lastResult().status, 'succeeded', 'rejected notices never abort the valid answer')
    assert.equal(ctx.store.getCommand('d1', live.request.command_id).status, 'succeeded')
    // The corresponding exact notice remains effective for a new scope.
    const other = startLiveAnswer(ctx, channel, { commandId: 'revoked-command', requestId: 'revoked-request' })
    while (!other.entered()) await new Promise(resolve => setImmediate(resolve))
    ctx.host.notifyPresence({ event: 'request_stale', reason: 'cloud_session_ended', request_id: 'revoked-request', host_id: 'host-1', device_id: 'd1',
      command_id: 'revoked-command', digest: Buffer.from(other.request.payload_digest).toString('base64'),
      deadline_unix_ms: other.request.expires_at_unix_ms })
    assert.equal((await other.pending).status, 'unknown')
    assert.equal(ctx.host.security.listChannels().length, 1, 'session cancellation leaves Device channels open')
  } finally {
    await ctx.host.stop(); ctx.store.close(); rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('an expired duplicate can be rejected after the same command already succeeded', async () => {
  let now = Date.now()
  const ctx = setup(silentLogger, () => now)
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    ctx.adapter.setRunning('s1', false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    const request = encryptedQuery({ security: ctx.host.security, store: ctx.store, channel,
      payload: { send_message: { session_id: 's1', text: 'fixture' } }, kind: 'REQUEST_KIND_COMMAND', commandId: 'stable-command',
      overrides: { request_id: 'stable-request', expires_at_unix_ms: now + 1000 } })
    const completed = await send(ctx.access, request)
    assert.equal(completed.status, 'succeeded')
    assert.equal(ctx.adapter.messages.length, 1)
    now += 2000
    const delayedDuplicate = await send(ctx.access, request)
    assert.equal(delayedDuplicate.status, 'rejected')
    assert.equal(delayedDuplicate.error_code, 'expired')
    assert.equal(ctx.adapter.messages.length, 1)
    assert.equal(ctx.store.getCommand('d1', 'stable-command').status, 'succeeded')
  } finally { await ctx.host.stop(); ctx.store.close(); rmSync(ctx.dir, { recursive: true, force: true }) }
})


test('when a request kind is unknown, the host reports a protocol version mismatch and names the kind', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    const result = await send(ctx.access, baseRequest({ request_id: 'r-kind', kind: 99 }))
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'protocol_version_incompatible')
    assert.match(result.error_message, /unsupported request kind 99/)
    assert.match(result.error_message, /upgrade/)
  } finally { await ctx.host.stop(); ctx.store.close(); rmSync(ctx.dir, { recursive: true, force: true }) }
})

test('when a protocol version is outside the window, the host names the side that must upgrade', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    const newer = await send(ctx.access, baseRequest({ request_id: 'r-newer', protocol_version: 2 }))
    assert.equal(newer.status, 'rejected')
    assert.equal(newer.error_code, 'protocol_version_incompatible')
    assert.match(newer.error_message, /upgrade this client/)

    const missing = await send(ctx.access, baseRequest({ request_id: 'r-missing', protocol_version: 0 }))
    assert.equal(missing.error_code, 'protocol_version_incompatible')
    assert.match(missing.error_message, /upgrade the older peer/)
  } finally { await ctx.host.stop(); ctx.store.close(); rmSync(ctx.dir, { recursive: true, force: true }) }
})

test('when an identifier exceeds the wire bound, the host neither logs nor replies with it', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    const before = ctx.access.results.length
    await ctx.access.handlers.onRequest(baseRequest({ request_id: 'r'.repeat(200) }))
    assert.equal(ctx.access.results.length, before)
    await ctx.access.handlers.onRequest(baseRequest({ request_id: 'r-identity', host_id: 'host|other' }))
    const rejected = ctx.access.lastResult()
    assert.equal(rejected.request_id, 'r-identity')
    assert.equal(rejected.error_code, 'invalid_argument')
  } finally { await ctx.host.stop(); ctx.store.close(); rmSync(ctx.dir, { recursive: true, force: true }) }
})

test('when an encrypted payload carries no known oneof variant, the host rejects it instead of returning an empty success', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    const result = await send(ctx.access, encryptedQuery({ security: ctx.host.security, store: ctx.store, channel, payload: {}, overrides: { request_id: 'r-empty' } }))
    assert.equal(result.status, 'failed')
    assert.equal(result.error_code, 'invalid_argument')
    assert.match(result.error_message, /no known payload variant/)
  } finally { await ctx.host.stop(); ctx.store.close(); rmSync(ctx.dir, { recursive: true, force: true }) }
})

test('when an encrypted payload carries an unknown enum value, the host reports an actionable version mismatch', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    // RequestPayload.send_message is tag 0x32. SendMessage.delivery_mode is tag 3.
    const message = Buffer.concat([
      Buffer.from([0x0a, 0x02]), Buffer.from('s1'),
      Buffer.from([0x12, 0x01]), Buffer.from('x'),
      Buffer.from([0x18, 99]),
    ])
    const payload = Buffer.concat([Buffer.from([0x32, message.length]), message])
    const result = await send(ctx.access, encryptedQuery({ security: ctx.host.security, store: ctx.store, channel, plaintext: payload, overrides: { request_id: 'r-enum' } }))
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'protocol_version_incompatible')
    assert.match(result.error_message, /DeliveryMode/)
  } finally { await ctx.host.stop(); ctx.store.close(); rmSync(ctx.dir, { recursive: true, force: true }) }
})

test('canonical history packing is delivered whole by reference above the receiver bound', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    const page = {
      session: { host_id: 'host-1', session_id: 's1' },
      batch: {
        from_cursor: 'bookmark:a', to_cursor: 'bookmark:b', batch_id: 'batch-1', projection_id: 'projection-1',
        changes: [{ message: { message_id: 'm1', role: 'ASSISTANT', blocks: [{ block_id: 't', text: { text: 'x'.repeat(4096) } }] } }],
      },
      cut_cursor: 'bookmark:b', complete: true, projection_id: 'projection-1', source: 'HISTORY_SOURCE_ONLINE',
    }
    ctx.adapter.readHistory = async () => page
    const query = async (transport, requestId) => {
      const request = encryptedQuery({
        security: ctx.host.security, store: ctx.store, channel,
        payload: { read_history: { session_id: 's1', max_bytes: 4096 } },
        overrides: { request_id: requestId, transport },
      })
      await ctx.access.handlers.onRequest(request)
      return { request, result: ctx.access.lastResult() }
    }
    const bounded = await query({ result_max_bytes: 1024, grant_generation: 3, revocation_generation: 9 }, 'bound-small')
    assert.equal(bounded.result.status, 'succeeded')
    assert.ok(bounded.result.result_reference, 'a page above the receiver bound travels by reference, whole')
    assert.equal(bounded.result.result_body?.length ?? 0, 0, 'no partial page is published inline')
    assert.equal(bounded.request.transport.revocation_generation, 9, 'request metadata is passed through unchanged')

    const retried = await query({ result_max_bytes: 16 * 1024 * 1024, grant_generation: 3, revocation_generation: 9 }, 'bound-large')
    assert.equal(retried.result.status, 'succeeded')
    assert.ok(retried.result.result_body?.length > 0)
    const aad = requestAad({
      requestId: retried.request.request_id, hostId: 'host-1', deviceId: 'd1',
      kind: retried.request.kind, commandId: '', expiresAt: retried.request.expires_at_unix_ms,
    })
    const frame = proto.decode('whalerein.v1.EncryptedFrame', retried.result.result_body)
    const response = proto.decode('whalerein.v1.ResponsePayload', open(channel.h2c, aad, frame.nonce, frame.ciphertext))
    assert.deepEqual(response.history_page.batch.from_cursor, 'bookmark:a')
    assert.deepEqual(response.history_page.batch.to_cursor, 'bookmark:b')
    assert.equal(response.history_page.batch.changes[0].message.blocks[0].text.text.length, 4096,
      'the complete canonical message is preserved')
  } finally {
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('malformed request transport metadata is rejected before the source or encryption runs', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    let reads = 0
    ctx.adapter.readHistory = async () => { reads++; return { session: { session_id: 's1' } } }
    for (const transport of [
      { result_max_bytes: 0, grant_generation: 1, revocation_generation: 0 },
      { result_max_bytes: 1024, grant_generation: -1, revocation_generation: 0 },
      { result_max_bytes: 1024, grant_generation: 1, revocation_generation: -1 },
      { result_max_bytes: 1024, grant_generation: 1, revocation_generation: 'not-a-number' },
    ]) {
      const request = encryptedQuery({
        security: ctx.host.security, store: ctx.store, channel,
        payload: { read_history: { session_id: 's1' } },
        overrides: { request_id: `bad-${Math.random()}`, transport },
      })
      await ctx.access.handlers.onRequest(request)
      const result = ctx.access.lastResult()
      assert.equal(result.status, 'rejected')
      assert.equal(result.error_code, 'invalid_argument')
    }
    assert.equal(reads, 0)
  } finally {
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('typed send replies are scoped to the request and attachment transfers fail explicitly', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    const channel = establishChannel({ security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', codec: proto })
    ctx.adapter.readMessageReply = async (sessionId, submittedId) => ({
      state: 'MESSAGE_REPLY_COMMITTED',
      committed: {
        session: { host_id: 'host-1', session_id: sessionId },
        message_id: 'm-final', source_locator: 'locator-1', projection_id: 'projection-1',
      },
    })
    const read = encryptedQuery({
      security: ctx.host.security, store: ctx.store, channel,
      payload: { read_message_reply: { session_id: 's1', submitted_message_id: 'submitted-1' } },
      overrides: { request_id: 'reply-1' },
    })
    await ctx.access.handlers.onRequest(read)
    const replyResult = ctx.access.lastResult()
    assert.equal(replyResult.status, 'succeeded')
    const aad = requestAad({ requestId: read.request_id, hostId: 'host-1', deviceId: 'd1', kind: read.kind, commandId: '', expiresAt: read.expires_at_unix_ms })
    const frame = proto.decode('whalerein.v1.EncryptedFrame', replyResult.result_body)
    const response = proto.decode('whalerein.v1.ResponsePayload', open(channel.h2c, aad, frame.nonce, frame.ciphertext))
    assert.equal(response.message_reply.session_id, 's1')
    assert.equal(response.message_reply.submitted_message_id, 'submitted-1')
    assert.equal(response.message_reply.state, 'MESSAGE_REPLY_COMMITTED')
    assert.equal(response.message_reply.committed.message_id, 'm-final')

    // The reply scope is the request, not the Source's claim.
    ctx.adapter.readMessageReply = async () => ({
      session_id: 'other-session', submitted_message_id: 'other-message',
      state: 'MESSAGE_REPLY_COMMITTED',
      committed: { session: { host_id: 'host-1', session_id: 'other-session' }, message_id: 'm-other', source_locator: 'locator-2', projection_id: 'projection-1' },
    })
    const second = encryptedQuery({
      security: ctx.host.security, store: ctx.store, channel,
      payload: { read_message_reply: { session_id: 's1', submitted_message_id: 'submitted-2' } },
      overrides: { request_id: 'reply-2' },
    })
    await ctx.access.handlers.onRequest(second)
    const secondResult = ctx.access.lastResult()
    assert.equal(secondResult.status, 'succeeded')
    const secondAad = requestAad({ requestId: second.request_id, hostId: 'host-1', deviceId: 'd1', kind: second.kind, commandId: '', expiresAt: second.expires_at_unix_ms })
    const secondFrame = proto.decode('whalerein.v1.EncryptedFrame', secondResult.result_body)
    const secondReply = proto.decode('whalerein.v1.ResponsePayload', open(channel.h2c, secondAad, secondFrame.nonce, secondFrame.ciphertext)).message_reply
    assert.equal(secondReply.session_id, 's1')
    assert.equal(secondReply.submitted_message_id, 'submitted-2')

    // The source fixture has no attachment support. Opening still resolves the
    // occurrence through the Source seam and reports the capability explicitly;
    // a pull or close for an unknown handle is scoped not_found. The retired
    // flat `session_id` shape is a typed invalid_argument, never a crash.
    const session = { host_id: 'host-1', session_id: 's1' }
    const legacy = encryptedQuery({
      security: ctx.host.security, store: ctx.store, channel,
      payload: { open_attachment: { session_id: 's1', attachment_ref: 'attachment-1' } },
      overrides: { request_id: 'attachment-legacy' },
    })
    await ctx.access.handlers.onRequest(legacy)
    assert.equal(ctx.access.lastResult().status, 'failed')
    assert.equal(ctx.access.lastResult().error_code, 'invalid_argument')
    for (const [payload, code] of [
      [{ open_attachment: { session, attachment_ref: 'attachment-1' } }, 'unsupported_capability'],
      [{ pull_attachment: { session, attachment_ref: 'attachment-1', transfer_id: 'f'.repeat(32), offset: 0, max_bytes: 1024 } }, 'not_found'],
      [{ close_attachment: { session, attachment_ref: 'attachment-1', transfer_id: 'f'.repeat(32) } }, 'not_found'],
    ]) {
      const request = encryptedQuery({
        security: ctx.host.security, store: ctx.store, channel, payload,
        overrides: { request_id: `attachment-${Math.random()}` },
      })
      await ctx.access.handlers.onRequest(request)
      assert.equal(ctx.access.lastResult().status, 'failed')
      assert.equal(ctx.access.lastResult().error_code, code)
    }
  } finally {
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('pairing requests require relay epoch 4 and safe expiry metadata', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    let result = await send(ctx.access, baseRequest({
      request_id: 'pair-old-relay', kind: 'REQUEST_KIND_PAIRING_CLAIM', relay_version: 1,
    }))
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'protocol_version_incompatible')

    result = await send(ctx.access, baseRequest({
      request_id: 'pair-bad-expiry', kind: 'REQUEST_KIND_PAIRING_CLAIM', expires_at_unix_ms: 'soon',
    }))
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'invalid_argument')

    result = await send(ctx.access, baseRequest({
      request_id: 'pair-negative-expiry', kind: 'REQUEST_KIND_PAIRING_CLAIM', expires_at_unix_ms: -1,
    }))
    assert.equal(result.status, 'rejected')
    assert.equal(result.error_code, 'invalid_argument')
  } finally {
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})

test('client inventory fails closed and a revocation handler failure is reported as failed', async () => {
  const ctx = setup()
  try {
    await ctx.host.start()
    ctx.host.security.setRevocationsPending(false)
    ctx.store.listPeerKeys = () => { throw new Error('database unavailable') }
    assert.equal(ctx.host.clientDevices(), null, 'a storage failure never exposes a partial client list')

    ctx.host.security.handleRevocation = () => { throw new Error('revocation storage unavailable') }
    const result = await send(ctx.access, baseRequest({
      request_id: 'rev-handler-failure',
      kind: 'REQUEST_KIND_REVOCATION',
      body: proto.encode('whalerein.v1.RevocationNotice', {
        replay_id: 'replay-failure', device_ids: ['d1'], final: true, revoked_at_unix_ms: 5,
      }),
    }))
    assert.equal(result.status, 'failed')
    assert.equal(result.error_code, 'internal')
  } finally {
    await ctx.host.stop()
    ctx.store.close()
    rmSync(ctx.dir, { recursive: true, force: true })
  }
})
