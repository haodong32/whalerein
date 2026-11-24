import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { CommandService } from '../src/commands/service.js'
import { TestSource } from './support/source.js'
import { StateStore } from '../src/state/store.js'
import { codec } from '../src/protocol.js'
import { AdapterError, DefinitiveExecutionError, PluginError } from '../src/errors.js'
import { sha256Bytes } from '../src/security/digest.js'

const proto = codec()
let serial = 0

function makeRequest({ commandId = `cmd-${++serial}`, body = new Uint8Array([1, 2, 3]), kind = 'REQUEST_KIND_COMMAND', expiresAt = 10_000, digest = null } = {}) {
  return {
    request_id: `req-${serial}`,
    host_id: 'host-1',
    device_id: 'device-1',
    kind,
    command_id: commandId,
    payload_digest: digest ?? sha256Bytes(body),
    expires_at_unix_ms: expiresAt,
    trace_id: 'trace',
    protocol_version: 1,
    channel_id: 'channel-1',
    body,
  }
}

function makeService(options = {}) {
  let now = options.now ?? 1000
  const store = new StateStore({ path: ':memory:', now: () => now })
  const adapter = new TestSource({
    now: () => now,
    sessions: options.sessions ?? [{ sessionId: 's1', running: false }],
  })
  const order = []
  const accepted = []
  const events = options.events ?? {
    readHistory: async (sessionId) => ({session:{session_id:sessionId},batch:{from_cursor:'a',to_cursor:'b',batch_id:'b',changes:[]},cut_cursor:'b',complete:true}),
  }
  const service = new CommandService({
    store,
    adapter,
    events,
    hostId: options.hostId,
    now: () => now,
    onAccepted: async (request) => {
      order.push('accepted')
      accepted.push(request)
    },
    codec: options.codec ?? proto,
  })
  const requestApproval = adapter.requestApproval.bind(adapter)
  adapter.requestApproval = request => { adapter.setRunning(request.sessionId, true); return requestApproval(request) }
  const originalSend = adapter.sendMessage.bind(adapter)
  adapter.sendMessage = async (...args) => {
    order.push('execute')
    return originalSend(...args)
  }
  const originalInterrupt = adapter.interrupt.bind(adapter)
  adapter.interrupt = async (...args) => {
    order.push('interrupt')
    return originalInterrupt(...args)
  }
  return {
    service,
    store,
    adapter,
    events,
    order,
    accepted,
    advance: (ms) => { now += ms },
    now: () => now,
  }
}

test('when a control command is valid, the host acknowledges it before execution and stores its terminal payload', async () => {
  const { service, store, order } = makeService()
  const request = makeRequest()
  const result = await service.handle({ request, payload: { send_message: { session_id: 's1', text: 'hello' } } })
  assert.deepEqual(order, ['accepted', 'execute'])
  assert.equal(result.status, 'succeeded')
  assert.ok(result.resultPayload instanceof Uint8Array)
  const response = proto.decode('whalerein.v1.ResponsePayload', result.resultPayload)
  assert.equal(typeof response.command_accepted.message_id, 'string')
  const stored = store.getCommand('device-1', request.command_id)
  assert.equal(stored.status, 'succeeded')
  assert.deepEqual([...stored.resultPayload], [...result.resultPayload])
  assert.equal(stored.errorCode, null)
})

test('when the same command ID and digest are submitted twice, the host executes once and replays the result', async () => {
  const { service, adapter, order } = makeService()
  const request = makeRequest()
  const payload = { send_message: { session_id: 's1', text: 'hello' } }
  const first = await service.handle({ request, payload })
  const executes = order.filter((entry) => entry === 'execute').length
  const second = await service.handle({ request, payload })
  assert.equal(order.filter((entry) => entry === 'execute').length, executes)
  assert.equal(second.status, 'succeeded')
  assert.deepEqual([...second.resultPayload], [...first.resultPayload])
  assert.equal(adapter.messages.length, 1)
})

test('when a command ID is reused with a different digest, the host rejects it without execution', async () => {
  const { service, store, order } = makeService()
  const request = makeRequest({ commandId: 'fixed' })
  await service.handle({ request, payload: { send_message: { session_id: 's1', text: 'first' } } })
  const conflictBody = new Uint8Array([9, 9, 9])
  const conflict = makeRequest({
    commandId: 'fixed',
    body: conflictBody,
    digest: sha256Bytes(conflictBody),
  })
  const result = await service.handle({ request: conflict, payload: { send_message: { session_id: 's1', text: 'second' } } })
  assert.equal(result.status, 'rejected')
  assert.equal(result.errorCode, 'payload_digest_mismatch')
  assert.equal(order.filter((entry) => entry === 'execute').length, 1)
  assert.equal(store.getCommand('device-1', 'fixed').digest.length, 32)
})

test('when a command has an invalid digest, ID, expiry, or payload, the host rejects it without execution', async () => {
  const { service, store, order } = makeService()
  const payload = { send_message: { session_id: 's1', text: 'x' } }

  const missingId = await service.handle({ request: makeRequest({ commandId: '' }), payload })
  assert.equal(missingId.errorCode, 'invalid_argument')
  const missingDigest = await service.handle({ request: makeRequest({ digest: new Uint8Array(3) }), payload })
  assert.equal(missingDigest.errorCode, 'payload_digest_mismatch')
  const wrongBody = await service.handle({ request: makeRequest({ digest: new Uint8Array(32) }), payload })
  assert.equal(wrongBody.errorCode, 'payload_digest_mismatch')
  const expired = await service.handle({ request: makeRequest({ expiresAt: 500 }), payload })
  assert.equal(expired.status, 'rejected')
  assert.equal(expired.errorCode, 'expired')
  const unsupported = await service.handle({ request: makeRequest(), payload: { ping: { client_time_unix_ms: 1 } } })
  assert.equal(unsupported.errorCode, 'invalid_argument')
  assert.deepEqual(order, [])
  assert.equal(store.countCommands('accepted'), 0)
})

test('when recovery finds an accepted unfinished command, it marks it unknown without re-execution', async () => {
  const { service, store, order } = makeService()
  const request = makeRequest({ commandId: 'crash-1' })
  store.createOrGetCommand({
    deviceId: 'device-1',
    commandId: 'crash-1',
    requestId: request.request_id,
    digest: request.payload_digest,
    expiresAt: 10_000,
  })
  assert.equal(service.recover(), 1)
  assert.equal(service.recover(), 0)
  const result = await service.handle({ request, payload: { send_message: { session_id: 's1', text: 'x' } } })
  assert.equal(result.status, 'unknown')
  assert.equal(result.errorCode, 'unavailable')
  assert.deepEqual(order, [])
  assert.equal(store.getCommand('device-1', 'crash-1').status, 'unknown')
})

test('when host_accepted cannot be published, the command remains unknown and is not executed', async () => {
  const { service, store, adapter, order } = makeService()
  service.onAccepted = async () => { throw new Error('mqtt down') }
  const request = makeRequest({ commandId: 'publish-fail' })
  const result = await service.handle({ request, payload: { send_message: { session_id: 's1', text: 'x' } } })
  assert.equal(result.status, 'unknown')
  assert.equal(result.errorCode, 'unavailable')
  assert.deepEqual(order, [])
  assert.equal(store.getCommand('device-1', 'publish-fail').status, 'unknown')
  assert.equal(adapter.messages.length, 0)
})

test('when the adapter rejects before executing, the host stores and replays the definitive failure without side effects', async () => {
  const { service, store, adapter } = makeService()
  adapter.failNext('sendMessage', 'unavailable')
  const request = makeRequest({ commandId: 'fail-1' })
  const result = await service.handle({ request, payload: { send_message: { session_id: 's1', text: 'x' } } })
  assert.equal(result.status, 'failed')
  assert.equal(result.errorCode, 'unavailable')
  const response = proto.decode('whalerein.v1.ResponsePayload', result.resultPayload)
  assert.equal(response.error_code, 'unavailable')
  assert.equal(store.getCommand('device-1', 'fail-1').status, 'failed')
  const replay = await service.handle({ request, payload: { send_message: { session_id: 's1', text: 'x' } } })
  assert.equal(replay.status, 'failed')
  assert.deepEqual(replay.resultPayload, result.resultPayload)
  assert.equal(adapter.messages.length, 0)
})

test('when a native action commits but its reply is lost, the host preserves an unknown outcome and never executes the duplicate', async () => {
  const { service, store, adapter } = makeService()
  const execute = adapter.execute.bind(adapter)
  let calls = 0
  adapter.execute = async (...args) => {
    calls++
    await execute(...args)
    throw new Error('native reply lost')
  }
  const request = makeRequest({ commandId: 'lost-native-reply' })
  const payload = { send_message: { session_id: 's1', text: 'fixture action' } }
  const first = await service.handle({ request, payload })
  const replay = await service.handle({ request, payload })
  assert.equal(calls, 1)
  assert.equal(adapter.messages.length, 1)
  assert.equal(first.status, 'unknown', 'A missing reply cannot prove that the native action failed')
  assert.equal(replay.status, 'unknown')
  assert.equal(store.getCommand('device-1', request.command_id).status, 'unknown')
  const lookup = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { get_command_result: { command_id: request.command_id } } })
  assert.equal(lookup.response.command_result.status, 'unknown')
})

test('when reply encoding fails after native success, the host preserves a recoverable outcome instead of falsely reporting execution failure', async () => {
  const { service, store, adapter, order } = makeService({ codec: { encode() { throw new Error('response encoding failed') } } })
  const execute = adapter.execute.bind(adapter)
  let nativeResult
  adapter.execute = async (...args) => { nativeResult = await execute(...args); return nativeResult }
  const request = makeRequest({ commandId: 'reply-encoding-failure' })
  const payload = { send_message: { session_id: 's1', text: 'fixture action' } }
  const first = await service.handle({ request, payload })
  const replay = await service.handle({ request, payload })
  assert.deepEqual(order, ['accepted', 'execute'])
  assert.equal(adapter.messages.length, 1)
  // Execution is known to have succeeded. The contract permits conservative
  // uncertainty or a durably recoverable success, but never a false failure.
  assert.ok(['unknown', 'succeeded'].includes(first.status), 'Reply encoding failure must not misreport a committed action as failed or rejected')
  assert.equal(replay.status, first.status)
  const stored = store.getCommand('device-1', request.command_id)
  assert.equal(stored.status, first.status)
  const lookup = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { get_command_result: { command_id: request.command_id } } })
  assert.equal(lookup.response.command_result.status, first.status)
  if (first.status === 'succeeded') {
    assert.deepEqual(proto.decode('whalerein.v1.ResponsePayload', first.resultPayload), nativeResult)
    assert.deepEqual(stored.resultPayload, first.resultPayload)
    assert.deepEqual(replay.resultPayload, first.resultPayload)
    assert.deepEqual(lookup.response.command_result.result_payload, first.resultPayload)
  }
})

test('a proven lifecycle outcome survives a failed terminal write and recovery reports it', async () => {
  const { service, store, adapter } = makeService()
  adapter.describeHostCapabilities = async () => ({ session_create: true })
  const setCommandStatus = store.setCommandStatus.bind(store)
  store.setCommandStatus = (fields) => {
    if (fields.status === 'succeeded') throw new Error('terminal write failed')
    return setCommandStatus(fields)
  }
  adapter.executeHost = async () => ({ entered: true, identity: { session_id: 'created-proven' } })
  const request = makeRequest({ commandId: 'proven-outcome' })
  const first = await service.handle({ request, payload: { create_session: {} } })
  assert.equal(first.status, 'unknown')
  const operation = store.getLifecycleOperation('device-1', request.command_id)
  assert.equal(operation?.outcome, 'READY')
  assert.equal(operation?.sessionId, 'created-proven')
  const lookup = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { get_command_result: { command_id: request.command_id } } })
  const receipt = proto.decode('whalerein.v1.ResponsePayload', lookup.response.command_result.result_payload).lifecycle_receipt
  assert.equal(receipt.disposition, 'READY')
  assert.equal(receipt.session_id, 'created-proven')
})

test('when an interrupt request is valid, the adapter executes it as a control command', async () => {
  const { service, order } = makeService()
  const result = await service.handle({ request: makeRequest(), payload: { interrupt: { session_id: 's1' } } })
  assert.equal(result.status, 'succeeded')
  assert.deepEqual(order, ['accepted', 'interrupt'])
})

test('when approval fields are valid, it resolves once; invalid digest, version, expiry, or state is rejected', async () => {
  const { service, adapter, store, advance } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 1000, reason: 'r' })
  const resolve = (decision, extra = {}) => ({
    resolve_approval: {
      approval_id: extra.approvalId ?? approval.approvalId,
      decision,
      expected_digest: extra.digest ?? approval.digest,
      expected_version: extra.version ?? approval.version,
    },
  })
  const unknown = await service.handle({ request: makeRequest(), payload: resolve('APPROVAL_DECISION_APPROVE', { approvalId: 'missing' }) })
  assert.equal(unknown.status, 'failed')
  assert.equal(unknown.errorCode, 'approval_unavailable')
  const badDecision = await service.handle({ request: makeRequest(), payload: resolve('APPROVAL_DECISION_UNSPECIFIED') })
  assert.equal(badDecision.errorCode, 'invalid_argument')
  const badDigest = await service.handle({ request: makeRequest(), payload: resolve('APPROVAL_DECISION_APPROVE', { digest: new Uint8Array(32) }) })
  assert.equal(badDigest.errorCode, 'approval_version_mismatch')
  const badVersion = await service.handle({ request: makeRequest(), payload: resolve('APPROVAL_DECISION_APPROVE', { version: 99 }) })
  assert.equal(badVersion.errorCode, 'approval_version_mismatch')
  assert.equal(store.countCommands('accepted'), 0)

  const approved = await service.handle({ request: makeRequest(), payload: resolve('APPROVAL_DECISION_APPROVE') })
  assert.equal(approved.status, 'succeeded')
  assert.equal((await adapter.getApproval(approval.approvalId)).state, 'approved')

  const second = adapter.requestApproval({ sessionId: 's1', ttlMs: 1000 })
  await adapter.simulateResolution(second.approvalId, 'reject')
  const already = await service.handle({ request: makeRequest(), payload: {
    resolve_approval: { approval_id: second.approvalId, decision: 'APPROVAL_DECISION_APPROVE', expected_digest: second.digest, expected_version: second.version },
  } })
  assert.equal(already.errorCode, 'approval_already_resolved')

  const expiring = adapter.requestApproval({ sessionId: 's1', ttlMs: 10 })
  advance(20)
  const expired = await service.handle({ request: makeRequest(), payload: {
    resolve_approval: { approval_id: expiring.approvalId, decision: 'APPROVAL_DECISION_REJECT', expected_digest: expiring.digest, expected_version: expiring.version },
  } })
  assert.equal(expired.errorCode, 'approval_expired')
})

test('when a consumed approval command is repeated, the host replays its stored terminal result', async () => {
  const { service, adapter } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 1000 })
  const request = makeRequest({ commandId: 'approval-replay' })
  const payload = {
    resolve_approval: {
      approval_id: approval.approvalId,
      decision: 'APPROVAL_DECISION_APPROVE',
      expected_digest: approval.digest,
      expected_version: approval.version,
    },
  }
  const first = await service.handle({ request, payload })
  assert.equal(first.status, 'succeeded')
  const replay = await service.handle({ request, payload })
  assert.equal(replay.status, 'succeeded')
  assert.deepEqual([...replay.resultPayload], [...first.resultPayload])
})

test('when an approval session ends after acceptance, resolution fails closed', async () => {
  const { service, adapter, store } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 1000 })
  adapter.setRunning('s1', false)
  const result = await service.handle({
    request: makeRequest({ commandId: 'approval-stopped' }),
    payload: {
      resolve_approval: {
        approval_id: approval.approvalId,
        decision: 'APPROVAL_DECISION_APPROVE',
        expected_digest: approval.digest,
        expected_version: approval.version,
      },
    },
  })
  assert.equal(result.status, 'failed')
  assert.equal(result.errorCode, 'approval_unavailable')
  assert.equal(store.getCommand('device-1', 'approval-stopped').status, 'failed')
})

test('when supported query kinds arrive, the host returns session, snapshot, event, approval, result, and ping responses', async () => {
  const events = {
    readHistory: async (sessionId) => {
      if(sessionId==='missing')throw new PluginError('snapshot_required','gap')
      return {session:{session_id:sessionId},batch:{from_cursor:'a',to_cursor:'b',batch_id:'b',changes:[]},cut_cursor:'b',complete:true}
    },
  }
  const { service, store, adapter } = makeService({ events })
  const list = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { list_sessions: {} } })
  assert.equal(list.status, 'succeeded')
  assert.equal(list.response.session_list.sessions[0].session_id, 's1')

  const snapshot = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { get_session: { session_id: 's1' } } })
  assert.equal(snapshot.response.session_info.session_id, 's1')

  const page = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { read_history: { session_id: 's1', snapshot: true } } })
  assert.equal(page.status, 'succeeded')
  const gap = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { read_history: { session_id: 'missing' } } })
  assert.equal(gap.status, 'failed')
  assert.equal(gap.errorCode, 'snapshot_required')

  const approval = adapter.requestApproval({ sessionId: 's1' })
  const got = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { get_approval: { approval_id: approval.approvalId } } })
  assert.equal(got.response.approval.approval_id, approval.approvalId)
  const missingApproval = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { get_approval: { approval_id: 'missing' } } })
  assert.equal(missingApproval.status, 'failed')
  assert.equal(missingApproval.errorCode, 'not_found')

  const ping = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { ping: { client_time_unix_ms: 1 } } })
  assert.equal(ping.response.pong.host_time_unix_ms, 1000)

  const unknownPayload = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: {} })
  assert.equal(unknownPayload.status, 'failed')
  assert.equal(unknownPayload.errorCode, 'invalid_argument')

  adapter.failNext('listSessions', 'unavailable')
  const failedList = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { list_sessions: {} } })
  assert.equal(failedList.status, 'failed')
  assert.equal(failedList.errorCode, 'unavailable')
})

test('when GetCommandResult receives an absent or completed command ID, it distinguishes missing state and returns the exact stored outcome', async () => {
  const { service, store } = makeService()
  const missing = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { get_command_result: { command_id: 'none' } } })
  assert.equal(missing.response.command_result.found, false)
  const request = makeRequest({ commandId: 'result-1' })
  const completed = await service.handle({ request, payload: { send_message: { session_id: 's1', text: 'x' } } })
  const found = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { get_command_result: { command_id: 'result-1' } } })
  assert.equal(found.response.command_result.found, true)
  assert.equal(found.response.command_result.status, 'succeeded')
  assert.deepEqual(found.response.command_result.result_payload, completed.resultPayload)
  const foreign = await service.handle({ request: { ...makeRequest({ kind: 'REQUEST_KIND_QUERY' }), device_id: 'another-device' }, payload: { get_command_result: { command_id: 'result-1' } } })
  assert.equal(foreign.response.command_result.found, false)
  store.setCommandStatus({ deviceId: 'device-1', commandId: 'result-1', status: 'failed', errorCode: 'internal' })
  const failed = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { get_command_result: { command_id: 'result-1' } } })
  assert.equal(failed.response.command_result.status, 'failed')
  assert.equal(failed.response.command_result.error_code, 'internal')
})

test('when a handshake reaches the internal command dispatcher, it rejects invalid_argument without admitting or executing a control', async () => {
  const { service, store, order } = makeService()
  await assert.rejects(service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_HANDSHAKE' }), payload: {} }), (error) => error.code === 'invalid_argument')
  assert.deepEqual(order, [])
  assert.equal(store.countCommands('accepted'), 0)
})

test('when GetCommandResult reads an admitted unfinished command, it reports host_accepted without a terminal payload', async () => {
  const { service, store } = makeService()
  store.createOrGetCommand({ deviceId: 'device-1', commandId: 'pending-result', requestId: 'pending-request', digest: new Uint8Array(32), expiresAt: 10_000 })
  const result = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { get_command_result: { command_id: 'pending-result' } } })
  assert.equal(result.response.command_result.found, true)
  assert.equal(result.response.command_result.status, 'host_accepted')
  assert.equal(result.response.command_result.result_payload.length, 0)
})

for (const [name, failure] of [
  ['adapter unavailable', new AdapterError('unavailable')],
  ['adapter invalid_argument without execution evidence', new AdapterError('invalid_argument')],
  ['plugin expiry without execution evidence', new PluginError('expired')],
  ['untyped exception', new Error('native reply unavailable')],
]) {
  test(`when execute throws ${name}, the host records unknown rather than inferring failure from an error code`, async () => {
    const { service, adapter, store } = makeService()
    let calls = 0
    adapter.execute = async () => { calls++; throw failure }
    const request = makeRequest()
    const payload = { send_message: { session_id: 's1', text: 'fixture action' } }
    assert.equal((await service.handle({ request, payload })).status, 'unknown')
    assert.equal((await service.handle({ request, payload })).status, 'unknown')
    assert.equal(store.getCommand('device-1', request.command_id).status, 'unknown')
    assert.equal(calls, 1)
  })
}

test('when a source explicitly confirms failure, the host journals a stable failed outcome and replays it without invoking again', async () => {
  const { service, adapter, store } = makeService()
  let calls = 0
  adapter.execute = async () => { calls++; throw new DefinitiveExecutionError('conflict') }
  const request = makeRequest()
  const payload = { send_message: { session_id: 's1', text: 'fixture action' } }
  const first = await service.handle({ request, payload })
  const replay = await service.handle({ request, payload })
  assert.equal(first.status, 'failed')
  assert.equal(first.errorCode, 'conflict')
  assert.equal(proto.decode('whalerein.v1.ResponsePayload', first.resultPayload).error_code, 'conflict')
  assert.deepEqual(replay.resultPayload, first.resultPayload)
  assert.equal(store.getCommand('device-1', request.command_id).status, 'failed')
  assert.equal(calls, 1)
})

for (const mode of ['throws once', 'updates no row once']) {
  test(`when terminal journaling ${mode}, the committed action remains unknown and the duplicate cannot execute`, async () => {
    const { service, store, order } = makeService()
    const update = store.setCommandStatus.bind(store)
    let writes = 0
    store.setCommandStatus = fields => {
      if (++writes === 1) {
        if (mode === 'throws once') throw new Error('journal unavailable')
        return false
      }
      return update(fields)
    }
    const request = makeRequest()
    const payload = { send_message: { session_id: 's1', text: 'fixture action' } }
    assert.equal((await service.handle({ request, payload })).status, 'unknown')
    assert.equal((await service.handle({ request, payload })).status, 'unknown')
    assert.equal(store.getCommand('device-1', request.command_id).status, 'unknown')
    assert.deepEqual(order, ['accepted', 'execute'])
  })
}

test('when all completion writes fail, durable admission prevents re-execution and recovery later marks it unknown', async () => {
  const { service, store, order } = makeService()
  const update = store.setCommandStatus.bind(store)
  store.setCommandStatus = () => { throw new Error('journal unavailable') }
  const request = makeRequest()
  const payload = { send_message: { session_id: 's1', text: 'fixture action' } }
  assert.equal((await service.handle({ request, payload })).status, 'unknown')
  assert.equal(store.getCommand('device-1', request.command_id).status, 'accepted')
  assert.equal((await service.handle({ request, payload })).status, 'host_accepted')
  assert.deepEqual(order, ['accepted', 'execute'])
  store.setCommandStatus = update
  assert.equal(service.recover(), 1)
  assert.equal((await service.handle({ request, payload })).status, 'unknown')
  assert.deepEqual(order, ['accepted', 'execute'])
})

test('when command admission cannot be persisted, the source is never invoked and acceptance is never published', async () => {
  const { service, store, order } = makeService()
  store.createOrGetCommand = () => { throw new PluginError('internal') }
  await assert.rejects(service.handle({ request: makeRequest(), payload: { send_message: { session_id: 's1', text: 'fixture action' } } }), error => error.code === 'internal')
  assert.deepEqual(order, [])
})

test('when a terminal write commits before its caller observes an error, the host preserves the recorded success and exact replay payload', async () => {
  const { service, store, order } = makeService()
  const update = store.setCommandStatus.bind(store)
  let writes = 0
  store.setCommandStatus = fields => {
    const result = update(fields)
    if (++writes === 1) throw new Error('write acknowledgement lost')
    return result
  }
  const request = makeRequest()
  const payload = { send_message: { session_id: 's1', text: 'fixture action' } }
  const first = await service.handle({ request, payload })
  const replay = await service.handle({ request, payload })
  assert.equal(first.status, 'succeeded')
  assert.deepEqual(replay.resultPayload, first.resultPayload)
  assert.deepEqual(store.getCommand('device-1', request.command_id).resultPayload, first.resultPayload)
  assert.deepEqual(order, ['accepted', 'execute'])
})


test('when success diagnostics throw, the host still returns its durably recorded success without executing a duplicate', async () => {
  const { service, store, order } = makeService()
  service.logger = { info() { throw new DefinitiveExecutionError('internal') } }
  const request = makeRequest()
  const payload = { send_message: { session_id: 's1', text: 'fixture action' } }
  const first = await service.handle({ request, payload })
  const replay = await service.handle({ request, payload })
  assert.equal(first.status, 'succeeded')
  assert.equal(store.getCommand('device-1', request.command_id).status, 'succeeded')
  assert.deepEqual(replay.resultPayload, first.resultPayload)
  assert.deepEqual(order, ['accepted', 'execute'])
})

test('when acceptance publication and its diagnostics both fail, the command stays unknown and never invokes the source', async () => {
  const { service, store, order } = makeService()
  service.onAccepted = async () => { throw new Error('publication unavailable') }
  service.logger = { error() { throw new Error('sink unavailable') } }
  const request = makeRequest()
  const result = await service.handle({ request, payload: { send_message: { session_id: 's1', text: 'fixture action' } } })
  assert.equal(result.status, 'unknown')
  assert.equal(store.getCommand('device-1', request.command_id).status, 'unknown')
  assert.deepEqual(order, [])
})

test('delivery operation identity is scoped to host, device and command without including text', async () => {
  const { service, adapter } = makeService()
  const identities = []
  adapter.execute = async (_session, _command, context) => {
    identities.push(context.operationId)
    return {command_accepted: {delivery_id: context.operationId}}
  }
  const payload = {send_message: {session_id: 's1', text: 'synthetic'}}
  const first = makeRequest({commandId: 'same'})
  const accepted = await service.handle({request: first, payload})
  const replayed = await service.handle({request: first, payload})
  assert.equal(identities.length, 1)
  assert.deepEqual([...accepted.resultPayload], [...replayed.resultPayload])
  assert.equal(proto.decode('whalerein.v1.ResponsePayload', accepted.resultPayload).command_accepted.delivery_id, identities[0])
  assert.match(identities[0], /^[0-9a-f]{64}$/)
  const second = {...makeRequest({commandId: 'same'}), device_id: 'device-2'}
  await service.handle({request: second, payload})
  assert.notEqual(identities[0], identities[1])
  const third = {...makeRequest({commandId: 'other'}), host_id: 'host-2'}
  await service.handle({request: third, payload})
  assert.notEqual(identities[1], identities[2])
})

test('delivery diagnostics omit synthetic message contents and transport body', async () => {
  const { service } = makeService()
  const records = []
  service.logger = Object.fromEntries(['info', 'warn', 'error'].map(level => [level, (...args) => records.push(args)]))
  const body = new TextEncoder().encode('transport-fixture-secret')
  const request = makeRequest({body})
  const result = await service.handle({request, payload: {send_message: {session_id: 's1', text: 'content-fixture-secret'}}})
  assert.equal(result.status, 'succeeded')
  const diagnostics = JSON.stringify(records)
  assert.ok(records.length)
  assert.equal(diagnostics.includes('content-fixture-secret'), false)
  assert.equal(diagnostics.includes('transport-fixture-secret'), false)
})

test('concurrent delivery replay does not invoke the source twice', async () => {
  const { service, adapter } = makeService()
  let release
  let entered
  const started = new Promise(resolve => { entered = resolve })
  const held = new Promise(resolve => { release = resolve })
  let calls = 0
  adapter.execute = async (_session, _command, context) => {
    calls++
    entered()
    await held
    return {command_accepted: {delivery_id: context.operationId}}
  }
  const request = makeRequest({commandId: 'concurrent-delivery'})
  const payload = {send_message: {session_id: 's1', text: 'fixture'}}
  const first = service.handle({request, payload})
  await started
  const pending = await service.handle({request, payload})
  assert.equal(pending.status, 'host_accepted')
  assert.equal(calls, 1)
  release()
  const completed = await first
  const replay = await service.handle({request, payload})
  assert.equal(completed.status, 'succeeded')
  assert.deepEqual(replay.resultPayload, completed.resultPayload)
  assert.equal(calls, 1)
})

test('model operations default to unsupported and never call the source', async () => {
  const { service, adapter } = makeService()
  let calls = 0
  adapter.listModels = async () => { calls++ }
  adapter.readSessionModel = async () => { calls++ }
  const query = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  assert.equal((await service.handle({ request: query, payload: { list_models: {} } })).errorCode, 'unsupported_capability')
  assert.equal((await service.handle({ request: query, payload: { read_session_model: { session_id: 's1' } } })).errorCode, 'unsupported_capability')
  const result = await service.handle({ request: makeRequest(), payload: { select_session_model: { session_id: 's1', provider: 'p', model: 'm' } } })
  assert.equal(result.errorCode, 'unsupported_capability')
  assert.equal(calls, 0)
})

test('model selection uses the durable command journal and retains its original receipt', async () => {
  const { service, adapter } = makeService()
  adapter.describe = () => ({ capabilities: ['model-select'] })
  let calls = 0
  adapter.execute = async () => { calls++; return { model_selection_receipt: { selected: { provider: 'p', model: 'm' }, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' } } }
  const request = makeRequest()
  const payload = { select_session_model: { session_id: 's1', provider: 'p', model: 'm' } }
  const first = await service.handle({ request, payload })
  const second = await service.handle({ request, payload })
  assert.equal(calls, 1)
  assert.deepEqual(second.resultPayload, first.resultPayload)
  assert.equal(proto.decode('whalerein.v1.ResponsePayload', first.resultPayload).model_selection_receipt.default_persistence, 'unconfirmed')
})

test('enabled model reads delegate separately and preserve source observations', async () => {
  const { service, adapter } = makeService()
  adapter.describeHostCapabilities = async () => ({ model_catalog: true, model_read: true })
  adapter.listModels = async () => ({ groups: [{ id: 'provider', models: [] }], failures: [] })
  adapter.readSessionModel = async id => ({ pending: null, last_used: null, default_selection: { provider: 'provider', model: id } })
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  assert.equal((await service.handle({ request, payload: { list_models: {} } })).response.model_catalog.groups[0].id, 'provider')
  assert.equal((await service.handle({ request, payload: { read_session_model: { session_id: 's1' } } })).response.session_model.default_selection.model, 's1')
})

test('uncertain model selection is never re-executed on command replay', async () => {
  const { service, adapter } = makeService()
  adapter.describe = () => ({ capabilities: ['model-select'] })
  let calls = 0
  adapter.execute = async () => { calls++; throw new AdapterError('unavailable') }
  const request = makeRequest()
  const payload = { select_session_model: { session_id: 's1', provider: 'p', model: 'm' } }
  assert.equal((await service.handle({ request, payload })).status, 'unknown')
  assert.equal((await service.handle({ request, payload })).status, 'unknown')
  assert.equal(calls, 1)
})

test('host capability query reports independent model operations and defaults missing fields to unsupported', async () => {
  const { service, adapter } = makeService()
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  assert.deepEqual((await service.handle({ request, payload: { get_host_capabilities: {} } })).response.host_capabilities, {})
  adapter.describeHostCapabilities = async () => ({ model_catalog: true, model_read: false })
  const result = await service.handle({ request, payload: { get_host_capabilities: {} } })
  assert.equal(result.response.host_capabilities.model_catalog, true)
  assert.equal(result.response.host_capabilities.model_read, false)
})

test('concurrent repeated model commands execute the source once and reject a changed digest', async () => {
  const { service, adapter } = makeService()
  adapter.describe = () => ({ capabilities: ['model-select'] })
  let release
  let calls = 0
  adapter.execute = async () => {
    calls++
    await new Promise(resolve => { release = resolve })
    return { model_selection_receipt: { selected: { provider: 'p', model: 'm' }, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' } }
  }
  const request = makeRequest()
  const payload = { select_session_model: { session_id: 's1', provider: 'p', model: 'm' } }
  const first = service.handle({ request, payload })
  while (!release) await new Promise(resolve => setImmediate(resolve))
  assert.equal((await service.handle({ request, payload })).status, 'host_accepted')
  const differentBody = new Uint8Array([7])
  const changed = makeRequest({ commandId: request.command_id, body: differentBody })
  assert.equal((await service.handle({ request: changed, payload })).errorCode, 'payload_digest_mismatch')
  release()
  assert.equal((await first).status, 'succeeded')
  assert.equal((await service.handle({ request, payload })).status, 'succeeded')
  assert.equal(calls, 1)
})

test('wait queries enforce capability and lifetime before invoking the source', async () => {
  const { service, adapter } = makeService()
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  const payload = { wait_session: { session_id: 's1', wait_id: 'w1', timeout_ms: 10000 } }
  assert.equal((await service.handle({ request, payload })).errorCode, 'unsupported_capability')
  assert.equal((await service.handle({ request, payload: { read_wait_state: { session_id: 's1' } } })).errorCode, 'unsupported_capability')
  adapter.describe = () => ({ capabilities: ['wait'] })
  adapter.readWaitState = async () => ({ active: false, running: false, queueAvailable: false, pendingInput: ['approval'] })
  const passive = await service.handle({ request, payload: { read_wait_state: { session_id: 's1' } } })
  assert.deepEqual(passive.response.session_wait_state_result.state.pending_input, ['INPUT_APPROVAL'])
  let actualTimeout
  adapter.waitSession = async (_id, timeout) => {
    actualTimeout = timeout
    return { reason: 'idle', state: { active: false, running: false, queueAvailable: false, pendingInput: [] } }
  }
  const result = await service.handle({ request, payload })
  assert.equal(actualTimeout, 8000)
  assert.equal(result.response.session_wait_result.reason, 'WAIT_IDLE')
  assert.equal(result.response.session_wait_result.wait_id, 'w1')
  assert.equal((await service.handle({ request: { ...request, expires_at_unix_ms: 1500 }, payload })).errorCode, 'expired')
  assert.equal((await service.handle({ request, payload: { wait_session: { ...payload.wait_session, timeout_ms: 0 } } })).errorCode, 'invalid_argument')
  assert.equal((await service.handle({ request, payload: { wait_session: { ...payload.wait_session, timeout_ms: 2147483648 } } })).errorCode, 'invalid_argument')
  assert.equal(service.waits.size, 0)
})

test('wait cancellation is scoped and releases even an uncooperative source', async () => {
  const { service, adapter } = makeService()
  adapter.describe = () => ({ capabilities: ['wait'] })
  const signals = []
  adapter.waitSession = async (_id, _timeout, signal) => {
    signals.push(signal)
    return new Promise(() => {})
  }
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  const observe = wait_id => service.handle({ request, payload: { wait_session: { session_id: 's1', wait_id, timeout_ms: 1000 } } })
  const first = observe('w1')
  const second = observe('w2')
  const other = { ...request, channel_id: 'other' }
  await service.handle({ request: other, payload: { cancel_session_wait: { session_id: 's1', wait_id: 'w1' } } })
  assert.equal(signals[0].aborted, false)
  await service.handle({ request, payload: { cancel_session_wait: { session_id: 's1', wait_id: 'w1' } } })
  assert.equal((await first).errorCode, 'unavailable')
  assert.equal(signals[1].aborted, false)
  service.cancelChannel('device-1', 'channel-1', 'channel_replaced')
  assert.equal((await second).errorCode, 'channel_replaced')
  assert.equal(service.waits.size, 0)
})

test('a wait result after its request expires is rejected and its observation is released', async () => {
  const { service, adapter, advance } = makeService()
  adapter.describe = () => ({ capabilities: ['wait'] })
  adapter.waitSession = async () => {
    advance(9000)
    return { reason: 'idle', state: { active: false, running: false, queueAvailable: false, pendingInput: [] } }
  }
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  const result = await service.handle({ request, payload: { wait_session: { session_id: 's1', wait_id: 'w1', timeout_ms: 1000 } } })
  assert.equal(result.errorCode, 'expired')
  assert.equal(service.waits.size, 0)
})

test('host-scoped creation journals a private-free receipt and replays without a second call', async () => {
  const {service, adapter, store} = makeService()
  let calls = 0
  adapter.describeHostCapabilities = async () => ({session_create:true})
  adapter.executeHost = async () => { calls++; return {lifecycle_receipt:{session_id:'new-id',operation:'create',disposition:'READY', cwd:'private',workspace_id:'private'}} }
  const request = makeRequest()
  const payload = {create_session:{cwd:'/synthetic'}}
  const first = await service.handle({request,payload})
  assert.equal(first.status,'succeeded')
  const decoded = proto.decode('whalerein.v1.ResponsePayload', first.resultPayload)
  assert.deepEqual(decoded.lifecycle_receipt, {session_id:'new-id',operation:'create',disposition:'READY'})
  assert.equal(Buffer.from(store.getCommand('device-1',request.command_id).resultPayload).includes(Buffer.from('private')),false)
  const second = await service.handle({request,payload})
  assert.deepEqual(second.resultPayload,first.resultPayload)
  assert.equal(calls,1)
})

test('host-scoped creation rejects missing capability and maps an invalid post-execution receipt to unknown', async () => {
  const {service,adapter} = makeService()
  let calls = 0
  adapter.describeHostCapabilities = async () => ({session_create:false})
  adapter.executeHost = async () => {calls++; return {lifecycle_receipt:{session_id:'',operation:'create',disposition:'READY'}}}
  assert.equal((await service.handle({request:makeRequest(),payload:{create_session:{}}})).errorCode,'unsupported_capability')
  assert.equal(calls,0)
  adapter.describeHostCapabilities = async () => ({session_create:true})
  const result = await service.handle({request:makeRequest(),payload:{create_session:{}}})
  assert.equal(result.status,'succeeded')
  const receipt = proto.decode('whalerein.v1.ResponsePayload', result.resultPayload).lifecycle_receipt
  assert.equal(receipt.disposition,'UNKNOWN')
  assert.equal(receipt.session_id, undefined)
  assert.equal(receipt.reconcile_mode,'OPERATION_ID')
  assert.ok(receipt.operation_id)
})

function enableQueue(adapter) {
  adapter.sessions.get('s1').sourceCapabilities = {pending_queue_read:true,pending_queue_edit:true,pending_queue_remove:true,pending_queue_steer:true}
}

test('pending queue query and mutation keep source authority and replay a single receipt', async () => {
  const {service, adapter} = makeService()
  enableQueue(adapter)
  const queue = {session_id:'s1',available:true,running:true,items:[]}
  adapter.readPendingQueue = async session => { assert.equal(session,'s1'); return queue }
  const query = await service.handle({request:makeRequest({kind:'REQUEST_KIND_QUERY'}),payload:{read_pending_queue:{session_id:'s1'}}})
  assert.deepEqual(query.response.pending_queue,queue)
  let calls=0
  adapter.execute = async (session, payload, context) => { calls++; assert.equal(session,'s1'); assert.equal(payload.update_pending_queue.item_id,'q1'); assert.match(context.operationId,/^[a-f0-9]{64}$/); assert.equal(context.expiresAt,10000); return {} }
  const request = makeRequest()
  const payload = {update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}
  const first = await service.handle({request,payload})
  const second = await service.handle({request,payload})
  assert.equal(first.status,'succeeded')
  assert.equal(second.status,'succeeded')
  assert.equal(calls,1)
})

test('question operations require separately advertised capabilities and never replay native calls', async () => {
  const { service, adapter } = makeService()
  let capabilities = {}
  let calls = 0
  adapter.getSession = async id => ({ sessionId: id, sourceCapabilities: capabilities })
  adapter.listQuestionPage = async id => ({ batches: [{ session_id: id, batch_id: 'b' }], next_cursor: '' })
  adapter.getQuestion = async (id, batch) => batch === 'b' ? { session_id: id, batch_id: batch } : null
  adapter.execute = async () => { calls++; return { question_receipt: { batch_id: 'b', phase: 'QUESTION_REPLY_QUEUED' } } }
  const query = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  assert.equal((await service.handle({ request: query, payload: { list_questions: { session_id: 's1' } } })).errorCode, 'unsupported_capability')
  assert.equal((await service.handle({ request: query, payload: { list_questions: { session_id: '' } } })).errorCode, 'invalid_argument')
  capabilities = { questions_read: true }
  assert.equal((await service.handle({ request: query, payload: { list_questions: { session_id: 's1' } } })).response.question_page.batches[0].batch_id, 'b')
  assert.equal((await service.handle({ request: query, payload: { get_question: { session_id: 's1', batch_id: 'b' } } })).response.question_batch.batch_id, 'b')
  assert.equal((await service.handle({ request: query, payload: { get_question: { session_id: 's1', batch_id: 'missing' } } })).errorCode, 'not_found')
  const payload = { answer_question: { session_id: 's1', batch_id: 'b', expected_phase: 'QUESTION_CONTINUED' } }
  assert.equal((await service.handle({ request: makeRequest(), payload })).errorCode, 'unsupported_capability')
  capabilities.questions_answer_continued = true
  const request = makeRequest()
  assert.equal((await service.handle({ request, payload })).status, 'succeeded')
  capabilities = {}
  assert.equal((await service.handle({ request, payload })).status, 'succeeded')
  assert.equal(calls, 1)
  const invalid = { answer_question: { session_id: 's1', expected_phase: 'QUESTION_REPLY_QUEUED' } }
  assert.equal((await service.handle({ request: makeRequest(), payload: invalid })).errorCode, 'invalid_argument')
})

test('session-observation/AC-14.watch interests are required and forwarded explicitly', async () => {
  const calls=[]
  const { service } = makeService({ events: {
    readHistory: async () => ({}),
    watch: async (...args) => { calls.push(args); return { watch_id: 'w1', state: { session: { host_id: 'host-1', session_id: 's1' } }, lease_ms: 1 } },
    versions: async () => ({ checks: [] }),
    unwatch() {},
  } })
  const query = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  for (const watch_session of [
    { session_id: 's1' },
    { session_id: 's1', interests: {} },
    { session_id: 's1', interests: { conversation: false, questions: false } },
    { session_id: 's1', interests: { conversation: 'yes', questions: false } },
    { session_id: '', interests: { conversation: true, questions: false } },
  ]) {
    assert.equal((await service.handle({ request: query, payload: { watch_session } })).errorCode, 'invalid_argument')
  }
  assert.equal(calls.length, 0, 'invalid interests never reach source registration')
  const accepted = await service.handle({ request: query, payload: {
    watch_session: { session_id: 's1', interests: { conversation: false, questions: true } } } })
  assert.equal(accepted.status, 'succeeded')
  assert.deepEqual(calls[0][4], { conversation: false, questions: true })
  assert.equal((await service.handle({ request: query, payload: { check_versions: { session_id: 's1' } } })).status, 'succeeded')
  assert.equal((await service.handle({ request: query, payload: { refresh_watches: { watch_ids: ['w1'] } } })).status, 'failed', 'the retired renewal payload is no longer understood')
})

test('fork journals only confirmed identities and replays one native invocation', async () => {
  const {service, adapter, store} = makeService()
  let calls = 0
  adapter.describeHostCapabilities = async () => ({session_fork:true})
  adapter.getSession = async () => ({sourceCapabilities:{session_fork:true}})
  adapter.execute = async (session, payload) => {
    calls++
    assert.equal(session, 'parent')
    assert.deepEqual(payload, {fork_session:{session_id:'parent'}})
    return {lifecycle_receipt:{session_id:'child',parent_session_id:'parent',operation:'fork',disposition:'READY',cwd:'private'}}
  }
  const request = makeRequest()
  const payload = {fork_session:{session_id:'parent'}}
  const first = await service.handle({request,payload})
  const replay = await service.handle({request,payload})
  assert.equal(first.status,'succeeded')
  assert.deepEqual(first.resultPayload,replay.resultPayload)
  assert.equal(calls,1)
  assert.equal(Buffer.from(store.getCommand('device-1',request.command_id).resultPayload).includes(Buffer.from('private')),false)
  assert.deepEqual(proto.decode('whalerein.v1.ResponsePayload', first.resultPayload).lifecycle_receipt, {session_id:'child',parent_session_id:'parent',operation:'fork',disposition:'READY'})
})

test('fork requires both host and source capabilities and maps an invalid receipt to unknown', async () => {
  const {service, adapter} = makeService()
  let calls = 0
  adapter.describeHostCapabilities = async () => ({session_fork:true})
  adapter.getSession = async () => ({sourceCapabilities:{session_fork:false}})
  adapter.execute = async () => {calls++; return {lifecycle_receipt:{session_id:'child',parent_session_id:'wrong',operation:'fork',disposition:'READY'}}}
  assert.equal((await service.handle({request:makeRequest(),payload:{fork_session:{session_id:'parent'}}})).errorCode,'unsupported_capability')
  assert.equal(calls,0)
  adapter.getSession = async () => ({sourceCapabilities:{session_fork:true}})
  const result = await service.handle({request:makeRequest(),payload:{fork_session:{session_id:'parent'}}})
  assert.equal(result.status,'succeeded')
  const receipt = proto.decode('whalerein.v1.ResponsePayload', result.resultPayload).lifecycle_receipt
  assert.equal(receipt.disposition,'UNKNOWN')
  assert.equal(receipt.session_id, undefined)
  assert.equal(receipt.reconcile_mode,'PARENT_SESSION_ID')
  assert.equal(receipt.reconcile_key,'parent')
  assert.equal(calls,1)
})

test('host shutdown releases observations from all channels without interrupting agents', async () => {
  const { service, adapter, order } = makeService()
  adapter.describe = () => ({ capabilities: ['wait'] })
  adapter.waitSession = async () => new Promise(() => {})
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  const observe = channel_id => service.handle({ request: { ...request, channel_id },
    payload: { wait_session: { session_id: 's1', wait_id: 'w1', timeout_ms: 1000 } } })
  const first = observe('channel-1')
  const second = observe('channel-2')
  service.closeWaits()
  assert.equal((await first).errorCode, 'unavailable')
  assert.equal((await second).errorCode, 'unavailable')
  assert.equal(service.waits.size, 0)
  assert.deepEqual(order, [])
})


test('expired abandoned wait is released even when the source ignores cancellation', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] })
  const { service, adapter, advance } = makeService()
  adapter.describe = () => ({ capabilities: ['wait'] })
  adapter.waitSession = async () => new Promise(() => {})
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY', expiresAt: 2100 })
  const pending = service.handle({ request, payload: { wait_session: { session_id: 's1', wait_id: 'w1', timeout_ms: 50 } } })
  advance(1100)
  t.mock.timers.tick(1100)
  assert.equal((await pending).errorCode, 'expired')
  assert.equal(service.waits.size, 0)
})


test('passive state reads are aborted on channel closure even if the source ignores the signal', async () => {
  const { service, adapter } = makeService()
  adapter.describe = () => ({ capabilities: ['wait'] })
  adapter.readWaitState = async () => new Promise(() => {})
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  const pending = service.handle({ request, payload: { read_wait_state: { session_id: 's1' } } })
  service.cancelChannel('device-1', 'other')
  assert.equal(service.waits.size, 1)
  service.cancelChannel('device-1', 'channel-1', 'channel_replaced')
  assert.equal((await pending).errorCode, 'channel_replaced')
  assert.equal(service.waits.size, 0)
})

test('pending queue mutation with a lost terminal write stays unknown across replay and recovery', async () => {
  const {service,store,adapter} = makeService()
  enableQueue(adapter)
  let calls=0
  adapter.execute = async () => { calls++; return {} }
  const update = store.setCommandStatus.bind(store)
  store.setCommandStatus = fields => {
    if (fields.status === 'succeeded') throw new Error('fixture terminal store fault')
    return update(fields)
  }
  const request = makeRequest({commandId:'queue-recovery'})
  const payload = {update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}
  assert.equal((await service.handle({request,payload})).status,'unknown')
  assert.equal((await service.handle({request,payload})).status,'unknown')
  assert.equal(calls,1)
  store.setCommandStatus = update
  service.recover()
  const result = await service.handle({request:makeRequest({kind:'REQUEST_KIND_QUERY'}),payload:{get_command_result:{command_id:'queue-recovery'}}})
  assert.equal(result.response.command_result.status,'unknown')
  const changedBody = new Uint8Array([8])
  const changed = {...request,body:changedBody,payload_digest:sha256Bytes(changedBody)}
  assert.equal((await service.handle({request:changed,payload})).errorCode,'payload_digest_mismatch')
  assert.equal(calls,1)
})

test('pending queue promotion failure after source admission remains unknown and is never replayed', async () => {
  const {service,adapter} = makeService()
  enableQueue(adapter)
  let calls=0
  adapter.execute = async () => { calls++; throw new AdapterError('unavailable') }
  const request=makeRequest()
  const payload={update_pending_queue:{session_id:'s1',item_id:'q1',action:'PROMOTE'}}
  assert.equal((await service.handle({request,payload})).status,'unknown')
  assert.equal((await service.handle({request,payload})).status,'unknown')
  assert.equal(calls,1)
})

test('a fork result journal fault stays unknown and a duplicate never forks again', async () => {
  const {service, adapter, store} = makeService()
  let calls = 0
  adapter.describeHostCapabilities = async () => ({session_fork:true})
  adapter.getSession = async () => ({sourceCapabilities:{session_fork:true}})
  adapter.execute = async () => { calls++; return {lifecycle_receipt:{session_id:'child',parent_session_id:'parent',operation:'fork',disposition:'READY'}} }
  const update = store.setCommandStatus.bind(store)
  let fail = true
  store.setCommandStatus = fields => {
    if (fail && fields.status === 'succeeded') { fail = false; throw new Error('injected failure') }
    return update(fields)
  }
  const request = makeRequest()
  const payload = {fork_session:{session_id:'parent'}}
  const first = await service.handle({request,payload})
  const replay = await service.handle({request,payload})
  assert.equal(first.status, 'unknown')
  assert.equal(replay.status, 'unknown')
  assert.equal(calls, 1)
  assert.equal(store.getCommand('device-1', request.command_id).status, 'unknown')
})

test('distinct fork command identities produce distinct confirmed children', async () => {
  const {service, adapter} = makeService()
  let calls = 0
  adapter.describeHostCapabilities = async () => ({session_fork:true})
  adapter.getSession = async () => ({sourceCapabilities:{session_fork:true}})
  adapter.execute = async () => ({lifecycle_receipt:{session_id:`child-${++calls}`,parent_session_id:'parent',operation:'fork',disposition:'READY'}})
  const payload = {fork_session:{session_id:'parent'}}
  const first = await service.handle({request:makeRequest(),payload})
  const second = await service.handle({request:makeRequest(),payload})
  assert.notDeepEqual(first.resultPayload,second.resultPayload)
  assert.equal(calls,2)
})

for (const failure of ['encoding', 'journal']) {
  test(`model selection ${failure} failure remains uncertain and is not replayed`, async () => {
    const { service, adapter, store } = makeService(failure === 'encoding' ? { codec: { encode() { throw new Error('fixture encoding failure') } } } : {})
    adapter.describe = () => ({ capabilities: ['model-select'] })
    let calls = 0
    adapter.execute = async () => { calls++; return { model_selection_receipt: { selected: { provider: 'p', model: 'm' }, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' } } }
    if (failure === 'journal') {
      const save = store.setCommandStatus.bind(store)
      let writes = 0
      store.setCommandStatus = fields => { if (++writes === 1) throw new Error('fixture journal failure'); return save(fields) }
    }
    const request = makeRequest()
    const payload = { select_session_model: { session_id: 's1', provider: 'p', model: 'm' } }
    assert.equal((await service.handle({ request, payload })).status, 'unknown')
    assert.equal((await service.handle({ request, payload })).status, 'unknown')
    assert.equal(store.getCommand('device-1', request.command_id).status, 'unknown')
    assert.equal(calls, 1)
  })
}

test('open answer claim follows its exact channel and expiry without replay', async () => {
  const { service, adapter } = makeService()
  adapter.getSession = async id => ({ sessionId: id, sourceCapabilities: { questions_answer_open: true } })
  let signal, invoked = 0
  adapter.execute = async (_session, _command, context) => {
    invoked++; signal = context.signal
    return new Promise((_, reject) => signal.addEventListener('abort', () => reject(new Error('aborted')), { once: true }))
  }
  const payload = { answer_question: { session_id: 's1', expected_phase: 'QUESTION_OPEN' } }
  const request = makeRequest()
  const running = service.handle({ request, payload })
  while (!signal) await new Promise(resolve => setImmediate(resolve))
  service.cancelChannel('other', request.channel_id)
  assert.equal(signal.aborted, false)
  service.cancelChannel(request.device_id, request.channel_id)
  assert.equal((await running).status, 'unknown')
  assert.equal((await service.handle({ request, payload })).status, 'unknown')
  assert.equal(invoked, 1)
  signal = undefined
  const expiring = service.handle({ request: makeRequest({ expiresAt: 1050 }), payload })
  while (!signal) await new Promise(resolve => setImmediate(resolve))
  // Keep the event loop alive while the service's unref'd deadline fires.
  const watchdog = setTimeout(() => {}, 100)
  assert.equal((await expiring).status, 'unknown')
  clearTimeout(watchdog)
  assert.equal(signal.aborted, true)
  signal = undefined
  const closing = service.handle({ request: makeRequest(), payload })
  while (!signal) await new Promise(resolve => setImmediate(resolve))
  service.closeQuestionAnswers()
  assert.equal((await closing).status, 'unknown')
})

test('explicit question cancellation is device, session and command scoped', async () => {
  const { service, adapter } = makeService()
  adapter.getSession = async id => ({ sessionId: id, sourceCapabilities: { questions_answer_open: true } })
  let signal
  adapter.execute = async (_session, _command, context) => {
    signal = context.signal
    return new Promise((_, reject) => signal.addEventListener('abort', () => reject(new Error('aborted')), { once: true }))
  }
  const answer = makeRequest()
  const payload = { answer_question: { session_id: 's1', expected_phase: 'QUESTION_OPEN' } }
  const pending = service.handle({ request: answer, payload })
  while (!signal) await new Promise(resolve => setImmediate(resolve))
  const query = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  const cancel = { cancel_question_answer: { session_id: 's1', command_id: answer.command_id } }
  const otherDevice = { ...query, device_id: 'other-device' }
  assert.equal((await service.handle({ request: otherDevice, payload: cancel })).response.question_cancel_receipt.cancellation_requested, false)
  assert.equal((await service.handle({ request: query, payload: { cancel_question_answer: { session_id: 'other', command_id: answer.command_id } } })).response.question_cancel_receipt.cancellation_requested, false)
  assert.equal(signal.aborted, false)
  assert.equal((await service.handle({ request: { ...query, channel_id: 'replacement' }, payload: cancel })).response.question_cancel_receipt.cancellation_requested, true)
  assert.equal((await pending).status, 'unknown')
  assert.equal((await service.handle({ request: query, payload: cancel })).response.question_cancel_receipt.cancellation_requested, false)
  assert.equal((await service.handle({ request: query, payload: { cancel_question_answer: { session_id: '', command_id: answer.command_id } } })).errorCode, 'invalid_argument')
})


test('queue edit contents stay out of host logs and durable command receipts', async () => {
  const store=new StateStore({path:':memory:',now:()=>1000})
  const recorded=[]
  const logger={info:(event,fields)=>recorded.push({event,fields}),warn:(event,fields)=>recorded.push({event,fields}),error:(event,fields)=>recorded.push({event,fields})}
  const adapter={getSession:async sessionId=>({sessionId,sourceCapabilities:{pending_queue_edit:true}}),execute:async()=>({})}
  const service=new CommandService({store,adapter,events:{},logger,now:()=>1000,codec:proto})
  const request=makeRequest()
  const marker='synthetic-private-queue-content'
  const payload={update_pending_queue:{session_id:'s1',item_id:'q1',action:'EDIT',text:marker}}
  const result=await service.handle({request,payload})
  assert.equal(result.status,'succeeded')
  assert.equal(JSON.stringify(recorded).includes(marker),false)
  assert.equal(JSON.stringify(store.getCommand('device-1',request.command_id)).includes(marker),false)
  assert.equal(Buffer.from(result.resultPayload).includes(Buffer.from(marker)),false)
  store.close()
})

test('queue command expiring during capability lookup never invokes the source mutation', async () => {
  const {service,adapter,advance}=makeService()
  let calls=0
  adapter.getSession=async sessionId=>{advance(1000);return {sessionId,sourceCapabilities:{pending_queue_remove:true}}}
  adapter.execute=async()=>{calls++;return {}}
  const request=makeRequest({expiresAt:1500})
  const payload={update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}
  const result=await service.handle({request,payload})
  assert.equal(result.errorCode,'expired')
  assert.equal(calls,0)
})

test('disabling queue capability during accepted work keeps the in-flight receipt and blocks new work', async () => {
  const {service,adapter}=makeService()
  let enabled=true,calls=0,entered,release
  const started=new Promise(resolve=>{entered=resolve})
  const barrier=new Promise(resolve=>{release=resolve})
  adapter.getSession=async sessionId=>({sessionId,sourceCapabilities:{pending_queue_remove:enabled}})
  adapter.execute=async()=>{calls++;entered();await barrier;return {}}
  const payload={update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}
  const request=makeRequest({commandId:'queue-in-flight'})
  const pending=service.handle({request,payload})
  await started
  enabled=false
  assert.equal((await service.handle({request,payload})).status,'host_accepted')
  assert.equal((await service.handle({request:makeRequest(),payload})).errorCode,'unsupported_capability')
  assert.equal(calls,1)
  release()
  const result=await pending
  assert.equal(result.status,'succeeded')
  assert.equal((await service.handle({request,payload})).status,'succeeded')
  assert.equal(calls,1)
})

test('concurrent duplicate queue commands invoke source once and replay the terminal receipt', async () => {
  const {service,adapter} = makeService()
  enableQueue(adapter)
  let release, entered
  const wait = new Promise(resolve=>{release=resolve})
  const started = new Promise(resolve=>{entered=resolve})
  let calls=0
  adapter.execute=async()=>{calls++;entered();await wait;return {}}
  const request=makeRequest({commandId:'queue-concurrent'})
  const payload={update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}
  const first=service.handle({request,payload})
  await started
  assert.equal((await service.handle({request,payload})).status,'host_accepted')
  release()
  const terminal=await first
  assert.equal(terminal.status,'succeeded')
  assert.deepEqual((await service.handle({request,payload})).resultPayload,terminal.resultPayload)
  assert.equal(calls,1)
})


test('delivery receipts and uncertain outcomes survive journal reopen for every mode', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-delivery-recovery-'))
  const path = join(directory, 'host.sqlite3')
  const events = {}
  const calls = []
  const adapter = {execute: async (_session, command, context) => {
    calls.push({mode: command.send_message.delivery_mode, operationId: context.operationId})
    if (command.send_message.text === 'synthetic lost reply') throw new Error('fixture reply lost')
    return {command_accepted: {delivery_id: context.operationId}}
  }}
  let store
  try {
    store = new StateStore({path, now: () => 1000})
    let service = new CommandService({store, adapter, events, now: () => 1000, codec: proto})
    const recorded = []
    for (const mode of ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER']) {
      const request = makeRequest({commandId: `recovery-${mode}`})
      const payload = {send_message: {session_id: 's1', text: 'synthetic accepted', delivery_mode: mode}}
      const result = await service.handle({request, payload})
      assert.equal(result.status, 'succeeded')
      const id = proto.decode('whalerein.v1.ResponsePayload', result.resultPayload).command_accepted.delivery_id
      assert.ok(id)
      recorded.push({request, payload, result, id})
    }
    const uncertainRequest = makeRequest({commandId: 'recovery-unknown'})
    const uncertainPayload = {send_message: {session_id: 's1', text: 'synthetic lost reply', delivery_mode: 'STEER'}}
    assert.equal((await service.handle({request: uncertainRequest, payload: uncertainPayload})).status, 'unknown')
    assert.equal(calls.length, 4)
    store.close()
    store = new StateStore({path, now: () => 1000})
    service = new CommandService({store, adapter, events, now: () => 1000, codec: proto})
    service.recover()
    for (const item of recorded) {
      const replay = await service.handle({request: item.request, payload: item.payload})
      assert.equal(replay.status, 'succeeded')
      assert.deepEqual(replay.resultPayload, item.result.resultPayload)
      assert.equal(proto.decode('whalerein.v1.ResponsePayload', replay.resultPayload).command_accepted.delivery_id, item.id)
      const query = await service.handle({request: makeRequest({kind: 'REQUEST_KIND_QUERY'}), payload: {get_command_result: {command_id: item.request.command_id}}})
      assert.equal(query.response.command_result.status, 'succeeded')
      assert.deepEqual(query.response.command_result.result_payload, item.result.resultPayload)
    }
    assert.equal((await service.handle({request: uncertainRequest, payload: uncertainPayload})).status, 'unknown')
    const query = await service.handle({request: makeRequest({kind: 'REQUEST_KIND_QUERY'}), payload: {get_command_result: {command_id: uncertainRequest.command_id}}})
    assert.equal(query.response.command_result.status, 'unknown')
    assert.equal(calls.length, 4)
    assert.equal(new Set(calls.map(item => item.operationId)).size, 4)
  } finally {
    store?.close()
    rmSync(directory, {recursive: true, force: true})
  }
})

test('source operation identity remains stable and is distinct across host and device scopes', async () => {
  const ids = []
  for (const [hostId, deviceId] of [['host-a','device-a'], ['host-b','device-a'], ['host-a','device-b'], ['host-a','device-a']]) {
    const {service, adapter, store} = makeService()
    adapter.execute = async (_session, _command, context) => {
      ids.push(context.operationId)
      return {command_accepted: {delivery_id: context.operationId}}
    }
    const request = {...makeRequest({commandId: 'same-command'}), host_id: hostId, device_id: deviceId}
    const result = await service.handle({request, payload: {send_message: {session_id: 's1', text: 'fixture'}}})
    assert.equal(result.status, 'succeeded')
    store.close()
  }
  assert.equal(ids[0], ids[3])
  assert.equal(new Set(ids.slice(0,3)).size, 3)
})

test('blank deliveries fail before the source is invoked for every delivery mode', async () => {
  const {service, adapter} = makeService()
  let calls = 0
  adapter.execute = async () => { calls++ }
  for (const mode of ['START_IF_IDLE', 'ENQUEUE_AFTER_RUN', 'STEER']) {
    const result = await service.handle({request: makeRequest(), payload: {send_message: {
      session_id: 's1', text: ' \n\t', delivery_mode: mode,
    }}})
    assert.equal(result.status, 'failed')
    assert.equal(result.errorCode, 'invalid_argument')
  }
  assert.equal(calls, 0)
})

test('question answer contents are absent from diagnostic logs and durable command journal', async () => {
  const { mkdtempSync, readFileSync, rmSync } = await import('node:fs')
  const { tmpdir } = await import('node:os')
  const { join } = await import('node:path')
  const { createLogger } = await import('../src/log.js')
  const directory = mkdtempSync(join(tmpdir(), 'question-journal-'))
  const path = join(directory, 'state.sqlite')
  const store = new StateStore({ path, now: () => 1000 })
  const records = []
  const logger = createLogger({ salt: 'ab'.repeat(16), sink: record => records.push(record) })
  const marker = 'synthetic-question-answer-private-marker'
  const adapter = {
    getSession: async id => ({ sessionId: id, sourceCapabilities: { questions_answer_open: true } }),
    execute: async () => ({ question_receipt: { batch_id: 'batch', phase: 'QUESTION_SETTLED' } }),
  }
  const service = new CommandService({ store, adapter, events: {}, logger, now: () => 1000, codec: proto })
  const request = makeRequest()
  try {
    const result = await service.handle({ request, payload: { answer_question: {
      session_id: 's1', batch_id: 'batch', expected_phase: 'QUESTION_OPEN', answers: [{ id: 'q', selected: [], custom: marker }],
    } } })
    assert.equal(result.status, 'succeeded')
    assert.equal(JSON.stringify(records).includes(marker), false)
    const journal = store.getCommand(request.device_id, request.command_id)
    assert.equal(Buffer.from(journal.resultPayload).includes(Buffer.from(marker)), false)
  } finally { store.close() }
  try { assert.equal(readFileSync(path).includes(Buffer.from(marker)), false) }
  finally { rmSync(directory, { recursive: true, force: true }) }
})


test('an omitted request host uses the authenticated configured host for delivery identity', async () => {
  const ids = []
  for (const [configured, supplied] of [['host-a', ''], ['host-b', ''], ['host-a', 'host-a']]) {
    const {service, adapter, store} = makeService({hostId: configured})
    adapter.execute = async (_session, _command, context) => {
      ids.push(context.operationId)
      return {command_accepted: {delivery_id: context.operationId}}
    }
    const request = {...makeRequest({commandId: 'same-command'}), host_id: supplied}
    const result = await service.handle({request, payload: {send_message: {session_id: 's1', text: 'fixture'}}})
    assert.equal(result.status, 'succeeded')
    store.close()
  }
  assert.notEqual(ids[0], ids[1])
  assert.equal(ids[0], ids[2])
})

test('a stale open answer with proven pre-admission conflict is definitive and not replayed', async () => {
  const { service, adapter } = makeService()
  adapter.getSession = async id => ({ sessionId: id, sourceCapabilities: { questions_answer_open: true } })
  let calls = 0
  adapter.execute = async () => { calls++; throw new DefinitiveExecutionError('conflict') }
  const request = makeRequest()
  const payload = { answer_question: { session_id: 's1', expected_phase: 'QUESTION_OPEN' } }
  assert.equal((await service.handle({ request, payload })).errorCode, 'conflict')
  assert.equal((await service.handle({ request, payload })).errorCode, 'conflict')
  assert.equal(calls, 1)
})

test('question controls deduplicate concurrent submissions and never replay uncertain outcomes', async () => {
  const { service, adapter, store } = makeService()
  adapter.getSession = async id => ({ sessionId: id, sourceCapabilities: { questions_answer_open: true } })
  const gate = Promise.withResolvers()
  let calls = 0
  adapter.execute = async () => { calls++; return gate.promise }
  const payload = { answer_question: { session_id: 's1', expected_phase: 'QUESTION_OPEN' } }
  const request = makeRequest()
  const first = service.handle({ request, payload })
  while (!calls) await new Promise(resolve => setImmediate(resolve))
  assert.equal((await service.handle({ request, payload })).status, 'host_accepted')
  gate.resolve({ question_receipt: { batch_id: 'batch', phase: 'QUESTION_SETTLED' } })
  const settled = await first
  assert.equal(settled.status, 'succeeded')
  assert.deepEqual((await service.handle({ request, payload })).resultPayload, settled.resultPayload)
  const changed = makeRequest({ commandId: request.command_id, body: new Uint8Array([9]) })
  assert.equal((await service.handle({ request: changed, payload })).errorCode, 'payload_digest_mismatch')
  assert.equal(calls, 1)

  const update = store.setCommandStatus.bind(store)
  let fail = true
  store.setCommandStatus = fields => {
    if (fields.status === 'succeeded' && fail) { fail = false; throw new Error('fixture result store fault') }
    return update(fields)
  }
  adapter.execute = async () => { calls++; return { question_receipt: { batch_id: 'batch', phase: 'QUESTION_SETTLED' } } }
  const unknownRequest = makeRequest()
  assert.equal((await service.handle({ request: unknownRequest, payload })).status, 'unknown')
  assert.equal((await service.handle({ request: unknownRequest, payload })).status, 'unknown')
  assert.equal(calls, 2)

  const recovered = makeRequest()
  store.createOrGetCommand({ deviceId: recovered.device_id, commandId: recovered.command_id,
    requestId: recovered.request_id, digest: recovered.payload_digest, expiresAt: recovered.expires_at_unix_ms, now: 1000 })
  service.recover()
  assert.equal((await service.handle({ request: recovered, payload })).status, 'unknown')
  assert.equal(calls, 2)
})

test('an expired fork is rejected before native invocation', async () => {
  const {service,adapter,store} = makeService()
  let calls = 0
  adapter.describeHostCapabilities = async () => ({session_fork:true})
  adapter.execute = async () => {calls++; return {}}
  const result = await service.handle({request:makeRequest({expiresAt:500}),payload:{fork_session:{session_id:'parent'}}})
  assert.equal(result.status, 'rejected')
  assert.equal(result.errorCode, 'expired')
  assert.equal(store.countCommands('accepted'), 0)
  assert.equal(calls, 0)
})

test('concurrent duplicate fork requests invoke the source once and recover the same receipt', async () => {
  const {service,adapter} = makeService()
  let calls = 0
  let release
  const held = new Promise(resolve => {release = resolve})
  adapter.describeHostCapabilities = async () => ({session_fork:true})
  adapter.getSession = async () => ({sourceCapabilities:{session_fork:true}})
  adapter.execute = async () => { calls++; await held; return {lifecycle_receipt:{session_id:'child',parent_session_id:'parent',operation:'fork',disposition:'READY'}} }
  const request = makeRequest()
  const payload = {fork_session:{session_id:'parent'}}
  const first = service.handle({request,payload})
  await new Promise(resolve => setImmediate(resolve))
  const duplicate = await service.handle({request,payload})
  assert.equal(duplicate.status, 'host_accepted')
  release()
  const result = await first
  const replay = await service.handle({request,payload})
  assert.equal(result.status, 'succeeded')
  assert.deepEqual(result.resultPayload, replay.resultPayload)
  assert.equal(calls, 1)
})

test('fork replay survives a journal restart and changed digest is rejected', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-fork-test-'))
  t.after(() => rmSync(directory, {recursive:true, force:true}))
  let store = new StateStore({path:join(directory, 'state.sqlite'), now:() => 1000})
  const adapter = new TestSource()
  let calls = 0
  adapter.describeHostCapabilities = async () => ({session_fork:true})
  adapter.getSession = async () => ({sourceCapabilities:{session_fork:true}})
  adapter.execute = async () => { calls++; return {lifecycle_receipt:{session_id:'child',parent_session_id:'parent',operation:'fork',disposition:'READY'}} }
  const service = new CommandService({store,adapter,events:{},now:() => 1000,codec:proto})
  const request = makeRequest()
  const payload = {fork_session:{session_id:'parent'}}
  const original = await service.handle({request,payload})
  assert.equal(original.status,'succeeded')
  store.close()
  store = new StateStore({path:join(directory, 'state.sqlite'), now:() => 1000})
  t.after(() => store.close())
  const restarted = new CommandService({store,adapter,events:{},now:() => 1000,codec:proto})
  restarted.recover()
  const replay = await restarted.handle({request,payload})
  assert.equal(replay.status,'succeeded')
  assert.deepEqual(replay.resultPayload,original.resultPayload)
  const body = new Uint8Array([8])
  const conflict = await restarted.handle({request:{...request,body,payload_digest:sha256Bytes(body)},payload})
  assert.equal(conflict.status,'rejected')
  assert.equal(conflict.errorCode,'payload_digest_mismatch')
  assert.equal(calls,1)
})

test('simultaneous distinct fork commands create distinct child identities', async () => {
  const {service,adapter} = makeService()
  let calls=0
  let release
  let bothStarted
  const held = new Promise(resolve => {release=resolve})
  const started = new Promise(resolve => {bothStarted=resolve})
  adapter.describeHostCapabilities = async () => ({session_fork:true})
  adapter.getSession = async () => ({sourceCapabilities:{session_fork:true}})
  adapter.execute = async () => { const count = ++calls; if (count === 2) bothStarted(); await held; return {lifecycle_receipt:{session_id:`child-${count}`,parent_session_id:'parent',operation:'fork',disposition:'READY'}} }
  const payload={fork_session:{session_id:'parent'}}
  const first=service.handle({request:makeRequest(),payload})
  const second=service.handle({request:makeRequest(),payload})
  await started
  release()
  const results=await Promise.all([first,second])
  assert.deepEqual(results.map(result => result.status),['succeeded','succeeded'])
  assert.notDeepEqual(results[0].resultPayload,results[1].resultPayload)
  assert.equal(calls,2)
})


test('queue operations do not call an adapter that declares the operation unavailable', async () => {
  const {service,adapter}=makeService()
  adapter.getSession=async()=>({sessionId:'s1',sourceCapabilities:{pending_queue_read:false,pending_queue_remove:false}})
  let reads=0,mutations=0
  adapter.readPendingQueue=async()=>{reads++;return {session_id:'s1',available:true,items:[]}}
  adapter.execute=async()=>{mutations++;return {}}
  const read=await service.handle({request:makeRequest({kind:'REQUEST_KIND_QUERY'}),payload:{read_pending_queue:{session_id:'s1'}}})
  assert.equal(read.errorCode,'unsupported_capability')
  const remove=await service.handle({request:makeRequest(),payload:{update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}})
  assert.equal(remove.errorCode,'unsupported_capability')
  assert.equal(reads,0)
  assert.equal(mutations,0)
})

test('queue capability gates are per session and action and never change a prior accepted receipt', async () => {
  const {service,adapter}=makeService()
  let caps={pending_queue_read:true,pending_queue_edit:true,pending_queue_remove:false,pending_queue_steer:false}
  let gets=0, reads=0, calls=0
  adapter.getSession=async sessionId=>{gets++;return {sessionId,sourceCapabilities:caps}}
  adapter.readPendingQueue=async()=>{reads++;return {session_id:'s1',available:true,items:[]}}
  adapter.execute=async()=>{calls++;return {}}
  assert.equal((await service.handle({request:makeRequest({kind:'REQUEST_KIND_QUERY'}),payload:{read_pending_queue:{session_id:'s1'}}})).status,'succeeded')
  const editRequest=makeRequest({commandId:'queue-before-disable'})
  const edit={update_pending_queue:{session_id:'s1',item_id:'q1',action:'EDIT',text:'fixture'}}
  const first=await service.handle({request:editRequest,payload:edit})
  assert.equal(first.status,'succeeded')
  for(const action of ['REMOVE','PROMOTE']) {
    const result=await service.handle({request:makeRequest(),payload:{update_pending_queue:{session_id:'s1',item_id:'q1',action}}})
    assert.equal(result.errorCode,'unsupported_capability')
  }
  assert.equal(calls,1)
  caps={}
  const checks=gets
  const replay=await service.handle({request:editRequest,payload:edit})
  assert.equal(replay.status,'succeeded')
  assert.deepEqual(replay.resultPayload,first.resultPayload)
  assert.equal(gets,checks)
  assert.equal((await service.handle({request:makeRequest({kind:'REQUEST_KIND_QUERY'}),payload:{read_pending_queue:{session_id:'s1'}}})).errorCode,'unsupported_capability')
  assert.equal(reads,1)
  assert.equal((await service.handle({request:makeRequest(),payload:{update_pending_queue:{session_id:'s1',item_id:'q1',action:'EDIT',text:'fixture'}}})).errorCode,'unsupported_capability')
  assert.equal(calls,1)
})

test('model query expires while a source never settles and releases its scoped tracking', async () => {
  const { service, adapter } = makeService()
  let captured
  adapter.describeHostCapabilities = async () => ({ model_catalog: true })
  adapter.listModels = ({ signal }) => { captured = signal; return new Promise(() => {}) }
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY', expiresAt: 1020 })
  const keepAlive = setTimeout(() => {}, 100)
  try {
    const result = await service.handle({ request, payload: { list_models: {} } })
    assert.equal(result.errorCode, 'expired')
    assert.equal(captured.aborted, true)
    assert.equal(service.modelQueries.size, 0)
  } finally { clearTimeout(keepAlive) }
})

test('model query channel cancellation is scoped, rejects late data and removes tracking', async () => {
  const { service, adapter } = makeService()
  let release
  let signal
  adapter.describeHostCapabilities = async () => ({ model_read: true })
  adapter.readSessionModel = (_session, context) => { signal = context.signal; return new Promise(resolve => { release = resolve }) }
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  const work = service.handle({ request, payload: { read_session_model: { session_id: 's1' } } })
  while (!release) await new Promise(resolve => setImmediate(resolve))
  service.cancelChannel('another-device', request.channel_id)
  assert.equal(signal.aborted, false)
  service.cancelChannel(request.device_id, request.channel_id)
  const result = await work
  assert.equal(result.errorCode, 'unavailable')
  assert.equal(signal.aborted, true)
  assert.equal(service.modelQueries.size, 0)
  release({ pending: null, default_selection: { provider: 'fixture-private-sentinel', model: 'm' } })
  await new Promise(resolve => setImmediate(resolve))
  assert.equal(JSON.stringify(result).includes('fixture-private-sentinel'), false)
})

test('model query cancellation during capability lookup never calls the source', async () => {
  const { service, adapter } = makeService()
  let release
  let calls = 0
  adapter.describeHostCapabilities = () => new Promise(resolve => { release = resolve })
  adapter.listModels = async () => { calls++; return {} }
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  const work = service.handle({ request, payload: { list_models: {} } })
  while (!release) await new Promise(resolve => setImmediate(resolve))
  service.closeModelQueries()
  assert.equal((await work).errorCode, 'unavailable')
  assert.equal(service.modelQueries.size, 0)
  release({ model_catalog: true })
  await new Promise(resolve => setImmediate(resolve))
  assert.equal(calls, 0)
})

test('expired or malformed model query never invokes capability lookup or source', async () => {
  const { service, adapter } = makeService()
  let calls = 0
  adapter.describeHostCapabilities = async () => { calls++; return { model_catalog: true } }
  adapter.listModels = async () => { calls++; return {} }
  for (const expiresAt of [0, 1000, Number.MAX_SAFE_INTEGER + 1]) {
    const request = makeRequest({ kind: 'REQUEST_KIND_QUERY', expiresAt })
    const result = await service.handle({ request, payload: { list_models: {} } })
    assert.equal(result.errorCode, 'expired')
  }
  assert.equal(calls, 0)
  assert.equal(service.modelQueries.size, 0)
})

test('a stalled native model catalog may reject after cancellation without leaking its error', async () => {
  const { service, adapter } = makeService()
  let rejectNative
  adapter.describeHostCapabilities = async () => ({ model_catalog: true })
  adapter.listModels = () => new Promise((_, reject) => { rejectNative = reject })
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  const work = service.handle({ request, payload: { list_models: {} } })
  while (!rejectNative) await new Promise(resolve => setImmediate(resolve))
  service.cancelChannel(request.device_id, request.channel_id)
  const result = await work
  assert.equal(result.errorCode, 'unavailable')
  rejectNative(new Error('fixture-private-sentinel'))
  await new Promise(resolve => setImmediate(resolve))
  assert.equal(JSON.stringify(result).includes('fixture-private-sentinel'), false)
  assert.equal(service.modelQueries.size, 0)
})

for (const [label, receipt] of [
  ['missing receipt', null],
  ['missing selection', { effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' }],
  ['wrong provider', { selected: { provider: 'fixture-private-sentinel', model: 'm', effort: 'high' }, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' }],
  ['wrong model', { selected: { provider: 'p', model: 'other', effort: 'high' }, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' }],
  ['wrong effort', { selected: { provider: 'p', model: 'm', effort: 'low' }, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' }],
  ['missing effort', { selected: { provider: 'p', model: 'm' }, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' }],
  ['wrong effect', { selected: { provider: 'p', model: 'm', effort: 'high' }, effect_scope: 'session', default_persistence: 'unconfirmed' }],
  ['wrong persistence', { selected: { provider: 'p', model: 'm', effort: 'high' }, effect_scope: 'session_and_source_default_attempt', default_persistence: 'confirmed' }],
]) {
  test(`invalid model ${label} stays unknown across recovery and never re-executes`, async () => {
    const { service, adapter, store } = makeService()
    adapter.describe = () => ({ capabilities: ['model-select'] })
    let calls = 0
    adapter.execute = async () => { calls++; return receipt ? { model_selection_receipt: receipt } : {} }
    const request = makeRequest()
    const payload = { select_session_model: { session_id: 's1', provider: 'p', model: 'm', effort: 'high' } }
    const first = await service.handle({ request, payload })
    assert.equal(first.status, 'unknown')
    assert.equal(JSON.stringify(first).includes('fixture-private-sentinel'), false)
    assert.equal(store.getCommand('device-1', request.command_id).status, 'unknown')
    assert.equal(JSON.stringify(store.getCommand('device-1', request.command_id)).includes('fixture-private-sentinel'), false)
    service.recover()
    assert.equal((await service.handle({ request, payload })).status, 'unknown')
    const lookup = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { get_command_result: { command_id: request.command_id } } })
    assert.equal(lookup.response.command_result.status, 'unknown')
    assert.equal(calls, 1)
  })
}

test('native selection receipt distinguishes omitted effort from explicit empty effort', async () => {
  const { service, adapter } = makeService()
  adapter.describe = () => ({ capabilities: ['model-select'] })
  let calls = 0
  adapter.execute = async () => { calls++; return { model_selection_receipt: { selected: { provider: 'p', model: 'm' }, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' } } }
  const omitted = makeRequest()
  assert.equal((await service.handle({ request: omitted, payload: { select_session_model: { session_id: 's1', provider: 'p', model: 'm' } } })).status, 'succeeded')
  const explicit = makeRequest()
  assert.equal((await service.handle({ request: explicit, payload: { select_session_model: { session_id: 's1', provider: 'p', model: 'm', effort: '' } } })).status, 'unknown')
  adapter.execute = async () => { calls++; return { model_selection_receipt: { selected: { provider: 'p', model: 'm', effort: null }, effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' } } }
  const nullEffort = makeRequest()
  assert.equal((await service.handle({ request: nullEffort, payload: { select_session_model: { session_id: 's1', provider: 'p', model: 'm' } } })).status, 'unknown')
  assert.equal(calls, 3)
})

test('catalog-only capability permits the catalog while rejecting model reads and writes before the source', async () => {
  const { service, adapter } = makeService()
  let catalogCalls = 0
  let readCalls = 0
  let writeCalls = 0
  adapter.describeHostCapabilities = async () => ({ model_catalog: true, model_read: false })
  adapter.describe = () => ({ capabilities: ['model-catalog'] })
  adapter.listModels = async () => { catalogCalls++; return { groups: [] } }
  adapter.readSessionModel = async () => { readCalls++; return {} }
  adapter.execute = async () => { writeCalls++; return {} }
  const query = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  assert.equal((await service.handle({ request: query, payload: { list_models: {} } })).status, 'succeeded')
  assert.equal((await service.handle({ request: query, payload: { read_session_model: { session_id: 's1' } } })).errorCode, 'unsupported_capability')
  const command = makeRequest()
  const result = await service.handle({ request: command, payload: { select_session_model: { session_id: 's1', provider: 'p', model: 'm' } } })
  assert.equal(result.errorCode, 'unsupported_capability')
  assert.equal(catalogCalls, 1)
  assert.equal(readCalls, 0)
  assert.equal(writeCalls, 0)
})

test('revocation and source failure remain distinct from channel replacement', async () => {
  const { service, adapter } = makeService()
  adapter.describe = () => ({ capabilities: ['wait'] })
  adapter.waitSession = async () => new Promise(() => {})
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  const payload = { wait_session: { session_id: 's1', wait_id: 'w1', timeout_ms: 1000 } }
  const pending = service.handle({ request, payload })
  service.cancelChannel('device-1', 'channel-1', 'revoked')
  assert.equal((await pending).errorCode, 'forbidden')
  const disconnected = service.handle({ request, payload })
  service.cancelChannel('device-1', 'channel-1')
  assert.equal((await disconnected).errorCode, 'unavailable')
  adapter.waitSession = async () => { throw new AdapterError('unavailable') }
  assert.equal((await service.handle({ request, payload })).errorCode, 'unavailable')
  adapter.waitSession = async () => { throw new AdapterError('channel_replaced') }
  assert.equal((await service.handle({ request, payload })).errorCode, 'unavailable')
  adapter.waitSession = async () => { throw new PluginError('channel_replaced') }
  assert.equal((await service.handle({ request, payload })).errorCode, 'unavailable')
  adapter.waitSession = async () => { throw new AdapterError('unauthorized', 'private fixture detail') }
  const denied = await service.handle({ request, payload })
  assert.equal(denied.sourceFailure, true)
  assert.deepEqual(denied.response, { error_code: 'unauthorized', error_detail: 'unauthorized' })
  assert.equal(JSON.stringify(denied).includes('private fixture detail'), false)
})

test('a simultaneous source result and channel closure preserves the closure reason', async () => {
  const { service, adapter } = makeService()
  adapter.describe = () => ({ capabilities: ['wait'] })
  const request = makeRequest({ kind: 'REQUEST_KIND_QUERY' })
  const state = { active: true, running: true, queueAvailable: true, pendingInput: [] }
  for (const [kind, reason, expected] of [['wait', 'channel_replaced', 'channel_replaced'], ['state', 'revoked', 'forbidden']]) {
    let release
    if (kind === 'wait') adapter.waitSession = () => new Promise(resolve => { release = () => resolve({ reason: 'idle', state }) })
    else adapter.readWaitState = () => new Promise(resolve => { release = () => resolve(state) })
    const payload = kind === 'wait' ? { wait_session: { session_id: 's1', wait_id: 'w', timeout_ms: 1000 } } : { read_wait_state: { session_id: 's1' } }
    const pending = service.handle({ request, payload })
    release()
    service.cancelChannel('device-1', 'channel-1', reason)
    assert.equal((await pending).errorCode, expected)
  }
})

test('all controls expiring while accepted publication waits never invoke the source', async () => {
  const cases = [
    {send_message:{session_id:'s1',text:'synthetic'}},
    {interrupt:{session_id:'s1'}},
    {create_session:{}},
    {fork_session:{session_id:'s1'}},
    {select_session_model:{session_id:'s1',provider:'fixture',model:'fixture'}},
    {update_pending_queue:{session_id:'s1',item_id:'item',action:'REMOVE'}},
    {answer_question:{session_id:'s1',expected_phase:'QUESTION_CONTINUED'}},
    {answer_question:{session_id:'s1',expected_phase:'QUESTION_OPEN'}},
  ]
  for (const payload of cases) {
    const {service,adapter,advance,store} = makeService()
    let calls = 0
    adapter.execute = async () => { calls++; return {} }
    adapter.executeHost = async () => { calls++; return {} }
    service.onAccepted = async () => advance(1100)
    const result = await service.handle({request:makeRequest({expiresAt:2000}),payload})
    assert.equal(result.status,'failed')
    assert.equal(result.errorCode,'expired')
    assert.equal(calls,0)
    assert.equal(store.countCommands('failed'),1)
  }
})

test('create and fork reject a command that expires during async capability lookup', async () => {
  for (const kind of ['create','fork']) {
    const {service,adapter,advance} = makeService()
    let calls=0
    adapter.describeHostCapabilities = async () => { if (kind === 'create') advance(1100); return {session_create:true,session_fork:true} }
    adapter.getSession = async () => { if (kind === 'fork') advance(1100); return {sourceCapabilities:{session_fork:true}} }
    adapter.executeHost = async () => { calls++; return {} }
    adapter.execute = async () => { calls++; return {} }
    const payload = kind === 'create' ? {create_session:{}} : {fork_session:{session_id:'s1'}}
    const result = await service.handle({request:makeRequest({expiresAt:2000}),payload})
    assert.equal(result.status,'failed')
    assert.equal(result.errorCode,'expired')
    assert.equal(calls,0)
  }
})

test('approval expiration during the second Host lookup does not settle the source', async () => {
  const {service,adapter,advance} = makeService()
  const approval = adapter.requestApproval({sessionId:'s1'})
  const original = adapter.getApproval.bind(adapter)
  let reads=0,calls=0
  adapter.getApproval = async id => { if (++reads === 2) advance(1100); return original(id) }
  adapter.execute = async () => { calls++; return {} }
  const payload={resolve_approval:{approval_id:approval.approvalId,decision:'APPROVAL_DECISION_APPROVE',expected_digest:approval.digest,expected_version:approval.version}}
  const result=await service.handle({request:makeRequest({expiresAt:2000}),payload})
  assert.equal(result.status,'failed')
  assert.equal(result.errorCode,'expired')
  assert.equal(calls,0)
})

test('a completed command remains replayable without another source invocation', async () => {
  const {service,adapter,advance} = makeService()
  let calls=0
  adapter.execute = async () => { calls++; return {} }
  const request=makeRequest({expiresAt:2000})
  const payload={interrupt:{session_id:'s1'}}
  const result=await service.handle({request,payload})
  advance(500)
  const replay=await service.handle({request,payload})
  assert.equal(result.status,'succeeded')
  assert.deepEqual(replay.resultPayload,result.resultPayload)
  assert.equal(calls,1)
})

test('question capability and acceptance delays cannot cross the command deadline into the source', async () => {
  for (const phase of ['QUESTION_OPEN', 'QUESTION_CONTINUED']) {
    const ctx = makeService()
    const gate = Promise.withResolvers()
    let sourceCalls = 0
    ctx.adapter.getSession = async id => { await gate.promise; return { sessionId: id,
      sourceCapabilities: { questions_answer_open: true, questions_answer_continued: true } } }
    ctx.adapter.execute = async () => { sourceCalls++; return {} }
    const request = makeRequest({ expiresAt: 1050 })
    const payload = { answer_question: { session_id: 's1', expected_phase: phase } }
    const pending = ctx.service.handle({ request, payload })
    await new Promise(resolve => setImmediate(resolve))
    ctx.advance(50); gate.resolve()
    const result = await pending
    assert.equal(result.status, 'failed'); assert.equal(result.errorCode, 'expired')
    assert.equal(sourceCalls, 0)
    assert.equal((await ctx.service.handle({ request, payload })).errorCode, 'expired')
  }
  const ctx = makeService()
  ctx.service.onAccepted = async () => ctx.advance(50)
  let sourceCalls = 0
  ctx.adapter.execute = async () => { sourceCalls++; return {} }
  const result = await ctx.service.handle({ request: makeRequest({ expiresAt: 1050 }),
    payload: { answer_question: { session_id: 's1', expected_phase: 'QUESTION_OPEN' } } })
  assert.equal(result.errorCode, 'expired'); assert.equal(sourceCalls, 0)
})

test('explicit cancellation or revocation stops question capability preflight', async () => {
  for (const phase of ['QUESTION_OPEN', 'QUESTION_CONTINUED']) {
    const ctx = makeService(), gate = Promise.withResolvers()
    let sourceCalls = 0
    ctx.adapter.getSession = async id => { await gate.promise; return { sessionId: id,
      sourceCapabilities: { questions_answer_open: true, questions_answer_continued: true } } }
    ctx.adapter.execute = async () => { sourceCalls++; return {} }
    const request = makeRequest()
    const pending = ctx.service.handle({ request, payload: { answer_question: { session_id: 's1', expected_phase: phase } } })
    await new Promise(resolve => setImmediate(resolve))
    if (phase === 'QUESTION_OPEN') {
      const cancel = await ctx.service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }),
        payload: { cancel_question_answer: { session_id: 's1', command_id: request.command_id } } })
      assert.equal(cancel.response.question_cancel_receipt.cancellation_requested, true)
    } else {
      // Continued answers are not addressable by the open-answer cancel
      // receipt; applicable revocation still fences the scope.
      ctx.service.cancelChannel(request.device_id, request.channel_id, 'revoked')
    }
    const result = await pending
    assert.equal(result.status, 'failed'); assert.equal(result.errorCode, 'unavailable')
    gate.resolve(); await new Promise(resolve => setImmediate(resolve))
    assert.equal(sourceCalls, 0)
  }
})

function answerService(phase = 'QUESTION_CONTINUED') {
  const ctx = makeService(), gate = Promise.withResolvers()
  let started = 0
  ctx.adapter.getSession = async id => ({ sessionId: id, sourceCapabilities: { questions_answer_open: true, questions_answer_continued: true } })
  ctx.adapter.execute = async (session, payload, context) => {
    started++
    return new Promise((resolve, reject) => {
      context.signal.addEventListener('abort', () => reject(new DefinitiveExecutionError('unavailable')), { once: true })
      gate.promise.then(() => resolve({ question_receipt: { batch_id: 'b', phase: 'QUESTION_REPLY_QUEUED' } }), reject)
    })
  }
  const request = makeRequest({ expiresAt: 60_000 })
  const pending = ctx.service.handle({ request, payload: { answer_question: { session_id: 's1', batch_id: 'b', expected_phase: phase } } })
  return { ctx, request, pending, gate, started: () => started }
}

test('session-observation/AC-25.channel closure does not abort a live HTTP answer', async () => {
  for (const cancel of [
    harness => harness.ctx.service.cancelChannel(harness.request.device_id, harness.request.channel_id, 'channel_replaced'),
    harness => harness.ctx.service.cancelChannel(harness.request.device_id, harness.request.channel_id),
  ]) {
    const harness = answerService()
    try {
      while (!harness.started()) await new Promise(resolve => setImmediate(resolve))
      cancel(harness)
      const result = await (async () => { harness.gate.resolve(); return harness.pending })()
      assert.equal(result.status, 'succeeded', 'observation loss cannot cancel an admitted answer')
      assert.equal(harness.started(), 1)
    } finally { harness.ctx.store.close() }
  }
})

test('session-observation/AC-33.revocation remains authoritative for a live HTTP answer', async () => {
  const harness = answerService()
  try {
    while (!harness.started()) await new Promise(resolve => setImmediate(resolve))
    harness.ctx.service.revokeDevice(harness.request.device_id)
    const result = await harness.pending
    assert.equal(result.status, 'unknown')
    assert.equal(harness.started(), 1)
  } finally { harness.ctx.store.close() }
})

test('session-observation/AC-33.exact request cancellation stops only its own answer', async () => {
  const first = answerService(), second = answerService()
  try {
    while (!first.started() || !second.started()) await new Promise(resolve => setImmediate(resolve))
    first.ctx.service.cancelRequest(first.request.device_id, first.request.request_id, 'client_poll_expired')
    const aborted = await first.pending
    assert.equal(aborted.status, 'unknown')
    second.gate.resolve()
    assert.equal((await second.pending).status, 'succeeded')
    assert.equal(second.started(), 1)
  } finally { first.ctx.store.close(); second.ctx.store.close() }
})

test('question completion after source invocation and deadline remains unknown', async () => {
  const ctx = makeService(), gate = Promise.withResolvers()
  let calls = 0
  ctx.adapter.getSession = async id => ({ sessionId: id, sourceCapabilities: { questions_answer_continued: true } })
  ctx.adapter.execute = async () => { calls++; await gate.promise; return { question_receipt: { batch_id: 'b', phase: 'QUESTION_REPLY_QUEUED' } } }
  const request = makeRequest({ expiresAt: 1050 })
  const pending = ctx.service.handle({ request, payload: { answer_question: { session_id: 's1', expected_phase: 'QUESTION_CONTINUED' } } })
  while (!calls) await new Promise(resolve => setImmediate(resolve))
  ctx.advance(50); gate.resolve()
  assert.equal((await pending).status, 'unknown')
  assert.equal(calls, 1)
})

test('question channel loss or explicit cancel during acceptance never reaches source preflight', async () => {
  for (const phase of ['QUESTION_OPEN', 'QUESTION_CONTINUED']) {
    const ctx = makeService(), entered = Promise.withResolvers(), release = Promise.withResolvers()
    let reads = 0, calls = 0
    ctx.service.onAccepted = async () => { entered.resolve(); await release.promise }
    ctx.adapter.getSession = async id => { reads++; return { sessionId: id, sourceCapabilities: { questions_answer_open: true, questions_answer_continued: true } } }
    ctx.adapter.execute = async () => { calls++; return {} }
    const request = makeRequest()
    const pending = ctx.service.handle({ request, payload: { answer_question: { session_id: 's1', expected_phase: phase } } })
    await entered.promise
    if (phase === 'QUESTION_OPEN') {
      const cancel = await ctx.service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }),
        payload: { cancel_question_answer: { session_id: 's1', command_id: request.command_id } } })
      assert.equal(cancel.response.question_cancel_receipt.cancellation_requested, true)
    } else ctx.service.cancelChannel(request.device_id, request.channel_id)
    release.resolve()
    const result = await pending
    assert.equal(result.status, 'failed'); assert.equal(result.errorCode, 'unavailable')
    assert.equal(reads, 0); assert.equal(calls, 0)
  }
})

test('a mismatched question capability scope cannot expose or answer another session', async () => {
  const ctx = makeService()
  let reads = 0, calls = 0
  ctx.adapter.getSession = async () => ({ sessionId: 'foreign', sourceCapabilities: { questions_read: true, questions_answer_open: true } })
  ctx.adapter.listQuestionPage = async () => { reads++; return { batches: [] } }
  ctx.adapter.execute = async () => { calls++; return {} }
  const query = await ctx.service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload: { list_questions: { session_id: 's1' } } })
  assert.equal(query.errorCode, 'not_found')
  const command = await ctx.service.handle({ request: makeRequest(), payload: { answer_question: { session_id: 's1', expected_phase: 'QUESTION_OPEN' } } })
  assert.equal(command.errorCode, 'not_found')
  assert.equal(reads, 0); assert.equal(calls, 0)
})

for (const reason of ['revoked','channel_replaced','unavailable']) {
  test(`channel lifecycle ${reason} prevents queue read and mutation after capability lookup`, async () => {
    for (const kind of ['REQUEST_KIND_QUERY','REQUEST_KIND_COMMAND']) {
      const {service,adapter,store}=makeService()
      let release,entered,reads=0,writes=0
      const barrier=new Promise(resolve=>{release=resolve})
      const started=new Promise(resolve=>{entered=resolve})
      adapter.getSession=async sessionId=>{entered();await barrier;return {sessionId,sourceCapabilities:{pending_queue_read:true,pending_queue_remove:true}}}
      adapter.readPendingQueue=async()=>{reads++;return {session_id:'s1',available:true,items:[]}}
      adapter.execute=async()=>{writes++;return {}}
      const request=makeRequest({kind})
      const payload=kind==='REQUEST_KIND_QUERY'?{read_pending_queue:{session_id:'s1'}}:{update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}
      const pending=service.handle({request,payload})
      await started
      service.cancelChannel(request.device_id,request.channel_id,reason)
      release()
      const result=await pending
      assert.equal(result.errorCode,'unavailable')
      assert.equal(reads,0)
      assert.equal(writes,0)
      assert.equal(service.requests.size,0)
      if(kind==='REQUEST_KIND_COMMAND') {
        const retry=await service.handle({request,payload})
        assert.equal(retry.errorCode,'unavailable')
        assert.equal(writes,0)
      }
      store.close()
    }
  })
}

test('channel lifecycle cancellation is scoped and preserves uncertain in-flight outcomes', async () => {
  const {service,adapter,store}=makeService()
  let release,entered,calls=0
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  adapter.getSession=async sessionId=>({sessionId,sourceCapabilities:{pending_queue_remove:true}})
  adapter.execute=async()=>{calls++;entered();await barrier;return {}}
  const request=makeRequest()
  const payload={update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}
  const pending=service.handle({request,payload})
  await started
  service.cancelChannel('other-device',request.channel_id,'revoked')
  assert.equal(service.requests.size,1)
  service.cancelChannel(request.device_id,request.channel_id,'revoked')
  release()
  const result=await pending
  assert.equal(result.status,'unknown')
  assert.equal((await service.handle({request,payload})).status,'unknown')
  assert.equal(calls,1)
  assert.equal(service.requests.size,0)
  store.close()
})

test('channel lifecycle cancellation after a source mutation keeps an uncertain failure unknown', async () => {
  const {service,adapter,store}=makeService()
  let release,entered,calls=0
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  adapter.getSession=async sessionId=>({sessionId,sourceCapabilities:{pending_queue_remove:true}})
  adapter.execute=async()=>{calls++;entered();await barrier;throw new Error('synthetic post-mutation failure')}
  const request=makeRequest()
  const payload={update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}
  const pending=service.handle({request,payload})
  await started
  service.cancelChannel(request.device_id,request.channel_id,'revoked')
  release()
  assert.equal((await pending).status,'unknown')
  assert.equal((await service.handle({request,payload})).status,'unknown')
  assert.equal(calls,1)
  store.close()
})

test('channel lifecycle close suppresses a pending query result and clears its scope', async () => {
  const {service,adapter,store}=makeService()
  let release,entered
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  adapter.listSessions=async()=>{entered();await barrier;return []}
  const pending=service.handle({request:makeRequest({kind:'REQUEST_KIND_QUERY'}),payload:{list_sessions:{}}})
  await started
  service.closeRequests()
  release()
  const result=await pending
  assert.equal(result.status,'failed')
  assert.equal(result.errorCode,'unavailable')
  assert.equal(result.response,undefined)
  assert.equal(service.requests.size,0)
  store.close()
})

test('channel lifecycle prevents create and fork after asynchronous host capability checks', async () => {
  for(const operation of ['create','fork']) {
    const {service,adapter,store}=makeService()
    let entered,release,gets=0,calls=0
    const barrier=new Promise(resolve=>{release=resolve})
    const started=new Promise(resolve=>{entered=resolve})
    adapter.describeHostCapabilities=async()=>{entered();await barrier;return {session_create:true,session_fork:true}}
    adapter.getSession=async sessionId=>{gets++;return {sessionId,sourceCapabilities:{session_fork:true}}}
    adapter.executeHost=async()=>{calls++;return {}}
    adapter.execute=async()=>{calls++;return {}}
    const request=makeRequest()
    const payload=operation==='create'?{create_session:{}}:{fork_session:{session_id:'s1'}}
    const pending=service.handle({request,payload})
    await started
    service.cancelChannel(request.device_id,request.channel_id,'revoked')
    release()
    const result=await pending
    assert.equal(result.errorCode,'unavailable')
    assert.equal(gets,0)
    assert.equal(calls,0)
    store.close()
  }
})

test('channel lifecycle prevents fork after asynchronous session capability lookup', async () => {
  const {service,adapter,store}=makeService()
  let entered,release,calls=0
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  adapter.describeHostCapabilities=async()=>({session_fork:true})
  adapter.getSession=async sessionId=>{entered();await barrier;return {sessionId,sourceCapabilities:{session_fork:true}}}
  adapter.execute=async()=>{calls++;return {}}
  const request=makeRequest()
  const pending=service.handle({request,payload:{fork_session:{session_id:'s1'}}})
  await started
  service.cancelChannel(request.device_id,request.channel_id,'channel_replaced')
  release()
  assert.equal((await pending).errorCode,'unavailable')
  assert.equal(calls,0)
  store.close()
})

test('channel lifecycle prevents question reads after asynchronous session capability lookup', async () => {
  for(const operation of ['list','get']) {
    const {service,adapter,store}=makeService()
    let entered,release,calls=0
    const barrier=new Promise(resolve=>{release=resolve})
    const started=new Promise(resolve=>{entered=resolve})
    adapter.getSession=async sessionId=>{entered();await barrier;return {sessionId,sourceCapabilities:{questions_read:true}}}
    adapter.listQuestionPage=async()=>{calls++;return {batches:[]}}
    adapter.getQuestion=async()=>{calls++;return null}
    const request=makeRequest({kind:'REQUEST_KIND_QUERY'})
    const payload=operation==='list'?{list_questions:{session_id:'s1'}}:{get_question:{session_id:'s1',batch_id:'b'}}
    const pending=service.handle({request,payload})
    await started
    service.cancelChannel(request.device_id,request.channel_id,'revoked')
    release()
    assert.equal((await pending).errorCode,'unavailable')
    assert.equal(calls,0)
    store.close()
  }
})

test('channel lifecycle prevents source entry while acceptance notification is pending', async () => {
  const {service,adapter,store}=makeService()
  let entered,release,lookups=0,calls=0
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{entered=resolve})
  service.onAccepted=async()=>{entered();await barrier}
  adapter.getSession=async sessionId=>{lookups++;return {sessionId,sourceCapabilities:{pending_queue_remove:true}}}
  adapter.execute=async()=>{calls++;return {}}
  const request=makeRequest()
  const payload={update_pending_queue:{session_id:'s1',item_id:'q',action:'REMOVE'}}
  const pending=service.handle({request,payload})
  await started
  service.cancelChannel(request.device_id,request.channel_id,'revoked')
  release()
  assert.equal((await pending).errorCode,'unavailable')
  assert.equal(lookups,0)
  assert.equal(calls,0)
  store.close()
})

test('channel lifecycle prevents approval settlement across both asynchronous approval lookups', async () => {
  for(const blockAt of [1,2]) {
    const {service,adapter,store}=makeService()
    const approval=adapter.requestApproval({sessionId:'s1',toolName:'fixture'})
    let release,entered,lookups=0,calls=0
    const barrier=new Promise(resolve=>{release=resolve})
    const started=new Promise(resolve=>{entered=resolve})
    const original=adapter.getApproval.bind(adapter)
    adapter.getApproval=async id=>{lookups++;if(lookups===blockAt){entered();await barrier}return original(id)}
    adapter.execute=async()=>{calls++;return {}}
    const request=makeRequest()
    const payload={resolve_approval:{approval_id:approval.approvalId,decision:'APPROVAL_DECISION_APPROVE',expected_digest:approval.digest,expected_version:approval.version}}
    const pending=service.handle({request,payload})
    await started
    service.cancelChannel(request.device_id,request.channel_id,'revoked')
    release()
    assert.equal((await pending).errorCode,'unavailable')
    assert.equal(calls,0)
    store.close()
  }
})

test('channel lifecycle leaves a previously confirmed terminal receipt unchanged', async () => {
  const {service,adapter,store}=makeService()
  let calls=0
  adapter.getSession=async sessionId=>({sessionId,sourceCapabilities:{pending_queue_remove:true}})
  adapter.execute=async()=>{calls++;return {}}
  const request=makeRequest()
  const payload={update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}
  const first=await service.handle({request,payload})
  assert.equal(first.status,'succeeded')
  service.cancelChannel(request.device_id,request.channel_id,'revoked')
  const replay=await service.handle({request,payload})
  assert.equal(replay.status,'succeeded')
  assert.deepEqual(replay.resultPayload,first.resultPayload)
  assert.equal(calls,1)
  store.close()
})

test('channel lifecycle promptly releases a hung queue precheck and never starts source work later', async () => {
  for(const kind of ['REQUEST_KIND_QUERY','REQUEST_KIND_COMMAND']) {
    const {service,adapter,store}=makeService()
    let entered,release,calls=0
    const started=new Promise(resolve=>{entered=resolve})
    const barrier=new Promise(resolve=>{release=resolve})
    adapter.getSession=async sessionId=>{entered();await barrier;return {sessionId,sourceCapabilities:{pending_queue_read:true,pending_queue_remove:true}}}
    adapter.readPendingQueue=async()=>{calls++;return {session_id:'s1',available:true,items:[]}}
    adapter.execute=async()=>{calls++;return {}}
    const request=makeRequest({kind})
    const payload=kind==='REQUEST_KIND_QUERY'?{read_pending_queue:{session_id:'s1'}}:{update_pending_queue:{session_id:'s1',item_id:'q',action:'REMOVE'}}
    const pending=service.handle({request,payload})
    await started
    service.cancelChannel(request.device_id,request.channel_id,'revoked')
    const result=await Promise.race([pending,new Promise((_,reject)=>setTimeout(()=>reject(new Error('scope did not stop')),1000))])
    assert.equal(result.errorCode,'unavailable')
    assert.equal(service.requests.size,0)
    release()
    await new Promise(resolve=>setImmediate(resolve))
    assert.equal(calls,0)
    store.close()
  }
})

test('channel lifecycle cancellation does not affect another device request', async () => {
  const {service,adapter,store}=makeService()
  let release,startedCount=0,allStarted
  const barrier=new Promise(resolve=>{release=resolve})
  const started=new Promise(resolve=>{allStarted=resolve})
  const calls=[]
  adapter.getSession=async sessionId=>{if(++startedCount===2)allStarted();await barrier;return {sessionId,sourceCapabilities:{pending_queue_remove:true}}}
  adapter.execute=async sessionId=>{calls.push(sessionId);return {}}
  const first=makeRequest({commandId:'first-device-command'})
  const second={...makeRequest({commandId:'second-device-command'}),device_id:'other-device',channel_id:'other-channel'}
  const a=service.handle({request:first,payload:{update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}})
  const b=service.handle({request:second,payload:{update_pending_queue:{session_id:'s2',item_id:'q2',action:'REMOVE'}}})
  await started
  service.cancelChannel(first.device_id,first.channel_id,'revoked')
  assert.equal((await a).status,'unknown')
  assert.equal(service.requests.size,1)
  release()
  assert.equal((await b).status,'succeeded')
  await new Promise(resolve=>setImmediate(resolve))
  assert.deepEqual(calls,['s2'])
  assert.equal(service.requests.size,0)
  store.close()
})

test('channel lifecycle retains unknown for every native control already entered', async () => {
  for(const operation of ['create','fork','send','model','approval']) {
    const {service,adapter,store}=makeService()
    let release,entered,calls=0
    const barrier=new Promise(resolve=>{release=resolve})
    const started=new Promise(resolve=>{entered=resolve})
    adapter.describeHostCapabilities=async()=>({session_create:true,session_fork:true})
    adapter.describe=()=>({capabilities:['model-select']})
    adapter.getSession=async sessionId=>({sessionId,sourceCapabilities:{session_fork:true}})
    const approval=adapter.requestApproval({sessionId:'s1',toolName:'fixture'})
    const receipt=operation==='create'?{lifecycle_receipt:{session_id:'child',operation:'create',disposition:'READY'}}
      :operation==='fork'?{lifecycle_receipt:{session_id:'child',parent_session_id:'s1',operation:'fork',disposition:'READY'}}
      :operation==='model'?{model_selection_receipt:{selected:{provider:'p',model:'m'},effect_scope:'session_and_source_default_attempt',default_persistence:'unconfirmed'}}
      :{}
    const execute=async()=>{calls++;entered();await barrier;return receipt}
    adapter.execute=execute
    adapter.executeHost=execute
    const payload={
      create:{create_session:{}},fork:{fork_session:{session_id:'s1'}},
      send:{send_message:{session_id:'s1',text:'fixture'}},
      model:{select_session_model:{session_id:'s1',provider:'p',model:'m'}},
      approval:{resolve_approval:{approval_id:approval.approvalId,decision:'APPROVAL_DECISION_APPROVE',expected_digest:approval.digest,expected_version:approval.version}},
    }[operation]
    const request=makeRequest()
    const pending=service.handle({request,payload})
    await started
    service.cancelChannel(request.device_id,request.channel_id,'revoked')
    assert.equal((await pending).status,'unknown',operation)
    release()
    await new Promise(resolve=>setImmediate(resolve))
    assert.equal(store.getCommand(request.device_id,request.command_id).status,'unknown',operation)
    assert.equal((await service.handle({request,payload})).status,'unknown',operation)
    assert.equal(calls,1,operation)
    store.close()
  }
})

test('channel lifecycle handles synchronous invalidation followed by a source rejection', async () => {
  const {service,adapter,store}=makeService()
  const request=makeRequest()
  const payload={update_pending_queue:{session_id:'s1',item_id:'q1',action:'REMOVE'}}
  let calls=0
  adapter.getSession=async sessionId=>({sessionId,sourceCapabilities:{pending_queue_remove:true}})
  adapter.execute=()=>{calls++;service.cancelChannel(request.device_id,request.channel_id,'revoked');return Promise.reject(new Error('synthetic late source failure'))}
  assert.equal((await service.handle({request,payload})).status,'unknown')
  await new Promise(resolve=>setImmediate(resolve))
  assert.equal((await service.handle({request,payload})).status,'unknown')
  assert.equal(calls,1)
  store.close()
})

test('request expiry releases hung prechecks without entering queue source', async () => {
  for(const kind of ['REQUEST_KIND_QUERY','REQUEST_KIND_COMMAND']) {
    const {service,adapter,store}=makeService()
    service.now=()=>Date.now()
    let entered,release,calls=0
    const started=new Promise(resolve=>{entered=resolve})
    const barrier=new Promise(resolve=>{release=resolve})
    adapter.getSession=async sessionId=>{entered();await barrier;return {sessionId,sourceCapabilities:{pending_queue_read:true,pending_queue_remove:true}}}
    adapter.readPendingQueue=async()=>{calls++;return {session_id:'s1',available:true,items:[]}}
    adapter.execute=async()=>{calls++;return {}}
    const request=makeRequest({kind,expiresAt:Date.now()+200})
    const payload=kind==='REQUEST_KIND_QUERY'?{read_pending_queue:{session_id:'s1'}}:{update_pending_queue:{session_id:'s1',item_id:'q',action:'REMOVE'}}
    const pending=service.handle({request,payload})
    await started
    const result=await Promise.race([pending,new Promise((_,reject)=>setTimeout(()=>reject(new Error('expiry did not release')),1000))])
    assert.equal(result.errorCode,'expired')
    assert.equal(result.status,kind==='REQUEST_KIND_QUERY'?'failed':'unknown')
    assert.equal(service.requests.size,0)
    release()
    await new Promise(resolve=>setImmediate(resolve))
    assert.equal(calls,0)
    store.close()
  }
})

test('request expiry during an unanswered acceptance never starts question source work', async () => {
  const {service,adapter,store}=makeService()
  service.now=()=>Date.now()
  let calls=0
  service.onAccepted=async()=>new Promise(()=>{})
  adapter.getSession=async sessionId=>{calls++;return {sessionId,sourceCapabilities:{questions_answer_open:true}}}
  adapter.execute=async()=>{calls++;return {}}
  const request=makeRequest({expiresAt:Date.now()+200})
  const pending=service.handle({request,payload:{answer_question:{session_id:'s1',expected_phase:'QUESTION_OPEN'}}})
  const result=await Promise.race([pending,new Promise((_,reject)=>setTimeout(()=>reject(new Error('expiry did not release')),1000))])
  assert.equal(result.status,'failed')
  assert.equal(result.errorCode,'expired')
  assert.equal(calls,0)
  assert.equal(service.requests.size,0)
  assert.equal(service.questionAnswers.size,0)
  store.close()
})

test('request expiry after source entry remains unknown and cannot replay', async () => {
  const {service,adapter,store}=makeService()
  service.now=()=>Date.now()
  let entered,release,calls=0
  const started=new Promise(resolve=>{entered=resolve})
  const barrier=new Promise(resolve=>{release=resolve})
  adapter.getSession=async sessionId=>({sessionId,sourceCapabilities:{pending_queue_remove:true}})
  adapter.execute=async()=>{calls++;entered();await barrier;return {}}
  const request=makeRequest({expiresAt:Date.now()+200})
  const payload={update_pending_queue:{session_id:'s1',item_id:'q',action:'REMOVE'}}
  const pending=service.handle({request,payload})
  await started
  const result=await Promise.race([pending,new Promise((_,reject)=>setTimeout(()=>reject(new Error('expiry did not release')),1000))])
  assert.equal(result.status,'unknown')
  assert.equal(result.errorCode,'expired')
  release()
  await new Promise(resolve=>setImmediate(resolve))
  assert.equal(store.getCommand(request.device_id,request.command_id).status,'unknown')
  assert.equal(calls,1)
  store.close()
})

test('concurrent duplicate creation replays the original receipt and rejects a changed digest', async () => {
  const {service, adapter, store} = makeService()
  let calls = 0, entered, release
  const started = new Promise(resolve => { entered = resolve })
  const gate = new Promise(resolve => { release = resolve })
  adapter.describeHostCapabilities = async () => ({session_create:true})
  adapter.executeHost = async () => { calls++; entered(); await gate; return {lifecycle_receipt:{session_id:'created-id',operation:'create',disposition:'READY'}} }
  const request = makeRequest()
  const payload = {create_session:{}}
  const first = service.handle({request,payload})
  await started
  const duplicate = await service.handle({request,payload})
  assert.equal(duplicate.status,'host_accepted')
  const body = new Uint8Array([9,8,7])
  const changed = {...request, body, payload_digest:sha256Bytes(body)}
  const rejected = await service.handle({request:changed,payload})
  assert.equal(rejected.status,'rejected')
  assert.equal(rejected.errorCode,'payload_digest_mismatch')
  release()
  const result = await first
  const replay = await service.handle({request,payload})
  assert.equal(result.status,'succeeded')
  assert.deepEqual(replay.resultPayload,result.resultPayload)
  assert.equal(proto.decode('whalerein.v1.ResponsePayload',replay.resultPayload).lifecycle_receipt.session_id,'created-id')
  assert.equal(calls,1)
  store.close()
})

test('fork diagnostics and journal omit synthetic private transport and source details', async () => {
  const { service, adapter, store } = makeService()
  const records = []
  service.logger = Object.fromEntries(['info', 'warn', 'error'].map(level => [level, (...args) => records.push(args)]))
  adapter.describeHostCapabilities = async () => ({session_fork:true})
  adapter.getSession = async () => ({sourceCapabilities:{session_fork:true}})
  adapter.execute = async () => ({lifecycle_receipt:{session_id:'child',parent_session_id:'parent',operation:'fork',disposition:'READY',cwd:'synthetic-private-workspace'}})
  const request = makeRequest({body:new TextEncoder().encode('synthetic-private-transport')})
  const result = await service.handle({request,payload:{fork_session:{session_id:'parent'}}})
  assert.equal(result.status, 'succeeded')
  assert.ok(records.length)
  const diagnostic = JSON.stringify(records)
  const receipt = Buffer.from(store.getCommand('device-1', request.command_id).resultPayload).toString('utf8')
  for (const secret of ['synthetic-private-transport', 'synthetic-private-workspace']) {
    assert.equal(diagnostic.includes(secret), false)
    assert.equal(receipt.includes(secret), false)
  }
})



test('old source capability bytes cannot query or answer questions', async () => {
  const { service, adapter } = makeService()
  const capabilities = proto.decode('whalerein.v1.SourceCapabilities', Buffer.from([8, 1, 40, 1]))
  let calls = 0
  adapter.getSession = async id => ({ sessionId: id, sourceCapabilities: capabilities })
  adapter.listQuestionPage = async () => { calls++; return { batches: [] } }
  adapter.getQuestion = async () => { calls++; return null }
  adapter.execute = async () => { calls++; return {} }
  for (const payload of [{ list_questions: { session_id: 's1' } }, { get_question: { session_id: 's1', batch_id: 'b' } }]) {
    const result = await service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }), payload })
    assert.equal(result.errorCode, 'unsupported_capability')
  }
  for (const phase of ['QUESTION_OPEN', 'QUESTION_CONTINUED']) {
    const payload = { answer_question: { session_id: 's1', batch_id: 'b', expected_phase: phase } }
    const result = await service.handle({ request: makeRequest(), payload })
    assert.equal(result.errorCode, 'unsupported_capability')
  }
  assert.equal(calls, 0)
})

function lifecycleReceiptOf(result) {
  return proto.decode('whalerein.v1.ResponsePayload', result.resultPayload).lifecycle_receipt
}

test('creation facts map to every lifecycle disposition without product guesses', async () => {
  const { service, adapter } = makeService()
  adapter.describeHostCapabilities = async () => ({ session_create: true })
  const cases = [
    ['ready', { entered: true, identity: { session_id: 'new-id' } }, 'READY'],
    ['warning', { entered: true, identity: { session_id: 'new-id' }, attach_error: 'workspace_attach_failed' }, 'READY_WITH_WARNING'],
    ['failed', { entered: false, failure_code: 'not_found' }, 'FAILED'],
    ['unknown', { entered: true, failure_code: 'internal' }, 'UNKNOWN'],
    ['unverified warning', { entered: true, attach_error: 'workspace_attach_failed' }, 'UNKNOWN'],
  ]
  for (const [label, facts, disposition] of cases) {
    adapter.preflightCreateSession = async () => ({ workspace: 'not_configured', preset: 'not_configured', reuse: '' })
    adapter.executeHost = async () => facts
    const receipt = lifecycleReceiptOf(await service.handle({ request: makeRequest(), payload: { create_session: {} } }))
    assert.equal(receipt.disposition, disposition, label)
    assert.equal(receipt.operation, 'create')
    assert.ok(receipt.operation_id, label)
    if (disposition === 'READY' || disposition === 'READY_WITH_WARNING') {
      assert.equal(receipt.session_id, 'new-id', label)
    } else {
      assert.equal(receipt.session_id, undefined, label)
    }
    if (disposition === 'READY_WITH_WARNING') assert.equal(receipt.warning_code, 'workspace_attach_failed')
    if (disposition === 'FAILED') {
      assert.equal(receipt.reason_code, 'NATIVE_NOT_FOUND')
      assert.equal(receipt.retryable, true)
      assert.equal(receipt.phase, 'PREFLIGHT')
    }
    if (disposition === 'UNKNOWN') {
      assert.equal(receipt.reconcile_mode, 'OPERATION_ID')
      assert.equal(receipt.retryable, undefined)
      assert.equal(receipt.phase, 'NATIVE')
    }
  }
})

test('unknown creation uses a preferred identity as the deterministic reconcile key', async () => {
  const { service, adapter } = makeService()
  adapter.describeHostCapabilities = async () => ({ session_create: true })
  adapter.preflightCreateSession = async () => ({ workspace: 'not_configured', preset: 'not_configured', reuse: '' })
  adapter.executeHost = async () => ({ entered: true, failure_code: 'unavailable' })
  const receipt = lifecycleReceiptOf(await service.handle({ request: makeRequest(), payload: { create_session: { preferred_session_id: 'wanted' } } }))
  assert.equal(receipt.disposition, 'UNKNOWN')
  assert.equal(receipt.reconcile_mode, 'PREFERRED_SESSION_ID')
  assert.equal(receipt.reconcile_key, 'wanted')
})

test('fork facts map to ready warning failed and unknown without inventing lineage', async () => {
  const { service, adapter } = makeService()
  adapter.describeHostCapabilities = async () => ({ session_fork: true })
  adapter.getSession = async () => ({ sourceCapabilities: { session_fork: true } })
  adapter.preflightForkSession = async () => ({ completed_turn: true })
  const cases = [
    ['ready', { entered: true, identity: { session_id: 'child', parent_session_id: 'parent' } }, 'READY'],
    ['warning', { entered: true, identity: { session_id: 'child', parent_session_id: 'parent' }, attach_error: 'workspace_attach_failed' }, 'READY_WITH_WARNING'],
    ['failed', { entered: false, failure_code: 'conflict' }, 'FAILED'],
    ['unknown', { entered: true, failure_code: 'internal' }, 'UNKNOWN'],
  ]
  for (const [label, facts, disposition] of cases) {
    adapter.execute = async () => facts
    const receipt = lifecycleReceiptOf(await service.handle({ request: makeRequest(), payload: { fork_session: { session_id: 'parent' } } }))
    assert.equal(receipt.disposition, disposition, label)
    assert.equal(receipt.operation, 'fork')
    if (disposition === 'READY' || disposition === 'READY_WITH_WARNING') {
      assert.equal(receipt.session_id, 'child')
      assert.equal(receipt.parent_session_id, 'parent')
    } else {
      assert.equal(receipt.session_id, undefined)
    }
    if (disposition === 'FAILED') {
      assert.equal(receipt.reason_code, 'NATIVE_CONFLICT')
      assert.equal(receipt.retryable, true)
      assert.equal(receipt.parent_session_id, 'parent')
    }
    if (disposition === 'UNKNOWN') {
      assert.equal(receipt.reconcile_mode, 'PARENT_SESSION_ID')
      assert.equal(receipt.reconcile_key, 'parent')
      assert.equal(receipt.parent_session_id, 'parent')
    }
  }
})

test('creation preflight failures never invoke native creation', async () => {
  for (const [facts, reason] of [
    [{ workspace: 'missing', preset: 'not_configured' }, 'WORKSPACE_NOT_FOUND'],
    [{ workspace: 'unattachable', preset: 'not_configured' }, 'WORKSPACE_UNAVAILABLE'],
    [{ workspace: 'present', preset: 'missing' }, 'PRESET_NOT_FOUND'],
    [{ workspace: 'present', preset: 'unavailable' }, 'PRESET_UNAVAILABLE'],
  ]) {
    const { service, adapter } = makeService()
    let calls = 0
    adapter.describeHostCapabilities = async () => ({ session_create: true })
    adapter.preflightCreateSession = async () => facts
    adapter.executeHost = async () => { calls++; return { entered: true, identity: { session_id: 'unexpected' } } }
    const receipt = lifecycleReceiptOf(await service.handle({ request: makeRequest(), payload: { create_session: { workspace_id: 'w' } } }))
    assert.equal(receipt.disposition, 'FAILED', reason)
    assert.equal(receipt.reason_code, reason)
    assert.equal(receipt.session_id, undefined)
    assert.equal(calls, 0, reason)
  }
})

test('invalid creation arguments fail at preflight without a native call', async () => {
  const { service, adapter } = makeService()
  adapter.describeHostCapabilities = async () => ({ session_create: true })
  let calls = 0
  adapter.executeHost = async () => { calls++; return { entered: true, identity: { session_id: 'x' } } }
  const receipt = lifecycleReceiptOf(await service.handle({ request: makeRequest(), payload: { create_session: { cwd: 'relative' } } }))
  assert.equal(receipt.disposition, 'FAILED')
  assert.equal(receipt.reason_code, 'INVALID_ARGUMENT')
  assert.equal(receipt.retryable, true)
  assert.equal(calls, 0)
})

test('creation preflight proves a reusable identity without another native create', async () => {
  const { service, adapter } = makeService()
  let calls = 0
  adapter.describeHostCapabilities = async () => ({ session_create: true })
  adapter.preflightCreateSession = async () => ({ workspace: 'present', preset: 'present', reuse: 'blank-id' })
  adapter.executeHost = async () => { calls++; return { entered: true, identity: { session_id: 'other' } } }
  const receipt = lifecycleReceiptOf(await service.handle({ request: makeRequest(), payload: { create_session: { preferred_session_id: 'blank-id' } } }))
  assert.equal(receipt.disposition, 'READY')
  assert.equal(receipt.session_id, 'blank-id')
  assert.equal(calls, 0)
})

test('fork preflight failures never invoke native fork', async () => {
  const { service, adapter } = makeService()
  let calls = 0
  adapter.describeHostCapabilities = async () => ({ session_fork: true })
  adapter.getSession = async () => ({ sourceCapabilities: { session_fork: true } })
  adapter.preflightForkSession = async () => ({ completed_turn: false })
  adapter.execute = async () => { calls++; return { entered: true, identity: { session_id: 'child', parent_session_id: 'parent' } } }
  const receipt = lifecycleReceiptOf(await service.handle({ request: makeRequest(), payload: { fork_session: { session_id: 'parent' } } }))
  assert.equal(receipt.disposition, 'FAILED')
  assert.equal(receipt.reason_code, 'SOURCE_NO_COMPLETE_TURN')
  assert.equal(receipt.session_id, undefined)
  assert.equal(receipt.retryable, undefined)
  assert.equal(calls, 0)
})

test('fork preflight reports a missing source before any native call', async () => {
  const { service, adapter } = makeService()
  let calls = 0
  adapter.describeHostCapabilities = async () => ({ session_fork: true })
  adapter.getSession = async () => { throw new DefinitiveExecutionError('not_found') }
  adapter.execute = async () => { calls++; return {} }
  const receipt = lifecycleReceiptOf(await service.handle({ request: makeRequest(), payload: { fork_session: { session_id: 'missing' } } }))
  assert.equal(receipt.disposition, 'FAILED')
  assert.equal(receipt.reason_code, 'SOURCE_NOT_FOUND')
  assert.equal(calls, 0)
})

test('a stale in-flight operation read after restart maps to unknown with its operation id', async () => {
  const { service, store } = makeService()
  const request = makeRequest()
  store.createOrGetCommand({ deviceId: 'device-1', commandId: request.command_id, requestId: request.request_id,
    digest: request.payload_digest, expiresAt: 10_000, now: 1000 })
  assert.equal(store.beginLifecycleOperation({ deviceId: 'device-1', commandId: request.command_id,
    operationId: 'op-stable', operation: 'create', phase: 'NATIVE', preferredSessionId: 'wanted', now: 1000 }), true)
  store.recoverAcceptedCommands()
  const query = async commandId => service.handle({ request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }),
    payload: { get_command_result: { command_id: commandId } } })
  const result = await query(request.command_id)
  assert.equal(result.response.command_result.found, true)
  assert.equal(result.response.command_result.status, 'unknown')
  const receipt = proto.decode('whalerein.v1.ResponsePayload', result.response.command_result.result_payload).lifecycle_receipt
  assert.equal(receipt.disposition, 'UNKNOWN')
  assert.equal(receipt.operation_id, 'op-stable')
  assert.equal(receipt.session_id, undefined)
  assert.equal(receipt.reconcile_mode, 'PREFERRED_SESSION_ID')
  assert.equal(receipt.reconcile_key, 'wanted')
  assert.equal(receipt.reason_code, 'PROCESS_INTERRUPTED')
})

test('lifecycle receipts never carry private cwd or native diagnostics', async () => {
  const { service, adapter } = makeService()
  adapter.describeHostCapabilities = async () => ({ session_create: true })
  adapter.preflightCreateSession = async () => ({ workspace: 'present', preset: 'present', reuse: '' })
  adapter.executeHost = async () => ({ entered: true, identity: { session_id: 'new-id' }, attach_error: 'workspace_attach_failed',
    cwd: '/private/workspace', detail: 'private native error' })
  const receipt = lifecycleReceiptOf(await service.handle({ request: makeRequest(), payload: { create_session: { workspace_id: 'w' } } }))
  assert.equal(JSON.stringify(receipt).includes('/private/workspace'), false)
  assert.equal(JSON.stringify(receipt).includes('private native error'), false)
})


test('definitive approval precheck rejections are journaled and replay the exact approval code after restart', async () => {
  const { service, adapter, store, advance } = makeService()
  const pending = adapter.requestApproval({ sessionId: 's1', ttlMs: 1000 })
  const payload = (extra = {}) => ({ resolve_approval: {
    approval_id: extra.approvalId ?? pending.approvalId,
    decision: extra.decision ?? 'APPROVAL_DECISION_APPROVE',
    expected_digest: extra.digest ?? pending.digest,
    expected_version: extra.version ?? pending.version,
  } })
  const cases = [
    ['approval-unknown', payload({ approvalId: 'missing' }), 'approval_unavailable'],
    ['approval-decision', payload({ decision: 'APPROVAL_DECISION_UNSPECIFIED' }), 'invalid_argument'],
    ['approval-digest', payload({ digest: new Uint8Array(32) }), 'approval_version_mismatch'],
    ['approval-version', payload({ version: 99 }), 'approval_version_mismatch'],
  ]
  for (const [commandId, body, code] of cases) {
    const request = makeRequest({ commandId })
    const result = await service.handle({ request, payload: body })
    assert.equal(result.status, 'failed', commandId)
    assert.equal(result.errorCode, code, commandId)
    const stored = store.getCommand('device-1', commandId)
    assert.equal(stored.status, 'failed')
    assert.equal(stored.errorCode, code)
    assert.ok(stored.resultPayload instanceof Uint8Array && stored.resultPayload.length > 0)
    const replay = await service.handle({ request, payload: body })
    assert.equal(replay.status, 'failed')
    assert.equal(replay.errorCode, code)
    assert.deepEqual([...replay.resultPayload], [...result.resultPayload])
  }
  const expired = adapter.requestApproval({ sessionId: 's1', ttlMs: 10 })
  advance(20)
  const expiredRequest = makeRequest({ commandId: 'approval-expired' })
  const expiredResult = await service.handle({ request: expiredRequest, payload: {
    resolve_approval: { approval_id: expired.approvalId, decision: 'APPROVAL_DECISION_REJECT', expected_digest: expired.digest, expected_version: expired.version },
  } })
  assert.equal(expiredResult.status, 'failed')
  assert.equal(expiredResult.errorCode, 'approval_expired')
  assert.equal(store.getCommand('device-1', 'approval-expired').errorCode, 'approval_expired')

  const approved = await service.handle({ request: makeRequest({ commandId: 'approval-first' }), payload: payload() })
  assert.equal(approved.status, 'succeeded')
  const repeatedRequest = makeRequest({ commandId: 'approval-repeated' })
  const repeated = await service.handle({ request: repeatedRequest, payload: payload() })
  assert.equal(repeated.status, 'failed')
  assert.equal(repeated.errorCode, 'approval_already_resolved')
  assert.equal(store.getCommand('device-1', 'approval-repeated').errorCode, 'approval_already_resolved')
  assert.equal(store.countCommands('accepted'), 0)
})

test('a model selection that expires during acceptance never reaches the source', async () => {
  const ctx = makeService()
  ctx.adapter.describe = () => ({ mode: 'fixture', available: true, capabilities: ['model-select'] })
  ctx.service.onAccepted = async () => ctx.advance(50)
  let sourceCalls = 0
  ctx.adapter.execute = async () => {
    sourceCalls++
    return { model_selection_receipt: { selected: { provider: 'p', model: 'm' },
      effect_scope: 'session_and_source_default_attempt', default_persistence: 'unconfirmed' } }
  }
  const result = await ctx.service.handle({ request: makeRequest({ expiresAt: 1050 }),
    payload: { select_session_model: { session_id: 's1', provider: 'p', model: 'm' } } })
  assert.equal(result.status, 'failed')
  assert.equal(result.errorCode, 'expired')
  assert.equal(sourceCalls, 0)
  // The same command replays its stored terminal outcome without a new native call.
  assert.equal((await ctx.service.handle({ request: makeRequest({ expiresAt: 1050 }),
    payload: { select_session_model: { session_id: 's1', provider: 'p', model: 'm' } } })).errorCode, 'expired')
  assert.equal(sourceCalls, 0)
})

test('question pagination cursors survive a Host restart through the persisted key', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'whalerein-pager-'))
  const statePath = join(directory, 'state.sqlite')
  const batch = (id, size = 20) => ({ session_id: 's', batch_id: id, source_epoch: 'e', version: 1,
    digest: new Uint8Array(32), phase: 'QUESTION_OPEN', answerable: true,
    questions: [{ id: 'q', question: 'x'.repeat(size) }] })
  const build = () => {
    const store = new StateStore({ path: statePath, now: () => 1000 })
    const service = new CommandService({ store, adapter: new TestSource({ sessions: [{ sessionId: 's1' }] }),
      events: {}, now: () => 1000 })
    return { store, service }
  }
  try {
    const first = build()
    const source = { batches: [batch('one'), batch('two'), batch('three')] }
    const inputs = { deviceId: 'device-1', sessionId: 's', source }
    let budget = 1
    while (budget < 1000) {
      try { if (first.service.questionPager.page({ ...inputs, maxBytes: budget }).batches[0]?.questions?.length === 1) break } catch {}
      budget++
    }
    const page = first.service.questionPager.page({ ...inputs, maxBytes: budget })
    assert.ok(page.next_cursor)
    const expected = first.service.questionPager.page({ ...inputs, cursor: page.next_cursor, maxBytes: budget }).batches[0].batch_id
    first.store.close()

    const restarted = build()
    try {
      assert.equal(restarted.service.questionPager.page({ ...inputs, cursor: page.next_cursor, maxBytes: budget }).batches[0].batch_id, expected)
    } finally { restarted.store.close() }
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
})

test('concurrent start-if-idle sends serialize so the loser observes the started turn instead of passing the idle check', async () => {
  const { service, adapter, store } = makeService()
  const session = adapter.sessions.get('s1')
  session.running = false
  let entered
  let release
  const started = new Promise((resolve) => { entered = resolve })
  const barrier = new Promise((resolve) => { release = resolve })
  let sends = 0
  const sendMessage = adapter.sendMessage.bind(adapter)
  adapter.sendMessage = async (...args) => {
    sends += 1
    entered()
    await barrier
    session.running = true
    return sendMessage(...args)
  }
  const payload = { send_message: { session_id: 's1', text: 'first', delivery_mode: 'START_IF_IDLE' } }
  const first = service.handle({ request: makeRequest({ commandId: 'start-first' }), payload })
  await started
  const second = service.handle({ request: makeRequest({ commandId: 'start-second' }), payload })
  await new Promise((resolve) => setImmediate(resolve))
  assert.equal(sends, 1, 'the second send waits behind the per-session gate')
  release()
  const results = await Promise.all([first, second])
  assert.equal(sends, 1)
  assert.deepEqual(results.map((result) => result.status).sort(), ['failed', 'succeeded'])
  assert.equal(results.find((result) => result.status === 'failed').errorCode, 'conflict')
  assert.equal(store.getCommand('device-1', 'start-second').errorCode, 'conflict')
  store.close()
})

for (const scenario of [
  { kind: 'create', label: 'create with a proven attach warning', disposition: 'READY_WITH_WARNING', sessionId: 'created-warning' },
  { kind: 'create', label: 'create with an ambiguous native outcome', disposition: 'UNKNOWN', sessionId: null },
  { kind: 'fork', label: 'fork with a proven attach warning', disposition: 'READY_WITH_WARNING', sessionId: 'child-warning' },
  { kind: 'fork', label: 'fork with an ambiguous native outcome', disposition: 'UNKNOWN', sessionId: null },
]) {
  test(`recovered ${scenario.label} keeps its disposition contract across a failed terminal write and reopen`, async t => {
    const directory = mkdtempSync(join(tmpdir(), 'whalerein-lifecycle-recovery-'))
    const path = join(directory, 'state.sqlite')
    let store = new StateStore({ path, now: () => 1000 })
    t.after(() => { store.close(); rmSync(directory, { recursive: true, force: true }) })
    let hostCalls = 0
    let forkCalls = 0
    const adapter = new TestSource({ now: () => 1000 })
    adapter.describeHostCapabilities = async () => ({ session_create: true, session_fork: true })
    adapter.getSession = async () => ({ sourceCapabilities: { session_fork: true } })
    adapter.executeHost = async () => {
      hostCalls += 1
      return scenario.sessionId
        ? { entered: true, identity: { session_id: scenario.sessionId }, attach_error: 'workspace_attach_failed' }
        : { entered: true, failure_code: 'transport_lost' }
    }
    adapter.execute = async () => {
      forkCalls += 1
      return scenario.sessionId
        ? { entered: true, identity: { session_id: scenario.sessionId }, attach_error: 'workspace_attach_failed' }
        : { entered: true, failure_code: 'transport_lost' }
    }
    const service = new CommandService({ store, adapter, events: {}, now: () => 1000, codec: proto })
    const failTerminalWrite = store.setCommandStatus.bind(store)
    store.setCommandStatus = fields => {
      if (fields.status === 'succeeded') throw new Error('fixture terminal write fault')
      return failTerminalWrite(fields)
    }
    const request = makeRequest({ commandId: `lifecycle-recovery-${scenario.kind}-${scenario.disposition}` })
    const payload = scenario.kind === 'create' ? { create_session: {} } : { fork_session: { session_id: 'parent' } }
    const first = await service.handle({ request, payload })
    assert.equal(first.status, 'unknown', 'a failed terminal write stays uncertain instead of claiming an outcome')
    assert.equal(scenario.kind === 'create' ? hostCalls : forkCalls, 1)

    store.close()
    store = new StateStore({ path, now: () => 1000 })
    const restarted = new CommandService({ store, adapter, events: {}, now: () => 1000, codec: proto })
    restarted.recover()

    const lookup = async () => (await restarted.handle({
      request: makeRequest({ kind: 'REQUEST_KIND_QUERY' }),
      payload: { get_command_result: { command_id: request.command_id } },
    })).response.command_result
    const command = await lookup()
    assert.equal(command.found, true)
    assert.equal(command.status, 'unknown')
    const inner = proto.decode('whalerein.v1.ResponsePayload', command.result_payload).lifecycle_receipt
    assert.equal(inner.operation, scenario.kind)
    assert.ok(typeof inner.operation_id === 'string' && inner.operation_id !== '')
    if (scenario.disposition === 'READY_WITH_WARNING') {
      assert.equal(inner.disposition, 'READY_WITH_WARNING')
      assert.equal(inner.session_id, scenario.sessionId, 'the observed identity survives recovery')
      assert.equal(inner.warning_code, 'workspace_attach_failed', 'the recovered warning code is the one the fixture proved')
      if (scenario.kind === 'fork') assert.equal(inner.parent_session_id, 'parent')
    } else {
      assert.equal(inner.disposition, 'UNKNOWN')
      assert.equal(inner.session_id, undefined, 'UNKNOWN must not claim an unproven identity')
      assert.ok(['OPERATION_ID', 'PREFERRED_SESSION_ID', 'PARENT_SESSION_ID', 'MANUAL'].includes(inner.reconcile_mode))
      assert.equal(typeof inner.reconcile_key, 'string')
      if (scenario.kind === 'create') assert.equal(inner.reconcile_mode, 'OPERATION_ID')
      if (scenario.kind === 'fork') {
        assert.equal(inner.reconcile_mode, 'PARENT_SESSION_ID')
        assert.equal(inner.reconcile_key, 'parent')
      }
    }
    const repeated = await lookup()
    assert.deepEqual(repeated.result_payload, command.result_payload, 'recovery is stable across repeated lookup')
    const replay = await restarted.handle({ request, payload })
    assert.equal(replay.status, 'unknown')
    assert.equal(scenario.kind === 'create' ? hostCalls : forkCalls, 1, 'recovery and replay never invoke the native action again')
  })
}

test('an approval cancelled after its precheck resolved is reported as unavailable', async () => {
  const { service, adapter } = makeService()
  const approval = adapter.requestApproval({ sessionId: 's1', ttlMs: 1000 })
  const request = makeRequest()
  const original = adapter.getApproval.bind(adapter)
  adapter.getApproval = async (approvalId) => {
    const value = await original(approvalId)
    // Cancel one microtask later: the precheck has already produced its value,
    // but the request scope observes the abort before registering acceptance.
    queueMicrotask(() => service.cancelRequest(request.device_id, request.request_id, 'unavailable'))
    return value
  }
  try {
    const result = await service.handle({
      request,
      payload: {
        resolve_approval: {
          approval_id: approval.approvalId,
          decision: 'APPROVAL_DECISION_APPROVE',
          expected_digest: approval.digest,
          expected_version: approval.version,
        },
      },
    })
    assert.equal(result.status, 'failed')
    assert.equal(result.errorCode, 'unavailable')
  } finally {
    adapter.getApproval = original
  }
})
