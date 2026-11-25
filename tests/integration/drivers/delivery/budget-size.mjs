// Calculate the exact encoded epoch-5 request budget for fixture identities.
// No message contents, ciphertext or credential material are emitted.
//
// Epoch 5 keeps a small complete request inline and uploads a larger one as a
// packet-sized body_reference. The admission bound is the complete sealed body
// (the EncryptedFrame), not the plaintext. The reference envelope itself is
// modeled too so a case can prove it is packet-sized and purpose-bound.
import { codec } from '../../../../host/runtime/src/protocol.js'
import { MESSAGE_BYTES, RELAY_VERSION, publishPacketBytes } from '../../../../host/runtime/src/access/transport.js'

const [hostId, deviceId, sessionId, itemId] = process.argv.slice(2)
const wire = codec()
const HTTPS_BODY_OVERHEAD_BYTES = 1024 * 1024

function encryptedFrame(size) {
  const request = itemId
    ? { update_pending_queue: { session_id: sessionId, item_id: itemId, action: 'EDIT', text: 'x'.repeat(size) } }
    : { send_message: { session_id: sessionId, text: 'x'.repeat(size), delivery_mode: 'ENQUEUE_AFTER_RUN' } }
  const payload = wire.encode('whalerein.v1.RequestPayload', request)
  return wire.encode('whalerein.v1.EncryptedFrame', {
    channel_id: 'a'.repeat(32),
    nonce: Buffer.alloc(12),
    ciphertext: Buffer.alloc(payload.length + 16),
  })
}

// The exact sealed body size and complete inline request the Core admits. The
// pre-submit forwardability check uses the complete encoded request, so the
// true inline admission boundary is that encoded size, not the raw body alone.
const bodySize = (size) => encryptedFrame(size).length
function requestSize(size) {
  return wire.encode('whalerein.v1.HostRequest', {
    request_id: 'a'.repeat(32), host_id: hostId, device_id: deviceId,
    kind: 'REQUEST_KIND_COMMAND', command_id: 'b'.repeat(32), payload_digest: Buffer.alloc(32),
    expires_at_unix_ms: Date.now() + 30000, trace_id: 'c'.repeat(32), protocol_version: 1,
    channel_id: 'a'.repeat(32), relay_version: RELAY_VERSION, body: encryptedFrame(size),
    transport: { result_max_bytes: Math.ceil(MESSAGE_BYTES * 4 / 3) + HTTPS_BODY_OVERHEAD_BYTES },
  }).length
}

function referenceSize(size) {
  const reference = {
    object_id: 'f'.repeat(32),
    owner_node_id: 'node-integration',
    ciphertext_bytes: String(bodySize(size)),
    sha256: Buffer.alloc(32),
    expires_at_unix_ms: String(Date.now() + 60000),
    purpose: 'PAYLOAD_PURPOSE_REQUEST',
  }
  const header = wire.encode('whalerein.v1.HostRequest', {
    request_id: 'a'.repeat(32), host_id: hostId, device_id: deviceId,
    kind: 'REQUEST_KIND_COMMAND', command_id: 'b'.repeat(32), payload_digest: Buffer.alloc(32),
    expires_at_unix_ms: Date.now() + 30000, trace_id: 'c'.repeat(32), protocol_version: 1,
    channel_id: 'a'.repeat(32), relay_version: RELAY_VERSION, body_reference: reference,
  })
  return publishPacketBytes(`whalerein/v1/h/${hostId}/request`, header.length)
}

// Largest plaintext whose complete inline request still fits the envelope bound.
let low = 0
let high = MESSAGE_BYTES
while (low < high) {
  const middle = Math.ceil((low + high) / 2)
  if (requestSize(middle) <= MESSAGE_BYTES) low = middle
  else high = middle - 1
}
const at = low
const sizes = [at - 1, at, at + 1].map(requestSize)
if (!(sizes[0] < sizes[1] && sizes[1] <= MESSAGE_BYTES && sizes[2] > MESSAGE_BYTES)) process.exit(1)
const referencePacketBytes = referenceSize(at)
const inlineAt = requestSize(at)
process.stdout.write(JSON.stringify({
  at,
  wire_limit: MESSAGE_BYTES,
  sizes,
  over: sizes[2],
  inline_request_bytes: inlineAt,
  reference_packet_bytes: referencePacketBytes,
  reference_fits_packet: referencePacketBytes <= 65536,
}) + '\n')
