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

    await waitFor(() => device.bufferedEvents.some(payload => payload.update.state?.approvals?.some(a=>a.state==='APPROVAL_STATE_PENDING')))
    const events = await device.events()
    const approvalEvent = events.find((payload) => payload.update.state?.approvals?.some(a=>a.state==='APPROVAL_STATE_PENDING'))
    assert.ok(approvalEvent)
    const approval = approvalEvent.update.state.approvals.find(a=>a.state==='APPROVAL_STATE_PENDING')
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
    const later = await device.events()
    assert.ok(later.some((payload) => payload.update.state && payload.update.state.approvals.length===0))

    const second = await device.command({
      resolve_approval: {
        approval_id: approval.approval_id,
        decision: 'APPROVAL_DECISION_REJECT',
        expected_digest: approval.digest,
        expected_version: approval.version,
      },
    }, { commandId: 'cmd-resolve-again' })
    assert.equal(second.status, 'rejected')
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
    for(const message of messages)assert.ok(message.blocks.some(block=>block.text?.text===payload),'History must preserve the complete fixture text')
    const repeated=await device.send(sent.request)
    assert.equal(repeated.status,'succeeded')
    assert.deepEqual(repeated.response,sent.response)
    const source=await harness.adapter.getSessionSnapshot('s')
    assert.equal(source.events.filter(event=>event.type==='user/message').length,1)
    assert.ok(harness.broker.messages.every(message=>message.payload.length<=64*1024))
    assert.equal(harness.host.access.describe().reconnects,0)
  } finally {await harness.stop()}
})
