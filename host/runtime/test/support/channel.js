// Test helper: run the client side of a channel handshake against
// SecurityManager and decrypt event envelopes with the derived h2c key.

import { randomBytes } from 'node:crypto'
import {
  NoiseXX,
  channelPrologue,
  deriveChannelKeys,
  eventAad,
  fingerprint,
  generateKeypair,
  open,
} from '../../src/security/noise.js'
import { codec as createCodec } from '../../src/protocol.js'

const sharedCodec = createCodec()

export function establishChannel({
  security,
  store,
  hostId,
  deviceId,
  identity = generateKeypair(),
  channelId = randomBytes(8).toString('hex'),
  codec = sharedCodec,
  persistPeer = true,
}) {
  if (persistPeer) {
    store.upsertPeerKey({
      deviceId,
      publicKey: identity.publicKey,
      fingerprint: fingerprint(identity.publicKey),
      claimId: `claim-${deviceId}`,
      pairedAt: 1,
    })
  }
  const prologue = channelPrologue({ hostId, deviceId, channelId })
  const noise = new NoiseXX({ initiator: true, staticKeypair: identity, prologue })
  const hello = codec.encode('whalerein.v1.ChannelHello', {
    device_id: deviceId,
    host_id: hostId,
    protocol_version: 1,
    capabilities: { capabilities: ['sessions', 'events'], protocol_version: 1 },
    client_nonce: 'client-nonce',
  })
  const first = security.handleChannelHandshake({
    request: { channel_id: channelId, device_id: deviceId, protocol_version: 1 },
    message: noise.writeMessage(hello),
  })
  const ready = codec.decode('whalerein.v1.ChannelReady', noise.readMessage(first.noiseMessage))
  const second = security.handleChannelHandshake({
    request: { channel_id: channelId, device_id: deviceId, protocol_version: 1 },
    message: noise.writeMessage(new Uint8Array(0)),
  })
  const derived = deriveChannelKeys(noise.handshakeHash, prologue)
  return {
    deviceId,
    channelId,
    identity,
    prologue,
    ready,
    first,
    second,
    c2h: derived.c2h,
    h2c: derived.h2c,
    noise,
    channel: security.getChannel(channelId),
  }
}

export function openEventEnvelope({ envelope, channel, hostId, deviceId, codec = sharedCodec }) {
  const aad = eventAad({
    hostId,
    deviceId,
    channelId: channel.channelId,
    createdAt: envelope.created_at_unix_ms,
  })
  const plaintext = open(channel.h2c, aad, envelope.nonce, envelope.ciphertext)
  if (!plaintext) return null
  return codec.decode('whalerein.v1.EventPayload', plaintext)
}
