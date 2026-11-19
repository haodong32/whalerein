import test from 'node:test'
import assert from 'node:assert/strict'
import { FakeDshAdapter } from '../src/adapter/fake.js'
import { RealDshAdapter } from '../src/adapter/real.js'

async function source(options = {}) {
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's' }, { sessionId: 'other' }], hostId: 'host-1', ...options })
  await adapter.open()
  return adapter
}

function emitAttachment(adapter, { kind = 'file', sessionId = 's', data = new Uint8Array([1, 2, 3]), mediaType = '', name = '' } = {}) {
  const native = adapter.saveFixtureAttachment({ kind, data, mediaType, name })
  adapter.emit(sessionId, {
    type: 'user/message',
    data: {
      id: `u-${native.attachmentId}`,
      role: 'user',
      content: [{ type: 'text', text: 'see attachment' }, { type: kind, attachment: native }],
      source: { kind: 'user' },
    },
  })
  return native
}

async function latestAttachment(adapter, sessionId = 's') {
  const result = await adapter.readHistory(sessionId, { page: {}, max_messages: 1 })
  return result.batch.changes[0].message.blocks.find(block => block.attachment).attachment
}

async function readAll(reader, chunk = 2) {
  const chunks = []
  for (let page = 0; page < 100; page++) {
    const result = await reader.read(chunk)
    chunks.push(...result.bytes)
    if (result.eof) return Uint8Array.from(chunks)
  }
  throw new Error('reader did not terminate')
}

test('a stored image opens with authoritative metadata and a bounded cancellable reader', async () => {
  const adapter = await source()
  try {
    emitAttachment(adapter, { kind: 'image', data: new Uint8Array([9, 8, 7, 6, 5]), mediaType: 'image/png', name: 'shot.png' })
    const content = await latestAttachment(adapter)
    assert.equal(content.available, true)
    assert.ok(content.attachment_ref)
    const opened = await adapter.openAttachment('s', content.attachment_ref)
    assert.equal(opened.attachment.size, 5)
    assert.equal(opened.attachment.media_type, 'image/png')
    assert.equal(opened.attachment.name, 'shot.png')
    assert.equal(opened.attachment.sha256, undefined)

    const first = await opened.reader.read(2)
    assert.deepEqual([...first.bytes], [9, 8])
    assert.equal(first.eof, false)
    const rest = await opened.reader.read(99)
    assert.deepEqual([...rest.bytes], [7, 6, 5])
    assert.equal(rest.eof, true)
    await opened.reader.close()
    await assert.rejects(opened.reader.read(1), error => error.code === 'not_found')
    // Close is idempotent.
    await opened.reader.close({ cancel: true })
  } finally {
    await adapter.close()
  }
})

test('an exact file read returns byte-identical data, a proven sha256 and handles empty files', async () => {
  const adapter = await source()
  try {
    const payload = new Uint8Array([10, 20, 30, 40, 50])
    emitAttachment(adapter, { kind: 'file', data: payload, name: 'report.bin' })
    const content = await latestAttachment(adapter)
    const opened = await adapter.openAttachment('s', content.attachment_ref)
    assert.equal(opened.attachment.size, payload.length)
    assert.equal(opened.attachment.sha256.length, 32)
    const bytes = await readAll(opened.reader, 3)
    assert.deepEqual([...bytes], [...payload])
    await opened.reader.close()

    emitAttachment(adapter, { kind: 'file', data: new Uint8Array(0), name: 'empty.bin' })
    const emptyContent = await latestAttachment(adapter)
    const empty = await adapter.openAttachment('s', emptyContent.attachment_ref)
    assert.equal(empty.attachment.size, 0)
    const result = await empty.reader.read(64)
    assert.equal(result.bytes.length, 0)
    assert.equal(result.eof, true)
    await empty.reader.close()
  } finally {
    await adapter.close()
  }
})

test('an occurrence outside the recent tail is proven from bounded pages before any byte read', async () => {
  const adapter = await source()
  try {
    emitAttachment(adapter, { kind: 'file', data: new Uint8Array([1, 2, 3, 4]), name: 'old.bin' })
    const content = await latestAttachment(adapter)
    for (let index = 0; index < 20; index++) adapter.output('s', `tail-${index}`)

    const original = adapter.nativeEventsPage.bind(adapter)
    let pages = 0
    adapter.nativeEventsPage = async (sessionId, options = {}) => {
      pages++
      return original(sessionId, { ...options, maxMessages: 2 })
    }
    const opened = await adapter.openAttachment('s', content.attachment_ref)
    assert.ok(pages > 1)
    assert.equal(opened.attachment.size, 4)
    assert.deepEqual([...(await readAll(opened.reader))], [1, 2, 3, 4])
    await opened.reader.close()
  } finally {
    await adapter.close()
  }
})

test('absent, foreign and corrupt occurrences fail closed before or during the first byte read', async () => {
  const adapter = await source()
  try {
    emitAttachment(adapter, { kind: 'file', data: new Uint8Array([1, 2, 3]), name: 'a.bin' })
    const content = await latestAttachment(adapter)

    await assert.rejects(adapter.openAttachment('s', 'deadbeef'), error => error.code === 'not_found')
    await assert.rejects(adapter.openAttachment('other', content.attachment_ref), error => error.code === 'not_found')
    await assert.rejects(adapter.openAttachment('missing', content.attachment_ref), error => error.code === 'not_found')
    // A foreign scope never reaches a native byte read.
    let reads = 0
    adapter.nativeReadFileStream = async () => { reads++; throw new Error('must not read') }
    adapter.nativeReadImage = async () => { reads++; throw new Error('must not read') }
    await assert.rejects(adapter.openAttachment('other', content.attachment_ref), error => error.code === 'not_found')
    assert.equal(reads, 0)
  } finally {
    await adapter.close()
  }
})

test('a corrupt stored object is rejected at verification instead of surfacing damaged bytes', async () => {
  const adapter = await source()
  try {
    const native = emitAttachment(adapter, { kind: 'file', data: new Uint8Array([1, 2, 3, 4]), name: 'c.bin' })
    const content = await latestAttachment(adapter)
    adapter.corruptFixtureAttachment(native.attachmentId)
    const opened = await adapter.openAttachment('s', content.attachment_ref)
    await assert.rejects(readAll(opened.reader, 4), error => error.code === 'payload_digest_mismatch')
    await opened.reader.close()

    const image = emitAttachment(adapter, { kind: 'image', data: new Uint8Array([5, 6, 7]), mediaType: 'image/png' })
    const imageContent = await latestAttachment(adapter)
    adapter.corruptFixtureAttachment(image.attachmentId)
    const imageReader = await adapter.openAttachment('s', imageContent.attachment_ref)
    await assert.rejects(imageReader.reader.read(3), error => error.code === 'payload_digest_mismatch')
    await imageReader.reader.close()
  } finally {
    await adapter.close()
  }
})

test('opening returns metadata without a native byte read and cancellation aborts a pending reader', async () => {
  const adapter = await source()
  try {
    emitAttachment(adapter, { kind: 'file', data: new Uint8Array([1, 2, 3]) })
    const content = await latestAttachment(adapter)
    let reads = 0
    const originalImage = adapter.nativeReadImage.bind(adapter)
    const originalFile = adapter.nativeReadFileStream.bind(adapter)
    adapter.nativeReadImage = async (...args) => { reads++; return originalImage(...args) }
    adapter.nativeReadFileStream = async (...args) => { reads++; return originalFile(...args) }
    const opened = await adapter.openAttachment('s', content.attachment_ref)
    assert.equal(reads, 0)
    await adapter.closeAttachment('s', content.attachment_ref, { cancel: true })
    await assert.rejects(opened.reader.read(1), error => error.code === 'not_found')

    const controller = new AbortController()
    controller.abort()
    await assert.rejects(adapter.openAttachment('s', content.attachment_ref, { signal: controller.signal }), error => error.code === 'unavailable')
  } finally {
    await adapter.close()
  }
})

test('attachment advertisement follows real native support and stays explicit when absent', async () => {
  const adapter = await source()
  try {
    const capabilities = (await adapter.getSession('s')).sourceCapabilities
    assert.equal(capabilities.attachments, true)
    assert.equal(capabilities.history_page, true)
    assert.equal(capabilities.history_exact, true)
  } finally {
    await adapter.close()
  }

  const attachments = {
    readImage: async () => ({ data: new Uint8Array() }),
    readFileStream: async function* () {},
  }
  const live = () => ({ id: 's', header: { id: 's' }, snapshotEvents: () => [] })
  const withSupport = new RealDshAdapter({
    ctx: { sessionController: { events: async () => ({ records: [], hasMore: false }) }, sessions: { list: () => [live()], get: () => live() }, attachments },
    config: { hostId: 'host-1' },
  })
  await withSupport.open()
  assert.equal((await withSupport.getSession('s')).sourceCapabilities.attachments, true)
  await withSupport.close()

  const without = new RealDshAdapter({
    ctx: { sessionController: { events: async () => ({ records: [], hasMore: false }) }, sessions: { list: () => [live()], get: () => live() } },
    config: { hostId: 'host-1' },
  })
  await without.open()
  assert.equal((await without.getSession('s')).sourceCapabilities.attachments, false)
  await without.close()
})

test('the real adapter proves the occurrence from passive native history before reading bytes', async () => {
  const payload = new Uint8Array([4, 5, 6])
  const digest = 'a'.repeat(64)
  const native = { attachmentId: `sha256:${digest}`, name: 'real.bin', bytes: payload.length }
  const event = {
    type: 'user/message',
    seq: 1,
    time: 1,
    data: { id: 'u1', role: 'user', content: [{ type: 'text', text: 'see file' }, { type: 'file', attachment: native }], source: { kind: 'user' } },
  }
  const live = { id: 's', header: { id: 's' }, seq: 2, snapshotEvents: () => [event] }
  let byteReads = 0
  const ctx = {
    sessions: { list: () => [live], get: (id) => (id === 's' ? live : undefined) },
    sessionController: {
      list: async () => ({ items: [{ sessionId: 's' }] }),
      events: async (request) => ({ records: request.beforeSeq === undefined ? [{ event }] : [], hasMore: false, cursor: 1 }),
    },
    attachments: {
      readImage: async () => ({ data: new Uint8Array() }),
      readFileStream: async function* () {
        byteReads++
        yield payload
      },
    },
  }
  const adapter = new RealDshAdapter({ ctx, config: { hostId: 'host-1' } })
  await adapter.open()
  try {
    const page = await adapter.readHistory('s', { page: {} })
    const content = page.batch.changes[0].message.blocks.find(block => block.attachment).attachment
    assert.equal(content.available, true)
    assert.equal(byteReads, 0)
    const opened = await adapter.openAttachment('s', content.attachment_ref)
    assert.equal(opened.attachment.size, payload.length)
    assert.equal(opened.attachment.sha256.length, 32)
    assert.deepEqual([...(await readAll(opened.reader, 2))], [...payload])
    assert.equal(byteReads, 1)
    await opened.reader.close()
  } finally {
    await adapter.close()
  }
})
