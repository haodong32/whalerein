import test from 'node:test'
import assert from 'node:assert/strict'
import { projectMessage } from '../src/adapter/projection.js'
import { FakeDshAdapter } from '../src/adapter/fake.js'
import { codec } from 'whalerein-host/protocol.js'

const event = (type, data, seq = 1) => ({ sessionId: 's', view: 'v', type, data, dshSeq: seq, createdAt: 5 })

test('assistant projections keep attachment metadata and a useful tool summary', () => {
  const message = projectMessage('s', 'v', event('assistant/message', {
    message: { content: [
      { type: 'text', text: 'hello' },
      { type: 'tool-call', id: 'call-1', name: 'bash', arguments: '{"command":"private"}' },
      { type: 'image', attachment: { mediaType: 'image/png', bytes: 42, name: 'shot.png', attachmentId: 'private' } },
      { type: 'file', attachment: { bytes: 7, name: '../secret.txt' } },
    ] },
  }))
  assert.equal(message.role, 'ASSISTANT')
  assert.match(message.blocks[0].text.text, /hello/)
  assert.deepEqual(message.blocks[1].tool_call, { call_id: 'call-1', name: 'bash', summary: 'bash' })
  assert.deepEqual(message.blocks[2].attachment, { media_type: 'image/png', name: 'shot.png', size: 42, available: false })
  assert.deepEqual(message.blocks[3].attachment, { media_type: '', name: '', size: 7, available: false })
  const projected = JSON.stringify(message)
  assert.doesNotMatch(projected, /private|secret\.txt|attachmentId/)
})

test('unknown content blocks keep a typed fallback instead of a generic placeholder', () => {
  const message = projectMessage('s', 'v', event('assistant/message', {
    message: { content: [{ type: 'mystery-block', payload: 'private' }] },
  }))
  assert.equal(message.blocks[0].notice.category, 'unsupported_content')
  assert.match(message.blocks[0].notice.text, /mystery-block/)
  assert.doesNotMatch(JSON.stringify(message), /private/)
})

test('runtime context and skill catalogs are projected as context notices, not user text', () => {
  for (const [kind, label] of [
    ['runtime-context', /Runtime context updated/],
    ['skill-invocation', /Skill catalog updated/],
    ['system-prompt', /System prompt context updated/],
  ]) {
    const message = projectMessage('s', 'v', event('user/message', {
      message: { content: [{ type: 'text', text: '/Users/private/skills/secret' }], source: { kind } },
    }))
    assert.equal(message.role, 'NOTICE')
    assert.equal(message.blocks[0].notice.category, 'source_context')
    assert.match(message.blocks[0].notice.text, label)
    assert.doesNotMatch(JSON.stringify(message), /Users|skills|secret/)
  }
  const realUser = projectMessage('s', 'v', event('user/message', {
    message: { content: [{ type: 'text', text: 'hello' }], source: { kind: 'user', rpcId: 'r' } },
  }))
  assert.equal(realUser.role, 'USER')
  assert.equal(realUser.blocks[0].text.text, 'hello')
})

test('unknown committed events are dropped while explicit notices and errors remain bounded notices', () => {
  assert.equal(projectMessage('s', 'v', event('plugin:unknown', { text: 'internal' })), null)
  assert.equal(projectMessage('s', 'v', event('hook/something', { text: 'internal' })), null)
  assert.equal(projectMessage('s', 'v', event('notice', { text: 'visible' })).blocks[0].notice.category, 'source_notice')
  assert.equal(projectMessage('s', 'v', event('tool/error', {})).blocks[0].notice.category, 'source_error')
})

test('a single oversized native message is bounded to the history page budget without splitting Unicode', async () => {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's' }] })
  await adapter.open()
  adapter.output('s', '😀x'.repeat(20_000))
  const page = await adapter.readHistory('s', { snapshot: true, max_bytes: 2048 })
  assert.equal(page.batch.changes.length, 1)
  const message = page.batch.changes[0].message
  const size = codec().encode('whalerein.v1.ConversationMessage', message).length + 32
  assert.ok(size <= 2048, `projected message is ${size} bytes`)
  assert.equal(message.blocks.at(-1).notice.text, 'Content truncated to the history page budget')
  const text = message.blocks[0].text.text
  assert.ok(text.length > 0 && text.length < 20_000)
  assert.equal(text.isWellFormed(), true)
  await adapter.close()
})

test('context sources outside the known labels still become bounded notices without content', () => {
  const message = projectMessage('s', 'v', event('user/message', {
    message: { content: [{ type: 'text', text: '/private/path' }], source: { kind: 'future-context' } },
  }))
  assert.equal(message.role, 'NOTICE')
  assert.equal(message.blocks[0].notice.text, 'Source context updated')
  assert.doesNotMatch(JSON.stringify(message), /private/)
})

test('tool call and tool result events keep a typed, bounded representation', () => {
  const call = projectMessage('s', 'v', event('tool/call', { callId: 'call-9', name: 'read', arguments: '{"path":"/private"}' }))
  assert.equal(call.role, 'TOOL')
  assert.deepEqual(call.blocks[0].tool_call, { call_id: 'call-9', name: 'read', summary: 'read' })
  const result = projectMessage('s', 'v', event('tool/result', { callId: 'call-9', result: 'failed output', isError: true }))
  assert.equal(result.blocks[0].tool_result.is_error, true)
  assert.equal(result.blocks[0].tool_result.text, 'failed output')
  assert.doesNotMatch(JSON.stringify([call, result]), /private/)
})

test('attachment blocks without a nested attachment object and empty messages stay bounded', () => {
  const message = projectMessage('s', 'v', event('assistant/message', {
    message: { content: [{ type: 'file', name: 'plain.txt', bytes: 3 }] },
  }))
  assert.deepEqual(message.blocks[0].attachment, { media_type: '', name: 'plain.txt', size: 3, available: false })
  const empty = projectMessage('s', 'v', event('assistant/message', { message: { content: [] } }))
  assert.equal(empty.blocks[0].notice.text, 'Source activity')
  const unknown = projectMessage('s', 'v', event('assistant/message', { message: { content: [null] } }))
  assert.match(unknown.blocks[0].notice.text, /unknown/)
})
