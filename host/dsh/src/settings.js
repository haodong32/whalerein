// Settings projection. Snapshot is read-only public state. invite asks Access
// for a one-time pairing URI and returns that URI plus the host fingerprint.
// revoke asks Access to revoke one device. The managed cloud connection is
// projected through a separate allowlisted snapshot; private keys, peer keys,
// the admin token, poll credentials, authorization documents, host ids, routes
// and raw approval locators stay off this API and out of logs.

import { Remote, RemoteError, TypertRemoteService } from '@deepseek-ai/dsh-typert-protocol'
import { createInvitation, revokeDevice } from './pairing.js'
import { testConnection } from './connection-test.js'

export const CONNECTION_STATES = Object.freeze([
  'idle', 'requesting', 'awaiting_approval', 'enrolling', 'recovering',
  'connecting', 'connected', 'reconnecting', 'disconnected', 'denied',
  'expired', 'action_required', 'removing', 'removed',
])
const ENVIRONMENTS = new Set(['production', 'staging', 'local', 'custom', 'standalone'])
const CAPABILITIES = new Set(['pairing', 'removal', 'switching', 'custom-service', 'staging'])
const ERROR_CODE = /^[a-z][a-z0-9_./-]{0,63}$/

export const EMPTY_SETTINGS_STATUS = Object.freeze({
  available: false,
  connected: false,
  hostId: '',
  pairingCode: '',
  devices: Object.freeze([]),
  diagnostics: null,
  configError: null,
  configuration: null,
})

export const EMPTY_MANAGED_SNAPSHOT = Object.freeze({
  available: false,
  managed: true,
  service: 'whalerein_cloud',
  state: 'idle',
  environment: 'production',
  profileId: '',
  label: '',
  hostName: '',
  stagingEnabled: false,
  developerToolsEnabled: false,
  approvalUrl: '',
  error: null,
  connected: false,
  pendingOperation: false,
  profiles: Object.freeze([]),
  capabilities: Object.freeze([]),
  configError: null,
  configuration: null,
})

/** Bound non-secret display text and remove control characters/newlines. */
function text(value, max = 200) {
  if (typeof value !== 'string') return ''
  return value.replace(/[\u0000-\u001f\u007f]/g, ' ').replace(/\s+/g, ' ').trim().slice(0, max)
}

function safeCode(value, fallback = 'internal') {
  return typeof value === 'string' && ERROR_CODE.test(value) ? value : fallback
}

function safeBoolean(value) {
  return value === true
}

function safeApprovalUrl(value) {
  if (typeof value !== 'string' || value.length === 0 || value.length > 2048) return ''
  let url
  try {
    url = new URL(value)
  } catch {
    return ''
  }
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.hash) return ''
  return url.toString()
}

function safeConfigurationError(raw) {
  if (!raw || typeof raw !== 'object') return null
  const code = typeof raw.code === 'string' && /^[a-z][a-z0-9_]{0,63}$/.test(raw.code) ? raw.code : 'unreadable_config'
  return {
    code,
    message: text(raw.message, 240) || 'configuration could not be resolved',
    environment: typeof raw.environment === 'string' && /^[a-z0-9][a-z0-9-]{0,63}$/.test(raw.environment) ? raw.environment : null,
  }
}

export function projectConfiguration() { return null }

function safeProfiles(value) {
  if (!Array.isArray(value)) return []
  return value.slice(0, 32).flatMap((profile) => {
    if (!profile || typeof profile !== 'object') return []
    const id = text(profile.id, 128)
    if (!id || !/^[A-Za-z0-9_-][A-Za-z0-9_.:-]{0,127}$/.test(id)) return []
    const environment = ENVIRONMENTS.has(profile.environment) ? profile.environment : 'custom'
    return [{
      id,
      label: text(profile.label, 64),
      environment,
      active: safeBoolean(profile.active),
      managed: safeBoolean(profile.managed ?? true),
      capabilities: Array.isArray(profile.capabilities)
        ? [...new Set(profile.capabilities.filter((name) => CAPABILITIES.has(name)))].slice(0, 8)
        : [],
    }]
  })
}

function safeCapabilities(value) {
  if (!Array.isArray(value)) return []
  return [...new Set(value.filter((name) => CAPABILITIES.has(name)))].slice(0, 8)
}

/**
 * Project the frozen ConnectionManager snapshot. Every field is selected
 * explicitly so a future runtime field cannot leak through this boundary.
 */
export function sanitizeManagerSnapshot(raw = {}, extra = {}) {
  const state = CONNECTION_STATES.includes(raw?.state) ? raw.state : 'action_required'
  return {
    available: raw?.available !== false,
    managed: true,
    state,
    environment: ENVIRONMENTS.has(raw?.environment) ? raw.environment : 'custom',
    profileId: text(raw?.profileId, 128),
    label: text(raw?.label, 64),
    hostName: text(raw?.hostName, 64),
    stagingEnabled: safeBoolean(raw?.stagingEnabled),
    developerToolsEnabled: safeBoolean(raw?.developerToolsEnabled) || extra.developerTools === true,
    // The browser handoff needs the approval destination. It is validated to a
    // credential-free http(s) origin and is never written to logs.
    approvalUrl: safeApprovalUrl(raw?.approvalUrl),
    error: raw?.error && typeof raw.error === 'object'
      ? { code: safeCode(raw.error.code) }
      : null,
    connected: safeBoolean(raw?.connected),
    pendingOperation: safeBoolean(raw?.pendingOperation),
    profiles: safeProfiles(raw?.profiles),
    capabilities: safeCapabilities(raw?.capabilities),
    configError: extra.configError ?? null,
  }
}

/** Project manager details without routes, credentials or host identifiers. */
export function sanitizeManagerDetails(raw = {}) {
  if (!raw || typeof raw !== 'object') return null
  return {
    state: CONNECTION_STATES.includes(raw.state) ? raw.state : null,
    environment: ENVIRONMENTS.has(raw.environment) ? raw.environment : null,
    label: text(raw.label, 64) || null,
    hostName: text(raw.hostName, 64) || null,
    accessOrigin: safeApprovalUrl(raw.accessOrigin) || null,
    region: text(raw.region, 64) || null,
    revision: Number.isSafeInteger(raw.revision) && raw.revision >= 0 ? raw.revision : null,
    lastErrorCode: raw.lastErrorCode ? safeCode(raw.lastErrorCode) : null,
    protocolVersion: Number.isSafeInteger(raw.protocolVersion) && raw.protocolVersion > 0 ? raw.protocolVersion : null,
  }
}

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

export function isManagedHost(host) {
  return host?.managed === true || host?.mode === 'managed' || !!host?.manager || !!host?.connectionManager
}

function managedManager(host) {
  return host?.manager ?? host?.connectionManager ?? null
}

export function settingsStatus(host) {
  const configError = host?.configError ?? null
  if (isManagedHost(host)) {
    const manager = managedManager(host)
    const configuration = projectConfiguration(host)
    if (!manager || typeof manager.snapshot !== 'function') {
      return { ...EMPTY_MANAGED_SNAPSHOT, error: { code: 'unavailable' }, configError, configuration }
    }
    let raw
    try {
      raw = manager.snapshot()
    } catch {
      return { ...EMPTY_MANAGED_SNAPSHOT, error: { code: 'unavailable' }, configError, configuration }
    }
    if (!raw || typeof raw !== 'object') {
      return { ...EMPTY_MANAGED_SNAPSHOT, error: { code: 'unavailable' }, configError, configuration }
    }
    raw = { ...raw, profiles: [], stagingEnabled: false, developerToolsEnabled: false, capabilities: (raw.capabilities ?? []).filter((c) => !['switching', 'custom-service', 'staging'].includes(c)) }
    const snapshot = sanitizeManagerSnapshot(raw, {
      configError,
      developerTools: false,
    })
    return { ...snapshot, service: host.service || host.config?.service || 'whalerein_cloud', configuration: null }
  }
  if (!host?.started) {
    return {
      ...EMPTY_SETTINGS_STATUS,
      hostId: typeof host?.config?.hostId === 'string' ? host.config.hostId : '',
      devices: [],
      configError,
      configuration: null,
    }
  }
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
    managed: false,
    service: 'standalone',
    connected: host.access?.connected === true,
    hostId: host.config.hostId,
    pairingCode: security?.hostFingerprintDisplay ?? '',
    devices,
    diagnostics: settingsDiagnostics(host),
    configError,
    configuration: null,
  }
}

const MANAGER_METHODS = Object.freeze([
  'startConnect', 'cancel', 'retry', 'disconnect', 'reconnect', 'removeComputer',
])

/** Non-secret profile identity returned by profile-adding manager methods. */
function sanitizeAction(raw) {
  if (!raw || typeof raw !== 'object') return null
  const id = text(raw.id ?? raw.deleted, 128)
  const action = {
    id: /^[A-Za-z0-9_-][A-Za-z0-9_.:-]{0,127}$/.test(id) ? id : '',
    label: text(raw.label, 64),
    environment: ENVIRONMENTS.has(raw.environment) ? raw.environment : '',
    active: raw.active === true,
    deleted: text(raw.deleted, 128),
    remoteRegistration: raw.remoteRegistration === 'not_revoked' ? 'not_revoked' : '',
    // Browser-authorized lifecycle destination (for example Remove this
    // computer). Validated as a credential-free http(s) URL; never logged.
    url: safeApprovalUrl(raw.url),
  }
  return Object.values(action).some((value) => value !== '' && value !== false) ? action : null
}

export class WhaleReinSettingsService extends TypertRemoteService {
  constructor(ctx, host) {
    super(ctx, 'whalereinSettings', { namespace: 'whalerein' })
    this.host = host
    this.manager = managedManager(host)
    this.managed = isManagedHost(host)
  }

  snapshot() {
    return settingsStatus(this.host)
  }

  details() {
    if (!this.manager || typeof this.manager.details !== 'function') return null
    try {
      return sanitizeManagerDetails(this.manager.details())
    } catch {
      throw new RemoteError('whalerein/unavailable', 'connection details unavailable')
    }
  }

  async _forward(method, ...args) {
    if (!this.manager || typeof this.manager[method] !== 'function') {
      throw new RemoteError(
        this.managed ? 'whalerein/unavailable' : 'whalerein/unsupported_capability',
        'connection manager method unavailable',
      )
    }
    let result
    try {
      result = await this.manager[method](...args)
    } catch {
      // Manager diagnostics may contain private state. Only a stable code
      // crosses the browser boundary.
      throw new RemoteError('whalerein/action-failed', 'connection action failed')
    }
    return { ...this.snapshot(), action: sanitizeAction(result) }
  }

  startConnect() {
    // The manager already owns its source adapter. Browser provenance is not
    // an adapter and must never replace it.
    return this._forward('startConnect')
  }

  cancel() { return this._forward('cancel') }
  retry() { return this._forward('retry') }
  disconnect() { return this._forward('disconnect') }
  reconnect() { return this._forward('reconnect') }
  removeComputer() { return this._forward('remove') }
  _requirePairing() {
    if (!this.managed) return
    const capabilities = this.snapshot()?.capabilities ?? []
    if (!capabilities.includes('pairing')) {
      throw new RemoteError('whalerein/unsupported_capability', 'device pairing is not available for this cloud connection')
    }
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
    this._requirePairing()
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
    this._requirePairing()
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
markRemote(WhaleReinSettingsService.prototype.details, 'details')
for (const method of MANAGER_METHODS) {
  markRemote(WhaleReinSettingsService.prototype[method], method)
}
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
