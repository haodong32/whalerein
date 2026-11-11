import test from 'node:test'
import assert from 'node:assert/strict'
import { CommandService } from 'whalerein-host/commands/service.js'
import { StateStore } from 'whalerein-host/state/store.js'
import { codec } from 'whalerein-host/protocol.js'
import { sha256Bytes } from 'whalerein-host/security/digest.js'
import { FakeDshAdapter } from '../src/adapter/fake.js'

test('when FakeDSH crashes after committing a user event, the Host preserves uncertainty without executing a duplicate', async (t) => {
  const store = new StateStore({ path: ':memory:' })
  t.after(() => store.close())
  const adapter = new FakeDshAdapter({ sessions: [{ sessionId: 's1' }] })
  const service = new CommandService({ store, adapter, codec: codec() })
  let committed = 0
  adapter.subscribeEvents(event => {
    if (event.type === 'user/message') { committed++; adapter.crash() }
  })
  const body = new Uint8Array([1, 2, 3])
  const request = { request_id: 'native-crash-request', device_id: 'device-1', command_id: 'native-crash-command', kind: 'REQUEST_KIND_COMMAND', body, payload_digest: sha256Bytes(body) }
  const payload = { send_message: { session_id: 's1', text: 'fixture action' } }
  assert.equal((await service.handle({ request, payload })).status, 'unknown')
  assert.equal(committed, 1)
  assert.equal(store.getCommand('device-1', request.command_id).status, 'unknown')
  adapter.recover()
  assert.equal((await service.handle({ request, payload })).status, 'unknown')
  assert.equal((await adapter.getSessionSnapshot('s1')).events.filter(event => event.type === 'user/message').length, 1)
})
