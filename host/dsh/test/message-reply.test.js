import assert from 'node:assert/strict'
import { test } from 'node:test'
import { messageReplyEvidence } from '../src/adapter/real.js'

const step = (type, turn, number) => ({ type: `step/${type}`, data: { turn, step: number } })
const user = rpcId => ({ type: 'user/message', data: { source: { kind: 'user', rpcId } } })
const assistant = (turn, number, dshSeq, ...content) => ({
  type: 'assistant/message', dshSeq, data: { turn, step: number, message: { content } },
})
const text = value => ({ type: 'text', text: value })

test('message reply matches durable admission and completed native attempt', () => {
  const unrelated = [
    step('start', 1, 1), user('other'), assistant(1, 1, 10, text('other')),
    step('end', 1, 1), { type: 'turn/end' },
  ]
  assert.deepEqual(messageReplyEvidence(unrelated, 'mine'), { state: 'MESSAGE_REPLY_PENDING' })
  const events = [
    ...unrelated, step('start', 2, 1), user('mine'), assistant(2, 1, 21, text('mine')),
    step('end', 2, 1),
  ]
  assert.deepEqual(messageReplyEvidence(events, 'mine'), {
    state: 'MESSAGE_REPLY_COMMITTED', attempt_id: '21', text: 'mine',
  })
})

test('message reply waits through a tool attempt before accepting committed text', () => {
  const prefix = [
    step('start', 1, 1), user('mine'),
    assistant(1, 1, 10, text('prefix'), { type: 'tool-call' }),
  ]
  assert.deepEqual(messageReplyEvidence(prefix, 'mine'), { state: 'MESSAGE_REPLY_PENDING', active: true })
  assert.deepEqual(messageReplyEvidence([
    ...prefix, step('end', 1, 1), step('start', 1, 2),
    assistant(1, 2, 20, text('done')), step('end', 1, 2),
  ], 'mine'), { state: 'MESSAGE_REPLY_COMMITTED', attempt_id: '20', text: 'done' })
})

test('message reply leaves batched or boundary-free events uncertain', () => {
  const events = [
    step('start', 1, 1), user('mine'), user('other'),
    assistant(1, 1, 10, text('shared')), step('end', 1, 1),
  ]
  assert.deepEqual(messageReplyEvidence(events, 'mine'), { state: 'MESSAGE_REPLY_UNCERTAIN' })
  assert.deepEqual(messageReplyEvidence(events.slice(1), 'mine'), { state: 'MESSAGE_REPLY_UNCERTAIN' })
})
