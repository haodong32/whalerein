// Settings projection. Snapshot is read-only public state. invite asks Access
// for a one-time pairing URI and returns that URI plus the host fingerprint.
// revoke asks Access to revoke one device. Private keys, peer keys, the admin
// token, and the raw invitation token stay off this API and out of logs.

import { Remote, TypertRemoteService } from '@deepseek-ai/dsh-typert-protocol'
import { createInvitation, revokeDevice } from './pairing.js'
import { testConnection } from './connection-test.js'

export const EMPTY_SETTINGS_STATUS = Object.freeze({
  available: false,
  connected: false,
  hostId: '',
  pairingCode: '',
  devices: Object.freeze([]),
  diagnostics: null,
})

// Keep this projection explicit: adapter summaries may gain private fields.
function settingsDiagnostics(host) {
  let runtime
  try {
    runtime = host.diagnostics?.()
  } catch {
    return null
  }
  if (!runtime) return null
  const count = (value) => Number.isSafeInteger(value) && value >= 0 ? value : null
  return {
    identityReady: !!host.security?.hostFingerprintDisplay,
    dshAvailable: typeof runtime.source?.available === 'boolean' ? runtime.source.available : null,
    revocationsPending: typeof runtime.revocationsPending === 'boolean' ? runtime.revocationsPending : null,
    reconnects: count(runtime.access?.reconnects),
    publishErrors: count(runtime.access?.publishErrors),
    pendingCiphertext: count(runtime.pendingCiphertext?.count),
    unknownCommands: count(runtime.commands?.unknown),
    snapshotRequired: count(runtime.cursors?.snapshotRequired),
  }
}

export function settingsStatus(host) {
  if (!host?.started) return { ...EMPTY_SETTINGS_STATUS, devices: [] }
  const security = host.security
  const channels = new Set((security?.listChannels() ?? []).map((channel) => channel.deviceId))
  let devices = []
  try {
    devices = host.store?.listPeerKeys().map((device) => ({
      deviceId: device.deviceId,
      displayName: device.displayName ?? '',
      os: device.os ?? '',
      fingerprint: device.fingerprint,
      pairedAt: device.pairedAt,
      connected: channels.has(device.deviceId),
    })) ?? []
  } catch {
    devices = []
  }
  return {
    available: true,
    connected: host.access?.connected === true,
    hostId: host.config.hostId,
    pairingCode: security?.hostFingerprintDisplay ?? '',
    devices,
    diagnostics: settingsDiagnostics(host),
  }
}

export class WhaleReinSettingsService extends TypertRemoteService {
  constructor(ctx, host) {
    super(ctx, 'whalereinSettings', { namespace: 'whalerein' })
    this.host = host
  }

  snapshot() {
    return settingsStatus(this.host)
  }

  async testConnection(draft) {
    if (this.connectionTest) return { configuration: { ok: false, code: 'busy' } }
    const now = this.host?.now?.() ?? Date.now()
    if (this.lastConnectionTestAt !== undefined && now - this.lastConnectionTestAt < 10_000) {
      return { configuration: { ok: false, code: 'rate_limited' } }
    }
    this.lastConnectionTestAt = now
    this.connectionTest = testConnection(this.host, draft)
    try {
      return await this.connectionTest
    } finally {
      this.connectionTest = null
    }
  }

  async invite() {
    const config = this.host?.config ?? {}
    const invitation = await createInvitation({
      accessUrl: config.accessUrl,
      hostId: config.hostId,
      privateKey: this.host?.security?.identity?.privateKey,
      now: this.host?.now,
    })
    return {
      pairingUri: invitation.pairingUri,
      expiresAtUnixMs: invitation.expiresAtUnixMs,
      fingerprint: this.host?.security?.hostFingerprintDisplay ?? '',
    }
  }

  async revoke(deviceId) {
    const generation = this.host?.security?.grantGeneration?.(deviceId) ?? 0
    const config = this.host?.config ?? {}
    const result = await revokeDevice({
      accessUrl: config.accessUrl,
      hostId: config.hostId,
      deviceId,
      privateKey: this.host?.security?.identity?.privateKey,
      now: this.host?.now,
    })
    // Access already committed this host's grant. Drop the local peer now;
    // the host does not wait for a revocation notice it would send to itself.
    this.host?.security?.forgetPairedDevice?.(result.deviceId, generation)
    return result
  }
}

// The package is plain JavaScript; invoke the standard decorator explicitly.
const remoteInitializers = []
function markRemote(method, name) {
  Remote(method, {
    name,
    addInitializer(initializer) {
      remoteInitializers.push(initializer)
    },
  })
}
markRemote(WhaleReinSettingsService.prototype.snapshot, 'snapshot')
markRemote(WhaleReinSettingsService.prototype.invite, 'invite')
markRemote(WhaleReinSettingsService.prototype.revoke, 'revoke')
markRemote(WhaleReinSettingsService.prototype.testConnection, 'testConnection')

function initializeRemoteSnapshot() {
  for (const initializer of remoteInitializers) initializer.call(this)
}

Object.defineProperty(WhaleReinSettingsService, 'initializeRemoteSnapshot', {
  value: initializeRemoteSnapshot,
})

export function createSettingsService(ctx, host) {
  const service = new WhaleReinSettingsService(ctx, host)
  WhaleReinSettingsService.initializeRemoteSnapshot.call(service)
  return service
}
