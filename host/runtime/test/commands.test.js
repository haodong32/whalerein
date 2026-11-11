import test from 'node:test'
import assert from 'node:assert/strict'
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
  assert.equal(unknown.status, 'rejected')
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
