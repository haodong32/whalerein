// Test helper: run the client side of a channel handshake against
// SecurityManager and decrypt event envelopes with the Noise Split h2c key.

import { randomBytes } from 'node:crypto'
import {
  NoiseXX,
  channelPrologue,
  eventAad,
  fingerprint,
  generateKeypair,
} from '../../src/security/noise.js'
import { codec as createCodec } from '../../src/protocol.js'
import { transportProfileBudget } from '../../src/contracts/transport.js'

export function defaultReceive(generation = 1) {
  return {
    profile: 'TRANSPORT_PROFILE_DEFAULT',
    budget: transportProfileBudget('default'),
    connection_generation: generation,
    relay_version: 5,
  }
}

export const DEFAULT_CONNECTION_REF = '0123456789abcdef:connection-1'

const sharedCodec = createCodec()

const DEFAULT_GRANT_RIGHTS = ['read', 'write', 'approve']

/**
 * Give the exact device key one active owner grant through the real writer.
 * Channel installation requires it now; a peer-only fixture is no longer
 * enough to establish session authority.
 */
export function ensureOwnerGrant({ security, deviceId, identity, useRights = DEFAULT_GRANT_RIGHTS }) {
  const trust = security?.trust
  if (!trust) throw new Error('security manager exposes no trust store')
  const active = trust.findActiveGrant({
    hostKeyFingerprint: security.hostFingerprint,
    devicePublicKey: identity.publicKey,
  })
  if (active) return active
  const candidateId = `claim-${deviceId}:${fingerprint(identity.publicKey)}`
  const now = security.now?.() ?? Date.now()
  try {
    trust.stageCandidate({
      candidateId,
      method: 'local',
      hostKeyFingerprint: security.hostFingerprint,
      candidatePublicKey: identity.publicKey,
      deviceId,
      deviceLabel: 'test device',
      useRights,
      grantableRights: [],
      addDevice: false,
      expiresAt: now + 3_600_000,
      now,
    })
  } catch (error) {
    if (error?.code !== 'conflict') throw error
  }
  const outcome = trust.commitEnrollment({
    candidateId,
    candidatePublicKey: identity.publicKey,
    useRights,
    grantableRights: [],
    addDevice: false,
    approver: { kind: 'owner' },
    now,
  })
  return outcome.grant
}

export function establishChannel({
  security,
  store,
  hostId,
  deviceId,
  identity = generateKeypair(),
  channelId = randomBytes(8).toString('hex'),
  codec = sharedCodec,
  persistPeer = true,
  receive = defaultReceive(1),
  omitReceive = false,
  // Legacy compatibility: omit the grant field entirely, as an old Client
  // would. The Host must still install only under its msg1 snapshot rules.
  omitGrantId = false,
  // Explicit override used by mismatch/stale-grant tests.
  grantId = null,
  connectionRef = DEFAULT_CONNECTION_REF,
  grantGeneration = 1,
  enrollmentGeneration = 1,
}) {
  let activeGrant = null
  if (persistPeer) {
    const grant = ensureOwnerGrant({ security, deviceId, identity })
    activeGrant = grant
    store.upsertPeerKey({
      deviceId,
      publicKey: identity.publicKey,
      fingerprint: fingerprint(identity.publicKey),
      claimId: `claim-${deviceId}`,
      pairedAt: 1,
      grantId: grant?.grantId ?? null,
      grantRevision: grant?.revision ?? null,
    })
  } else {
    activeGrant = security.trust?.findActiveGrant({
      hostKeyFingerprint: security.hostFingerprint,
      devicePublicKey: identity.publicKey,
    }) ?? null
  }
  const helloGrantId = omitGrantId ? '' : (grantId ?? activeGrant?.grantId ?? '')
  const prologue = channelPrologue({ hostId, deviceId, channelId })
  // A fixed (but random) ephemeral marks this as a test handshake, enabling
  // the transcript readers below; production never injects one.
  const noise = new NoiseXX({
    initiator: true,
    staticKeypair: identity,
    prologue,
    ephemeralKeypair: generateKeypair(),
  })
  const hello = codec.encode('whalerein.v1.ChannelHello', {
    device_id: deviceId,
    host_id: hostId,
    protocol_version: 1,
    capabilities: { capabilities: ['sessions', 'events'], protocol_version: 1 },
    client_nonce: 'client-nonce',
    ...(omitReceive ? {} : { receive }),
    ...(helloGrantId ? { grant_id: helloGrantId } : {}),
  })
  const reply = security.handleChannelHandshake({
    channel_id: channelId, device_id: deviceId, protocol_version: 1,
    connection_ref: connectionRef,
    grant_generation: grantGeneration,
    enrollment_generation: enrollmentGeneration,
    body: noise.writeMessage(hello),
  })
  const ready = codec.decode('whalerein.v1.ChannelReady', noise.readMessage(reply))
  security.handleChannelHandshake({
    channel_id: channelId, device_id: deviceId, protocol_version: 1,
    connection_ref: connectionRef,
    grant_generation: grantGeneration,
    enrollment_generation: enrollmentGeneration,
    body: noise.writeMessage(new Uint8Array(0)),
  })
  return {
    deviceId,
    channelId,
    identity,
    prologue,
    ready,
    // Device-side ciphers from the secret Split; request seals advance the
    // shared c2h counter, so tests must reuse this object.
    cipher: noise.takeChannelCipher(),
    noise,
    connectionRef,
    grantGeneration,
    enrollmentGeneration,
    channel: security.getChannel(channelId),
  }
}

/**
 * Decrypt one host-to-device frame for assertions.
 *
 * Test transcripts are scanned repeatedly by `payloads()`/`updates()`, so this
 * uses a fresh replay window over the same keys. Production framing and replay
 * behavior is covered by security-manager and noise unit tests.
 */
export function openFrame({ cipher, direction, aad, nonce, ciphertext }) {
  return cipher.readerForTest().open(direction, aad, nonce, ciphertext)
}

export function openEventEnvelope({ envelope, cipher, hostId, deviceId, codec = sharedCodec }) {
  const aad = eventAad({
    hostId,
    deviceId,
    channelId: envelope.channel_id,
    createdAt: envelope.created_at_unix_ms,
  })
  try {
    const plaintext = openFrame({
      cipher, direction: 'h2c', aad, nonce: envelope.nonce, ciphertext: envelope.ciphertext,
    })
    return codec.decode('whalerein.v1.EventPayload', plaintext)
  } catch {
    return null
  }
}
