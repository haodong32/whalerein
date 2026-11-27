// Host-owned device trust integration.
//
// These cases exercise the normal Host paths: pairing COMMIT becomes the grant
// writer, an ordinary channel binds one active grant, dispatch checks the
// current right, native handoff checks the authenticated deadline and command
// journal, and revocation fences exactly the affected grant.

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
import { NoiseXX, channelPrologue, generateKeypair, requestAad } from '../src/security/noise.js'
import { classifyOperation } from '../src/security/grants.js'
import { bytesEqual, sha256Bytes } from '../src/security/digest.js'
import { DEFAULT_CONNECTION_REF, establishChannel } from './support/channel.js'
import { DeviceClient } from './support/wire-client.js'
import { startHarness } from './support/harness.js'

const proto = codec()
const ALL_RIGHTS = ['read', 'write', 'approve']

class FakeHostAccess {
  constructor() {
    this.connected = true
    this.handlers = null
    this.results = []
  }

  start(handlers) {
    this.handlers = handlers
    handlers.onConnected?.()
    return this
  }

  async stop() { this.connected = false }

  async publishResult(payload) {
    this.results.push(proto.decode('whalerein.v1.HostResult', payload))
  }

  async publishEvent() {}

  describe() { return { connected: this.connected, reconnects: 0 } }

  lastResult() { return this.results[this.results.length - 1] }
}

function setup({ now = Date.now, monotonicNow = () => Math.floor(performance.now()) } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'whalerein-trust-'))
  const config = testConfig({
    adapter: 'fake',
    mqtt_url: 'mqtts://broker.test',
    host_id: 'host-1',
    host_token: 'token',
    state_dir: dir,
    event_batch_ms: 0,
  })
  const store = new StateStore({ path: ':memory:', now })
  const adapter = new TestSource({ now, sessions: [{ sessionId: 's1', running: false }] })
  const access = new FakeHostAccess()
  const host = new HostRuntime({
    credentials: new MemoryHostCredentials(),
    config,
    adapter,
    store,
    access,
    logger: silentLogger,
    now,
    monotonicNow,
    codec: proto,
  })
  return { dir, store, adapter, access, host }
}

async function start(options) {
  const ctx = setup(options)
  await ctx.host.start()
  ctx.host.security.setRevocationsPending(false)
  return ctx
}

async function stop(ctx) {
  await ctx.host.stop()
  ctx.store.close()
  rmSync(ctx.dir, { recursive: true, force: true })
}

function request({ channel, payload, kind = 'REQUEST_KIND_QUERY', commandId = '', expiresAt = null, overrides = {} }) {
  const requestId = overrides.request_id ?? `r-${Math.random()}`
  const deadline = expiresAt ?? Date.now() + 60_000
  const aad = requestAad({
    requestId,
    hostId: 'host-1',
    deviceId: channel.deviceId,
    kind,
    commandId,
    expiresAt: deadline,
  })
  const plaintext = proto.encode('whalerein.v1.RequestPayload', payload)
  const sealed = channel.cipher.seal('c2h', aad, plaintext)
  const frame = proto.encode('whalerein.v1.EncryptedFrame', {
    channel_id: channel.channelId,
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
    expires_at_unix_ms: deadline,
    protocol_version: 1,
    relay_version: 5,
    channel_id: channel.channelId,
    body: frame,
    connection_ref: channel.connectionRef,
    grant_generation: 1,
    enrollment_generation: 1,
    ...overrides,
  }
}

async function send(ctx, frame) {
  await ctx.access.handlers.onRequest(frame)
  return ctx.access.lastResult()
}

function openResponse(channel, frame, result) {
  if (!(result?.result_body instanceof Uint8Array) || result.result_body.length === 0) return null
  const aad = requestAad({
    requestId: frame.request_id,
    hostId: 'host-1',
    deviceId: channel.deviceId,
    kind: frame.kind,
    commandId: frame.command_id ?? '',
    expiresAt: frame.expires_at_unix_ms,
  })
  const encrypted = proto.decode('whalerein.v1.EncryptedFrame', result.result_body)
  return proto.decode('whalerein.v1.ResponsePayload', channel.cipher.readerForTest().open(
    'h2c', aad, encrypted.nonce, encrypted.ciphertext,
  ))
}

function createGrant(ctx, { deviceId, identity, useRights = ALL_RIGHTS, grantableRights = [], addDevice = false }) {
  return ctx.host.deviceTrust.createGrant({
    deviceId,
    devicePublicKey: identity.publicKey,
    deviceLabel: deviceId,
    useRights,
    grantableRights,
    addDevice,
  })
}

function beginChannel({ security, deviceId, identity, channelId, grantId = '' }) {
  const prologue = channelPrologue({ hostId: 'host-1', deviceId, channelId })
  const noise = new NoiseXX({ initiator: true, staticKeypair: identity, prologue })
  const hello = proto.encode('whalerein.v1.ChannelHello', {
    device_id: deviceId,
    host_id: 'host-1',
    protocol_version: 1,
    capabilities: { capabilities: ['sessions'], protocol_version: 1 },
    client_nonce: 'nonce',
    ...(grantId ? { grant_id: grantId } : {}),
  })
  const reply = security.handleChannelHandshake({
    connection_ref: DEFAULT_CONNECTION_REF,
    channel_id: channelId,
    device_id: deviceId,
    protocol_version: 1,
    body: noise.writeMessage(hello),
  })
  const ready = proto.decode('whalerein.v1.ChannelReady', noise.readMessage(reply))
  return { noise, channelId, deviceId, ready }
}

function finishChannel(security, pending) {
  return security.handleChannelHandshake({
    connection_ref: DEFAULT_CONNECTION_REF,
    channel_id: pending.channelId,
    device_id: pending.deviceId,
    protocol_version: 1,
    body: pending.noise.writeMessage(new Uint8Array(0)),
  })
}

// --- pairing COMMIT is the Host grant writer --------------------------------

test('pairing COMMIT writes exactly one Host grant and an exact retry returns it', async () => {
  const harness = await startHarness()
  try {
    const device = await harness.createDevice('device-1')
    await device.beginPairing({ claimId: 'claim-1', tokenHash: harness.tokenHash })
    await device.finishPairing({ authorize: false })
    const authorized = await harness.access.pairingAuthorize({
      claimId: 'claim-1',
      deviceId: 'device-1',
      devicePublicKey: device.identity.publicKey,
    })
    assert.equal(authorized.authorized, true)

    const fingerprint = harness.host.security.hostFingerprint
    const grant = harness.host.trust.findActiveGrant({
      hostKeyFingerprint: fingerprint,
      devicePublicKey: device.identity.publicKey,
    })
    assert.ok(grant, 'COMMIT installs the grant')
    assert.match(grant.grantId, /^grant_[A-Za-z0-9_-]{40,}$/)
    assert.equal(authorized.grant_id, grant.grantId, 'the COMMIT ack carries the exact committed grant ID')
    assert.deepEqual(grant.useRights, ALL_RIGHTS)
    assert.deepEqual(grant.grantableRights, [], 'the default flow grants no ceiling')
    assert.equal(grant.addDevice, false)
    assert.equal(grant.approverKind, 'owner')
    assert.equal(grant.state, 'active')
    assert.equal(grant.revision, 1)

    const peer = harness.host.store.getPeerKey('device-1')
    assert.equal(peer.grantId, grant.grantId, 'the enrollment identity is the grant ID')
    assert.equal(peer.grantRevision, grant.revision)

    // Exact COMMIT retry: same decision, no second grant.
    const retried = await harness.access.pairingAuthorize({
      claimId: 'claim-1',
      deviceId: 'device-1',
      devicePublicKey: device.identity.publicKey,
    })
    assert.equal(retried.authorized, true)
    assert.equal(retried.grant_id, grant.grantId, 'an exact retry acknowledges the same grant ID')
    assert.equal(harness.host.trust.listGrants({ hostKeyFingerprint: fingerprint }).length, 1)
    assert.equal(
      harness.host.trust.findActiveGrant({ hostKeyFingerprint: fingerprint, devicePublicKey: device.identity.publicKey }).grantId,
      grant.grantId,
    )
  } finally {
    await harness.stop()
  }
})

test('a competing COMMIT for the same key conflicts and never replaces the grant', async () => {
  const harness = await startHarness()
  let intruder = null
  try {
    const device = await harness.createDevice('device-1')
    await device.beginPairing({ claimId: 'claim-1', tokenHash: harness.tokenHash })
    await device.finishPairing({ authorize: false })
    await harness.access.pairingAuthorize({
      claimId: 'claim-1',
      deviceId: 'device-1',
      devicePublicKey: device.identity.publicKey,
    })
    const fingerprint = harness.host.security.hostFingerprint
    const winner = harness.host.trust.findActiveGrant({
      hostKeyFingerprint: fingerprint,
      devicePublicKey: device.identity.publicKey,
    })

    intruder = new DeviceClient({
      access: harness.access,
      broker: harness.broker,
      codec: harness.codec,
      hostId: harness.config.hostId,
      deviceId: 'device-1',
      identity: device.identity,
    })
    await intruder.ready
    await intruder.beginPairing({ claimId: 'claim-2', tokenHash: harness.tokenHash })
    await intruder.finishPairing({ authorize: false })
    await assert.rejects(
      harness.access.pairingAuthorize({
        claimId: 'claim-2',
        deviceId: 'device-1',
        devicePublicKey: device.identity.publicKey,
      }),
      (error) => error.code === 'conflict',
    )
    const current = harness.host.trust.findActiveGrant({
      hostKeyFingerprint: fingerprint,
      devicePublicKey: device.identity.publicKey,
    })
    assert.equal(current.grantId, winner.grantId)
    assert.deepEqual(current.useRights, winner.useRights)
    assert.equal(harness.host.trust.listGrants({ hostKeyFingerprint: fingerprint }).length, 1)
  } finally {
    await intruder?.close()
    await harness.stop()
  }
})

// --- channel binding ---------------------------------------------------------

test('an ordinary channel requires an active Host grant', async () => {
  const ctx = await start()
  try {
    const identity = generateKeypair()
    // A pinned peer row alone is not session authority.
    ctx.store.upsertPeerKey({ deviceId: 'd1', publicKey: identity.publicKey, fingerprint: 'f', claimId: 'claim' })
    assert.throws(() => beginChannel({
      security: ctx.host.security, deviceId: 'd1', identity, channelId: 'channel-1',
    }), (error) => error.code === 'forbidden')
    assert.equal(ctx.host.security.listChannels().length, 0)

    createGrant(ctx, { deviceId: 'd1', identity })
    const grant = ctx.host.trust.findActiveGrant({
      hostKeyFingerprint: ctx.host.security.hostFingerprint,
      devicePublicKey: identity.publicKey,
    })
    const channel = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', identity, codec: proto,
    })
    assert.equal(channel.ready.grant_id, grant.grantId, 'ChannelReady echoes the bound grant ID')
    assert.equal(ctx.host.security.getChannel(channel.channelId).grantId, grant.grantId)
  } finally {
    await stop(ctx)
  }
})

test('a channel hello naming a different grant fails closed', async () => {
  const ctx = await start()
  try {
    const identity = generateKeypair()
    const grant = createGrant(ctx, { deviceId: 'd1', identity })
    const forged = createGrant(ctx, { deviceId: 'other', identity: generateKeypair() }).grant.grantId
    assert.notEqual(forged, grant.grant.grantId)
    assert.throws(() => beginChannel({
      security: ctx.host.security,
      deviceId: 'd1',
      identity,
      channelId: 'channel-mismatch',
      grantId: forged,
    }), (error) => error.code === 'forbidden')
    assert.equal(ctx.host.security.getChannel('channel-mismatch'), null, 'no channel is installed')
    assert.equal(ctx.host.security.listChannels().length, 0)

    // The mismatched attempt left no pending handshake behind: a correct one
    // still installs the same snapshot grant.
    const pending = beginChannel({
      security: ctx.host.security,
      deviceId: 'd1',
      identity,
      channelId: 'channel-ok',
      grantId: grant.grant.grantId,
    })
    assert.equal(pending.ready.grant_id, grant.grant.grantId)
    finishChannel(ctx.host.security, pending)
    assert.ok(ctx.host.security.getChannel('channel-ok'))
  } finally {
    await stop(ctx)
  }
})

test('an empty legacy grant field installs only under the Host snapshot rules', async () => {
  const ctx = await start()
  try {
    const identity = generateKeypair()
    const first = createGrant(ctx, { deviceId: 'd1', identity })
    const legacy = beginChannel({
      security: ctx.host.security, deviceId: 'd1', identity, channelId: 'channel-legacy',
    })
    assert.equal(legacy.ready.grant_id, first.grant.grantId, 'the Host still names the snapshot grant')
    finishChannel(ctx.host.security, legacy)
    assert.equal(ctx.host.security.getChannel('channel-legacy').grantId, first.grant.grantId)

    // An empty field never bypasses the finish recheck: revoke and re-enroll
    // G2 after an empty-field msg1 still rejects the old pending channel.
    const stale = beginChannel({
      security: ctx.host.security, deviceId: 'd1', identity, channelId: 'channel-legacy-stale',
    })
    ctx.host.deviceTrust.revokeGrants({ grantIds: [first.grant.grantId] })
    const second = createGrant(ctx, { deviceId: 'd1', identity })
    assert.throws(() => finishChannel(ctx.host.security, stale), (error) => error.code === 'unauthorized')
    assert.equal(ctx.host.security.getChannel('channel-legacy-stale'), null)

    const fresh = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', identity, codec: proto,
      grantId: second.grant.grantId,
    })
    assert.equal(fresh.ready.grant_id, second.grant.grantId)
    assert.equal(ctx.host.security.getChannel(fresh.channelId).grantId, second.grant.grantId)
  } finally {
    await stop(ctx)
  }
})

test('a handshake started under G1 cannot install after revoke and re-enrollment to G2', async () => {
  const ctx = await start()
  try {
    const identity = generateKeypair()
    const first = createGrant(ctx, { deviceId: 'd1', identity })
    const pending = beginChannel({
      security: ctx.host.security,
      deviceId: 'd1',
      identity,
      channelId: 'channel-g1',
      grantId: first.grant.grantId,
    })
    assert.equal(pending.ready.grant_id, first.grant.grantId)
    ctx.host.deviceTrust.revokeGrants({ grantIds: [first.grant.grantId] })
    const second = createGrant(ctx, { deviceId: 'd1', identity })
    assert.notEqual(second.grant.grantId, first.grant.grantId)

    assert.throws(() => finishChannel(ctx.host.security, pending), (error) => error.code === 'unauthorized')
    assert.equal(ctx.host.security.getChannel('channel-g1'), null, 'the stale finish installs nothing')

    // A fresh G2 handshake that names G2 succeeds.
    const fresh = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', identity, codec: proto,
      grantId: second.grant.grantId,
    })
    assert.equal(fresh.ready.grant_id, second.grant.grantId)
    assert.equal(ctx.host.security.getChannel(fresh.channelId).grantId, second.grant.grantId)
    assert.equal(ctx.host.security.listChannels().length, 1)
  } finally {
    await stop(ctx)
  }
})

// --- dispatch authorization --------------------------------------------------

test('a read-only grant can list sessions but cannot send or approve', async () => {
  const ctx = await start()
  try {
    const identity = generateKeypair()
    createGrant(ctx, { deviceId: 'd1', identity, useRights: ['read'] })
    const channel = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', identity, codec: proto,
    })
    let executed = 0
    const original = ctx.adapter.execute.bind(ctx.adapter)
    ctx.adapter.execute = async (...args) => { executed += 1; return original(...args) }

    const listed = await send(ctx, request({ channel, payload: { list_sessions: {} } }))
    assert.equal(listed.status, 'succeeded')
    const sent = await send(ctx, request({
      channel, payload: { send_message: { session_id: 's1', text: 'hello' } },
      kind: 'REQUEST_KIND_COMMAND', commandId: 'send-1',
    }))
    assert.equal(sent.status, 'rejected')
    assert.equal(sent.error_code, 'forbidden')
    const approval = await send(ctx, request({ channel, payload: { get_approval: { approval_id: 'a1' } } }))
    assert.equal(approval.status, 'rejected')
    assert.equal(approval.error_code, 'forbidden')
    assert.equal(executed, 0, 'no native call was admitted')
  } finally {
    await stop(ctx)
  }
})

test('a write-only grant can send but cannot read sessions or approval detail', async () => {
  const ctx = await start()
  try {
    const identity = generateKeypair()
    createGrant(ctx, { deviceId: 'd1', identity, useRights: ['write'] })
    const channel = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', identity, codec: proto,
    })
    let executed = 0
    const original = ctx.adapter.execute.bind(ctx.adapter)
    ctx.adapter.execute = async (...args) => { executed += 1; return original(...args) }

    const listed = await send(ctx, request({ channel, payload: { list_sessions: {} } }))
    assert.equal(listed.status, 'rejected')
    assert.equal(listed.error_code, 'forbidden')
    const approval = await send(ctx, request({ channel, payload: { get_approval: { approval_id: 'a1' } } }))
    assert.equal(approval.error_code, 'forbidden')
    const sent = await send(ctx, request({
      channel, payload: { send_message: { session_id: 's1', text: 'hello' } },
      kind: 'REQUEST_KIND_COMMAND', commandId: 'send-1',
    }))
    assert.equal(sent.status, 'succeeded')
    assert.equal(executed, 1)
  } finally {
    await stop(ctx)
  }
})

test('an unclassified operation is denied by the Host operation map', async () => {
  const ctx = await start()
  try {
    const identity = generateKeypair()
    const created = createGrant(ctx, { deviceId: 'd1', identity })
    assert.equal(classifyOperation('delete_everything').classified, false)
    const denied = ctx.host.trust.authorizeOperation({
      grantId: created.grant.grantId,
      operation: 'delete_everything',
    })
    assert.equal(denied.allowed, false)
    assert.equal(denied.reason, 'unknown_operation')
  } finally {
    await stop(ctx)
  }
})

test('removing a right mid-channel stops new sends/output and leaves another device live', async () => {
  const ctx = await start()
  try {
    const a = generateKeypair()
    const b = generateKeypair()
    const grantA = createGrant(ctx, { deviceId: 'da', identity: a })
    createGrant(ctx, { deviceId: 'db', identity: b, useRights: ALL_RIGHTS })
    const channelA = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'da', identity: a, codec: proto,
    })
    const channelB = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'db', identity: b, codec: proto,
    })
    let executed = 0
    const original = ctx.adapter.execute.bind(ctx.adapter)
    ctx.adapter.execute = async (...args) => { executed += 1; return original(...args) }

    // Remove write only: the channel stays for read, but no new send is admitted.
    ctx.host.deviceTrust.editGrant({ grantId: grantA.grant.grantId, useRights: ['read', 'approve'] })
    assert.ok(ctx.host.security.getChannel(channelA.channelId), 'read remains usable')
    const listed = await send(ctx, request({ channel: channelA, payload: { list_sessions: {} } }))
    assert.equal(listed.status, 'succeeded')
    const deniedSend = await send(ctx, request({
      channel: channelA, payload: { send_message: { session_id: 's1', text: 'hi' } },
      kind: 'REQUEST_KIND_COMMAND', commandId: 'send-a',
    }))
    assert.equal(deniedSend.error_code, 'forbidden')
    assert.equal(executed, 0)

    // Remove read: output for that grant stops and its channel is closed.
    ctx.host.deviceTrust.editGrant({ grantId: grantA.grant.grantId, useRights: ['write'] })
    assert.equal(ctx.host.security.getChannel(channelA.channelId), null)
    const other = await send(ctx, request({ channel: channelB, payload: { list_sessions: {} } }))
    assert.equal(other.status, 'succeeded', 'the unrelated device keeps its channel and rights')
  } finally {
    await stop(ctx)
  }
})

// --- deadline and command journal -------------------------------------------

test('a late, at-deadline, missing or outer-extended deadline admits zero native calls', async () => {
  const ctx = await start()
  try {
    const identity = generateKeypair()
    createGrant(ctx, { deviceId: 'd1', identity })
    const channel = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', identity, codec: proto,
    })
    let executed = 0
    const original = ctx.adapter.execute.bind(ctx.adapter)
    ctx.adapter.execute = async (...args) => { executed += 1; return original(...args) }

    const late = await send(ctx, request({
      channel, payload: { send_message: { session_id: 's1', text: 'late' } },
      kind: 'REQUEST_KIND_COMMAND', commandId: 'late', expiresAt: ctx.host.now() - 1,
    }))
    assert.equal(late.status, 'rejected')
    assert.equal(late.error_code, 'expired')

    const atDeadline = await send(ctx, request({
      channel, payload: { send_message: { session_id: 's1', text: 'now' } },
      kind: 'REQUEST_KIND_COMMAND', commandId: 'at', expiresAt: ctx.host.now(),
    }))
    assert.equal(atDeadline.status, 'rejected')

    const missing = await send(ctx, request({
      channel, payload: { send_message: { session_id: 's1', text: 'none' } },
      kind: 'REQUEST_KIND_COMMAND', commandId: 'missing', expiresAt: 0,
    }))
    assert.equal(missing.status, 'rejected')

    // A later outer deadline cannot match the authenticated inner deadline.
    const extended = request({
      channel, payload: { send_message: { session_id: 's1', text: 'extended' } },
      kind: 'REQUEST_KIND_COMMAND', commandId: 'extended', expiresAt: ctx.host.now() + 60_000,
    })
    extended.expires_at_unix_ms = ctx.host.now() + 120_000
    const extendedResult = await send(ctx, extended)
    assert.equal(extendedResult.status, 'rejected')
    assert.equal(extendedResult.error_code, 'unauthorized')
    assert.equal(executed, 0, 'no deadline variant reached the native source')
  } finally {
    await stop(ctx)
  }
})

test('an admission that expires during native preflight admits no native handoff', async () => {
  let monotonic = 1000
  const ctx = await start({ monotonicNow: () => monotonic })
  try {
    const identity = generateKeypair()
    createGrant(ctx, { deviceId: 'd1', identity })
    const channel = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', identity, codec: proto,
    })
    ctx.adapter.describeHostCapabilities = async () => ({ session_create: true })
    ctx.adapter.preflightCreateSession = async () => {
      // Time passes inside the asynchronous preflight, past the admitted window.
      monotonic += 5000
      return {}
    }
    let executed = 0
    ctx.adapter.executeHost = async () => {
      executed += 1
      return { identity: { session_id: 'created' } }
    }
    const frame = request({
      channel, payload: { create_session: { cwd: '/' } },
      kind: 'REQUEST_KIND_COMMAND', commandId: 'create-late', expiresAt: Date.now() + 2000,
    })
    const result = await send(ctx, frame)
    const receipt = openResponse(channel, frame, result)?.lifecycle_receipt
    assert.equal(receipt?.disposition, 'FAILED')
    assert.equal(receipt?.reason_code, 'PREFLIGHT_EXPIRED')
    assert.equal(executed, 0, 'no native handoff happened after the admitted window elapsed')
  } finally {
    await stop(ctx)
  }
})

test('a duplicate command ID replays the stored receipt and never executes twice', async () => {
  const ctx = await start()
  try {
    const identity = generateKeypair()
    createGrant(ctx, { deviceId: 'd1', identity })
    const channel = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', identity, codec: proto,
    })
    let executed = 0
    const original = ctx.adapter.execute.bind(ctx.adapter)
    ctx.adapter.execute = async (...args) => { executed += 1; return original(...args) }
    const payload = { send_message: { session_id: 's1', text: 'once' } }
    // A duplicate keeps the authenticated original deadline; the frame nonce
    // is fresh so the channel accepts the new frame.
    const deadline = Date.now() + 60_000

    const firstFrame = request({ channel, payload, kind: 'REQUEST_KIND_COMMAND', commandId: 'dup-1', expiresAt: deadline })
    const first = await send(ctx, firstFrame)
    assert.equal(first.status, 'succeeded')
    assert.ok(openResponse(channel, firstFrame, first))

    const secondFrame = request({ channel, payload, kind: 'REQUEST_KIND_COMMAND', commandId: 'dup-1', expiresAt: deadline })
    const second = await send(ctx, secondFrame)
    assert.equal(second.status, 'succeeded')
    assert.equal(executed, 1, 'the native action ran once')
    assert.equal(ctx.host.journal.countCommands('succeeded'), 1)
  } finally {
    await stop(ctx)
  }
})

test('a foreign grant or unknown command ID gets the same generic lookup', async () => {
  const ctx = await start()
  try {
    const a = generateKeypair()
    const b = generateKeypair()
    createGrant(ctx, { deviceId: 'da', identity: a })
    createGrant(ctx, { deviceId: 'db', identity: b })
    const channelA = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'da', identity: a, codec: proto,
    })
    const channelB = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'db', identity: b, codec: proto,
    })
    await send(ctx, request({
      channel: channelA, payload: { send_message: { session_id: 's1', text: 'owned' } },
      kind: 'REQUEST_KIND_COMMAND', commandId: 'shared-command',
    }))

    const ownFrame = request({ channel: channelA, payload: { get_command_result: { command_id: 'shared-command' } } })
    const own = await send(ctx, ownFrame)
    assert.equal(own.status, 'succeeded')
    assert.equal(openResponse(channelA, ownFrame, own).command_result.found, true)

    const foreignFrame = request({ channel: channelB, payload: { get_command_result: { command_id: 'shared-command' } } })
    const foreign = await send(ctx, foreignFrame)
    const foreignBody = openResponse(channelB, foreignFrame, foreign)
    assert.notEqual(foreignBody.command_result?.found, true)
    assert.equal(foreignBody.command_result?.status ?? 'online_status_unspecified', 'online_status_unspecified')

    const unknownFrame = request({ channel: channelB, payload: { get_command_result: { command_id: 'never-existed' } } })
    const unknown = await send(ctx, unknownFrame)
    const unknownBody = openResponse(channelB, unknownFrame, unknown)
    assert.deepEqual(unknownBody.command_result, foreignBody.command_result, 'foreign and unknown answers are identical')
  } finally {
    await stop(ctx)
  }
})

test('a re-enrolled grant cannot repeat or read an earlier command', async () => {
  const ctx = await start()
  try {
    const identity = generateKeypair()
    const first = createGrant(ctx, { deviceId: 'd1', identity })
    const channel1 = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', identity, codec: proto,
    })
    let executed = 0
    const original = ctx.adapter.execute.bind(ctx.adapter)
    ctx.adapter.execute = async (...args) => { executed += 1; return original(...args) }
    const payload = { send_message: { session_id: 's1', text: 'old work' } }
    const deadline = Date.now() + 60_000
    const original_frame = request({ channel: channel1, payload, kind: 'REQUEST_KIND_COMMAND', commandId: 'old-command', expiresAt: deadline })
    assert.equal((await send(ctx, original_frame)).status, 'succeeded')
    assert.equal(executed, 1)

    ctx.host.deviceTrust.revokeGrants({ grantIds: [first.grant.grantId] })
    const second = createGrant(ctx, { deviceId: 'd1', identity })
    assert.notEqual(second.grant.grantId, first.grant.grantId)
    const channel2 = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'd1', identity, codec: proto,
    })
    const repeat = await send(ctx, request({
      channel: channel2, payload, kind: 'REQUEST_KIND_COMMAND', commandId: 'old-command', expiresAt: deadline,
    }))
    assert.equal(repeat.status, 'failed')
    assert.equal(repeat.error_code, 'forbidden')
    assert.equal(executed, 1, 'no second native entry under the new grant')

    const lookupFrame = request({ channel: channel2, payload: { get_command_result: { command_id: 'old-command' } } })
    const lookup = await send(ctx, lookupFrame)
    assert.notEqual(openResponse(channel2, lookupFrame, lookup).command_result?.found, true)
  } finally {
    await stop(ctx)
  }
})

// --- revocation fencing ------------------------------------------------------

test('revoking one grant closes exactly its channels, pending handshakes and outbox', async () => {
  const ctx = await start()
  try {
    const a = generateKeypair()
    const b = generateKeypair()
    const grantA = createGrant(ctx, { deviceId: 'da', identity: a })
    createGrant(ctx, { deviceId: 'db', identity: b })
    const channelA = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'da', identity: a, codec: proto,
    })
    const channelB = establishChannel({
      security: ctx.host.security, store: ctx.store, hostId: 'host-1', deviceId: 'db', identity: b, codec: proto,
    })
    const pendingA = beginChannel({
      security: ctx.host.security, deviceId: 'da', identity: a, channelId: 'pending-a',
    })
    ctx.store.addOutbox({ deviceId: 'da', channelId: channelA.channelId, payload: new Uint8Array([1]) })
    ctx.store.addOutbox({ deviceId: 'db', channelId: channelB.channelId, payload: new Uint8Array([2]) })
    assert.equal(ctx.store.countOutbox('da'), 1)

    ctx.host.deviceTrust.revokeGrants({ grantIds: [grantA.grant.grantId] })
    assert.equal(ctx.host.security.getChannel(channelA.channelId), null)
    assert.equal(ctx.host.security.getChannel(channelB.channelId)?.channelId, channelB.channelId)
    assert.throws(() => finishChannel(ctx.host.security, pendingA), (error) => error.code === 'unauthorized')
    assert.equal(ctx.store.countOutbox('da'), 0, 'the revoked device loses queued ciphertext')
    assert.equal(ctx.store.countOutbox('db'), 1, 'the unrelated device keeps its queue')
    assert.equal(ctx.host.store.getPeerKey('da'), null)
    assert.equal(ctx.host.trust.getGrant(grantA.grant.grantId).state, 'revoked')
    assert.equal(ctx.host.trust.findActiveGrant({
      hostKeyFingerprint: ctx.host.security.hostFingerprint,
      devicePublicKey: a.publicKey,
    }), null)
  } finally {
    await stop(ctx)
  }
})

// --- owner management and loss review ---------------------------------------

test('owner edits keep the ceiling inside use rights and never regrow it', async () => {
  const ctx = await start()
  try {
    const identity = generateKeypair()
    const created = createGrant(ctx, {
      deviceId: 'd1',
      identity,
      useRights: ['read', 'write'],
      grantableRights: ['read'],
      addDevice: true,
    })
    const grantId = created.grant.grantId
    // Increasing use rights later leaves the smaller ceiling untouched.
    const grown = ctx.host.deviceTrust.editGrant({ grantId, useRights: ['read', 'write', 'approve'] })
    assert.deepEqual(grown.grant.useRights, ['read', 'write', 'approve'])
    assert.deepEqual(grown.grant.grantableRights, ['read'])
    // Reducing use rights intersects the ceiling in the same transaction.
    const shrunk = ctx.host.deviceTrust.editGrant({ grantId, useRights: ['read'] })
    assert.deepEqual(shrunk.grant.useRights, ['read'])
    assert.deepEqual(shrunk.grant.grantableRights, ['read'])
    // A ceiling outside current use rights is refused outright.
    assert.throws(
      () => ctx.host.deviceTrust.editGrant({ grantId, grantableRights: ['read', 'write'] }),
      (error) => error.code === 'forbidden',
    )
    const listed = ctx.host.listDeviceGrants()
    assert.equal(listed.length, 1)
    assert.equal(listed[0].approverKind, 'owner')
    assert.equal(typeof listed[0].createdAt, 'number')
    assert.equal(typeof listed[0].updatedAt, 'number')

    // Replace goes through the same writer: the old row is retired, never
    // merged, and the new grant gets a fresh Host ID.
    const replacement = ctx.host.replaceDeviceGrant({
      deviceId: 'd1',
      devicePublicKey: identity.publicKey,
      useRights: ['read', 'write'],
      grantableRights: [],
    })
    assert.notEqual(replacement.grant.grantId, grantId)
    assert.equal(ctx.host.getDeviceGrant(grantId).state, 'revoked')
    assert.equal(ctx.host.getDeviceGrant(replacement.grant.grantId).state, 'active')
    assert.equal(ctx.host.getDeviceGrant('missing-grant'), null)
  } finally {
    await stop(ctx)
  }
})

test('loss review lists directly approved children with timestamps and preserves them on ordinary retirement', async () => {
  const ctx = await start()
  try {
    const parentIdentity = generateKeypair()
    const childIdentity = generateKeypair()
    const parent = createGrant(ctx, {
      deviceId: 'parent', identity: parentIdentity, useRights: ALL_RIGHTS, grantableRights: ALL_RIGHTS, addDevice: true,
    })
    const now = ctx.host.now()
    const candidateId = 'child-candidate'
    ctx.host.trust.stageCandidate({
      candidateId,
      method: 'sponsor',
      hostKeyFingerprint: ctx.host.security.hostFingerprint,
      candidatePublicKey: childIdentity.publicKey,
      deviceId: 'child',
      deviceLabel: 'Child',
      useRights: ['read'],
      grantableRights: [],
      addDevice: false,
      sponsorGrantId: parent.grant.grantId,
      sponsorRevision: parent.grant.revision,
      expiresAt: now + 60_000,
      now,
    })
    const child = ctx.host.trust.commitEnrollment({
      candidateId,
      candidatePublicKey: childIdentity.publicKey,
      useRights: ['read'],
      grantableRights: [],
      addDevice: false,
      approver: { kind: 'sponsor', grantId: parent.grant.grantId, revision: parent.grant.revision },
      now,
    })

    const review = ctx.host.deviceLossReview(parent.grant.grantId)
    assert.equal(review.children.length, 1)
    assert.equal(review.children[0].grantId, child.grant.grantId)
    assert.equal(review.children[0].approverKind, 'sponsor')
    assert.equal(typeof review.children[0].createdAt, 'number')
    assert.equal(typeof review.children[0].updatedAt, 'number')
    assert.deepEqual(review.children[0].useRights, ['read'])

    // Ordinary retirement of the sponsor must not cascade to the child.
    ctx.host.revokeDeviceGrants({ grantIds: [parent.grant.grantId] })
    assert.equal(ctx.host.getDeviceGrant(parent.grant.grantId).state, 'revoked')
    assert.equal(ctx.host.getDeviceGrant(child.grant.grantId).state, 'active')
    assert.equal(ctx.host.listDeviceChildren(parent.grant.grantId)[0].state, 'active')
  } finally {
    await stop(ctx)
  }
})

test('recovery verifiers can register, issue a single-use challenge and commit once', async () => {
  const ctx = await start()
  try {
    const verifier = generateKeypair()
    const applicant = generateKeypair()
    const registered = ctx.host.registerRecoveryVerifier({
      publicKey: verifier.publicKey,
      allowedRights: ['read', 'write'],
    })
    assert.equal(registered.active, true)
    assert.equal(typeof registered.revision, 'number')

    const now = ctx.host.now()
    const challenge = ctx.host.issueRecoveryChallenge({
      verifierRevision: registered.revision,
      deviceId: 'recovered',
      devicePublicKey: applicant.publicKey,
      useRights: ['read'],
      expiresAt: now + 60_000,
    })
    const committed = ctx.host.commitRecovery({
      challengeId: challenge.challengeId,
      nonce: challenge.nonce,
      verifierRevision: registered.revision,
      devicePublicKey: applicant.publicKey,
      useRights: ['read'],
    })
    assert.equal(committed.grant.approverKind, 'recovery')
    assert.equal(committed.grant.state, 'active')
    assert.equal(ctx.host.store.getPeerKey('recovered').grantId, committed.grant.grantId)

    const retry = ctx.host.commitRecovery({
      challengeId: challenge.challengeId,
      nonce: challenge.nonce,
      verifierRevision: registered.revision,
      devicePublicKey: applicant.publicKey,
      useRights: ['read'],
    })
    assert.equal(retry.retried, true)
    assert.equal(retry.grant.grantId, committed.grant.grantId)

    // A policy edit retires the proven revision and issues a fresh one, so an
    // outstanding challenge under the old scope dies.
    const narrowed = ctx.host.updateRecoveryPolicy({
      revision: registered.revision,
      allowedRights: ['read'],
    })
    assert.notEqual(narrowed.revision, registered.revision)
    assert.deepEqual(narrowed.allowedRights, ['read'])
    assert.equal(ctx.host.listRecoveryVerifiers().find((verifier) => verifier.revision === registered.revision).active, false)
    assert.throws(() => ctx.host.commitRecovery({
      challengeId: challenge.challengeId,
      nonce: challenge.nonce,
      verifierRevision: narrowed.revision,
      devicePublicKey: applicant.publicKey,
      useRights: ['read'],
    }), (error) => error.code === 'conflict' || error.code === 'forbidden')

    ctx.host.retireRecoveryVerifier({ revision: narrowed.revision })
    const [verifierState] = ctx.host.listRecoveryVerifiers()
    assert.equal(verifierState.active, false)
    assert.throws(() => ctx.host.issueRecoveryChallenge({
      verifierRevision: narrowed.revision,
      deviceId: 'another',
      devicePublicKey: generateKeypair().publicKey,
      useRights: ['read'],
      expiresAt: now + 60_000,
    }), (error) => error.code === 'conflict')
  } finally {
    await stop(ctx)
  }
})