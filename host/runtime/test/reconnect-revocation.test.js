import test from 'node:test'
import assert from 'node:assert/strict'
import { startHarness, waitFor } from './support/harness.js'

test('a broker-only reconnect publishes HostReady again and the final revocation replay clears revocations_pending', async () => {
  const harness = await startHarness({ sessions: [{ sessionId: 's1' }], config: { reconnectMinMs: 50, reconnectMaxMs: 100 } })
  try {
    await waitFor(() => harness.host.security.revocationsPending === false, 5000)
    const replays = harness.access.revocationResults.length
    assert.ok(replays >= 1, 'the startup HostReady is acknowledged')
    harness.broker.goOffline()
    await waitFor(() => harness.host.access.connected === false, 5000)
    assert.equal(harness.host.security.revocationsPending, false)
    harness.broker.goOnline()
    // The Host reconnects before Access resubscribes, so its first HostReady is
    // lost. The bounded re-announcement must recover the replay.
    await waitFor(() => harness.host.access.connected === true, 10000)
    await waitFor(() => harness.host.security.revocationsPending === true, 5000)
    await harness.access.reconnect()
    // The Host clears revocations_pending on the notice, before its ACK result
    // reaches the simulated Access. Wait for the replay record itself.
    await waitFor(() => harness.access.revocationResults.length > replays, 8000)
    await waitFor(() => harness.host.security.revocationsPending === false, 8000)
    assert.ok(harness.access.revocationResults.length > replays, 'the reconnect HostReady reached revocation replay')
    const terminal = harness.access.revocationResults.at(-1)
    assert.equal(terminal.status, 'succeeded')
    const device = await harness.newPairedDevice('device-after-reconnect')
    const listed = await device.query({ list_sessions: {} })
    assert.equal(listed.status, 'succeeded')
    assert.equal(listed.response.session_list.sessions.length, 1)
  } finally {
    await harness.stop()
  }
})
