// Calculate the actual encoded MQTT HostRequest budget for fixture identities.
// No message contents, ciphertext or credential material are emitted.
import { codec } from '../../../../host/runtime/src/protocol.js'
import { MESSAGE_BYTES } from '../../../../host/runtime/src/access/transport.js'
const [hostId, deviceId, sessionId, itemId] = process.argv.slice(2)
const wire = codec()
function encodedSize(size) {
  const request = itemId ? {update_pending_queue:{session_id:sessionId,item_id:itemId,action:'EDIT',text:'x'.repeat(size)}} : {send_message:{session_id:sessionId,text:'x'.repeat(size),delivery_mode:'ENQUEUE_AFTER_RUN'}}
  const payload = wire.encode('whalerein.v1.RequestPayload', request)
  const frame = wire.encode('whalerein.v1.EncryptedFrame', {
    channel_id: 'a'.repeat(32), nonce: Buffer.alloc(12), ciphertext: Buffer.alloc(payload.length + 16),
  })
  return wire.encode('whalerein.v1.HostRequest', {
    request_id: 'a'.repeat(32), host_id: hostId, device_id: deviceId,
    kind: 'REQUEST_KIND_COMMAND', command_id: 'b'.repeat(32), payload_digest: Buffer.alloc(32),
    expires_at_unix_ms: Date.now() + 30000, trace_id: 'c'.repeat(32), protocol_version: 1,
    channel_id: 'a'.repeat(32), body: frame, client_transport_version: 2, relay_transport_version: 2,
  }).length
}
const probe = MESSAGE_BYTES - 4096
const at = MESSAGE_BYTES - (encodedSize(probe) - probe)
const sizes = [at - 1, at, at + 1].map(encodedSize)
if (sizes.some((value, index) => value !== MESSAGE_BYTES + index - 1)) process.exit(1)
process.stdout.write(JSON.stringify({at, wire_limit: MESSAGE_BYTES, sizes}) + '\n')
