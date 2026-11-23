import test from 'node:test'
import assert from 'node:assert/strict'
import { startHarness, waitFor } from './support/harness.js'

const crashFree = { approvalOnFirstMessage: false }

test('when a client pairs with FakeDSH, queries, commands, event replay, sync, and interrupt complete', async () => {
  const harness = await startHarness({
    sessions: [{ sessionId: 's1', title: 'One', cwd: '/tmp', running: false }],
    behavior: crashFree,
    config: { reconnect_min_ms: 50, reconnect_max_ms: 100 },
  })
  try {
    const device = await harness.newPairedDevice('device-1')
    assert.equal(harness.host.diagnostics().activeChannels, 1)

    const list = await device.query({ list_sessions: {} })
    assert.equal(list.status, 'succeeded')
    assert.equal(list.response.session_list.sessions[0].session_id, 's1')
    assert.equal(list.response.session_list.sessions[0].cursor ?? 0, 0)

    const send = await device.command({ send_message: { session_id: 's1', text: 'hello from remote' } }, { commandId: 'cmd-1' })
    assert.equal(send.status, 'succeeded')
    assert.equal(send.errorCode ?? 'error_code_unspecified', 'error_code_unspecified')
    assert.equal(typeof send.response.command_accepted.message_id, 'string')
    assert.deepEqual(send.results.map((result) => result.status), ['host_accepted', 'succeeded'])
    assert.equal(harness.host.diagnostics().commands.accepted, 0)

    await waitFor(() => device.bufferedEvents.length >= 3)
    const events = await device.events()
    const messages=events.flatMap(p=>p.update.history?.changes??[]).map(c=>c.message)
    assert.deepEqual(messages.map(m=>m.role), ['USER','ASSISTANT'])
    assert.equal(messages[1].blocks[0].text.text.length, 'hello from remote'.length)
    assert.ok(events.every(p=>p.update.watch_id))
    await waitFor(() => harness.host.diagnostics().pendingCiphertext.count === 0)

    const replay = await device.send(send.request)
    assert.equal(replay.status, 'succeeded')
    const afterReplay = (await harness.adapter.getSessionSnapshot('s1')).events
    assert.equal(afterReplay.filter((event) => event.type === 'user/message').length, 1)

    const commandResult = await device.query({ get_command_result: { command_id: 'cmd-1' } })
    assert.equal(commandResult.status, 'succeeded')
    assert.equal(commandResult.response.command_result.found, true)
    assert.equal(commandResult.response.command_result.status, 'succeeded')
    assert.ok(commandResult.response.command_result.result_payload.length > 0)

    const current = (await device.query({ get_command_result: { command_id: 'cmd-1' } })).status
    assert.equal(current, 'succeeded')
    const baseline=await device.query({read_history:{session_id:'s1',snapshot:true}})
    const cursor=baseline.response.history_page.batch.to_cursor
    const empty=await device.query({read_history:{session_id:'s1',after_cursor:cursor}})
    assert.equal(empty.response.history_page.batch.changes.length,0)
    assert.equal(empty.response.history_page.complete,true)

    const snapshot=await device.query({read_history:{session_id:'s1',snapshot:true}})
    assert.equal(snapshot.status,'succeeded')
    assert.equal(snapshot.response.history_page.batch.changes.length,2)

    const interrupted = await device.command({ interrupt: { session_id: 's1' } }, { commandId: 'cmd-2' })
    assert.equal(interrupted.status, 'succeeded')
    const ping = await device.query({ ping: { client_time_unix_ms: 1 } })
    assert.equal(typeof ping.response.pong.host_time_unix_ms, 'number')
    assert.deepEqual(Object.keys(harness.host.diagnostics()).sort(), [
      'access',
      'activeChannels',
      'attachments',
      'commands',
      'cursors',
      'pendingCiphertext',
      'revocationsPending',
      'source',
    ])
  } finally {
    await harness.stop()
  }
})

test('when FakeDSH requests approval, the client reads it and the host reports its resolution', async () => {
  const harness = await startHarness({
    sessions: [{ sessionId: 's1', running: false }],
    behavior: { approvalOnFirstMessage: true, approvalTtlMs: 300_000 },
  })
  try {
    const device = await harness.newPairedDevice('device-1')
    const send = await device.command({ send_message: { session_id: 's1', text: 'needs approval' } }, { commandId: 'cmd-approval' })
    assert.equal(send.status, 'succeeded')

    // Pending approvals are content: a session state is a bounded transition
    // summary, so the client reads them from the bounded ApprovalList query.
    let approval
    await waitFor(async () => {
      const list = await device.query({ list_approvals: { session_id: 's1' } })
      assert.equal(list.status, 'succeeded')
      approval = list.response.approval_list.approvals.find(a => a.state === 'APPROVAL_STATE_PENDING')
      return approval !== undefined
    }, 5000)
    assert.equal(approval.state, 'APPROVAL_STATE_PENDING')
    assert.equal(approval.session_id, 's1')

    const fetched = await device.query({ get_approval: { approval_id: approval.approval_id } })
    assert.equal(fetched.response.approval.approval_id, approval.approval_id)

    const resolved = await device.command({
      resolve_approval: {
        approval_id: approval.approval_id,
        decision: 'APPROVAL_DECISION_APPROVE',
        expected_digest: approval.digest,
        expected_version: approval.version,
      },
    }, { commandId: 'cmd-resolve' })
    assert.equal(resolved.status, 'succeeded')
    const approvalNow = await device.query({ get_approval: { approval_id: approval.approval_id } })
    assert.equal(approvalNow.response.approval.state, 'APPROVAL_STATE_APPROVED')
    // A resolution arrives as a state transition; the approval itself is no
    // longer in the state and the bounded list now reports it resolved.
    const laterList = await device.query({ list_approvals: { session_id: 's1' } })
    assert.ok(laterList.response.approval_list.approvals
      .every((entry) => entry.approval_id !== approval.approval_id
        || entry.state === 'APPROVAL_STATE_APPROVED'))

    const second = await device.command({
      resolve_approval: {
        approval_id: approval.approval_id,
        decision: 'APPROVAL_DECISION_REJECT',
        expected_digest: approval.digest,
        expected_version: approval.version,
      },
    }, { commandId: 'cmd-resolve-again' })
    assert.equal(second.status, 'failed')
    assert.equal(second.errorCode, 'approval_already_resolved')
  } finally {
    await harness.stop()
  }
})

test('after reconnect, the host establishes a fresh channel and rejects old-channel frames', async () => {
  const harness = await startHarness({
    sessions: [{ sessionId: 's1', running: false }],
    behavior: crashFree,
    config: { reconnect_min_ms: 50, reconnect_max_ms: 100 },
  })
  try {
    const device = await harness.newPairedDevice('device-1')
    const oldChannel = device.channel.channelId
    harness.broker.disconnectClientId('whalerein-host-host-1')
    await waitFor(() => harness.host.access.describe().connected === false)
    await waitFor(() => harness.host.access.describe().connected === true, 5000)
    await waitFor(() => harness.host.diagnostics().activeChannels === 0, 5000)
    assert.equal(harness.host.security.revocationsPending, false)

    const rejected = await device.query({ list_sessions: {} })
    assert.equal(rejected.status, 'rejected')
    assert.equal(rejected.errorCode, 'unauthorized')

    await device.connectChannel()
    assert.notEqual(device.channel.channelId, oldChannel)
    const list = await device.query({ list_sessions: {} })
    assert.equal(list.status, 'succeeded')
    assert.equal(harness.host.diagnostics().activeChannels, 1)
  } finally {
    await harness.stop()
  }
})

test('after revocation replay, the host removes the peer and rejects new handshakes', async () => {
  const harness = await startHarness({
    sessions: [{ sessionId: 's1', running: false }],
    behavior: crashFree,
    config: { reconnect_min_ms: 50, reconnect_max_ms: 100 },
  })
  try {
    const device = await harness.newPairedDevice('device-1')
    const replayBefore = harness.access.revocationResults.length
    harness.access.queueRevocations([{ replay_id: 'revoke-1', device_ids: ['device-1'], final: true, revoked_at_unix_ms: 10 }])
    harness.broker.disconnectClientId('whalerein-host-host-1')
    await waitFor(() => harness.access.revocationResults.length > replayBefore, 5000)
    await waitFor(() => harness.host.security.revocationsPending === false, 5000)
    assert.equal(harness.host.security.listChannels().length, 0)
    assert.equal(harness.host.diagnostics().activeChannels, 0)
    await assert.rejects(device.connectChannel(), (error) => error.code === 'unauthorized')
  } finally {
    await harness.stop()
  }
})

test('when a two MiB command crosses a 64 KiB broker frame limit, exact committed content survives events and paged history while duplicate execution is suppressed', async () => {
  const harness=await startHarness({maxPacketBytes:64*1024,sessions:[{sessionId:'s',running:false}],behavior:crashFree})
  try {
    const device=await harness.newPairedDevice('large-device')
    const payload='x'.repeat(2*1024*1024)
    const sent=await device.command({send_message:{session_id:'s',text:payload}},{commandId:'large-once'})
    assert.equal(sent.status,'succeeded')
    await waitFor(()=>device.bufferedEvents.length>=3)
    const events=await device.events()
    assert.ok(events.some(event=>(event.update.history?.changes??[]).some(c=>c.message.blocks.some(b=>b.text?.text===payload))))
    const result=await device.query({read_history:{session_id:'s',snapshot:true}})
    assert.equal(result.status,'succeeded')
    const page=result.response.history_page
    assert.equal(page.batch.changes.length,1)
    const messages=page.batch.changes.map(change=>change.message)
    let cursor=page.batch.to_cursor,delivered=page.batch.changes.length,complete=page.complete
    while(!complete) {
      const rest=await device.query({read_history:{session_id:'s',snapshot:true,after_cursor:cursor,cut_cursor:page.cut_cursor}})
      const next=rest.response.history_page
      assert.notEqual(next.batch.to_cursor,cursor)
      messages.push(...next.batch.changes.map(change=>change.message))
      delivered+=next.batch.changes.length;cursor=next.batch.to_cursor;complete=next.complete
    }
    assert.equal(delivered,2)
    assert.equal(new Set(messages.map(message=>message.message_id)).size,2)
    // A single canonical message larger than the soft page target occupies its
    // page alone and is never truncated: the Host owns the hard encoded bound.
    for(const message of messages) {
      assert.equal(message.blocks.length,1)
      assert.equal(message.blocks[0].text.text,payload)
      assert.equal(message.blocks.some(block=>block.notice),false)
    }
    const repeated=await device.send(sent.request)
    assert.equal(repeated.status,'succeeded')
    assert.deepEqual(repeated.response,sent.response)
    const source=await harness.adapter.getSessionSnapshot('s')
    assert.equal(source.events.filter(event=>event.type==='user/message').length,1)
    assert.ok(harness.broker.messages.every(message=>message.payload.length<=64*1024))
    assert.equal(harness.host.access.describe().reconnects,0)
  } finally {await harness.stop()}
})

test('typed live reasoning, tool activity and notices traverse the Host watch with stable kinds and capabilities', async () => {
  const harness = await startHarness({ sessions: [{ sessionId: 's', running: false }], behavior: crashFree })
  try {
    const device = await harness.newPairedDevice('typed-device')
    const session = await device.query({ list_sessions: {} })
    const capabilities = session.response.session_list.sessions[0].source_capabilities
    assert.equal(capabilities.live_reasoning, true)
    assert.equal(capabilities.tool_activity, true)
    assert.equal(capabilities.conversation_notices, true)
    assert.equal(capabilities.attachments, true)
    assert.equal(capabilities.history_page, true)
    assert.equal(capabilities.history_exact, true)
    assert.equal(capabilities.send_followup, true)

    const emit = data => harness.adapter.emit('s', { type: 'assistant/preview', kind: 'output', ephemeral: true, data: { stream_id: 'typed', ...data } })
    emit({ phase: 'start', revision: 1, reply_id: '1:1' })
    emit({ phase: 'chunk', revision: 2, block_id: 'reasoning-0', block_kind: 'BLOCK_KIND_REASONING', text: 'why' })
    emit({ phase: 'chunk', revision: 3, block_id: 'text-1', block_kind: 'BLOCK_KIND_TEXT', text: 'answer' })
    await waitFor(() => device.bufferedEvents.some(payload => payload.update.live?.delta?.kind === 'BLOCK_KIND_REASONING'))
    await waitFor(() => device.bufferedEvents.some(payload => payload.update.live?.delta?.kind === 'BLOCK_KIND_TEXT'))
    const live = (await device.events()).flatMap(payload => payload.update.live?.delta ? [payload.update.live.delta] : [])
    assert.deepEqual(live.map(block => [block.kind, block.text]), [['BLOCK_KIND_REASONING', 'why'], ['BLOCK_KIND_TEXT', 'answer']])

    harness.adapter.emit('s', { type: 'assistant/message', data: {
      turn: 1, step: 1,
      message: { id: 'a', role: 'assistant', content: [{ type: 'tool-call', id: 'call-typed', name: 'bash', arguments: '{"private":true}' }], source: { kind: 'model' } },
    } })
    harness.adapter.emit('s', { type: 'tool/call', data: { turn: 1, step: 1, callId: 'call-typed', name: 'bash', arguments: '{"private":true}' } })
    harness.adapter.emit('s', { type: 'tool/result', data: { turn: 1, step: 1, message: { content: [{ type: 'text', text: 'ok' }], source: { kind: 'tool', callId: 'call-typed' }, isError: false } } })
    harness.adapter.emit('s', { type: 'llm/retry', data: { retryId: 'retry-typed', turn: 2, step: 1, retry: 1, maxRetries: 1 } })
    await waitFor(() => device.bufferedEvents.some(payload => (payload.update.history?.changes ?? []).some(change => change.message.blocks.some(block => block.tool_activity?.call_id === 'call-typed'))))
    await waitFor(() => device.bufferedEvents.some(payload => (payload.update.history?.changes ?? []).some(change => change.message.blocks.some(block => block.notice?.category === 'NOTICE_CATEGORY_RETRY'))))
    const staged = (await device.events()).flatMap(payload => payload.update.history?.changes ?? []).flatMap(change => change.message.blocks)
    const activities = staged.filter(block => block.tool_activity).map(block => block.tool_activity)
    assert.deepEqual(activities.map(activity => activity.call_id), ['call-typed', 'call-typed', 'call-typed'])
    assert.deepEqual(activities.map(activity => activity.stages[0]), ['TOOL_STAGE_PREPARATION', 'TOOL_STAGE_EXECUTION', 'TOOL_STAGE_SETTLEMENT'])
    assert.ok(staged.some(block => block.notice?.category === 'NOTICE_CATEGORY_RETRY' && block.notice.state === 'NOTICE_STATE_SCHEDULED'))
    assert.doesNotMatch(JSON.stringify(staged), /private/)
  } finally {
    await harness.stop()
  }
})

test('a confirmed send resolves through Host to a canonical exact history message', async () => {
  const harness = await startHarness({ sessions: [{ sessionId: 's1', running: false }], behavior: crashFree })
  try {
    const device = await harness.newPairedDevice('reply-device')
    const send = await device.command({ send_message: { session_id: 's1', text: 'host reply' } }, { commandId: 'reply-command' })
    assert.equal(send.status, 'succeeded')
    const submitted = send.response.command_accepted.message_id
    const reply = await device.query({ read_message_reply: { session_id: 's1', submitted_message_id: submitted } })
    assert.equal(reply.status, 'succeeded')
    const value = reply.response.message_reply
    assert.equal(value.state, 'MESSAGE_REPLY_COMMITTED')
    assert.equal(value.submitted_message_id, submitted)
    assert.equal(value.committed.session.host_id, 'host-1')
    assert.equal(value.committed.session.session_id, 's1')

    const exact = await device.query({ read_history: { session_id: 's1', exact: value.committed } })
    assert.equal(exact.status, 'succeeded')
    const message = exact.response.history_page.batch.changes[0].message
    assert.equal(message.message_id, value.committed.message_id)
    assert.equal(message.blocks[0].text.text, 'host reply')
  } finally {
    await harness.stop()
  }
})

test('a committed attachment occurrence opens, pulls and closes through the Host encrypted path', async () => {
  const harness = await startHarness({ sessions: [{ sessionId: 's1', running: false }], behavior: crashFree })
  try {
    const device = await harness.newPairedDevice('attachment-device')
    const native = harness.adapter.saveFixtureAttachment({ kind: 'file', data: new Uint8Array([7, 8, 9]), name: 'fixture.bin' })
    harness.adapter.emit('s1', {
      type: 'user/message',
      data: { id: 'u-attachment', role: 'user', content: [{ type: 'file', attachment: native }], source: { kind: 'user' } },
    })
    await waitFor(() => device.bufferedEvents.length >= 1)
    const page = await device.query({ read_history: { session_id: 's1', page: {} } })
    assert.equal(page.status, 'succeeded')
    const content = page.response.history_page.batch.changes
      .flatMap(change => change.message.blocks)
      .find(block => block.attachment).attachment
    assert.equal(content.available, true)
    const session = { host_id: 'host-1', session_id: 's1' }

    const opened = await device.query({ open_attachment: { session, attachment_ref: content.attachment_ref } })
    assert.equal(opened.status, 'succeeded')
    const metadata = opened.response.open_attachment_result.attachment
    assert.equal(metadata.size, 3)
    assert.equal(metadata.name, 'fixture.bin')
    assert.equal(metadata.attachment_ref, content.attachment_ref)

    const first = await device.query({
      pull_attachment: { session, attachment_ref: content.attachment_ref, transfer_id: metadata.transfer_id, offset: 0, max_bytes: 2 },
    })
    assert.equal(first.status, 'succeeded')
    assert.deepEqual([...first.response.attachment_part.data], [7, 8])
    assert.ok(!first.response.attachment_part.final)
    const second = await device.query({
      pull_attachment: { session, attachment_ref: content.attachment_ref, transfer_id: metadata.transfer_id, offset: 2, max_bytes: 2 },
    })
    assert.equal(second.status, 'succeeded')
    assert.deepEqual([...second.response.attachment_part.data], [9])
    assert.equal(second.response.attachment_part.final, true)
    assert.equal(second.response.attachment_part.total_size, 3)

    const closed = await device.query({
      close_attachment: { session, attachment_ref: content.attachment_ref, transfer_id: metadata.transfer_id },
    })
    assert.equal(closed.status, 'succeeded')
    assert.equal(closed.response.close_attachment_result.closed, true)
  } finally {
    await harness.stop()
  }
})

test('paged history crosses the Host latest-first with a usable continuation', async () => {
  const harness = await startHarness({ sessions: [{ sessionId: 's1', running: false }], behavior: crashFree })
  try {
    for (const text of ['m0', 'm1', 'm2', 'm3', 'm4']) harness.adapter.output('s1', text)
    const device = await harness.newPairedDevice('page-device')
    const first = await device.query({ read_history: { session_id: 's1', page: {}, max_messages: 2 } })
    assert.equal(first.status, 'succeeded')
    const page = first.response.history_page
    assert.deepEqual(page.batch.changes.map(change => change.message.blocks[0].text.text), ['m3', 'm4'])
    assert.ok(!page.complete)
    assert.ok(page.continuation)
    assert.equal(page.source, 'HISTORY_SOURCE_ONLINE')

    const second = await device.query({
      read_history: { session_id: 's1', page: { before_cursor: page.continuation }, cut_cursor: page.cut_cursor, max_messages: 2 },
    })
    assert.equal(second.status, 'succeeded')
    assert.deepEqual(second.response.history_page.batch.changes.map(change => change.message.blocks[0].text.text), ['m1', 'm2'])
    assert.equal(second.response.history_page.cut_cursor, page.cut_cursor)
    assert.equal(second.response.history_page.projection_id, page.projection_id)
  } finally {
    await harness.stop()
  }
})
