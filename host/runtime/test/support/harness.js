import { MemoryHostCredentials } from '../../src/security/credentials.js'
// Local Host harness: the real source-independent runtime with a test Source,
// an in-process MQTT broker, a simulated Access service, and device clients.
// `startHarness()` needs no HTTP port, no real broker, and no DSH process.
//
// Connectors may supply their own SourceAdapter and resolved configuration.

import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { testConfig } from './config.js'
import { HostRuntime } from '../../src/host.js'
import { MqttAccess } from '../../src/access/mqtt.js'
import { TestSource } from './source.js'
import { ProtoCodec } from '../../src/protocol.js'
import { silentLogger } from '../../src/log.js'
import { hex, sha256Text } from '../../src/security/digest.js'
import { FakeBroker } from './fake-mqtt.js'
import { DeviceClient, FakeAccessSimulator, MemoryRelayObjects } from './wire-client.js'

export function pairingTokenHash(token) {
  return hex(sha256Text(token))
}

export async function waitFor(predicate, timeoutMs = 5000, intervalMs = 5) {
  const start = Date.now()
  for (;;) {
    const value = await predicate()
    if (value) return value
    if (Date.now() - start > timeoutMs) throw new Error('condition timeout')
    await new Promise((resolve) => setTimeout(resolve, intervalMs))
  }
}

/**
 * @param {object} [options]
 * @param {string} [options.hostId]
 * @param {string} [options.hostToken]
 * @param {Array<object>} [options.sessions] initial fixture sessions.
 * @param {object} [options.adapter] connector-owned SourceAdapter.
 * @param {string} [options.stateDir] keep an explicit state directory.
 * @param {object} [options.config] config overrides.
 * @param {() => number} [options.now]
 * @param {string} [options.protoPath]
 */
export async function startHarness(options = {}) {
  const hostId = options.hostId ?? 'host-1'
  const hostToken = options.hostToken ?? 'test-host-token'
  const now = options.now ?? Date.now
  const ownStateDir = !options.stateDir
  const stateDir = options.stateDir ?? mkdtempSync(join(tmpdir(), 'whalerein-host-'))
  const codec = new ProtoCodec(options.protoPath)
  const broker = new FakeBroker({ maxPacketBytes: options.maxPacketBytes ?? Infinity })
  const relay = new MemoryRelayObjects()
  const access = new FakeAccessSimulator({ broker, codec, hostId, now, relay })
  await access.ready
  const adapter = options.adapter ?? new TestSource({ now, sessions: options.sessions ?? [] })
  const config = { ...testConfig({ host_id: hostId, host_token: hostToken, state_dir: stateDir }), ...options.config, stateDir }
  const mqtt = new MqttAccess({
    url: config.mqttUrl,
    hostId,
    token: config.hostToken,
    connect: broker.connectFn(),
    logger: silentLogger,
    now,
    codec,
    relayClient: relay,
  })
  const host = new HostRuntime({ credentials: new MemoryHostCredentials(), config, adapter, access: mqtt, logger: silentLogger, now, codec })
  await host.start()
  await mqtt.waitForConnected(5000)
  await access.waitForHostReady(5000)
  await waitFor(() => host.security.revocationsPending === false, 5000)

  const devices = new Set()
  const tokenHash = pairingTokenHash(hostToken)

  async function createDevice(deviceId = 'device-1') {
    const device = new DeviceClient({ access, broker, codec, hostId, deviceId, now })
    await device.ready
    devices.add(device)
    return device
  }

  async function pairDevice(device, { claimId, token = hostToken, authorize = true } = {}) {
    const pairing = await device.beginPairing({ claimId, tokenHash: pairingTokenHash(token) })
    await device.finishPairing({ authorize })
    await device.connectChannel()
    return pairing
  }

  async function newPairedDevice(deviceId = 'device-1', pairingOptions = {}) {
    const device = await createDevice(deviceId)
    await pairDevice(device, pairingOptions)
    for (const session of await adapter.listSessions()) {
      const watched = await device.query({watch_session:{session_id:session.sessionId,interests:{conversation:true,questions:false}}})
      if(watched.status!=='succeeded')throw new Error('fixture watch unavailable')
    }
    return device
  }

  async function stop() {
    for (const device of devices) {
      try {
        await device.close()
      } catch {
        // contained
      }
    }
    devices.clear()
    await host.stop()
    broker.goOffline()
    if (ownStateDir) rmSync(stateDir, { recursive: true, force: true })
  }

  return {
    host,
    adapter,
    broker,
    access,
    codec,
    config,
    stateDir,
    tokenHash,
    createDevice,
    pairDevice,
    newPairedDevice,
    stop,
    diagnostics: () => host.diagnostics(),
  }
}
