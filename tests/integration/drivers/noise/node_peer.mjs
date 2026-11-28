#!/usr/bin/env node
// WhaleRein Noise XX Node.js peer (test vector generator/verifier).
//
// Uses the same implementation as the DSH plugin:
// host/runtime/src/security/noise.js. The fixed keys, ephemerals, prologue,
// payloads and nonce are test-only and must never be used in production.
//
// Usage:
//   node node_peer.mjs --write  <fixture.json>
//   node node_peer.mjs --check  <fixture.json>
//   node node_peer.mjs                 # print the transcript as JSON

import { readFileSync, writeFileSync } from 'node:fs'
import { resolve, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const root = resolve(here, '../../../..')
const pluginRoot = resolve(root, 'host/runtime')
const noise = await import(
  pathToFileURL(resolve(pluginRoot, 'src/security/noise.js')).href
)
const { sha256 } = await import(
  pathToFileURL(resolve(pluginRoot, 'node_modules/@noble/hashes/sha2.js')).href
)

const STATIC_DEVICE = '01'.repeat(32)
const STATIC_HOST = '02'.repeat(32)
const EPHEMERAL_DEVICE = '03'.repeat(32)
const EPHEMERAL_HOST = '04'.repeat(32)
const PROLOGUE = 'whalerein/v2/channel|host-fixture-1|device-fixture-1|channel-fixture-1'
const PAYLOAD_1 = Buffer.from('pairing-hello', 'utf8')
const PAYLOAD_2 = Buffer.from('pairing-ready', 'utf8')
const PAYLOAD_3 = Buffer.from('pairing-done', 'utf8')
const PLAINTEXT = Buffer.from('whalerein test payload', 'utf8')

function aad() {
  return noise.requestAad({
    requestId: 'req-fixture-1',
    hostId: 'host-fixture-1',
    deviceId: 'device-fixture-1',
    kind: 'REQUEST_KIND_COMMAND',
    commandId: 'cmd-fixture-1',
    expiresAt: '1700000000000',
  })
}

function transcript() {
  const device = noise.parseKeypair(noise.unhex(STATIC_DEVICE))
  const host = noise.parseKeypair(noise.unhex(STATIC_HOST))
  const deviceEph = noise.parseKeypair(noise.unhex(EPHEMERAL_DEVICE))
  const hostEph = noise.parseKeypair(noise.unhex(EPHEMERAL_HOST))

  const initiator = new noise.NoiseXX({
    initiator: true,
    staticKeypair: device,
    prologue: PROLOGUE,
    ephemeralKeypair: deviceEph,
  })
  const responder = new noise.NoiseXX({
    initiator: false,
    staticKeypair: host,
    prologue: PROLOGUE,
    ephemeralKeypair: hostEph,
  })

  const msg1 = initiator.writeMessage(PAYLOAD_1)
  const read1 = responder.readMessage(msg1)
  const msg2 = responder.writeMessage(PAYLOAD_2)
  const read2 = initiator.readMessage(msg2)
  const msg3 = initiator.writeMessage(PAYLOAD_3)
  const read3 = responder.readMessage(msg3)

  if (!noise.equal(initiator.handshakeHash, responder.handshakeHash)) {
    throw new Error('handshake hashes differ between Node initiator and responder')
  }
  if (!noise.equal(initiator.remoteStaticKey, host.publicKey)) {
    throw new Error('initiator learned the wrong remote static key')
  }
  if (!noise.equal(responder.remoteStaticKey, device.publicKey)) {
    throw new Error('responder learned the wrong remote static key')
  }
  // Application keys are the secret Split outputs; `keyExport` is available
  // only because this handshake injects fixed test ephemerals.
  const cipher = initiator.takeChannelCipher()
  const keys = cipher.keysHexForTest()
  // This fixture channel id is a fixed label; a real channel id is chosen at
  // random by the client and bound through the prologue.
  const channelId = noise.hex(
    sha256(
      Buffer.concat([
        Buffer.from(initiator.handshakeHash),
        Buffer.from('whalerein/v2/channel-id-fixture', 'utf8'),
      ]),
    ).slice(0, 16),
  )
  const aadBytes = aad()
  const sealed = cipher.seal('c2h', aadBytes, PLAINTEXT)

  return {
    comment: 'Test-only Noise XX transcript. Never use these keys.',
    protocol: noise.PROTOCOL_NAME,
    prologue: PROLOGUE,
    static_device_private: STATIC_DEVICE,
    static_host_private: STATIC_HOST,
    ephemeral_device_private: EPHEMERAL_DEVICE,
    ephemeral_host_private: EPHEMERAL_HOST,
    device_public: noise.hex(device.publicKey),
    host_public: noise.hex(host.publicKey),
    device_fingerprint: noise.fingerprint(device.publicKey),
    msg1: noise.hex(msg1),
    msg2: noise.hex(msg2),
    msg3: noise.hex(msg3),
    payload1: PAYLOAD_1.toString('utf8'),
    payload2: PAYLOAD_2.toString('utf8'),
    payload3: PAYLOAD_3.toString('utf8'),
    read1: Buffer.from(read1).toString('utf8'),
    read2: Buffer.from(read2).toString('utf8'),
    read3: Buffer.from(read3).toString('utf8'),
    handshake_hash: noise.hex(initiator.handshakeHash),
    channel_id: channelId,
    split_fingerprint: cipher.keyFingerprint(),
    c2h: keys.c2h,
    h2c: keys.h2c,
    aad: noise.hex(aadBytes),
    nonce: noise.hex(sealed.nonce),
    plaintext: PLAINTEXT.toString('utf8'),
    ciphertext: noise.hex(sealed.ciphertext),
  }
}

const argv = process.argv.slice(2)
const value = transcript()
if (argv[0] === '--write') {
  writeFileSync(argv[1], JSON.stringify(value, null, 2) + '\n')
  console.log(`wrote ${argv[1]}`)
} else if (argv[0] === '--check') {
  const fixture = JSON.parse(readFileSync(argv[1], 'utf8'))
  const mismatches = []
  for (const key of Object.keys(value)) {
    if (JSON.stringify(value[key]) !== JSON.stringify(fixture[key])) {
      mismatches.push(key)
    }
  }
  if (mismatches.length) {
    console.error(`node peer does not match fixture: ${mismatches.join(', ')}`)
    process.exit(1)
  }
  console.log(`Noise XX transcript matches the fixed fixture (${Object.keys(value).length} fields)`)
} else {
  console.log(JSON.stringify(value, null, 2))
}
