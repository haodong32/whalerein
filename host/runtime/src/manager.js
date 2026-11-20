// Host service connection manager.
//
// One manager owns one selected Host profile per plugin installation. It
// composes the profile registry, cross-process activation ownership, the
// enrollment client and the existing HostRuntime transport. It never shares
// CLI/Core state and never exposes secrets in snapshots.

import { hostname as osHostname } from 'node:os'
import { createHash, randomBytes } from 'node:crypto'
import { HostRuntime } from './host.js'
import { MqttAccess } from './access/mqtt.js'
import { PluginError } from './errors.js'
import { generateHostId } from './host-id.js'
import { ActivationOwner } from './activation.js'
import { HostProfileRegistry, RegistryError, hasLegacyMarkers } from './registry.js'
import {
  CREDENTIAL_PURPOSES,
  ProfileCredentialStore,
  createRuntimeCredentials,
  ensureRecoverySecret,
  prepareProfileIdentity,
} from './credentials.js'
import {
  EnrollmentError,
  IntentJournal,
  ManagedEnrollmentClient,
  fetchServiceMetadata,
} from './enrollment.js'
import { open } from './configuration/open.js'
import { resolveEnvironment } from './configuration/resolve.js'
import { view as viewConfiguration } from './configuration/view.js'
import { update as updateConfigurationDocument } from './configuration/update.js'
import { createScopedClient } from './configuration/tls.js'
import { redactError } from './configuration/errors.js'
import { customService, validateServiceOrigin } from './services.js'
import { isBuiltinEnvironment } from './configuration/catalog.js'
import { IDENTIFIER_PATTERN } from './configuration/document.js'

const STATES = new Set([
  'idle',
  'requesting',
  'awaiting_approval',
  'enrolling',
  'recovering',
  'connecting',
  'connected',
  'reconnecting',
  'disconnected',
  'denied',
  'expired',
  'action_required',
  'removing',
  'removed',
])

const LOCAL_ERROR_CODES = new Set([
  'invalid_argument',
  'unauthorized',
  'forbidden',
  'not_found',
  'conflict',
  'expired',
  'unsupported_capability',
  'protocol_version_incompatible',
  'rate_limited',
  'internal',
  'unavailable',
  'recovery_required',
  'removal_pending',
  'disposed',
  'corrupt_intent',
  'registry_future_schema',
  'registry_corrupt',
  'registry_missing_key',
  'registry_unsafe_state',
  'registry_write_failed',
  'registry_lock_failed',
  'profile_not_found',
  'profile_conflict',
])

function codeOf(error) {
  const code = typeof error?.code === 'string' ? error.code : ''
  return LOCAL_ERROR_CODES.has(code) ? code : 'internal'
}

function canonicalJson(value) {
  return JSON.stringify([value.environment, value.id, value.api_origin, value.issuer, value.native_client_id, value.mode])
}

/** Stable non-secret authority fingerprint for the resolved definition. */
export function authorityOf(resolution) {
  return createHash('sha256').update(canonicalJson(resolution.definition)).digest('hex')
}

function sameService(service, definition) {
  if (!service || !definition) return false
  return service.id === definition.id
    && service.environment === (definition.environment ?? '')
    && service.apiOrigin === (definition.api_origin ?? null)
}

function sanitizeName(value, fallback = 'WhaleRein Host') {
  if (typeof value !== 'string') return fallback
  const text = value.replace(/[\u0000-\u001f\u007f]/g, ' ').trim()
  return text === '' ? fallback : text.slice(0, 64)
}

export class ConnectionManager {
  #options
  #configSnapshot = null
  #resolution = null
  #configError = null
  #desiredSnapshot = null
  #desiredResolution = null
  #desiredError = null
  #scopedClients = new Map()
  #registry
  #activation
  #now
  #hostName
  #installationId
  #state = 'idle'
  #errorCode = null
  #connected = false
  #runtime = null
  #source = null
  #generation = 0
  #pollAbort = null
  #operation = null
  #queue = Promise.resolve()
  #disposed = false
  #lastErrorCode = null
  #metadataCache = new Map()
  ready = null

  constructor(options = {}) {
    if (!options || typeof options !== 'object') throw new PluginError('invalid_argument', 'manager options are required')
    if (typeof options.stateRoot !== 'string' || options.stateRoot === '') {
      throw new PluginError('invalid_argument', 'manager stateRoot is required')
    }
    if (!options.credentials) throw new PluginError('invalid_argument', 'credential adapter is required')
    const configFile = options.configFile ?? options.configPath
    if (configFile !== undefined && (typeof configFile !== 'string' || configFile === '')) {
      throw new PluginError('invalid_argument', 'configFile must be a non-empty path')
    }
    const environmentOverride = options.environment === undefined || options.environment === null || options.environment === ''
      ? null
      : options.environment
    if (environmentOverride !== null && typeof environmentOverride !== 'string') {
      throw new PluginError('invalid_argument', 'environment override must be a string')
    }
    if (options.profileConfiguration === true && !options.configSnapshot) {
      throw new PluginError('invalid_argument', 'profile configuration snapshot is required')
    }
    this.#options = {
      configFile: typeof configFile === 'string' ? configFile : undefined,
      environment: environmentOverride,
      envVars: options.envVars,
      homeDir: options.homeDir,
      platform: options.platform,
      configSnapshot: options.configSnapshot ?? null,
      profileConfiguration: options.profileConfiguration === true,
      allowDefaultInit: options.allowDefaultInit !== false,
      stateRoot: options.stateRoot,
      credentials: options.credentials,
      now: options.now ?? Date.now,
      fetch: options.fetch ?? null,
      mqtt: options.mqtt ?? ((mqttOptions) => new MqttAccess(mqttOptions)),
      runtimeFactory: options.runtimeFactory ?? ((runtimeOptions) => new HostRuntime(runtimeOptions)),
      log: options.log ?? null,
      installationId: options.installationId ?? 'dsh',
      hostName: options.hostName ?? osHostname(),
      random: options.random ?? randomBytes,
      locale: options.locale ?? 'en',
      displayName: options.displayName,
      allowLoopback: Boolean(options.allowLoopback),
      verifyAuthorization: options.verifyAuthorization ?? null,
      expectedIssuer: options.expectedIssuer ?? '',
      source: options.source ?? null,
      getSource: options.getSource ?? null,
      standalone: options.standalone ?? null,
      runtimeConfig: options.runtimeConfig ?? {},
      runtimeReadyTimeoutMs: options.runtimeReadyTimeoutMs ?? 10_000,
      removalStatus: options.removalStatus ?? null,
      lock: options.lock,
    }
    this.#now = this.#options.now
    this.#hostName = sanitizeName(this.#options.hostName)
    this.#installationId = this.#options.installationId
    this.#source = this.#options.source
    this.#registry = new HostProfileRegistry({
      stateRoot: this.#options.stateRoot,
      now: this.#options.now,
      random: this.#options.random,
      legacy: Boolean(options.legacy),
    })
    this.#activation = new ActivationOwner({ stateRoot: this.#options.stateRoot, lock: this.#options.lock })
    this.#registry.ensure()
    if (options.developerToolsEnabled) this.#registry.markDeveloperTools(true)
    try {
      this.#registry.recoverPrepared()
    } catch {
      // A prepared marker never selects; a later switch retries safely.
    }
    this.#resolveConfiguration()
    try {
      this.#reconcileDesiredProfile()
    } catch (error) {
      // Keep the settings surface and current profile available; a changed
      // authority must never be silently rebound.
      this.#configError = this.#redactConfigurationError(error)
    }
    this.#refreshDerivedState()
    this.ready = this.#initialize()
  }

  // --- frozen API ---------------------------------------------------------

  snapshot() {
    this.#refreshDesired()
    const record = this.#activeRecord()
    const intent = this.#readIntent(record)
    const staging = this.#stagingProfile()
    const profiles = this.#registry.list().map((profile) => ({
      id: profile.id,
      label: profile.label,
      environment: profile.environment,
      active: profile.id === record?.id,
      managed: profile.mode === 'managed',
      capabilities: this.#capabilitiesFor(profile),
    }))
    const desired = this.#desiredResolution
    return {
      state: this.#state,
      environment: record?.environment ?? desired?.environment ?? 'standalone',
      desiredEnvironment: desired?.environment ?? null,
      selectionSource: desired?.selection_source ?? null,
      definitionSource: desired?.definition_source ?? null,
      configPath: this.#desiredSnapshot?.path ?? this.#configSnapshot?.path ?? this.#options.configFile ?? null,
      configError: this.#desiredError ?? this.#configError,
      profileId: record?.id ?? null,
      label: record?.label ?? '',
      hostName: this.#hostName,
      stagingEnabled: staging !== null,
      developerToolsEnabled: this.#developerToolsEnabled(),
      approvalUrl: this.#state === 'awaiting_approval' ? (intent?.approveUrl ?? null) : null,
      error: this.#errorCode === null ? null : { code: this.#errorCode },
      connected: this.#connected,
      devices: this.#runtime?.clientDevices?.() ?? null,
      pendingOperation: this.#pendingOperation(record, intent),
      action: this.#actionFor(record, intent),
      profiles,
      capabilities: record ? this.#capabilitiesFor(record) : [],
    }
  }

  details() {
    const record = this.#activeRecord()
    return {
      ...this.snapshot(),
      mode: record?.mode ?? null,
      hostId: record?.bindings?.hostId ?? null,
      serviceId: record?.service?.id ?? null,
      apiOrigin: record?.service?.apiOrigin ?? null,
      accessOrigin: record?.bindings?.accessOrigin ?? null,
      region: record?.bindings?.region ?? null,
      revision: record?.bindings?.enrollmentRevision ?? null,
      removalState: record?.pending?.removal ?? null,
      lastErrorCode: this.#lastErrorCode,
    }
  }

  async startConnect({ source } = {}) {
    return this.#run(async () => {
      this.#assertUsable()
      if (source) this.#source = source
      const record = this.#activeRecord()
      if (!record) throw new PluginError('internal', 'no active profile')
      if (record.pending?.removal === 'removing') throw this.#refuse('removal_pending', 'removal is pending; re-adding requires fresh approval')
      let readd = false
      if (record.pending?.removal === 'removed') {
        // Re-adding always creates a fresh approval intent.
        readd = true
        await this.#registry.setPending(record.id, { removal: null, paused: false })
        await this.#registry.clearPendingPhase(record.id)
        await this.#clearPollCredential(record)
        this.#intentJournal(record)?.clear()
      }
      if (record.mode === 'standalone') return this.#startStandalone(record, { resume: true })
      if (this.#runtime !== null) return this.snapshot()
      if (this.#operation !== null) return this.snapshot()
      // Connect resumes an established binding instead of requesting approval
      // again. A removed profile must instead obtain fresh approval.
      const existing = this.#readIntent(record)
      if (!readd && this.#hasBinding(record) && record.bindings?.operationId && (!existing || existing.state === 'enrolled')) {
        return this.#reconnect(this.#activeRecord())
      }
      this.#activation.acquire()
      this.#generation += 1
      const generation = this.#generation
      this.#setState('requesting', null)
      try {
        return await this.#beginApproval(record, generation, { retry: false })
      } catch (error) {
        this.#handleOperationFailure(error)
        throw error
      }
    })
  }

  async cancel() {
    return this.#run(async () => {
      this.#assertUsable()
      const record = this.#activeRecord()
      this.#generation += 1
      this.#abortPolling()
      this.#operation = null
      const uncertain = record?.pending?.uncertain === true
      await this.#stopRuntime()
      const intent = this.#readIntent(record)
      if (intent && !uncertain) {
        await this.#clearPollCredential(record)
        this.#intentJournal(record)?.clear()
      }
      if (!uncertain && record) {
        try {
          await this.#registry.clearPendingPhase(record.id)
        } catch {
          // keep state derived from what is persisted
        }
      }
      this.#activation.release()
      if (uncertain) {
        this.#setState('action_required', 'recovery_required')
      } else {
        this.#state = this.#establishedState(record)
        this.#errorCode = null
      }
      return this.snapshot()
    })
  }

  async retry() {
    return this.#run(async () => {
      this.#assertUsable()
      const record = this.#activeRecord()
      if (!record) throw new PluginError('internal', 'no active profile')
      if (record.pending?.removal === 'removing' || record.pending?.removal === 'removed') throw this.#refuse('removal_pending', 'removal is pending')
      if (['denied', 'expired', 'action_required'].includes(this.#state) || record.pending?.phase === undefined) {
        if (record.pending?.uncertain) {
          const reconciled = await this.#reconcileUncertain(record)
          if (reconciled === false) throw this.#failFast('recovery_required', 'the previous operation remains unresolved')
          if (this.#hasBinding(record)) return this.#reconnect(record)
        }
        if (this.#state === 'disconnected') return this.#reconnect(record)
        return this.#restartApproval(record)
      }
      if (this.#state === 'disconnected') return this.#reconnect(record)
      if (this.#state === 'idle' || this.#state === 'expired' || this.#state === 'denied') return this.#restartApproval(record)
      return this.snapshot()
    })
  }

  async disconnect() {
    return this.#run(async () => {
      this.#assertUsable()
      const record = this.#activeRecord()
      if (!record) throw new PluginError('internal', 'no active profile')
      this.#generation += 1
      this.#abortPolling()
      this.#operation = null
      // Persist the explicit pause before teardown so a crash mid-stop cannot
      // reconnect on the next start, then stop transport before acknowledging.
      await this.#registry.setPending(record.id, { paused: true })
      await this.#registry.clearPendingPhase(record.id)
      const intent = this.#readIntent(record)
      if (intent && record.pending?.phase === 'approval') {
        await this.#clearPollCredential(record)
        this.#intentJournal(record)?.clear()
      }
      await this.#stopRuntime()
      this.#activation.release()
      this.#connected = false
      this.#setState('disconnected', null)
      return this.snapshot()
    })
  }

  async reconnect() {
    return this.#run(async () => {
      this.#assertUsable()
      const record = this.#activeRecord()
      if (!record) throw new PluginError('internal', 'no active profile')
      return this.#reconnect(record)
    })
  }

  /**
   * Finish a lost enrollment reply with a fresh browser-authorized
   * `host_credential_recovery` authorization. Without a supplied authorization,
   * create the durable Cloud recovery intent and open its approval URL; the
   * same Host polls it with a one-time token and performs the replacement.
   */
  async recoverCredential({ authorization } = {}) {
    return this.#run(async () => {
      this.#assertUsable()
      const record = this.#activeRecord()
      if (!record || record.mode !== 'managed') throw this.#failFast('unsupported_capability', 'credential recovery requires a managed profile')
      if (record.pending?.removal === 'removing' || record.pending?.removal === 'removed') {
        throw this.#refuse('removal_pending', 'removal is pending; credential recovery is fenced')
      }
      const apiOrigin = record.service?.apiOrigin
      const accessOrigin = record.bindings?.accessOrigin
      if (typeof accessOrigin !== 'string' || accessOrigin === '') throw this.#failFast('not_found', 'this profile has no enrolled Access route')
      if (!authorization) {
        if (typeof apiOrigin !== 'string' || apiOrigin === '') throw this.#failFast('unavailable', 'service origin is unavailable')
        if (!this.#activation.owned) this.#activation.acquire()
        this.#generation += 1
        const generation = this.#generation
        this.#setState('recovering', null)
        try {
          const prepared = await this.#prepareProfile(record, { allowCreate: false })
          const metadata = await this.#metadataFor(record)
          const client = this.#createClient(record, prepared, metadata)
          const intent = await client.createRecoveryIntent({ apiOrigin })
          this.#continueRecovery(record, generation, client, intent, accessOrigin)
          return { ...this.snapshot(), url: intent.approvalUrl, recoveryIntentId: intent.intentId }
        } catch (error) {
          this.#handleOperationFailure(error)
          throw error
        }
      }
      if (!this.#activation.owned) this.#activation.acquire()
      this.#generation += 1
      const generation = this.#generation
      this.#setState('recovering', 'recovery_required')
      try {
        const prepared = await this.#prepareProfile(record, { allowCreate: false })
        const metadata = await this.#metadataFor(record)
        const client = this.#createClient(record, prepared, metadata)
        const credential = await client.replaceCredential({ accessOrigin, authorization })
        await this.#registry.setBindings(record.id, {
          enrollmentRevision: credential.enrollmentRevision || record.bindings?.enrollmentRevision,
          accessOrigin,
        })
        await this.#registry.setPending(record.id, { uncertain: false, phase: 'credential' })
        await this.#startManagedRuntime(record, credential, generation)
        return this.snapshot()
      } catch (error) {
        this.#handleOperationFailure(error)
        throw error
      }
    })
  }

  #continueRecovery(record, generation, client, intent, accessOrigin) {
    if (this.#operation !== null) return this.#operation
    const controller = new AbortController()
    this.#pollAbort = controller
    const task = (async () => {
      try {
        const approved = await client.pollRecovery({
          apiOrigin: record.service?.apiOrigin,
          intentId: intent.intentId,
          pollToken: intent.pollToken,
          isActive: () => generation === this.#generation && !this.#disposed,
          signal: controller.signal,
          deadlineMs: intent.expiresAtUnixMs ? Math.max(0, intent.expiresAtUnixMs - this.#now()) : 10 * 60_000,
        })
        if (generation !== this.#generation) return
        const credential = await client.replaceCredential({ accessOrigin, authorization: approved.authorization })
        await this.#registry.setBindings(record.id, {
          enrollmentRevision: credential.enrollmentRevision || record.bindings?.enrollmentRevision,
          accessOrigin,
        })
        await this.#registry.setPending(record.id, { uncertain: false, phase: 'credential' })
        await this.#startManagedRuntime(record, credential, generation)
      } catch (error) {
        if (generation !== this.#generation) return
        if (error instanceof EnrollmentError && error.code === 'cancelled') return
        this.#handleOperationFailure(error)
      }
    })()
    this.#operation = task
    task.finally(() => {
      if (this.#operation === task) this.#operation = null
      if (generation === this.#generation) this.#pollAbort = null
    })
    return task
  }

  async remove() {
    return this.#run(async () => {
      this.#assertUsable()
      const record = this.#activeRecord()
      if (!record) throw new PluginError('internal', 'no active profile')
      if (record.mode !== 'managed') throw this.#failFast('unsupported_capability', 'remote removal is unavailable for Standalone profiles')
      const hostId = record.bindings?.hostId
      if (typeof hostId !== 'string' || hostId === '') throw this.#failFast('not_found', 'this profile has no bound Host')
      const apiOrigin = record.service?.apiOrigin
      if (typeof apiOrigin !== 'string' || apiOrigin === '') throw this.#failFast('unavailable', 'service origin is unavailable')
      // Do not claim removal locally before Cloud accepts the intent. The
      // browser confirmation creates the durable intent; `refreshRemoval`
      // observes it and fences activation only after that proof.
      const url = `${apiOrigin}/hosts/${encodeURIComponent(hostId)}?removal=1`
      return { state: 'confirm_removal', url, remoteRevocation: 'unconfirmed' }
    })
  }

  /** Observe the durable Cloud/Access removal lifecycle without a browser. */
  async refreshRemoval() {
    return this.#run(() => this.#syncRemoval())
  }

  /** Reconcile a browser-authorized removal without claiming it locally. */
  async reconcileRemoval() {
    return this.#run(() => this.#syncRemoval())
  }

  async #syncRemoval() {
    this.#assertUsable()
    const record = this.#activeRecord()
    if (!record || record.mode !== 'managed') return this.snapshot()
    let observed
    try {
      if (typeof this.#options.removalStatus === 'function') {
        observed = await this.#options.removalStatus({ apiOrigin: record.service?.apiOrigin, hostId: record.bindings?.hostId })
      } else {
        const metadata = await this.#metadataFor(record)
        const prepared = await this.#prepareProfile(record, { allowCreate: false })
        const client = this.#createClient(record, prepared, metadata)
        observed = await client.readRemoval({ apiOrigin: record.service?.apiOrigin, hostId: record.bindings?.hostId })
      }
    } catch {
      return this.snapshot()
    }
    const state = typeof observed === 'string' ? observed : observed?.state
    if (state === 'enforced' || state === 'removed') {
      this.#generation += 1
      this.#abortPolling()
      await this.#stopRuntime()
      await this.#registry.setPending(record.id, { removal: 'removed' })
      await this.#clearTransportCredential(record)
      this.#setState('removed', null)
    } else if (state === 'failed') {
      await this.#registry.setPending(record.id, { removal: 'failed' })
      this.#setState('action_required', 'unavailable')
    } else if (state === 'pending' || state === 'disabled' || state === 'removing') {
      this.#generation += 1
      this.#abortPolling()
      await this.#stopRuntime()
      await this.#registry.setPending(record.id, { removal: 'removing' })
      this.#setState('removing', null)
    }
    return this.snapshot()
  }

  async switchProfile(id) {
    if (this.#options.profileConfiguration) throw new PluginError('unsupported_capability', 'edit the owning profile configuration')
    return this.#run(async () => {
      this.#assertUsable()
      if (typeof id !== 'string' || id === '') throw new PluginError('invalid_argument', 'profile id is required')
      const current = this.#activeRecord()
      if (!current) throw new PluginError('internal', 'no active profile')
      const target = this.#resolveRecord(this.#registry.get(id))
      if (!target) throw new PluginError('not_found', 'profile does not exist')
      if (current.id === target.id) return this.snapshot()
      // Validate the target locally before changing ownership or selection.
      this.#validateTarget(target)
      const acquiredHere = !this.#activation.owned
      this.#activation.acquire()
      try {
        this.#generation += 1
        this.#abortPolling()
        this.#operation = null
        try {
          await this.#stopRuntime()
        } catch {
          throw new PluginError('conflict', 'the active runtime did not stop; selection is unchanged')
        }
        this.#persistUncertainForCurrent(current)
        const before = this.#registry.read()
        if (!before) throw new PluginError('internal', 'profile registry is missing')
        const expectedRevision = before.revision
        this.#registry.prepareActivation(target.id, expectedRevision)
        const preparedRevision = this.#registry.read().revision
        try {
          this.#registry.commitActivation(target.id, preparedRevision)
        } catch (error) {
          try {
            this.#registry.abortActivation(this.#registry.read().revision)
          } catch {
            // the prepared marker never selects
          }
          throw error
        }
        this.#metadataCache.delete(current.id)
        this.#refreshDerivedState()
        this.#setState(this.#establishedState(target), null)
        // Start the target only after the pointer is committed. Failure stays
        // on the selected target; it never falls back to the old profile.
        const selection = this.#activeRecord()
        if (selection && selection.mode === 'managed' && this.#hasBinding(selection) && this.#source) {
          try {
            await this.#resumeManaged(selection)
          } catch (error) {
            this.#handleOperationFailure(error)
          }
        }
        if (this.#runtime === null) this.#activation.release()
        return this.snapshot()
      } catch (error) {
        if (acquiredHere && this.#runtime === null) this.#activation.release()
        this.#errorCode = codeOf(error)
        this.#lastErrorCode = codeOf(error)
        throw error
      }
    })
  }

  async enableStaging() {
    if (this.#options.profileConfiguration) throw new PluginError('unsupported_capability', 'edit the owning profile configuration')
    return this.#run(async () => {
      this.#assertUsable()
      const record = this.#registry.enableStaging()
      return { id: record.id, label: record.label, environment: record.environment, developerToolsEnabled: true }
    })
  }

  async addCustomService({ label, apiOrigin } = {}) {
    if (this.#options.profileConfiguration) throw new PluginError('unsupported_capability', 'edit the owning profile configuration')
    return this.#run(async () => {
      this.#assertUsable()
      const descriptor = customService({ label, apiOrigin }, { allowLoopback: this.#options.allowLoopback })
      if (typeof this.#options.fetch !== 'function') throw this.#failFast('unavailable', 'metadata client is unavailable')
      const metadata = await fetchServiceMetadata({
        apiOrigin: descriptor.apiOrigin,
        fetch: this.#options.fetch,
        allowLoopback: this.#options.allowLoopback,
      })
      const record = this.#registry.addProfile({
        label: descriptor.label,
        // A custom service keeps its own identity and environment; the Host
        // profile is a custom managed profile, never an official one.
        environment: 'custom',
        mode: 'managed',
        service: {
          id: metadata.service,
          environment: metadata.environment,
          label: descriptor.label,
          apiOrigin: descriptor.apiOrigin,
        },
      })
      this.#metadataCache.set(record.id, metadata)
      return { id: record.id, label: record.label, environment: record.environment, active: false }
    })
  }

  async deleteProfile(id) {
    if (this.#options.profileConfiguration) throw new PluginError('unsupported_capability', 'edit the owning profile configuration')
    return this.#run(async () => {
      this.#assertUsable()
      const record = this.#registry.get(id)
      if (!record) throw new PluginError('not_found', 'profile does not exist')
      if (record.id === this.#activeRecord()?.id) throw new PluginError('conflict', 'the active profile cannot be deleted')
      if (record.mode === 'managed') {
        const store = new ProfileCredentialStore({
          credentials: this.#options.credentials,
          installationId: this.#installationId,
          profileId: record.id,
        })
        for (const purpose of Object.values(CREDENTIAL_PURPOSES)) {
          try {
            await store.remove(purpose)
          } catch {
            // local profile metadata removal remains the primary action
          }
        }
      }
      const outcome = this.#registry.removeProfile(id)
      this.#metadataCache.delete(id)
      return {
        deleted: outcome.profile.id,
        remoteRegistration: 'not_revoked',
        message: 'Local profile data was deleted. Any remote Host registration remains until removed in the browser.',
      }
    })
  }

  /** Read-only projection of the selected file, latest desired default and the pinned runtime. */
  configurationView() {
    if (this.#options.profileConfiguration) return this.#resolution ? viewConfiguration(this.#resolution) : null
    const requested = this.#options.configFile
    let snapshot = null
    let error = this.#configError
    if (this.#activeRecord()?.mode !== 'standalone') {
      try {
        snapshot = open({
          configFile: requested,
          envVars: this.#options.envVars,
          homeDir: this.#options.homeDir,
          inspectionOnly: true,
          allowDefaultInit: false,
        })
        error = null
        return viewConfiguration(snapshot, { override: this.#options.environment })
      } catch (failure) {
        error = this.#redactConfigurationError(failure)
      }
    }
    return {
      path: requested ?? this.#configSnapshot?.path ?? null,
      source_kind: 'explicit',
      revision: snapshot?.revision ?? this.#configSnapshot?.revision ?? null,
      selected_environment: this.#resolution?.environment ?? null,
      selection_source: this.#resolution?.selection_source ?? null,
      definition_source: this.#resolution?.definition_source ?? null,
      desired_default: snapshot?.document?.environment ?? this.#configSnapshot?.document?.environment ?? null,
      effective_override: this.#options.environment,
      definition: this.#resolution?.definition ?? null,
      field_provenance: {},
      validation: { scope: 'offline', live_readiness: 'not_checked' },
      errors: error ? [error] : [],
    }
  }

  /** Scoped trust metadata for the pinned environment (never a global TLS change). */
  environmentTrust() {
    const record = this.#activeRecord()
    if (!record || record.mode === 'standalone') return null
    const trust = this.#trustFor(record)
    if (!trust) return null
    return { environment: trust.environment, provider: trust.provider, caFile: trust.caFile, scope: trust.scope }
  }

  /** Persist a typed settings patch through the Host configuration module only. */
  async updateConfiguration(patch) {
    if (this.#options.profileConfiguration) throw new PluginError('unsupported_capability', 'edit the owning profile configuration')
    return this.#run(async () => {
      this.#assertUsable()
      const snapshot = open({
        configFile: this.#options.configFile,
        envVars: this.#options.envVars,
        homeDir: this.#options.homeDir,
        allowDefaultInit: this.#options.allowDefaultInit,
        legacyState: () => hasLegacyMarkers(this.#options.stateRoot),
      })
      const next = updateConfigurationDocument(snapshot, patch, snapshot.revision)
      this.#configSnapshot = next
      this.#desiredSnapshot = next
      try {
        this.#resolution = resolveEnvironment(next, this.#options.environment)
        this.#desiredResolution = this.#resolution
        this.#configError = null
        this.#desiredError = null
      } catch (error) {
        this.#resolution = null
        this.#desiredResolution = null
        this.#configError = this.#redactConfigurationError(error)
        this.#desiredError = this.#configError
      }
      return this.configurationView()
    })
  }

  /**
   * Explicitly switch the running authority. The file is reread, the target
   * authority gets its own profile, and pending work refuses the switch.
   */
  async applyEnvironment(environment) {
    if (this.#options.profileConfiguration) throw new PluginError('unsupported_capability', 'edit the owning profile configuration')
    return this.#run(async () => {
      this.#assertUsable()
      if (typeof environment !== 'string' || !IDENTIFIER_PATTERN.test(environment)) {
        throw new PluginError('invalid_argument', 'environment name is invalid')
      }
      const record = this.#activeRecord()
      if (this.#pendingOperation(record, this.#readIntent(record))) {
        throw new PluginError('conflict', 'unfinished work must settle before switching environments')
      }
      const snapshot = open({
        configFile: this.#options.configFile,
        envVars: this.#options.envVars,
        homeDir: this.#options.homeDir,
        allowDefaultInit: this.#options.allowDefaultInit,
        legacyState: () => hasLegacyMarkers(this.#options.stateRoot),
      })
      const resolution = resolveEnvironment(snapshot, environment)
      if (resolution.definition.mode === 'standalone') {
        throw new PluginError('unsupported_capability', 'standalone profile state is not configured through the environment file')
      }
      const target = this.#resolveProfileTarget(resolution)
      const current = this.#activeRecord()
      if (current?.id === target.id) {
        this.#configSnapshot = snapshot
        this.#resolution = resolution
        this.#desiredSnapshot = snapshot
        this.#desiredResolution = resolution
        this.#configError = null
        this.#desiredError = null
        return this.snapshot()
      }
      this.#validateTarget(target)
      const acquiredHere = !this.#activation.owned
      this.#activation.acquire()
      try {
        this.#generation += 1
        this.#abortPolling()
        this.#operation = null
        try {
          await this.#stopRuntime()
        } catch {
          throw new PluginError('conflict', 'the active runtime did not stop; selection is unchanged')
        }
        this.#persistUncertainForCurrent(current)
        const before = this.#registry.read()
        if (!before) throw new PluginError('internal', 'profile registry is missing')
        const expectedRevision = before.revision
        this.#registry.prepareActivation(target.id, expectedRevision)
        const preparedRevision = this.#registry.read().revision
        try {
          this.#registry.commitActivation(target.id, preparedRevision)
        } catch (error) {
          try {
            this.#registry.abortActivation(this.#registry.read().revision)
          } catch {
            // the prepared marker never selects
          }
          throw error
        }
        this.#metadataCache.delete(current?.id)
        this.#metadataCache.delete(target.id)
        this.#closeScopedClients()
        this.#configSnapshot = snapshot
        this.#resolution = resolution
        this.#desiredSnapshot = snapshot
        this.#desiredResolution = resolution
        this.#configError = null
        this.#desiredError = null
        this.#refreshDerivedState()
        this.#setState(this.#establishedState(target), null)
        const selection = this.#activeRecord()
        if (selection && selection.mode === 'managed' && this.#hasBinding(selection) && this.#source) {
          try {
            await this.#resumeManaged(selection)
          } catch (error) {
            this.#handleOperationFailure(error)
          }
        }
        if (this.#runtime === null) this.#activation.release()
        return this.snapshot()
      } catch (error) {
        if (acquiredHere && this.#runtime === null) this.#activation.release()
        this.#errorCode = codeOf(error)
        this.#lastErrorCode = codeOf(error)
        throw error
      }
    })
  }

  async dispose() {
    if (this.#disposed) return
    this.#disposed = true
    this.#generation += 1
    this.#abortPolling()
    const operation = this.#operation
    this.#operation = null
    try {
      await this.#stopRuntime()
    } catch {
      // contained; ownership is released below
    }
    this.#closeScopedClients()
    try {
      await operation
    } catch {
      // the operation observes its own generation fence
    }
    this.#activation.release()
    this.#connected = false
    this.#clearError()
  }

  // --- configuration reconciliation ---------------------------------------

  #resolveConfiguration() {
    this.#configError = null
    this.#configSnapshot = null
    this.#resolution = null
    const active = this.#registry.active()
    if (active?.mode === 'standalone' && !this.#options.profileConfiguration) return
    if (this.#options.configSnapshot) {
      try {
        this.#configSnapshot = this.#options.configSnapshot
        this.#resolution = resolveEnvironment(this.#configSnapshot, this.#options.environment)
        this.#desiredSnapshot = this.#configSnapshot
        this.#desiredResolution = this.#resolution
        this.#desiredError = null
      } catch (error) {
        this.#configSnapshot = null
        this.#configError = this.#redactConfigurationError(error)
        this.#desiredError = this.#configError
      }
      return
    }
    try {
      this.#configSnapshot = open({
        configFile: this.#options.configFile,
        envVars: this.#options.envVars,
        homeDir: this.#options.homeDir,
        allowDefaultInit: this.#options.allowDefaultInit,
        legacyState: () => hasLegacyMarkers(this.#options.stateRoot),
      })
      this.#resolution = resolveEnvironment(this.#configSnapshot, this.#options.environment)
      this.#desiredSnapshot = this.#configSnapshot
      this.#desiredResolution = this.#resolution
      this.#desiredError = null
    } catch (error) {
      this.#configSnapshot = null
      this.#resolution = null
      this.#configError = this.#redactConfigurationError(error)
      this.#desiredSnapshot = null
      this.#desiredResolution = null
      this.#desiredError = this.#configError
    }
  }

  /** Re-read only the desired selection; the pinned runtime never follows it. */
  #refreshDesired() {
    if (this.#options.profileConfiguration) return
    if (this.#registry.active()?.mode === 'standalone') return
    try {
      const snapshot = open({
        configFile: this.#options.configFile,
        envVars: this.#options.envVars,
        homeDir: this.#options.homeDir,
        inspectionOnly: true,
        allowDefaultInit: false,
      })
      this.#desiredSnapshot = snapshot
      this.#desiredResolution = resolveEnvironment(snapshot, this.#options.environment)
      this.#desiredError = null
    } catch (error) {
      this.#desiredSnapshot = null
      this.#desiredResolution = null
      this.#desiredError = this.#redactConfigurationError(error)
    }
  }

  #redactConfigurationError(error) {
    const code = typeof error?.code === 'string' && /^[a-z][a-z0-9_]{0,63}$/.test(error.code)
      ? error.code
      : 'unreadable_config'
    const redacted = error?.name === 'ConfigurationError' ? redactError(error) : null
    return {
      code: redacted?.code ?? code,
      message: redacted?.message ?? 'configuration could not be resolved',
      environment: typeof error?.environment === 'string' ? error.environment : null,
    }
  }

  #reconcileDesiredProfile() {
    if (!this.#resolution) return
    const definition = this.#resolution.definition
    if (definition.mode === 'standalone') return
    const target = this.#resolveProfileTarget(this.#resolution)
    if (target && target.id !== this.#registry.active()?.id) this.#activateAtStartup(target.id)
  }

  #activateAtStartup(id) {
    this.#activation.acquire()
    try {
      const registry = this.#registry.read()
      if (!registry) throw new PluginError('internal', 'profile registry is missing')
      this.#registry.prepareActivation(id, registry.revision)
      this.#registry.commitActivation(id, this.#registry.read().revision)
    } finally {
      this.#activation.release()
    }
  }

  /** Find or create the managed profile for one resolved environment. */
  #resolveProfileTarget(resolution) {
    const definition = resolution.definition
    if (definition.mode !== 'account') return null
    const environment = resolution.environment
    const authority = authorityOf(resolution)
    const profiles = this.#registry.list().filter((profile) => profile.mode === 'managed')
    const match = profiles.find((profile) => profile.bindings?.serviceAuthority === authority)
    if (match) return match
    const sameEnvironment = profiles.find((profile) => profile.environment === environment)
    if (sameEnvironment) {
      if (sameEnvironment.bindings?.serviceAuthority) {
        throw new PluginError('invalid_argument', 'environment authority changed; use a new Host profile')
      }
      if (!sameService(sameEnvironment.service, definition)) {
        throw new PluginError('invalid_argument', 'environment authority changed; use a new Host profile')
      }
      this.#registry.setBindings(sameEnvironment.id, { serviceAuthority: authority })
      return this.#registry.get(sameEnvironment.id)
    }
    const label = this.#uniqueProfileLabel(definition.label ?? `WhaleRein ${environment}`, environment)
    const service = {
      id: definition.id,
      environment: definition.environment ?? 'custom',
      label,
      apiOrigin: definition.api_origin,
      issuer: definition.issuer,
      nativeClientId: definition.native_client_id,
    }
    return this.#registry.addProfile({
      label,
      environment,
      service,
      bindings: { serviceAuthority: authority },
    })
  }

  #uniqueProfileLabel(preferred, environment) {
    const base = sanitizeName(preferred, `WhaleRein ${environment}`).slice(0, 64)
    const labels = new Set(this.#registry.list().map((profile) => profile.label.toLowerCase()))
    if (!labels.has(base.toLowerCase())) return base
    for (let index = 2; index < 100; index += 1) {
      const suffix = ` (${index})`
      const candidate = `${base.slice(0, 64 - suffix.length)}${suffix}`
      if (!labels.has(candidate.toLowerCase())) return candidate
    }
    return `${environment.slice(0, 50)}-${createHash('sha256').update(base).digest('hex').slice(0, 8)}`.slice(0, 64)
  }

  #resolutionFor(record) {
    if (!record || record.environment === 'standalone' || !this.#configSnapshot) return null
    try {
      return resolveEnvironment(this.#configSnapshot, this.#options.profileConfiguration ? this.#options.environment : record.environment)
    } catch {
      return null
    }
  }

  #scopedClientFor(record) {
    const resolution = this.#resolutionFor(record)
    if (!resolution) return null
    const key = `${resolution.environment}:${authorityOf(resolution)}`
    let client = this.#scopedClients.get(key)
    if (!client) {
      client = createScopedClient(resolution, {
        homeDir: this.#options.homeDir,
        envVars: this.#options.envVars,
        platform: this.#options.platform,
      })
      this.#scopedClients.set(key, client)
    }
    return client
  }

  #fetchFor(record) {
    if (typeof this.#options.fetch === 'function') return this.#options.fetch
    return this.#scopedClientFor(record)?.fetch ?? globalThis.fetch
  }

  #trustFor(record) {
    return this.#scopedClientFor(record)?.trust ?? null
  }

  #closeScopedClients() {
    const clients = [...this.#scopedClients.values()]
    this.#scopedClients.clear()
    for (const client of clients) client.close().catch(() => {})
  }

  // --- initialization -----------------------------------------------------

  async #initialize() {
    if (this.#options.profileConfiguration && this.#configError) {
      this.#setState('action_required', 'invalid_argument')
      return
    }
    let record
    try {
      record = this.#activeRecord()
    } catch (error) {
      this.#setState('action_required', codeOf(error))
      return
    }
    if (!record) return
    if (record.pending?.removal === 'removed' || record.pending?.removal === 'removing') {
      await this.#refreshRemovalOnStart(record)
      return
    }
    const intent = this.#readIntent(record)
    if (intent && ['awaiting_approval', 'creating', 'create_uncertain'].includes(intent.state)) {
      this.#state = record.pending?.uncertain ? 'action_required' : 'awaiting_approval'
      this.#errorCode = record.pending?.uncertain ? 'recovery_required' : null
    } else if (record.pending?.phase === 'enrollment' || record.pending?.phase === 'credential') {
      this.#state = 'recovering'
      this.#errorCode = 'recovery_required'
    } else {
      this.#state = this.#establishedState(record)
    }
    const source = this.#source ?? this.#callSourceProvider()
    if (!source) return
    this.#source = source
    try {
      if (record.mode === 'standalone') {
        await this.#run(() => this.#startStandalone(record, { resume: true }))
      } else if (record.pending?.phase === 'approval' && intent) {
        await this.#run(async () => {
          this.#activation.acquire()
          this.#generation += 1
          await this.#continuePersisted(record, this.#generation)
        })
      } else if (record.pending?.uncertain || record.pending?.phase === 'enrollment'
        || record.pending?.phase === 'credential') {
        await this.#run(async () => {
          const reconciled = await this.#reconcileUncertain(record)
          if (reconciled && this.#hasBinding(record) && record.pending?.paused !== true) {
            await this.#reconnect(record)
          }
        })
      } else if (this.#hasBinding(record) && record.pending?.paused !== true) {
        await this.#run(() => this.#reconnect(record))
      }
    } catch (error) {
      this.#handleOperationFailure(error)
    }
  }

  async #refreshRemovalOnStart(record) {
    if (typeof this.#options.removalStatus !== 'function') return
    try {
      const observed = await this.#options.removalStatus({
        apiOrigin: record.service?.apiOrigin,
        hostId: record.bindings?.hostId,
      })
      const state = typeof observed === 'string' ? observed : observed?.state
      if (state === 'none' || state === 'absent') {
        // The browser confirmation never created an intent; keep the original
        // connection policy and let the user reopen the correct confirmation.
        await this.#registry.setPending(record.id, { removal: null })
        this.#refreshDerivedState()
      }
    } catch {
      // unavailable status preserves the original state and link
    }
  }

  // --- approval and enrollment -------------------------------------------

  async #restartApproval(record) {
    this.#activation.acquire()
    this.#generation += 1
    const generation = this.#generation
    const journal = this.#intentJournal(record)
    try {
      journal?.clear()
    } catch {
      // a new intent is never blocked on stale local metadata
    }
    await this.#clearPollCredential(record)
    this.#metadataCache.delete(record.id)
    this.#setState('requesting', null)
    return this.#beginApproval(record, generation, { retry: true })
  }

  async #beginApproval(record, generation, { retry = false } = {}) {
    if (!this.#source) throw this.#failFast('invalid_argument', 'a source adapter is required to connect')
    const prepared = await this.#prepareProfile(record)
    const metadata = await this.#metadataFor(record)
    const client = this.#createClient(record, prepared, metadata)
    const existing = client.journal.read()
    if (!retry && existing && existing.state !== 'create_failed' && existing.state !== 'expired' && existing.state !== 'denied') {
      this.#setState(existing.state === 'uncertain' ? 'recovering' : 'awaiting_approval', existing.state === 'uncertain' ? 'recovery_required' : null)
      await this.#continuePersisted(record, generation)
      return this.snapshot()
    }
    const intent = await client.createIntent({ retry })
    if (generation !== this.#generation) throw new EnrollmentError('cancelled', 'profile changed during approval')
    this.#setState('awaiting_approval', null)
    await this.#registry.setPending(record.id, { phase: 'approval', intentId: intent.intentId, operationId: intent.id })
    this.#continue(record, generation, client, intent)
    return this.snapshot()
  }

  async #continuePersisted(record, generation) {
    const prepared = await this.#prepareProfile(record)
    const metadata = await this.#metadataFor(record)
    const client = this.#createClient(record, prepared, metadata)
    const intent = client.journal.read()
    if (!intent) throw this.#failFast('not_found', 'approval intent is missing')
    if (intent.state === 'approved' && intent.authorization && intent.route) {
      this.#setState('enrolling', null)
      await this.#continueApproved(record, generation, client, this.#approvedFromIntent(intent))
      return this.snapshot()
    }
    if (intent.state === 'uncertain' || record.pending?.uncertain) {
      this.#setState('recovering', 'recovery_required')
      return this.snapshot()
    }
    this.#setState('awaiting_approval', null)
    this.#continue(record, generation, client, intent)
    return this.snapshot()
  }

  #continue(record, generation, client, intent) {
    if (this.#operation !== null) return this.#operation
    this.#pollAbort = new AbortController()
    const task = (async () => {
      try {
        const approved = intent.state === 'approved' && intent.authorization && intent.route
          ? this.#approvedFromIntent(intent)
          : await client.poll({
            isActive: () => generation === this.#generation && !this.#disposed,
            signal: this.#pollAbort?.signal,
          })
        if (generation !== this.#generation) return
        this.#setState('enrolling', null)
        await this.#continueApproved(record, generation, client, approved)
      } catch (error) {
        if (generation !== this.#generation) return
        if (error instanceof EnrollmentError && error.code === 'cancelled') return
        this.#handleOperationFailure(error)
      }
    })()
    this.#operation = task
    task.finally(() => {
      if (this.#operation === task) this.#operation = null
      if (generation === this.#generation) this.#pollAbort = null
    })
    return task
  }

  #approvedFromIntent(intent) {
    if (!intent.authorization || !intent.route) {
      throw new EnrollmentError('recovery_required', 'approved intent has no recoverable authorization', { committed: null })
    }
    return { authorization: intent.authorization, route: intent.route, operationId: intent.operationId }
  }

  async #continueApproved(record, generation, client, approved) {
    // Access may have committed even if this process dies mid-request. Mark the
    // operation uncertain before the secret-bearing call so cancel/restart
    // reconcile it instead of presenting a local cancellation as remote.
    await this.#registry.setPending(record.id, { phase: 'enrollment', operationId: approved.operationId, uncertain: true })
    const enrollment = await client.enroll(approved)
    if (generation !== this.#generation) return
    this.#setState('connecting', null)
    await this.#registry.setPending(record.id, { phase: 'enrollment', operationId: approved.operationId })
    await this.#registry.setBindings(record.id, {
      enrollmentRevision: enrollment.status.enrollmentRevision ?? record.bindings?.enrollmentRevision,
      operationId: approved.operationId,
      accessOrigin: approved.route.accessOrigin,
      region: approved.route.region,
    })
    await this.#startManagedRuntime(record, enrollment.credential, generation)
  }

  // --- runtime ------------------------------------------------------------

  async #startManagedRuntime(record, credential, generation) {
    // Re-read the committed record: the caller's snapshot predates the Host
    // binding, and preparing from it would mint a second identity that does
    // not match the credential this enrollment just issued.
    record = this.#registry.get(record.id) ?? record
    const prepared = await this.#prepareProfile(record)
    if (typeof credential.hostId === 'string' && credential.hostId !== prepared.hostId) {
      throw this.#failFast('conflict', 'the issued credential belongs to another Host identity')
    }
    const config = this.#managedConfig(record, prepared, credential)
    const rawAccess = this.#options.mqtt({
      url: credential.mqttUrl,
      hostId: prepared.hostId,
      token: credential.password,
      authorizationPublicKey: prepared.identity.signingPublicKey,
      caFile: this.#trustFor(record)?.caFile ?? '',
      reconnectMinMs: config.reconnectMinMs,
      reconnectMaxMs: config.reconnectMaxMs,
      logger: this.#options.log,
      now: this.#now,
    })
    if (!rawAccess || typeof rawAccess.start !== 'function') throw this.#failFast('internal', 'MQTT transport is unavailable')
    const access = this.#observeAccess(rawAccess, generation)
    const runtime = this.#options.runtimeFactory({
      config,
      adapter: this.#source,
      credentials: createRuntimeCredentials({ store: prepared.store, noisePrivateKey: prepared.identity.noisePrivateKey }),
      access,
      logger: this.#options.log ?? undefined,
      now: this.#now,
    })
    this.#runtime = runtime
    try {
      await runtime.start()
      await this.#waitReady(access)
    } catch (error) {
      // A transient readiness/transport failure keeps the existing MQTT retry
      // loop; onReady promotes the connection later. Terminal credential, TLS
      // or configuration refusals are still surfaced and torn down.
      if (codeOf(error) === 'unavailable') {
        this.#connected = false
        this.#setState('reconnecting', null)
        return
      }
      await this.#stopRuntime()
      throw error
    }
    if (generation !== this.#generation) {
      await this.#stopRuntime()
      return
    }
    this.#connected = true
    this.#setState('connected', null)
    await this.#registry.setPending(record.id, { paused: false })
    await this.#registry.clearPendingPhase(record.id)
  }

  async #startStandalone(record, { resume = false } = {}) {
    if (!this.#source) throw this.#failFast('invalid_argument', 'a source adapter is required to connect')
    const standalone = this.#options.standalone
    if (!standalone || !standalone.config) throw this.#failFast('invalid_argument', 'legacy Standalone configuration is unavailable')
    if (this.#runtime !== null) return this.snapshot()
    this.#activation.acquire()
    this.#generation += 1
    const generation = this.#generation
    this.#setState(resume ? 'reconnecting' : 'connecting', null)
    const config = { ...standalone.config, stateDir: standalone.config.stateDir ?? this.#options.stateRoot }
    const runtime = this.#options.runtimeFactory({
      config,
      adapter: this.#source,
      credentials: standalone.credentials ?? this.#options.credentials,
      logger: this.#options.log ?? undefined,
      now: this.#now,
    })
    this.#runtime = runtime
    try {
      await runtime.start()
    } catch (error) {
      await this.#stopRuntime()
      throw error
    }
    if (generation !== this.#generation) {
      await this.#stopRuntime()
      return this.snapshot()
    }
    this.#connected = true
    await this.#registry.setPending(record.id, { paused: false })
    this.#setState('connected', null)
    if (!resume) this.#activation.release()
    return this.snapshot()
  }

  async #reconnect(record) {
    if (record.pending?.removal === 'removing' || record.pending?.removal === 'removed') {
      throw this.#refuse('removal_pending', 'removal is pending')
    }
    if (this.#runtime !== null) return this.snapshot()
    if (!this.#source) throw this.#failFast('invalid_argument', 'a source adapter is required to connect')
    if (!this.#activation.owned) this.#activation.acquire()
    this.#generation += 1
    const generation = this.#generation
    this.#setState('reconnecting', null)
    try {
      if (record.mode === 'standalone') return await this.#startStandalone(record, { resume: true })
      const prepared = await this.#prepareProfile(record, { allowCreate: false })
      const credential = await prepared.store.getJson(CREDENTIAL_PURPOSES.transportCredential)
      if (!credential) {
        // A confirmed binding with a missing/superseded credential is
        // recoverable through the original enrollment; never a new approval.
        throw this.#failFast(
          this.#hasBinding(record) ? 'recovery_required' : 'not_found',
          this.#hasBinding(record) ? 'the saved transport credential is missing; recover it' : 'this profile is not enrolled',
        )
      }
      // `routeExpiresAt` is legacy approval metadata. Access confirms the
      // enrollment and the credential; an established Host reconnects without
      // a new approval even after the original authorization deadline.
      await this.#startManagedRuntime(record, credential, generation)
      return this.snapshot()
    } catch (error) {
      this.#handleOperationFailure(error)
      throw error
    }
  }

  async #resumeManaged(record) {
    if (this.#runtime !== null) return this.snapshot()
    if (record.pending?.removal === 'removing' || record.pending?.removal === 'removed') {
      throw this.#refuse('removal_pending', 'removal is pending')
    }
    if (record.pending?.paused === true) {
      this.#setState('disconnected', null)
      return this.snapshot()
    }
    const prepared = await this.#prepareProfile(record, { allowCreate: false })
    const credential = await prepared.store.getJson(CREDENTIAL_PURPOSES.transportCredential)
    if (!credential) return this.snapshot()
    this.#setState('reconnecting', null)
    await this.#startManagedRuntime(record, credential, this.#generation)
    return this.snapshot()
  }

  async #stopRuntime() {
    const runtime = this.#runtime
    this.#runtime = null
    this.#connected = false
    if (runtime) {
      try {
        await runtime.stop()
      } catch (error) {
        this.#runtime = runtime
        throw error
      }
    }
  }

  #observeAccess(access, generation) {
    return new Proxy(access, {
      get: (target, property) => {
        if (property === 'start') {
          return (handlers = {}) => target.start({
            ...handlers,
            onReady: () => {
              if (generation === this.#generation && !this.#disposed) {
                this.#connected = true
                if (!['removing', 'removed'].includes(this.#state)) this.#setState('connected', null)
              }
              handlers.onReady?.()
            },
            onDisconnected: () => {
              if (generation === this.#generation && !this.#disposed) {
                this.#connected = false
                if (!['removing', 'removed', 'disconnected', 'action_required'].includes(this.#state)) {
                  this.#setState('reconnecting', null)
                }
              }
              handlers.onDisconnected?.()
            },
            onFailure: (code) => {
              if (generation === this.#generation && !this.#disposed) {
                this.#connected = false
                this.#setState('action_required', code)
              }
              handlers.onFailure?.(code)
            },
          })
        }
        const value = target[property]
        return typeof value === 'function' ? value.bind(target) : value
      },
    })
  }

  async #waitReady(access) {
    if (typeof access.waitForConnected === 'function') {
      await access.waitForConnected(this.#options.runtimeReadyTimeoutMs)
      return
    }
    if (access.connected === true) return
    throw new PluginError('unavailable', 'Host readiness was not observed')
  }

  #managedConfig(record, prepared, credential) {
    const base = { ...this.#options.runtimeConfig }
    return {
      ...base,
      hostId: prepared.hostId,
      hostToken: credential.password,
      mqttUrl: credential.mqttUrl,
      stateDir: this.#registry.stateDirFor(record.id),
      caFile: this.#trustFor(record)?.caFile ?? base.caFile ?? '',
      outboxPerDeviceLimit: base.outboxPerDeviceLimit ?? 100,
      outboxTotalLimit: base.outboxTotalLimit ?? 1000,
      shutdownTimeoutMs: base.shutdownTimeoutMs ?? 5000,
      reconnectMinMs: base.reconnectMinMs ?? 500,
      reconnectMaxMs: base.reconnectMaxMs ?? 30_000,
      logLevel: base.logLevel ?? 'info',
      accessUrl: credential.accessOrigin,
    }
  }

  // --- profile preparation -----------------------------------------------

  async #prepareProfile(record, { allowCreate = true } = {}) {
    if (record.mode !== 'managed') throw this.#failFast('invalid_argument', 'only managed profiles enroll')
    const store = new ProfileCredentialStore({
      credentials: this.#options.credentials,
      installationId: this.#installationId,
      profileId: record.id,
    })
    try {
      const identity = await prepareProfileIdentity({ store, bindings: record.bindings })
      const changes = {}
      const signingPublicKey = Buffer.from(identity.signingPublicKey).toString('base64url')
      const noisePublicKey = Buffer.from(identity.noisePublicKey).toString('base64url')
      if (record.bindings?.signingPublicKey !== signingPublicKey) changes.signingPublicKey = signingPublicKey
      if (record.bindings?.noisePublicKey !== noisePublicKey) changes.noisePublicKey = noisePublicKey
      let hostId = record.bindings?.hostId
      if (typeof hostId !== 'string' || hostId === '') {
        if (!allowCreate) throw new PluginError('not_found', 'profile has no Host identity')
        hostId = generateHostId(this.#hostName, this.#options.random)
        changes.hostId = hostId
      }
      if (Object.keys(changes).length > 0) {
        await this.#registry.setBindings(record.id, changes)
      }
      await ensureRecoverySecret(store, this.#options.random)
      return {
        store,
        hostId,
        identity,
        record: this.#registry.get(record.id),
      }
    } catch (error) {
      if (error instanceof RegistryError) {
        throw new PluginError(error.code, error.detail || 'profile identity could not be loaded')
      }
      throw error
    }
  }

  async #metadataFor(record) {
    const cached = this.#metadataCache.get(record.id)
    if (cached) return cached
    const builtin = isBuiltinEnvironment(record.environment)
    const metadata = await fetchServiceMetadata({
      apiOrigin: record.service?.apiOrigin,
      fetch: this.#fetchFor(record),
      allowLoopback: this.#options.allowLoopback,
      expectedService: builtin ? record.service.id : '',
      expectedEnvironment: builtin ? (record.service.environment ?? record.environment) : '',
    })
    this.#metadataCache.set(record.id, metadata)
    return metadata
  }

  #createClient(record, prepared, metadata) {
    if (!metadata || typeof metadata !== 'object') throw new PluginError('protocol_version_incompatible', 'service metadata is unavailable')
    return new ManagedEnrollmentClient({
      apiOrigin: record.service.apiOrigin,
      metadata,
      hostId: prepared.hostId,
      signingPrivateKey: prepared.identity.signingPrivateKey,
      signingPublicKey: prepared.identity.signingPublicKey,
      noisePublicKey: prepared.identity.noisePublicKey,
      credentialStore: prepared.store,
      journal: this.#intentJournal(record),
      fetch: this.#fetchFor(record),
      now: this.#now,
      log: this.#options.log,
      allowLoopback: this.#options.allowLoopback,
      verifyAuthorization: this.#options.verifyAuthorization,
      expectedIssuer: this.#options.expectedIssuer,
      expectedAudience: '',
      displayName: this.#options.displayName ?? this.#hostName,
      locale: this.#options.locale,
    })
  }

  #intentJournal(record) {
    if (!record || record.mode !== 'managed') return null
    return new IntentJournal({ stateDir: this.#registry.stateDirFor(record.id) })
  }

  #readIntent(record) {
    if (!record || record.mode !== 'managed') return null
    try {
      return this.#intentJournal(record).read()
    } catch {
      return null
    }
  }

  async #clearPollCredential(record) {
    if (!record || record.mode !== 'managed') return
    try {
      const store = new ProfileCredentialStore({
        credentials: this.#options.credentials,
        installationId: this.#installationId,
        profileId: record.id,
      })
      await store.remove(CREDENTIAL_PURPOSES.pollCredential)
    } catch {
      // a terminal intent no longer polls
    }
  }

  async #clearTransportCredential(record) {
    if (!record || record.mode !== 'managed') return
    try {
      const store = new ProfileCredentialStore({
        credentials: this.#options.credentials,
        installationId: this.#installationId,
        profileId: record.id,
      })
      await store.remove(CREDENTIAL_PURPOSES.transportCredential)
    } catch {
      // local credential cleanup is best effort
    }
  }

  // --- state helpers ------------------------------------------------------

  #activeRecord() {
    const record = this.#registry.active()
    return this.#resolveRecord(record)
  }

  #resolveRecord(record) {
    // The pinned profile is the running authority. A changed desired
    // configuration is reported separately and only an explicit switch
    // resolves or refuses it; startup never silently rebinds here.
    return record
  }

  #stagingProfile() {
    return this.#registry.list().find((record) => record.service?.id === 'official-staging') ?? null
  }

  #developerToolsEnabled() {
    return this.#registry.read()?.developerToolsEnabled === true
  }

  #capabilitiesFor(record) {
    const metadata = this.#metadataCache.get(record?.id)
    if (!metadata || !Array.isArray(metadata.capabilities)) return []
    return metadata.capabilities.filter((capability) => typeof capability === 'string' && /^[a-z0-9-]{1,64}$/.test(capability))
  }

  #establishedState(record) {
    if (!record) return 'idle'
    if (record.pending?.removal === 'removed') return 'removed'
    if (record.pending?.removal === 'removing') return 'removing'
    if (record.pending?.paused) return 'disconnected'
    if (record.mode === 'standalone') return 'disconnected'
    if (this.#hasBinding(record)) return 'disconnected'
    return 'idle'
  }

  #actionFor(record, intent) {
    if (record?.pending?.removal === 'removed') return null
    if (record?.pending?.removal === 'removing') return { kind: 'verify' }
    if (this.#desiredError || this.#configError) return { kind: 'fix-config' }
    if (record?.pending?.paused === true) {
      return this.#hasBinding(record) ? { kind: 'resume' } : { kind: 'connect' }
    }
    if (this.#state === 'awaiting_approval' || this.#state === 'requesting') {
      const url = intent?.approveUrl ?? null
      return url ? { kind: 'open-approval', url } : { kind: 'retry' }
    }
    if (this.#state === 'idle' || this.#state === 'disconnected') return { kind: 'connect' }
    if (this.#state === 'action_required' || this.#state === 'denied' || this.#state === 'expired') {
      if (this.#errorCode === 'unauthorized' || this.#errorCode === 'tls_trust' || this.#errorCode === 'invalid_config') {
        return { kind: 'fix-config' }
      }
      if (this.#errorCode === 'recovery_required' || (this.#hasBinding(record) && this.#errorCode === 'not_found')) {
        // An uncertain enrollment/setup attempt is reconciled, not treated as a
        // credential recovery; only an established credential may recover.
        if (record?.pending?.phase === 'enrollment') return { kind: 'retry' }
        return this.#hasBinding(record) ? { kind: 'recover-credential' } : { kind: 'retry' }
      }
      return this.#hasBinding(record) ? { kind: 'retry' } : { kind: 'connect' }
    }
    return { kind: 'wait' }
  }

  #hasBinding(record) {
    return typeof record?.bindings?.hostId === 'string' && record.bindings.hostId !== ''
  }

  #pendingOperation(record, intent) {
    if (record?.pending?.removal !== undefined && record.pending.removal !== 'removed') return true
    if (record?.pending?.phase !== undefined) return true
    if (intent && ['creating', 'create_uncertain', 'awaiting_approval', 'approved', 'uncertain'].includes(intent.state)) return true
    return this.#operation !== null
  }

  #refreshDerivedState() {
    const record = this.#activeRecord()
    this.#state = this.#establishedState(record)
  }

  #setState(state, code) {
    if (!STATES.has(state)) throw new PluginError('internal', 'invalid manager state')
    this.#state = state
    this.#errorCode = code === null ? null : this.#sanitizeCode(code)
    if (code !== null) this.#lastErrorCode = this.#errorCode
  }

  #clearError() {
    this.#errorCode = null
  }

  #sanitizeCode(code) {
    const text = typeof code === 'string' ? code : 'internal'
    return /^[a-z][a-z0-9_]{0,63}$/.test(text) ? text : 'internal'
  }

  #refuse(code, message) {
    this.#lastErrorCode = this.#sanitizeCode(code)
    return new PluginError(LOCAL_ERROR_CODES.has(code) ? code : 'internal', message)
  }

  #failFast(code, message) {
    this.#setState('action_required', code)
    this.#lastErrorCode = this.#sanitizeCode(code)
    return new PluginError(LOCAL_ERROR_CODES.has(code) ? code : 'internal', message)
  }

  #handleOperationFailure(error) {
    // A failed enrollment/reconnect that owns no runtime must release the
    // activation lock, otherwise the recovery action cannot reacquire it.
    if (this.#runtime === null) {
      try { this.#activation.release() } catch { /* not owned */ }
    }
    const code = codeOf(error)
    this.#lastErrorCode = code
    if (code === 'recovery_required' || (error instanceof EnrollmentError && error.committed === true)) {
      const record = this.#activeRecord()
      if (record?.mode === 'managed') {
        try {
          this.#registry.setPending(record.id, { uncertain: true, phase: 'enrollment' })
        } catch {
          // the original failure remains the actionable state
        }
      }
    }
    if (error instanceof EnrollmentError) {
      if (error.code === 'denied') return this.#setState('denied', 'forbidden')
      if (error.code === 'expired') return this.#setState('expired', 'expired')
      if (error.code === 'recovery_required') return this.#setState('action_required', 'recovery_required')
      if (error.code === 'conflict') return this.#setState('action_required', 'conflict')
      if (error.code === 'unauthorized' || error.code === 'forbidden') {
        return this.#setState('action_required', error.code)
      }
      return this.#setState('action_required', code)
    }
    if (error instanceof RegistryError) {
      return this.#setState('action_required', code)
    }
    if (error instanceof PluginError) return this.#setState('action_required', code)
    this.#setState('action_required', 'internal')
  }

  async #reconcileUncertain(record) {
    const intent = this.#readIntent(record)
    const operationId = record.pending?.operationId ?? intent?.operationId ?? record.bindings?.operationId ?? ''
    const accessOrigin = record.bindings?.accessOrigin
    if (!operationId || !accessOrigin) return false
    try {
      const metadata = await this.#metadataFor(record)
      const prepared = await this.#prepareProfile(record, { allowCreate: false })
      const client = this.#createClient(record, prepared, metadata)
      const status = await client.readOperation({
        accessOrigin,
        operationId,
        authorizationExpiresAtUnixMs: intent?.authorizationExpiresAtUnixMs ?? intent?.expiresAtUnixMs ?? 0,
      })
      if (status.outcome === 'committed') {
        const credential = await prepared.store.getJson(CREDENTIAL_PURPOSES.transportCredential)
        if (!credential) {
          // The enrollment committed but its one-time credential was lost. Keep
          // the exact operation actionable; a fresh recovery operation is required.
          await this.#registry.setPending(record.id, { uncertain: true, phase: 'credential', operationId })
          this.#setState('action_required', 'recovery_required')
          return false
        }
        await this.#registry.setBindings(record.id, {
          operationId,
          enrollmentRevision: credential.enrollmentRevision || record.bindings?.enrollmentRevision,
        })
        await this.#registry.setPending(record.id, { uncertain: false })
        await this.#registry.clearPendingPhase(record.id)
        try { this.#intentJournal(record)?.clear() } catch { /* contained */ }
        this.#lastErrorCode = null
        return true
      }
      if (['refused', 'fenced', 'absent', 'expired'].includes(status.outcome)) {
        // A definitive no-effect outcome proves the newer attempt did not
        // register. Repair the local binding to the confirmed credential and
        // settle the rejected journal without a new enrollment.
        const credential = await prepared.store.getJson(CREDENTIAL_PURPOSES.transportCredential)
        if (!credential) return false
        await this.#registry.setPending(record.id, { uncertain: false })
        await this.#registry.clearPendingPhase(record.id)
        try {
          const journal = this.#intentJournal(record)
          if (journal) journal.write({ ...(journal.read() ?? {}), state: 'rejected', operationId, settledAt: this.#now() })
        } catch { /* contained */ }
        this.#lastErrorCode = null
        return true
      }
      // admitted/pending/unknown: retain the original operation across restart.
      return false
    } catch (error) {
      if (process.env.WR_DEBUG_RECONCILE) console.error('RECONCILE', error)
      return false
    }
  }

  #persistUncertainForCurrent(record) {
    if (this.#readIntent(record)?.state === 'uncertain' || record.pending?.uncertain) {
      try {
        this.#registry.setPending(record.id, { uncertain: true, phase: record.pending?.phase ?? 'enrollment' })
      } catch {
        // the original profile keeps its persisted pending flags
      }
    }
  }

  #validateTarget(record) {
    if (!record) throw new PluginError('not_found', 'profile does not exist')
    if (record.mode === 'managed') {
      if (record.environment !== 'standalone') {
        const origin = record.service?.apiOrigin
        if (typeof origin !== 'string' || origin === '') throw new PluginError('invalid_argument', 'target service origin is missing')
        validateServiceOrigin(origin, { allowLoopback: this.#options.allowLoopback })
      }
      // The registry validates the state directory (plain, non-symlink).
      this.#registry.stateDirFor(record.id)
    }
  }

  #callSourceProvider() {
    if (typeof this.#options.getSource !== 'function') return null
    try {
      return this.#options.getSource() ?? null
    } catch {
      return null
    }
  }

  #abortPolling() {
    try {
      this.#pollAbort?.abort()
    } catch {
      // contained
    }
    this.#pollAbort = null
  }

  #assertUsable() {
    if (this.#options.profileConfiguration && this.#configError) throw new PluginError('invalid_argument', 'profile configuration is invalid')
    if (this.#disposed) {
      const error = new PluginError('conflict', 'connection manager was disposed')
      error.code = 'disposed'
      throw error
    }
  }

  #run(fn) {
    const run = this.#queue.then(fn, fn)
    this.#queue = run.then(() => {}, () => {})
    return run
  }
}

export function createConnectionManager(options) {
  return new ConnectionManager(options)
}
