// Settings projection. Snapshot is read-only public state. invite asks Access
// for a one-time pairing URI and returns that URI plus the host fingerprint.
// revoke asks Access to revoke one device. The managed cloud connection is
// projected through a separate allowlisted snapshot; private keys, peer keys,
// the admin token, poll credentials, authorization documents, host ids, routes
// and raw approval locators stay off this API and out of logs.

import { Remote, RemoteError, TypertRemoteService } from '@deepseek-ai/dsh-typert-protocol'
import { createInvitation, revokeDevice } from './pairing.js'
import { testConnection } from './connection-test.js'
import { open, view as configurationView } from 'whalerein-host/configuration'

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

const CONFIG_SOURCES = new Set(['invocation', 'file', 'default'])
const CONFIG_DEFINITION_SOURCES = new Set(['builtin', 'file'])

/** Project the Host configuration view without secrets or credential URLs. */
export function sanitizeConfigurationView(raw = {}) {
  if (!raw || typeof raw !== 'object') return null
  const definition = raw.definition && typeof raw.definition === 'object'
    ? {
        id: text(raw.definition.id, 128),
        mode: raw.definition.mode === 'account' || raw.definition.mode === 'standalone' ? raw.definition.mode : null,
        label: text(raw.definition.label, 64),
      }
    : null
  const provenance = {}
  if (raw.field_provenance && typeof raw.field_provenance === 'object') {
    for (const key of ['id', 'mode', 'label', 'environment', 'tls', 'api_origin', 'issuer', 'native_client_id']) {
      const value = raw.field_provenance[key]
      if (value === 'catalog' || value === 'file' || value === 'default') provenance[key] = value
    }
  }
  return {
    path: text(raw.path, 2048),
    sourceKind: ['explicit', 'environment', 'default'].includes(raw.source_kind) ? raw.source_kind : 'explicit',
    revision: typeof raw.revision === 'string' && /^sha256:[a-f0-9]{64}$/.test(raw.revision) ? raw.revision : null,
    selectedEnvironment: typeof raw.selected_environment === 'string' && /^[a-z0-9][a-z0-9-]{0,63}$/.test(raw.selected_environment)
      ? raw.selected_environment : null,
    selectionSource: CONFIG_SOURCES.has(raw.selection_source) ? raw.selection_source : null,
    definitionSource: CONFIG_DEFINITION_SOURCES.has(raw.definition_source) ? raw.definition_source : null,
    desiredDefault: typeof raw.desired_default === 'string' && /^[a-z0-9][a-z0-9-]{0,63}$/.test(raw.desired_default)
      ? raw.desired_default : null,
    effectiveOverride: typeof raw.effective_override === 'string' && /^[a-z0-9][a-z0-9-]{0,63}$/.test(raw.effective_override)
      ? raw.effective_override : null,
    definition,
    provenance,
    validation: {
      scope: raw.validation?.scope === 'live' ? 'live' : 'offline',
      liveReadiness: text(raw.validation?.live_readiness, 64) || 'not_checked',
    },
    errors: Array.isArray(raw.errors) ? raw.errors.slice(0, 4).map(safeConfigurationError).filter(Boolean) : [],
  }
}

/** Read the configuration view from the manager, or the plugin's reference. */
export function projectConfiguration(host) {
  const manager = managedManager(host)
  if (manager && typeof manager.configurationView === 'function') {
    try {
      return sanitizeConfigurationView(manager.configurationView())
    } catch {
      return null
    }
  }
  if (typeof host?.configPath === 'string' && host.configPath !== '') {
    try {
      const snapshot = open({ configFile: host.configPath, inspectionOnly: true })
      const view = configurationView(snapshot, { override: host.environmentOverride ?? null })
      return sanitizeConfigurationView(view)
    } catch (error) {
      return sanitizeConfigurationView({
        path: host.configPath,
        source_kind: 'explicit',
        selected_environment: null,
        desired_default: null,
        effective_override: host.environmentOverride ?? null,
        errors: [{ code: error?.code ?? 'unreadable_config', message: 'configuration could not be resolved' }],
      })
    }
  }
  return null
}

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
    environment: ENVIRONMENTS.has(raw?.environment) ? raw.environment : 'production',
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
    const snapshot = sanitizeManagerSnapshot(raw, {
      configError,
      developerTools: host.developerToolsEnabled === true,
    })
    return { ...snapshot, configuration: projectConfiguration(host) }
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
  'switchProfile', 'enableStaging', 'addCustomService', 'deleteProfile',
])

function profileId(value) {
  if (typeof value !== 'string' || !/^[A-Za-z0-9_-][A-Za-z0-9_.:-]{0,127}$/.test(value)) {
    throw new RemoteError('whalerein/invalid-input', 'invalid profile identifier')
  }
  return value
}

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

function customService(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new RemoteError('whalerein/invalid-input', 'invalid custom service')
  }
  const label = text(value.label, 64)
  const apiOrigin = text(value.apiOrigin, 2048)
  let parsed
  try {
    parsed = new URL(apiOrigin)
  } catch {
    throw new RemoteError('whalerein/invalid-input', 'invalid service origin')
  }
  if (!['http:', 'https:'].includes(parsed.protocol) || parsed.username || parsed.password) {
    throw new RemoteError('whalerein/invalid-input', 'invalid service origin')
  }
  return { label, apiOrigin: parsed.toString() }
}

const CONFIG_NAME = /^[a-z0-9][a-z0-9-]{0,63}$/

function configName(value, label = 'environment') {
  if (typeof value !== 'string' || !CONFIG_NAME.test(value)) {
    throw new RemoteError('whalerein/invalid-input', `invalid ${label}`)
  }
  return value
}

function optionalLabel(value) {
  if (value === undefined || value === null || value === '') return undefined
  const label = text(value, 64)
  if (label === '') throw new RemoteError('whalerein/invalid-input', 'invalid label')
  return label
}

function optionalTls(value) {
  if (value === undefined || value === null) return undefined
  if (typeof value !== 'object' || Array.isArray(value)) throw new RemoteError('whalerein/invalid-input', 'invalid trust settings')
  const provider = value.ca_provider
  if (!['system', 'mkcert', 'file'].includes(provider)) throw new RemoteError('whalerein/invalid-input', 'invalid trust provider')
  if (provider === 'file') {
    const caFile = text(value.ca_file, 4096)
    if (caFile === '') throw new RemoteError('whalerein/invalid-input', 'invalid CA file')
    return { ca_provider: 'file', ca_file: caFile }
  }
  return { ca_provider: provider }
}

function customDefinition(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new RemoteError('whalerein/invalid-input', 'invalid environment definition')
  }
  const mode = value.mode
  if (!['account', 'standalone'].includes(mode)) throw new RemoteError('whalerein/invalid-input', 'invalid environment mode')
  const definition = { mode }
  const label = optionalLabel(value.label)
  if (label !== undefined) definition.label = label
  if (mode === 'account') {
    for (const key of ['api_origin', 'issuer', 'native_client_id']) {
      definition[key] = text(value[key], 2048)
      if (definition[key] === '') throw new RemoteError('whalerein/invalid-input', `missing ${key}`)
    }
  }
  const tls = optionalTls(value.tls)
  if (tls !== undefined) definition.tls = tls
  return definition
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

  startConnect(request) {
    const source = text(request?.source, 32) || 'settings'
    return this._forward('startConnect', { source: source === 'browser' ? 'browser' : 'settings' })
  }

  cancel() { return this._forward('cancel') }
  retry() { return this._forward('retry') }
  disconnect() { return this._forward('disconnect') }
  reconnect() { return this._forward('reconnect') }
  removeComputer() { return this._forward('remove') }
  async switchProfile(id) { return this._forward('switchProfile', profileId(id)) }
  enableStaging() { return this._forward('enableStaging') }
  async addCustomService(value) { return this._forward('addCustomService', customService(value)) }
  async deleteProfile(id) { return this._forward('deleteProfile', profileId(id)) }

  configuration() {
    return projectConfiguration(this.host)
  }

  async _updateConfiguration(patch) {
    const manager = this.manager
    if (!manager || typeof manager.updateConfiguration !== 'function') {
      throw new RemoteError(this.managed ? 'whalerein/unavailable' : 'whalerein/unsupported_capability', 'configuration settings are unavailable')
    }
    let view
    try {
      view = await manager.updateConfiguration(patch)
    } catch {
      throw new RemoteError('whalerein/action-failed', 'configuration update failed')
    }
    return { ...this.snapshot(), configuration: sanitizeConfigurationView(view) }
  }

  async saveDefaultEnvironment(value) {
    return this._updateConfiguration({ type: 'SetDefaultEnvironment', environment: configName(value?.environment) })
  }

  async specializeBuiltin(value) {
    const environment = configName(value?.environment)
    if (!['local', 'staging', 'production'].includes(environment)) {
      throw new RemoteError('whalerein/invalid-input', 'invalid built-in environment')
    }
    const patch = { type: 'SpecializeBuiltin', environment }
    const label = optionalLabel(value?.label)
    if (label !== undefined) patch.label = label
    const tls = optionalTls(value?.tls)
    if (tls !== undefined) patch.tls = tls
    return this._updateConfiguration(patch)
  }

  async saveCustomEnvironment(value) {
    return this._updateConfiguration({
      type: 'UpsertCustomEnvironment',
      name: configName(value?.name, 'environment name'),
      definition: customDefinition(value?.definition),
    })
  }

  async removeCustomEnvironment(value) {
    return this._updateConfiguration({
      type: 'RemoveCustomEnvironment',
      environment: configName(value?.name ?? value?.environment, 'environment name'),
    })
  }

  async applyEnvironment(value) {
    const environment = configName(value?.environment)
    const manager = this.manager
    if (!manager || typeof manager.applyEnvironment !== 'function') {
      throw new RemoteError(this.managed ? 'whalerein/unavailable' : 'whalerein/unsupported_capability', 'environment switching is unavailable')
    }
    let result
    try {
      result = await manager.applyEnvironment(environment)
    } catch {
      throw new RemoteError('whalerein/action-failed', 'environment switch failed')
    }
    return { ...this.snapshot(), action: sanitizeAction({ id: result.profileId, environment: result.environment, active: true }) }
  }

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
markRemote(WhaleReinSettingsService.prototype.configuration, 'configuration')
markRemote(WhaleReinSettingsService.prototype.saveDefaultEnvironment, 'saveDefaultEnvironment')
markRemote(WhaleReinSettingsService.prototype.specializeBuiltin, 'specializeBuiltin')
markRemote(WhaleReinSettingsService.prototype.saveCustomEnvironment, 'saveCustomEnvironment')
markRemote(WhaleReinSettingsService.prototype.removeCustomEnvironment, 'removeCustomEnvironment')
markRemote(WhaleReinSettingsService.prototype.applyEnvironment, 'applyEnvironment')

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
