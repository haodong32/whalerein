// Host side of the Access self-status exchange (AC-40).
//
// Access admits a bounded HTTPS self-status claim and relays it on the private
// service topic. The Host must answer only the exact claimed grant: an active
// or revoked grant returns succeeded with active/revoked_at, an unknown or
// foreign grant returns a bodyless rejected result (Access renders that as
// inaccessible), and anything that is not this exact exchange is dropped
// without a reply.

import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { HostRuntime } from '../src/host.js'
import { StateStore } from '../src/state/store.js'
import { MemoryHostCredentials } from '../src/security/credentials.js'
import { TestSource } from './support/source.js'
import { codec } from '../src/protocol.js'
import { testConfig } from './support/config.js'
import { silentLogger } from '../src/log.js'

const proto = codec()
const HOST_ID = 'host-1'
const T0 = 1_700_000_000_000
const CHALLENGE = Buffer.from('0123456789abcdef', 'utf8')

let serial = 0

class FakeHostAccess {
  constructor() {
    this.connected = true
    this.handlers = null
    this.results = []
    this.selfStatusReplies = []
  }

  start(handlers) {
    this.handlers = handlers
    handlers.onConnected?.()
    return this
  }

  async stop() { this.connected = false }

  async publishResult(payload) { this.results.push(Buffer.from(payload)) }

  async publishEvent() {}

  async publishSelfStatusReply(payload) { this.selfStatusReplies.push(Buffer.from(payload)) }

  describe() { return { connected: this.connected, reconnects: 0 } }

  lastSelfStatusReply() { return this.selfStatusReplies.at(-1) ?? null }
}

function setup({ logger = silentLogger } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-self-status-'))
  let clock = T0
  const store = new StateStore({ path: ':memory:', now: () => clock })
  const config = testConfig({ host_id: HOST_ID, host_token: 'token', state_dir: dir })
  const access = new FakeHostAccess()
  const host = new HostRuntime({
    credentials: new MemoryHostCredentials(),
    config,
    adapter: new TestSource({ now: () => clock }),
    store,
    access,
    logger,
    now: () => clock,
    monotonicNow: () => clock - T0,
    codec: proto,
  })
  return {
    dir,
    store,
    host,
    access,
    clock: () => clock,
    advance: (ms) => { clock += ms },
  }
}

async function start(options) {
  const ctx = setup(options)
  await ctx.host.start()
  return ctx
}

async function stop(ctx) {
  await ctx.host.stop()
  ctx.store.close()
  rmSync(ctx.dir, { recursive: true, force: true })
}

function keyFor(seed) {
  return Buffer.alloc(32, seed)
}

function issueGrant(ctx, {
  hostKeyFingerprint = ctx.host.security.hostFingerprint,
  key = keyFor(0xa1 + serial),
  useRights = ['read'],
  now = ctx.clock(),
} = {}) {
  serial += 1
  const candidateId = `candidate-${serial}`
  ctx.host.trust.stageCandidate({
    candidateId,
    method: 'local',
    hostKeyFingerprint,
    candidatePublicKey: key,
    deviceId: 'phone-a',
    deviceLabel: 'Phone A',
    useRights,
    grantableRights: [],
    addDevice: false,
    sponsorGrantId: null,
    sponsorRevision: null,
    expiresAt: now + 60_000,
    now,
  })
  const committed = ctx.host.trust.commitEnrollment({
    candidateId,
    candidatePublicKey: key,
    useRights,
    grantableRights: [],
    addDevice: false,
    approver: { kind: 'owner' },
    now,
  })
  return committed.grant
}

function frame({ grantId, challenge = CHALLENGE, requestId, expiresAt, overrides = {} }) {
  serial += 1
  return {
    request_id: requestId ?? `req-${serial}`,
    host_id: HOST_ID,
    kind: 'REQUEST_KIND_SELF_STATUS',
    protocol_version: 1,
    relay_version: 5,
    expires_at_unix_ms: expiresAt ?? T0 + 5_000,
    body: proto.encode('whalerein.v1.SelfStatusRequest', { grant_id: grantId, challenge }),
    ...overrides,
  }
}

async function dispatch(ctx, request) {
  await ctx.access.handlers.onSelfStatus(request)
  return ctx.access.lastSelfStatusReply()
}

function decodeReply(reply) {
  assert.ok(reply, 'the Host must answer a decodable self-status request')
  return proto.decode('whalerein.v1.HostResult', reply)
}

function decodeStatus(result) {
  return proto.decode('whalerein.v1.SelfStatus', result.result_body)
}

test('an active grant answers succeeded/active and echoes the exact claim', async () => {
  const ctx = await start()
  try {
    const grant = issueGrant(ctx)
    const request = frame({ grantId: grant.grantId })
    const reply = await dispatch(ctx, request)
    assert.ok(reply)
    const result = decodeReply(reply)
    assert.equal(result.request_id, request.request_id)
    assert.equal(result.host_id, HOST_ID)
    assert.equal(result.status, 'succeeded')
    assert.equal(result.protocol_version, 1)
    assert.equal(result.relay_version, 5)
    assert.equal(ctx.access.results.length, 0, 'the reply never uses the ordinary result lane')
    const status = decodeStatus(result)
    assert.equal(status.grant_id, grant.grantId)
    assert.deepEqual(Buffer.from(status.challenge), CHALLENGE)
    assert.equal(status.active, true)
    assert.equal(status.revoked_at_unix_ms ?? 0, 0)
    assert.deepEqual(Object.keys(status).sort(), ['active', 'challenge', 'grant_id'])
    // No device id, label, key or other grant fact may travel in the body.
    assert.ok(reply.length < 512, 'the reply stays inside the bounded exchange')
  } finally {
    await stop(ctx)
  }
})

test('a revoked grant answers succeeded/inactive with the revocation time', async () => {
  const ctx = await start()
  try {
    const grant = issueGrant(ctx)
    const revokedAt = ctx.clock()
    ctx.host.trust.revokeGrants({ grantIds: [grant.grantId], now: revokedAt })
    const reply = await dispatch(ctx, frame({ grantId: grant.grantId }))
    const result = decodeReply(reply)
    assert.equal(result.status, 'succeeded')
    const status = decodeStatus(result)
    assert.equal(status.grant_id, grant.grantId)
    assert.deepEqual(Buffer.from(status.challenge), CHALLENGE)
    assert.equal(status.active ?? false, false)
    assert.equal(status.revoked_at_unix_ms, revokedAt)
  } finally {
    await stop(ctx)
  }
})

test('an unknown grant is a bodyless rejected result, indistinguishable from absent', async () => {
  const ctx = await start()
  try {
    const request = frame({ grantId: 'grant-does-not-exist' })
    const reply = await dispatch(ctx, request)
    const result = decodeReply(reply)
    assert.equal(result.request_id, request.request_id)
    assert.equal(result.status, 'rejected')
    assert.equal(result.result_body?.length ?? 0, 0)
    assert.equal(ctx.access.selfStatusReplies.length, 1)
  } finally {
    await stop(ctx)
  }
})

test('a grant owned by another Host fingerprint is refused without leaking it', async () => {
  const ctx = await start()
  try {
    const foreign = issueGrant(ctx, { hostKeyFingerprint: 'another-host-fingerprint', key: keyFor(0xb1) })
    assert.notEqual(foreign.hostKeyFingerprint, ctx.host.security.hostFingerprint)
    const reply = await dispatch(ctx, frame({ grantId: foreign.grantId }))
    const result = decodeReply(reply)
    assert.equal(result.status, 'rejected')
    assert.equal(result.result_body?.length ?? 0, 0)
  } finally {
    await stop(ctx)
  }
})

test('a wrong host, wrong kind, bad protocol, bad body or bad claim gets no reply', async () => {
  const ctx = await start()
  try {
    const grant = issueGrant(ctx)
    const cases = [
      ['wrong host', frame({ grantId: grant.grantId, overrides: { host_id: 'host-2' } })],
      ['wrong kind', frame({ grantId: grant.grantId, overrides: { kind: 'REQUEST_KIND_QUERY' } })],
      ['wrong protocol', frame({ grantId: grant.grantId, overrides: { protocol_version: 99 } })],
      ['wrong relay epoch', frame({ grantId: grant.grantId, overrides: { relay_version: 4 } })],
      ['missing request id', frame({ grantId: grant.grantId, overrides: { request_id: '' } })],
      ['malformed body', frame({ grantId: grant.grantId, overrides: { body: Buffer.from('not a SelfStatusRequest') } })],
      ['empty body', frame({ grantId: grant.grantId, overrides: { body: new Uint8Array(0) } })],
      ['oversized body', frame({ grantId: grant.grantId, overrides: { body: new Uint8Array(600) } })],
      ['short challenge', frame({ grantId: grant.grantId, challenge: Buffer.alloc(4) })],
      ['long challenge', frame({ grantId: grant.grantId, challenge: Buffer.alloc(65) })],
      ['invalid grant id', frame({ grantId: 'bad grant!' })],
      ['expired request', frame({ grantId: grant.grantId, expiresAt: T0 - 1 })],
    ]
    for (const [name, request] of cases) {
      const reply = await dispatch(ctx, request)
      assert.equal(reply, null, `${name} must not be answered`)
    }
    assert.equal(ctx.access.selfStatusReplies.length, 0)
    assert.equal(ctx.access.results.length, 0)
  } finally {
    await stop(ctx)
  }
})

test('no grant id, challenge or token is logged on success or refusal', async () => {
  const records = []
  const { createLogger } = await import('../src/log.js')
  const logger = createLogger({ salt: 'aa'.repeat(16), level: 'debug', sink: (record) => records.push(record) })
  const ctx = await start({ logger })
  try {
    const grant = issueGrant(ctx)
    await dispatch(ctx, frame({ grantId: grant.grantId }))
    await dispatch(ctx, frame({ grantId: 'grant-unknown' }))
    await dispatch(ctx, frame({ grantId: 'bad grant!' }))
    const logged = JSON.stringify(records)
    assert.equal(logged.includes(grant.grantId), false)
    assert.equal(logged.includes('grant-unknown'), false)
    assert.equal(logged.includes('0123456789abcdef'), false)
    assert.equal(logged.includes(CHALLENGE.toString('base64')), false)
    const refusals = records.filter((record) => record.event === 'self_status_rejected')
    assert.equal(refusals.length > 0, true)
    for (const record of refusals) {
      assert.equal(record.code, 'rejected')
      assert.equal(typeof record.reason, 'string')
    }
  } finally {
    await stop(ctx)
  }
})

test('a self-status envelope misplaced on the ordinary request topic still answers only on the service lane', async () => {
  const ctx = await start()
  try {
    const grant = issueGrant(ctx)
    // The ordinary router must never emit a result-lane reply for this kind.
    await ctx.access.handlers.onRequest(frame({ grantId: grant.grantId }))
    assert.equal(ctx.access.results.length, 0)
    const result = decodeReply(ctx.access.lastSelfStatusReply())
    assert.equal(result.status, 'succeeded')
    assert.equal(decodeStatus(result).active, true)
  } finally {
    await stop(ctx)
  }
})