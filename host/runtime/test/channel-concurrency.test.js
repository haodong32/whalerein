import assert from 'node:assert/strict'
import test from 'node:test'
import { startHarness, waitFor } from './support/harness.js'
import { DeviceClient } from './support/wire-client.js'

test('a same-Device send and reply polling preserve the attached live Watch', async (t) => {
  const harness = await startHarness({ sessions: [{ sessionId: 's1' }] })
  t.after(() => harness.stop())
  const watcher = await harness.newPairedDevice()
  const watched = await watcher.query({
    watch_session: { session_id: 's1', interests: { conversation: true, questions: false } },
  })
  const watchId = watched.response.watch_ready.watch_id
  const sender = new DeviceClient({
    access: harness.access,
    broker: harness.broker,
    codec: harness.codec,
    hostId: watcher.hostId,
    deviceId: watcher.deviceId,
    identity: watcher.identity,
  })
  t.after(() => sender.close())
  await sender.ready
  await sender.connectChannel()

  const send = await sender.command({ send_message: { session_id: 's1', text: 'fixture' } })
  assert.equal(send.status, 'succeeded')
  const messageId = send.response.command_accepted.message_id
  const readReply = { read_message_reply: { session_id: 's1', submitted_message_id: messageId } }
  harness.adapter.readMessageReply = async () => ({ state: 'MESSAGE_REPLY_PENDING' })
  for (const seq of [1, 2]) {
    const read = await sender.query(readReply)
    assert.equal(read.response.message_reply.state, 'MESSAGE_REPLY_PENDING')
    const refresh = await watcher.query({ check_versions: { session_id: 's1' } })
    assert.equal(refresh.status, 'succeeded')
    assert.equal(refresh.response.version_checks.checks[0].watch_id, watchId)
    harness.adapter.publish('s1', {
      live: {
        attempt: { live_epoch: 'fixture-epoch', attempt_id: 'fixture-attempt' },
        block_id: 'fixture-block',
        first_seq: seq,
        last_seq: seq,
        delta: { kind: 'BLOCK_KIND_TEXT', text: 'x' },
      },
    })
    const update = await waitFor(() => watcher.bufferedEvents
      .find(event => event.update?.live?.last_seq === seq)?.update)
    assert.equal(update.watch_id, watchId)
  }
})
