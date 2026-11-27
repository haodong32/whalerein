// Rust Core <-> Node DSH Noise XX interop check.
//
// The Node agent is the host (Noise responder). This script drives the
// `noise-peer responder` binary as the Rust side would drive a host, then
// exchanges ChaCha20-Poly1305 frames in both directions.
//
// Usage:
//   cargo build -p noise-peer
//   node tools/noise-peer/interop.mjs target/debug/noise-peer

import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'

import {
  NoiseXX,
  generateKeypair,
  fingerprint,
  requestAad,
  hex,
} from '../../host/runtime/src/security/noise.js'

const binary = process.argv[2] ?? 'target/debug/noise-peer'
const host = generateKeypair()
const device = generateKeypair()
const channelId = 'e372075fe44f25a0f0e06bb6604889e9'
const prologue = `whalerein/v1/channel|host-1|device-1|${channelId}`

const child = spawn(binary, [
  'responder',
  '--prologue', prologue,
  '--private-key', hex(host.privateKey),
  '--channel-id', channelId,
], { stdio: ['pipe', 'pipe', 'inherit'] })

const lines = createInterface({ input: child.stdout })
const queue = []
const waiters = []
lines.on('line', (line) => {
  if (waiters.length > 0) waiters.shift()(line)
  else queue.push(line)
})

function nextLine() {
  if (queue.length > 0) return Promise.resolve(queue.shift())
  return new Promise((resolve) => waiters.push(resolve))
}

function send(line) {
  child.stdin.write(`${line}\n`)
}

function check(condition, message) {
  if (!condition) throw new Error(message)
}

const timeout = setTimeout(() => {
  child.kill()
  console.error('interop timed out')
  process.exit(1)
}, 10_000)

try {
  const initiator = new NoiseXX({ initiator: true, staticKeypair: device, prologue })

  const msg1 = initiator.writeMessage(Buffer.from('whalerein-noise-peer'))
  send(`msg ${hex(msg1)}`)
  const msg2Line = await nextLine()
  check(msg2Line.startsWith('msg '), `expected msg2, got ${msg2Line}`)
  const msg2 = Buffer.from(msg2Line.slice(4), 'hex')
  initiator.readMessage(msg2)

  const msg3 = initiator.writeMessage(Buffer.alloc(0))
  send(`msg ${hex(msg3)}`)

  const ready = (await nextLine()).split(' ')
  check(ready[0] === 'ready', `expected ready, got ${ready.join(' ')}`)
  // The responder sees the initiator's static key as its remote static.
  check(ready[1] === fingerprint(device.publicKey), 'rust remote fingerprint mismatch')
  check(ready[3] === channelId, 'channel id mismatch')

  // The application keys come from the secret Noise Split outputs, so the
  // Node and snow implementations must agree on the non-reversible fingerprint.
  const cipher = initiator.takeChannelCipher()
  check(ready[4] === cipher.keyFingerprint(), 'split key fingerprint mismatch')

  const aad = requestAad({
    requestId: 'req-1',
    hostId: 'host-1',
    deviceId: 'device-1',
    kind: 'REQUEST_KIND_QUERY',
    commandId: '',
    expiresAt: 1700000000000,
  })
  const request = Buffer.from('fixed request payload')
  const sealed = cipher.seal('c2h', aad, request)
  send(`open ${hex(aad)} ${hex(sealed.nonce)} ${hex(sealed.ciphertext)}`)
  const opened = await nextLine()
  check(opened === `opened ${hex(request)}`, `rust open mismatch: ${opened}`)

  const reply = Buffer.from('fixed reply payload')
  send(`seal ${hex(aad)} ${hex(reply)}`)
  const sealedLine = await nextLine()
  check(sealedLine.startsWith('sealed '), `expected sealed, got ${sealedLine}`)
  const [, nonceHex, ciphertextHex] = sealedLine.split(' ')
  const decrypted = cipher.open('h2c', aad, Buffer.from(nonceHex, 'hex'), Buffer.from(ciphertextHex, 'hex'))
  check(hex(decrypted) === hex(reply), 'node open mismatch')

  send('quit')
  console.log('rust-node noise xx interop: ok')
  clearTimeout(timeout)
  child.stdin.end()
} catch (error) {
  clearTimeout(timeout)
  child.kill()
  console.error(error.message)
  process.exit(1)
}
