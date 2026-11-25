// Calculate the exact encoded epoch-5 request budget for fixture identities.
// No message contents, ciphertext or credential material are emitted.
//
// Epoch 5 keeps a small complete request inline and uploads a larger one as a
// packet-sized body_reference. The admission bound is the complete sealed body
// (the EncryptedFrame), not the plaintext. The reference envelope itself is
// modeled too so a case can prove it is packet-sized and purpose-bound.
import { codec } from '../../../../host/runtime/src/protocol.js'
import { DEFAULT_TRANSPORT_PROFILE, RELAY_VERSION, transportProfileBudget } from '../../../../host/runtime/src/contracts/transport.js'
import { maxPlaintextBytes, publishPacketBytes } from '../../../../host/runtime/src/access/packet.js'

const [hostId, deviceId, sessionId, itemId] = process.argv.slice(2)
const wire = codec()
const budget = transportProfileBudget(DEFAULT_TRANSPORT_PROFILE)
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
    transport: { result_max_bytes: Math.ceil(budget.envelope_bytes * 4 / 3) + HTTPS_BODY_OVERHEAD_BYTES },
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
const at = maxPlaintextBytes(requestSize, budget.envelope_bytes)
const sizes = [at - 1, at, at + 1].map(requestSize)
if (!(sizes[0] < sizes[1] && sizes[1] <= budget.envelope_bytes && sizes[2] > budget.envelope_bytes)) process.exit(1)
const referencePacketBytes = referenceSize(at)
process.stdout.write(JSON.stringify({
  at,
  wire_limit: budget.envelope_bytes,
  sizes,
  over: sizes[2],
  inline_request_bytes: sizes[1],
  reference_packet_bytes: referencePacketBytes,
  reference_fits_packet: referencePacketBytes <= budget.packet_bytes,
}) + '\n')
