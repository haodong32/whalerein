import test from 'node:test'
import assert from 'node:assert/strict'
import { projectMessage } from '../src/adapter/projection.js'
import { FakeDshAdapter } from '../src/adapter/fake.js'
import { codec } from 'whalerein-host/protocol.js'
import { validateContentBlock } from 'whalerein-host/contracts/source.js'

const event = (type, data, seq = 1) => ({ sessionId: 's', view: 'v', type, data, dshSeq: seq, createdAt: 5 })
const assistant = (content, seq = 1) => event('assistant/message', {
  turn: 1, step: 1,
  message: { id: 'a', role: 'assistant', content, source: { kind: 'model', provider: 'p', model: 'm' } },
}, seq)
const user = (content, source, seq = 2) => event('user/message', { id: 'u', role: 'user', content, source }, seq)

test('assistant projections keep ordered reasoning, answer and declaration blocks with adapter-owned identities', () => {
  const message = projectMessage('s', 'v', assistant([
    { type: 'text', text: 'hello' },
    { type: 'reasoning', text: 'because' },
    { type: 'tool-call', id: 'call-1', name: 'bash', arguments: '{"command":"private"}' },
    { type: 'image', attachment: { attachmentId: 'image-hash', mediaType: 'image/png', bytes: 42, name: 'shot.png' } },
    { type: 'file', attachment: { attachmentId: 'file-hash', name: '../secret.txt', bytes: 7 } },
    { type: 'mystery-block', payload: 'private' },
  ]))
  assert.equal(message.role, 'ASSISTANT')
  assert.deepEqual(message.blocks.map(block => block.block_id), ['text-0', 'reasoning-1', 'tool-2', 'attachment-3', 'attachment-4'])
  assert.equal(message.blocks[0].text.text, 'hello')
  assert.equal(message.blocks[1].reasoning.text, 'because')
  assert.deepEqual(message.blocks[2].tool_activity, {
    call_id: 'call-1', stages: ['TOOL_STAGE_PREPARATION'], name: 'bash', summary: 'bash', outcome: 'TOOL_OUTCOME_UNSPECIFIED',
  })
  assert.equal(message.blocks[3].attachment.available, false)
  assert.equal(message.blocks[3].attachment.name, 'shot.png')
  assert.equal(message.blocks[4].attachment.name, '')
  assert.equal(message.blocks[4].attachment.size, 7)
  for (const block of message.blocks) assert.doesNotThrow(() => validateContentBlock(block))
  const projected = JSON.stringify(message)
  assert.doesNotMatch(projected, /private|secret\.txt|image-hash|file-hash|mystery-block/)
})

test('a reasoning-disabled model emits ordinary answers with no synthetic reasoning block', () => {
  const message = projectMessage('s', 'v', assistant([{ type: 'text', text: 'plain' }]))
  assert.deepEqual(message.blocks, [{ block_id: 'text-0', text: { text: 'plain' } }])
})

test('genuine user input stays USER while every other user-role source is a safe context notice or omission', () => {
  const real = projectMessage('s', 'v', user([{ type: 'text', text: 'hello' }], { kind: 'user', rpcId: 'r' }))
  assert.equal(real.role, 'USER')
  assert.equal(real.blocks[0].text.text, 'hello')

  for (const [kind, label] of [
    ['runtime-context', /Runtime context updated/],
    ['skill-invocation', /Skill context updated/],
    ['system-prompt', /System prompt context updated/],
  ]) {
    const message = projectMessage('s', 'v', user([{ type: 'text', text: '/Users/private/skills/secret' }], { kind }))
    assert.equal(message.role, 'MESSAGE_ROLE_CONTEXT')
    assert.equal(message.blocks[0].notice.category, 'NOTICE_CATEGORY_CONTEXT')
    assert.match(message.blocks[0].notice.summary, label)
    assert.doesNotMatch(JSON.stringify(message), /Users|skills|secret/)
  }

  const supplied = projectMessage('s', 'v', user([{ type: 'text', text: 'private body' }], {
    kind: 'agent-instructions', form: 'notice', summary: 'Instructions changed',
  }))
  assert.equal(supplied.blocks[0].notice.summary, 'Instructions changed')
  assert.doesNotMatch(JSON.stringify(supplied), /private body/)

  const unknown = projectMessage('s', 'v', user([{ type: 'text', text: '/private/path' }], { kind: 'future-context' }))
  assert.equal(unknown.role, 'MESSAGE_ROLE_UNKNOWN')
  assert.equal(unknown.blocks[0].notice.category, 'NOTICE_CATEGORY_OMISSION')
  assert.doesNotMatch(JSON.stringify(unknown), /private/)

  const missing = projectMessage('s', 'v', user([{ type: 'text', text: '/private/path' }], undefined))
  assert.equal(missing.role, 'MESSAGE_ROLE_UNKNOWN')
  assert.equal(missing.blocks[0].notice.category, 'NOTICE_CATEGORY_OMISSION')
  assert.doesNotMatch(JSON.stringify(missing), /private/)
})

test('private prompts, provider replay state and unmapped committed events never enter normalized views', () => {
  assert.equal(projectMessage('s', 'v', event('system/message', { message: { content: [{ type: 'text', text: 'private prompt' }] } })), null)
  assert.equal(projectMessage('s', 'v', event('developer/message', { message: { content: [{ type: 'text', text: 'private instructions' }] } })), null)
  assert.equal(projectMessage('s', 'v', event('assistant/attempt', { stream: [{ text: 'private partial' }] })), null)
  assert.equal(projectMessage('s', 'v', event('plugin:unknown', { text: 'private plugin body' })), null)
  assert.equal(projectMessage('s', 'v', event('notice', { text: 'raw' })), null)
  assert.equal(projectMessage('s', 'v', event('tool/error', {})), null)

  const assistant = projectMessage('s', 'v', event('assistant/message', {
    turn: 1, step: 1,
    message: {
      id: 'a', role: 'assistant', content: [{ type: 'text', text: 'answer' }],
      source: { kind: 'model', provider: 'p', model: 'm', replayState: { secret: 'provider-state' } },
    },
  }))
  assert.equal(assistant.blocks[0].text.text, 'answer')
  assert.doesNotMatch(JSON.stringify(assistant), /provider-state|replayState/)
})

test('tool dispatch and results project one correlation identity with evidenced stages and ordered displayable result content', () => {
  const call = projectMessage('s', 'v', event('tool/call', {
    turn: 1, step: 1, callId: 'call-9', name: 'read', arguments: '{"path":"/private"}',
  }, 3))
  assert.equal(call.role, 'TOOL')
  assert.deepEqual(call.blocks[0].tool_activity, {
    call_id: 'call-9', stages: ['TOOL_STAGE_EXECUTION'], name: 'read', summary: 'read', outcome: 'TOOL_OUTCOME_UNSPECIFIED',
  })

  const result = projectMessage('s', 'v', event('tool/result', {
    turn: 1, step: 1,
    message: {
      id: 't', role: 'tool',
      content: [
        { type: 'text', text: 'line one' },
        { type: 'file', attachment: { attachmentId: 'file-hash', name: 'report.pdf', bytes: 12 } },
        { type: 'text', text: 'line two' },
        { type: 'mystery', payload: 'private' },
      ],
      source: { kind: 'tool', callId: 'call-9' },
      isError: false,
    },
  }, 4))
  const activity = result.blocks[0].tool_activity
  assert.equal(activity.call_id, 'call-9')
  assert.deepEqual(activity.stages, ['TOOL_STAGE_SETTLEMENT'])
  assert.equal(activity.outcome, 'TOOL_OUTCOME_SUCCEEDED')
  assert.deepEqual(activity.result.map(part => Object.keys(part)[0]), ['text', 'attachment', 'text'])
  assert.equal(activity.result[0].text.text, 'line one')
  assert.equal(activity.result[2].text.text, 'line two')
  assert.equal(activity.result[1].attachment.name, 'report.pdf')
  assert.equal(activity.result[1].attachment.available, false)
  assert.doesNotMatch(JSON.stringify([call, result]), /private|file-hash/)
})

test('a failed tool result settles with a display-safe stable failure code and no raw reason', () => {
  const result = projectMessage('s', 'v', event('tool/result', {
    turn: 1, step: 1,
    message: {
      id: 't', role: 'tool', content: [{ type: 'text', text: 'failed output' }],
      source: { kind: 'tool', callId: 'call-9' }, isError: true,
    },
    error: { name: 'BashError', code: 'EXIT_NONZERO', reason: 'secret raw reason' },
  }, 4))
  const activity = result.blocks[0].tool_activity
  assert.equal(activity.outcome, 'TOOL_OUTCOME_FAILED')
  assert.equal(activity.failure_code, 'EXIT_NONZERO')
  assert.equal(activity.result[0].text.text, 'failed output')
  assert.doesNotMatch(JSON.stringify(result), /secret raw reason|BashError/)
  const missingCode = projectMessage('s', 'v', event('tool/result', {
    message: { content: [], source: { kind: 'tool', callId: 'call-2' }, isError: true },
  }, 5))
  assert.equal(missingCode.blocks[0].tool_activity.failure_code, 'tool_error')
})

test('a result whose call record is outside the loaded window still establishes a settled activity', () => {
  const result = projectMessage('s', 'v', event('tool/result', {
    turn: 1, step: 1,
    message: { content: [{ type: 'text', text: 'ok' }], source: { kind: 'tool', callId: 'call-3' }, isError: false },
  }, 1))
  assert.equal(result.blocks[0].tool_activity.call_id, 'call-3')
  assert.deepEqual(result.blocks[0].tool_activity.stages, ['TOOL_STAGE_SETTLEMENT'])
  assert.equal(result.blocks[0].tool_activity.outcome, 'TOOL_OUTCOME_SUCCEEDED')
})

test('PTC sub-dispatches keep their own session-scoped call identities without arguments', () => {
  const start = projectMessage('s', 'v', event('tool/ptc-dispatch-start', {
    rootCallId: 'root', parentCallId: 'root', subCallId: 'root:ptc:1', name: 'read', arguments: { path: '/private' },
  }, 6))
  assert.equal(start.blocks[0].tool_activity.call_id, 'root:ptc:1')
  assert.deepEqual(start.blocks[0].tool_activity.stages, ['TOOL_STAGE_EXECUTION'])
  const done = projectMessage('s', 'v', event('tool/ptc-dispatch', {
    rootCallId: 'root', parentCallId: 'root', subCallId: 'root:ptc:1', name: 'read', arguments: { path: '/private' },
    content: [{ type: 'text', text: 'ok' }], isError: false,
  }, 7))
  assert.equal(done.blocks[0].tool_activity.call_id, 'root:ptc:1')
  assert.deepEqual(done.blocks[0].tool_activity.stages, ['TOOL_STAGE_SETTLEMENT'])
  assert.doesNotMatch(JSON.stringify([start, done]), /private/)
})

test('attachment occurrence references are deterministic and scoped to the committed session position', () => {
  const sample = (sessionId, seq) => projectMessage(sessionId, 'v', event('user/message', {
    id: 'u', role: 'user', content: [{ type: 'image', attachment: { attachmentId: 'shared', mediaType: 'image/png', bytes: 3 } }],
    source: { kind: 'user' },
  }, seq))
  const first = sample('s', 1).blocks[0].attachment.attachment_ref
  assert.equal(sample('s', 1).blocks[0].attachment.attachment_ref, first)
  assert.notEqual(sample('other', 1).blocks[0].attachment.attachment_ref, first)
  assert.notEqual(sample('s', 2).blocks[0].attachment.attachment_ref, first)
  assert.equal(sample('s', 1).blocks[0].attachment.available, false)
})

test('history never truncates a canonical message; one oversize item occupies the page alone', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's' }] })
  await adapter.open()
  adapter.output('s', '😀x'.repeat(20_000))
  const page = await adapter.readHistory('s', { snapshot: true, max_bytes: 2048 })
  assert.equal(page.batch.changes.length, 1)
  const message = page.batch.changes[0].message
  const text = message.blocks[0].text.text
  assert.equal(text, '😀x'.repeat(20_000))
  assert.equal(message.blocks.length, 1)
  assert.ok(codec().encode('whalerein.v1.ConversationMessage', message).length > 2048)
  assert.equal(page.complete, true)
  await adapter.close()
})

test('empty and unmapped assistant content stays out of history instead of a raw fallback', () => {
  assert.equal(projectMessage('s', 'v', assistant([])), null)
  assert.equal(projectMessage('s', 'v', assistant([null, { type: 'mystery' }])), null)
})
