import test from 'node:test'
import assert from 'node:assert/strict'
import {
  DshAdapter,
  adapterError,
  isControlEventKind,
  isEventKind,
  normalizeApproval,
  normalizeEvent,
  normalizeSummary,
  summaryToProto,
  CONTROL_EVENT_KINDS,
  EVENT_KINDS,
  APPROVAL_STATES,
  APPROVAL_DECISIONS,
} from '../src/adapter/base.js'
import { AdapterError, PluginError } from 'whalerein-host/errors.js'

test('when abstract adapter methods are called, they fail closed with stable error codes', async () => {
  const adapter = new DshAdapter()
  await adapter.open()
  assert.equal(adapter.opened, true)
  await adapter.close()
  assert.equal(adapter.opened, false)
  await assert.rejects(adapter.listSessions(), (error) => error.code === 'unsupported_capability')
  await assert.rejects(adapter.getSessionSnapshot('s'), (error) => error.code === 'unsupported_capability')
  await assert.rejects(adapter.sendUserMessage('s', 'm'), (error) => error.code === 'unsupported_capability')
  await assert.rejects(adapter.interrupt('s'), (error) => error.code === 'unsupported_capability')
  await assert.rejects(adapter.resolveApproval(), (error) => error.code === 'approval_unavailable')
  assert.equal(await adapter.getApproval('a'), null)
  assert.equal(typeof adapter.subscribeEvents(), 'function')
  assert.equal(typeof adapter.subscribeApprovals(), 'function')
  assert.deepEqual(adapter.describe(), { mode: 'abstract', available: false })
})

test('when native event and approval fields are missing or invalid, normalization supplies explicit safe defaults and rejects missing session scope', () => {
  const event = normalizeEvent({ sessionId: 's1', type: 'x', kind: 'output', createdAt: 0, dshSeq: -1 })
  assert.equal(normalizeEvent({ sessionId: 's1', type: 12 }).type, '')
  assert.equal(event.createdAt, 0)
  assert.equal(event.dshSeq, null)
  assert.equal(event.ephemeral, false)
  assert.equal(event.approval, null)
  assert.throws(() => normalizeEvent({}), (error) => error.code === 'invalid_argument')
  assert.throws(() => normalizeEvent({ sessionId: 's1', kind: 'bogus' }), (error) => error.code === 'internal')

  const approval = normalizeApproval({
    approvalId: 'a',
    sessionId: 's',
    digest: [1, 2],
    state: 'bogus',
    decision: 'bogus',
  })
  assert.deepEqual([...approval.digest], [1, 2])
  assert.equal(approval.state, 'unavailable')
  assert.equal(approval.decision, 'unspecified')
  assert.equal(normalizeApproval(null), null)
  const emptyApproval = normalizeApproval({ approvalId: 'a', sessionId: 's' })
  assert.equal(emptyApproval.digest.length, 0)
  assert.equal(emptyApproval.toolName, '')

  const summary = normalizeSummary({ sessionId: 's1' })
  assert.equal(summary.title, '')
  assert.equal(summary.cursor, 0)
  assert.equal(summary.archived, false)
  assert.equal(normalizeSummary({ sessionId: 's1', archived: true }).archived, true)
  const proto = summaryToProto({ ...summary, cursor: 3 })
  assert.equal(proto.archived, false)
  assert.equal(summaryToProto({ sessionId: 's2', archived: true }).archived, true)
  assert.equal(summaryToProto({ sessionId: 's2' }).title, '')
  assert.equal(proto.session_id, 's1')
  assert.equal(proto.history_cursor, '')
  assert.equal(proto.agent_available, false)
})

test('when classifying native events, the adapter recognizes the fixed DSH event vocabulary and treats only lifecycle kinds as control', () => {
  const expected = ['output', 'approval_requested', 'approval_resolved', 'session_state', 'notice']
  const controls = ['approval_requested', 'approval_resolved', 'session_state', 'notice']
  assert.deepEqual(EVENT_KINDS, expected)
  assert.deepEqual(CONTROL_EVENT_KINDS, controls)
  for (const kind of expected) assert.equal(isEventKind(kind), true)
  for (const kind of controls) assert.equal(isControlEventKind(kind), true)
  assert.equal(isEventKind('bogus'), false)
  assert.equal(isControlEventKind('output'), false)
  for (const kind of [null, undefined, '', 'OUTPUT', 'bogus']) {
    assert.equal(isEventKind(kind), false)
    assert.equal(isControlEventKind(kind), false)
  }
  assert.deepEqual(APPROVAL_STATES, ['pending', 'approved', 'rejected', 'expired', 'unavailable'])
  assert.deepEqual(APPROVAL_DECISIONS, ['unspecified', 'approve', 'reject'])
})

test('when an adapter fails, typed errors are preserved and unknown failures map to stable errors', () => {
  const adapterFailure = new AdapterError('not_found')
  assert.equal(adapterError(adapterFailure), adapterFailure)
  const pluginFailure = new PluginError('expired')
  assert.equal(adapterError(pluginFailure), pluginFailure)
  assert.equal(adapterError(new Error('session/not-found')).code, 'not_found')
  assert.equal(adapterError(new Error('something odd')).code, 'internal')
})
