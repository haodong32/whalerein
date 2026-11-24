// Host-specific, versioned profile registry. This is deliberately not
// `whalerein/v1/profiles`: the Node Host owns its own metadata schema and never
// reads or mutates the CLI/Core registry.
//
// The registry is non-secret. Private keys and poll/transport credentials live
// behind the credential adapter. Writes are atomic (temp file + fsync + rename)
// and serialized with a Node SQLite transaction so two processes cannot lose an
// update. SQLite is already a maintained dependency of the state layer and its
// locks are released by the OS on crash, so it is reused instead of a custom
// PID lock. A prepared marker records an activation intent; only a matching
// commit publishes the active pointer.

import {
  closeSync,
  existsSync,
  fsyncSync,
  lstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  renameSync,
  rmSync,
  unlinkSync,
  writeSync,
} from 'node:fs'
import { randomBytes } from 'node:crypto'
import { DatabaseSync } from 'node:sqlite'
import { dirname, isAbsolute, join, normalize, relative, resolve, sep } from 'node:path'
import { isBuiltinEnvironment } from './configuration/catalog.js'
import { IDENTIFIER_PATTERN } from './configuration/document.js'
import { OFFICIAL_SERVICES, PROFILE_MODES, validateServiceOrigin } from './services.js'

export const HOST_PROFILES_SCHEMA = 'whalerein-host/v1/profiles'
export const HOST_PROFILES_SCHEMA_VERSION = 1
export const HOST_PROFILES_FILE = 'host-profiles.json'
export const HOST_PROFILES_DIR = 'profiles'
export const LEGACY_PROFILE_ID = 'standalone'
export const LEGACY_PROFILE_LABEL = 'Existing configuration'

const PROFILE_ID_RE = /^[a-f0-9]{32}$/
const FIXED_PROFILE_IDS = new Set([LEGACY_PROFILE_ID])
const BINDING_KEYS = new Set([
  'serviceAuthority',
  'hostId',
  'signingPublicKey',
  'noisePublicKey',
  'workspaceId',
  'enrollmentRevision',
  'operationId',
  'accessOrigin',
  'region',
])
const PENDING_KEYS = new Set(['paused', 'removal', 'removalUrl', 'intentId', 'operationId', 'phase', 'uncertain'])
const PENDING_PHASES = new Set(['approval', 'enrollment', 'credential', 'removal'])
const REMOVAL_STATES = new Set(['removing', 'removed', 'failed'])
const RECORD_KEYS = new Set(['id', 'label', 'environment', 'mode', 'service', 'stateDir', 'bindings', 'pending', 'legacy', 'createdAt'])
const SERVICE_KEYS = new Set(['id', 'environment', 'label', 'apiOrigin', 'issuer', 'nativeClientId'])
const REGISTRY_KEYS = new Set(['schema', 'schemaVersion', 'revision', 'activationGeneration', 'active', 'developerToolsEnabled', 'prepared', 'profiles'])
const PREPARED_KEYS = new Set(['profileId', 'expectedRevision', 'generation'])

export class RegistryError extends Error {
  constructor(code, detail = '') {
    super(detail || code)
    this.name = 'RegistryError'
    this.code = code
    this.detail = detail
  }
}

function fail(code, detail) {
  throw new RegistryError(code, detail)
}

function isPlainObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

function exactKeys(value, allowed, name) {
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) fail('registry_corrupt', `${name} contains an unknown field`)
  }
}

function nowMs(now) {
  const value = now()
  if (!Number.isSafeInteger(value) || value < 0) fail('registry_corrupt', 'invalid clock value')
  return value
}

function safeId(id) {
  return typeof id === 'string' && (PROFILE_ID_RE.test(id) || FIXED_PROFILE_IDS.has(id))
}

function createProfileId(random = randomBytes) {
  return Buffer.from(random(16)).toString('hex')
}

function normalizeStateRoot(stateRoot) {
  if (typeof stateRoot !== 'string' || stateRoot === '' || stateRoot.includes('\0')) {
    fail('registry_unsafe_state', 'state root is required')
  }
  if (!isAbsolute(stateRoot)) fail('registry_unsafe_state', 'state root must be an absolute path')
  const normalized = normalize(stateRoot)
  if (normalized === sep) fail('registry_unsafe_state', 'the filesystem root cannot be a state root')
  return normalized
}

function atomicWriteFile(path, bytes, { fs = null } = {}) {
  const io = fs ?? { closeSync, fsyncSync, mkdirSync, openSync, renameSync, unlinkSync, writeSync }
  const temporary = `${path}.${process.pid}.${randomBytes(6).toString('hex')}.tmp`
  let fd
  try {
    mkdirSync(dirname(path), { recursive: true })
    fd = io.openSync(temporary, 'w', 0o600)
    let offset = 0
    while (offset < bytes.length) {
      const written = io.writeSync(fd, bytes, offset, bytes.length - offset)
      if (!Number.isSafeInteger(written) || written <= 0) throw new Error('short write')
      offset += written
    }
    io.fsyncSync(fd)
    io.closeSync(fd)
    fd = undefined
    io.renameSync(temporary, path)
    // Fsync the directory so the rename itself survives a crash on POSIX.
    try {
      const directory = io.openSync(dirname(path), 'r')
      try {
        io.fsyncSync(directory)
      } finally {
        io.closeSync(directory)
      }
    } catch {
      // Directory fsync is not available on every platform (Windows).
    }
  } catch (error) {
    if (fd !== undefined) {
      try {
        io.closeSync(fd)
      } catch {
        // contained
      }
    }
    try {
      io.unlinkSync(temporary)
    } catch {
      // contained
    }
    if (error instanceof RegistryError) throw error
    fail('registry_write_failed', 'profile registry could not be written')
  }
}

function validateBindings(bindings) {
  if (!isPlainObject(bindings)) fail('registry_corrupt', 'profile bindings must be an object')
  exactKeys(bindings, BINDING_KEYS, 'profile bindings')
  for (const [key, value] of Object.entries(bindings)) {
    if (value === undefined || value === null) continue
    if (typeof value !== 'string') fail('registry_corrupt', `profile binding ${key} must be a string`)
  }
  if (bindings.hostId !== undefined && !/^[A-Za-z0-9_.:-]{1,128}$/.test(bindings.hostId)) {
    fail('registry_corrupt', 'profile binding host id is unsafe')
  }
  for (const key of ['signingPublicKey', 'noisePublicKey']) {
    const value = bindings[key]
    if (value !== undefined && (typeof value !== 'string' || !/^[A-Za-z0-9_-]{16,128}$/.test(value))) {
      fail('registry_corrupt', `profile binding ${key} is invalid`)
    }
  }
  if (bindings.accessOrigin !== undefined) {
    try {
      validateServiceOrigin(bindings.accessOrigin, { allowLoopback: true })
    } catch {
      fail('registry_corrupt', 'profile binding access origin is invalid')
    }
  }
}

function validatePending(pending) {
  if (!isPlainObject(pending)) fail('registry_corrupt', 'profile pending state must be an object')
  exactKeys(pending, PENDING_KEYS, 'profile pending state')
  for (const key of ['paused', 'uncertain']) {
    if (pending[key] !== undefined && typeof pending[key] !== 'boolean') fail('registry_corrupt', `profile pending ${key} is invalid`)
  }
  for (const key of ['intentId', 'operationId']) {
    if (pending[key] !== undefined && (typeof pending[key] !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(pending[key]))) {
      fail('registry_corrupt', `profile pending ${key} is invalid`)
    }
  }
  if (pending.phase !== undefined && !PENDING_PHASES.has(pending.phase)) fail('registry_corrupt', 'profile pending phase is invalid')
  if (pending.removal !== undefined && !REMOVAL_STATES.has(pending.removal)) fail('registry_corrupt', 'profile removal state is invalid')
  if (pending.removalUrl !== undefined) {
    // The retained browser-confirmation target is a credential-free web URL.
    if (typeof pending.removalUrl !== 'string' || pending.removalUrl.length > 2048) {
      fail('registry_corrupt', 'profile pending removal URL is invalid')
    }
    let url
    try {
      url = new URL(pending.removalUrl)
    } catch {
      fail('registry_corrupt', 'profile pending removal URL is invalid')
    }
    if (!['http:', 'https:'].includes(url.protocol) || url.username !== '' || url.password !== '') {
      fail('registry_corrupt', 'profile pending removal URL is invalid')
    }
  }
}

function validateService(service, environment, label) {
  if (service === null) {
    if (environment !== 'standalone') fail('registry_corrupt', 'only standalone profiles may omit a service')
    return
  }
  if (!isPlainObject(service)) fail('registry_corrupt', 'profile service must be an object')
  exactKeys(service, SERVICE_KEYS, 'profile service')
  if (typeof service.id !== 'string' || service.id === '' || typeof service.label !== 'string') {
    fail('registry_corrupt', 'profile service is missing its identity')
  }
  if (environment === 'standalone') fail('registry_corrupt', 'standalone profile cannot select a managed service')
  if (isBuiltinEnvironment(environment)) {
    const official = OFFICIAL_SERVICES[environment]
    if (service.id !== official.id) fail('registry_corrupt', 'official service descriptor is reserved')
    if (service.environment !== official.environment) fail('registry_corrupt', 'official service environment does not match')
    if (service.apiOrigin !== official.apiOrigin) fail('registry_corrupt', 'official service origin does not match the catalog')
  } else {
    // A custom environment's service identity is reported by its own metadata,
    // never inferred from the local configuration name.
    if (typeof service.environment !== 'string' || service.environment === '' || service.environment === 'standalone') {
      fail('registry_corrupt', 'custom service environment is invalid')
    }
    let origin
    try {
      origin = validateServiceOrigin(service.apiOrigin, { allowLoopback: true })
    } catch {
      fail('registry_corrupt', 'profile service origin is invalid')
    }
    if (origin !== service.apiOrigin) fail('registry_corrupt', 'profile service origin is not canonical')
  }
  for (const key of ['issuer', 'nativeClientId']) {
    if (service[key] !== undefined && service[key] !== null && typeof service[key] !== 'string') {
      fail('registry_corrupt', `profile service ${key} is invalid`)
    }
  }
  if (label.length > 64) fail('registry_corrupt', 'profile label is too long')
}

function validateRecord(record, stateRoot) {
  if (!isPlainObject(record)) fail('registry_corrupt', 'profile record is not an object')
  exactKeys(record, RECORD_KEYS, 'profile record')
  if (!safeId(record.id)) fail('registry_corrupt', 'profile id is invalid')
  if (typeof record.label !== 'string' || record.label === '' || record.label.trim() !== record.label || record.label.length > 64) {
    fail('registry_corrupt', 'profile label is invalid')
  }
  const environment = record.environment
  if (environment === 'standalone') {
    // reserved for the legacy Standalone profile
  } else if (typeof environment !== 'string' || !IDENTIFIER_PATTERN.test(environment)) {
    fail('registry_corrupt', 'profile environment is invalid')
  }
  if (!PROFILE_MODES.includes(record.mode)) fail('registry_corrupt', 'profile mode is invalid')
  if (typeof record.legacy !== 'boolean') fail('registry_corrupt', 'profile legacy flag is invalid')
  if (!Number.isSafeInteger(record.createdAt) || record.createdAt < 0) fail('registry_corrupt', 'profile creation time is invalid')
  if (record.legacy) {
    if (record.id !== LEGACY_PROFILE_ID) fail('registry_corrupt', 'legacy profile must keep its reserved id')
    if (record.mode !== 'standalone' || record.environment !== 'standalone' || record.service !== null) {
      fail('registry_corrupt', 'legacy profile must remain standalone')
    }
    if (record.stateDir !== '.') fail('registry_corrupt', 'legacy profile must keep the root state')
  } else {
    if (record.mode === 'standalone' && record.environment !== 'standalone') fail('registry_corrupt', 'standalone mode requires the standalone environment')
    if (record.mode === 'managed' && record.environment === 'standalone') fail('registry_corrupt', 'managed mode requires a managed environment')
    const expected = `${HOST_PROFILES_DIR}/${record.id}`
    if (record.stateDir !== expected) fail('registry_corrupt', 'profile state directory is not canonical')
  }
  validateService(record.service, record.environment, record.label)
  validateBindings(record.bindings)
  validatePending(record.pending)
  // Reject paths that are absolute, escape the state root, or are symlinks.
  if (record.stateDir !== '.') {
    const resolved = resolve(stateRoot, record.stateDir)
    const rel = relative(stateRoot, resolved)
    if (rel === '' || rel.startsWith('..') || isAbsolute(rel)) fail('registry_unsafe_state', 'profile state escapes the state root')
    const stateDir = resolved
    let metadata
    try {
      metadata = lstatSync(stateDir)
    } catch (error) {
      if (error?.code === 'ENOENT') fail('registry_unsafe_state', 'profile state is missing; restore it or delete the profile')
      fail('registry_unsafe_state', 'profile state could not be inspected')
    }
    if (!metadata.isDirectory() || metadata.isSymbolicLink()) fail('registry_unsafe_state', 'profile state must be a plain directory')
  }
}

function validateRegistry(registry, stateRoot) {
  if (!isPlainObject(registry)) fail('registry_corrupt', 'profile registry is not an object')
  exactKeys(registry, REGISTRY_KEYS, 'profile registry')
  if (registry.schema !== HOST_PROFILES_SCHEMA) {
    if (typeof registry.schema === 'string' && registry.schema.startsWith('whalerein-host/v')) {
      fail('registry_future_schema', 'unsupported Host profile registry schema; stop older plugins')
    }
    fail('registry_corrupt', 'profile registry schema is unsupported')
  }
  if (registry.schemaVersion !== HOST_PROFILES_SCHEMA_VERSION) {
    fail('registry_future_schema', 'unsupported Host profile registry schema version')
  }
  if (!Number.isSafeInteger(registry.revision) || registry.revision < 1) fail('registry_corrupt', 'profile registry revision is invalid')
  if (!Number.isSafeInteger(registry.activationGeneration) || registry.activationGeneration < 0) {
    fail('registry_corrupt', 'profile activation generation is invalid')
  }
  if (typeof registry.active !== 'string' || registry.active === '') fail('registry_corrupt', 'profile registry has no active profile')
  if (typeof registry.developerToolsEnabled !== 'boolean') fail('registry_corrupt', 'developer tools flag is invalid')
  if (!Array.isArray(registry.profiles) || registry.profiles.length === 0) fail('registry_corrupt', 'profile registry has no profiles')
  const ids = new Set()
  const labels = new Set()
  for (const record of registry.profiles) {
    validateRecord(record, stateRoot)
    if (ids.has(record.id)) fail('registry_corrupt', 'duplicate profile id')
    if (labels.has(record.label.toLowerCase())) fail('registry_corrupt', 'duplicate profile label')
    ids.add(record.id)
    labels.add(record.label.toLowerCase())
  }
  if (!ids.has(registry.active)) fail('registry_corrupt', 'active profile is not present in the registry')
  if (registry.prepared !== null) {
    if (!isPlainObject(registry.prepared)) fail('registry_corrupt', 'prepared activation marker is invalid')
    exactKeys(registry.prepared, PREPARED_KEYS, 'prepared activation marker')
    if (!ids.has(registry.prepared.profileId)) fail('registry_corrupt', 'prepared activation names an unknown profile')
    if (!Number.isSafeInteger(registry.prepared.expectedRevision) || !Number.isSafeInteger(registry.prepared.generation)) {
      fail('registry_corrupt', 'prepared activation marker is invalid')
    }
    if (registry.prepared.profileId === registry.active) {
      fail('registry_corrupt', 'prepared activation already names the active profile')
    }
  }
}

function cloneRegistry(registry) {
  return JSON.parse(JSON.stringify(registry))
}

function profileStateDir(stateRoot, record) {
  return record.stateDir === '.' ? stateRoot : join(stateRoot, record.stateDir)
}

function ensurePrivateDirectory(path) {
  if (!existsSync(path)) mkdirSync(path, { recursive: true, mode: 0o700 })
  const metadata = lstatSync(path)
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) fail('registry_unsafe_state', 'profile state must be a plain directory')
}

function hasLegacyMarkers(stateRoot) {
  const markers = [
    'config.json',
    'state.sqlite',
    'state.sqlite-wal',
    'state.sqlite-shm',
    'runtime-lock.sqlite',
    'cache.sqlite3',
    'commands.sqlite',
    'pairing-epoch',
    'identity.lock',
    'admission.lock',
  ]
  return markers.some((name) => existsSync(join(stateRoot, name)))
}

function unsafeDelete(stateRoot, directory) {
  const resolved = resolve(stateRoot, directory)
  const rel = relative(stateRoot, resolved)
  if (rel === '' || rel === '.' || rel.startsWith('..') || isAbsolute(rel)) fail('registry_unsafe_state', 'refusing to remove an unsafe profile path')
  const metadata = lstatSync(resolved)
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) fail('registry_unsafe_state', 'refusing to remove a non-directory profile state')
  rmSync(resolved, { recursive: true, force: true })
}

export class HostProfileRegistry {
  #stateRoot
  #now
  #random
  #fs
  #io
  #legacy
  #lockTimeoutMs

  constructor({ stateRoot, now = Date.now, random = randomBytes, fs = null, atomicWrite = atomicWriteFile, legacy = false, lockTimeoutMs = 2000 } = {}) {
    this.#stateRoot = normalizeStateRoot(stateRoot)
    this.#now = now
    this.#random = random
    this.#fs = fs
    this.#io = atomicWrite
    this.#legacy = Boolean(legacy)
    this.#lockTimeoutMs = lockTimeoutMs
  }

  get stateRoot() {
    return this.#stateRoot
  }

  get filePath() {
    return join(this.#stateRoot, HOST_PROFILES_FILE)
  }

  /** Bootstrap a fresh default or adopt existing manual state. */
  ensure() {
    return this.#withLock(() => {
      const existing = this.#read()
      if (existing) return existing
      if (existsSync(join(this.#stateRoot, HOST_PROFILES_DIR))) {
        fail('registry_corrupt', 'profile state exists without a profile registry; repair or remove it explicitly')
      }
      ensurePrivateDirectory(this.#stateRoot)
      const createdAt = nowMs(this.#now)
      let registry
      if (this.#legacy || hasLegacyMarkers(this.#stateRoot)) {
        registry = {
          schema: HOST_PROFILES_SCHEMA,
          schemaVersion: HOST_PROFILES_SCHEMA_VERSION,
          revision: 1,
          activationGeneration: 1,
          active: LEGACY_PROFILE_ID,
          developerToolsEnabled: false,
          prepared: null,
          profiles: [{
            id: LEGACY_PROFILE_ID,
            label: LEGACY_PROFILE_LABEL,
            environment: 'standalone',
            mode: 'standalone',
            service: null,
            stateDir: '.',
            bindings: {},
            pending: {},
            legacy: true,
            createdAt,
          }],
        }
      } else {
        const id = createProfileId(this.#random)
        const record = {
          id,
          label: OFFICIAL_SERVICES.production.label,
          environment: 'production',
          mode: 'managed',
          service: { ...OFFICIAL_SERVICES.production },
          stateDir: `${HOST_PROFILES_DIR}/${id}`,
          bindings: {},
          pending: {},
          legacy: false,
          createdAt,
        }
        ensurePrivateDirectory(join(this.#stateRoot, record.stateDir))
        registry = {
          schema: HOST_PROFILES_SCHEMA,
          schemaVersion: HOST_PROFILES_SCHEMA_VERSION,
          revision: 1,
          activationGeneration: 1,
          active: id,
          developerToolsEnabled: false,
          prepared: null,
          profiles: [record],
        }
      }
      this.#write(registry)
      return cloneRegistry(registry)
    })
  }

  read() {
    return this.#read()
  }

  active() {
    const registry = this.ensure()
    return registry.profiles.find((record) => record.id === registry.active)
  }

  get(id) {
    const registry = this.ensure()
    return registry.profiles.find((record) => record.id === id) ?? null
  }

  list() {
    return cloneRegistry(this.ensure()).profiles
  }

  stateDirFor(id) {
    const record = this.get(id)
    if (!record) throw new RegistryError('profile_not_found', 'profile does not exist')
    return profileStateDir(this.#stateRoot, record)
  }

  markDeveloperTools(enabled = true) {
    return this.#mutate((registry) => {
      registry.developerToolsEnabled = Boolean(enabled)
    })
  }

  /** Add official staging without activating it. Idempotent for repeated enable. */
  enableStaging() {
    return this.#mutate((registry) => {
      registry.developerToolsEnabled = true
      let record = registry.profiles.find((profile) => profile.service?.id === OFFICIAL_SERVICES.staging.id)
      if (record) return record
      const id = createProfileId(this.#random)
      record = {
        id,
        label: OFFICIAL_SERVICES.staging.label,
        environment: 'staging',
        mode: 'managed',
        service: { ...OFFICIAL_SERVICES.staging },
        stateDir: `${HOST_PROFILES_DIR}/${id}`,
        bindings: {},
        pending: {},
        legacy: false,
        createdAt: nowMs(this.#now),
      }
      ensurePrivateDirectory(join(this.#stateRoot, record.stateDir))
      registry.profiles.push(record)
      return record
    })
  }

  addProfile({ label, environment, mode = 'managed', service = null, bindings = {}, pending = {} }) {
    return this.#mutate((registry) => {
      if (typeof label !== 'string' || label.trim() === '' || label.trim() !== label || label.length > 64) {
        fail('profile_conflict', 'profile label is invalid')
      }
      if (registry.profiles.some((profile) => profile.label.toLowerCase() === label.toLowerCase())) {
        fail('profile_conflict', 'a profile with this label already exists')
      }
      if (mode !== 'managed') fail('profile_conflict', 'only managed profiles can be added')
      const id = createProfileId(this.#random)
      const record = {
        id,
        label,
        environment,
        mode,
        service,
        stateDir: `${HOST_PROFILES_DIR}/${id}`,
        bindings,
        pending,
        legacy: false,
        createdAt: nowMs(this.#now),
      }
      ensurePrivateDirectory(join(this.#stateRoot, record.stateDir))
      validateRecord(record, this.#stateRoot)
      registry.profiles.push(record)
      return record
    })
  }

  setBindings(id, bindings, { expectedRevision = null } = {}) {
    return this.#mutate((registry) => {
      if (expectedRevision !== null && registry.revision !== expectedRevision) {
        fail('registry_write_failed', 'profile registry changed; retry the operation')
      }
      const record = registry.profiles.find((profile) => profile.id === id)
      if (!record) fail('profile_not_found', 'profile does not exist')
      const next = { ...record.bindings, ...bindings }
      for (const [key, value] of Object.entries(next)) {
        if (value === undefined || value === null || value === '') delete next[key]
      }
      validateBindings(next)
      record.bindings = next
      return record
    })
  }

  setPending(id, pending, { expectedRevision = null } = {}) {
    return this.#mutate((registry) => {
      if (expectedRevision !== null && registry.revision !== expectedRevision) {
        fail('registry_write_failed', 'profile registry changed; retry the operation')
      }
      const record = registry.profiles.find((profile) => profile.id === id)
      if (!record) fail('profile_not_found', 'profile does not exist')
      const next = { ...record.pending, ...pending }
      for (const [key, value] of Object.entries(next)) {
        if (value === undefined || value === null || value === false) delete next[key]
      }
      validatePending(next)
      record.pending = next
      return record
    })
  }

  clearPendingPhase(id, { expectedRevision = null } = {}) {
    return this.#mutate((registry) => {
      if (expectedRevision !== null && registry.revision !== expectedRevision) {
        fail('registry_write_failed', 'profile registry changed; retry the operation')
      }
      const record = registry.profiles.find((profile) => profile.id === id)
      if (!record) fail('profile_not_found', 'profile does not exist')
      record.pending = { ...record.pending }
      delete record.pending.phase
      delete record.pending.uncertain
      if (record.pending.removal === undefined) delete record.pending.removal
      validatePending(record.pending)
      return record
    })
  }

  /** Record the two-phase activation intent without publishing it. */
  prepareActivation(id, expectedRevision) {
    return this.#mutate((registry) => {
      if (registry.revision !== expectedRevision) fail('registry_write_failed', 'profile registry changed; retry the switch')
      const record = registry.profiles.find((profile) => profile.id === id)
      if (!record) fail('profile_not_found', 'profile does not exist')
      if (record.id === registry.active) fail('profile_conflict', 'profile is already active')
      registry.prepared = {
        profileId: id,
        expectedRevision,
        generation: registry.activationGeneration + 1,
      }
      return registry.prepared
    })
  }

  /** Publish a prepared activation only when the registry revision is unchanged. */
  commitActivation(id, expectedRevision) {
    return this.#mutate((registry) => {
      if (registry.revision !== expectedRevision) fail('registry_write_failed', 'profile registry changed; retry the switch')
      if (registry.prepared === null || registry.prepared.profileId !== id) {
        fail('registry_write_failed', 'activation was not prepared')
      }
      const record = registry.profiles.find((profile) => profile.id === id)
      if (!record) fail('profile_not_found', 'profile does not exist')
      registry.active = id
      registry.activationGeneration += 1
      registry.prepared = null
      return record
    })
  }

  abortActivation(expectedRevision) {
    return this.#mutate((registry) => {
      if (registry.revision !== expectedRevision) fail('registry_write_failed', 'profile registry changed; retry the switch')
      if (registry.prepared === null) return null
      registry.prepared = null
      return null
    })
  }

  /** Recover a half-written activation. A prepared marker never selects. */
  recoverPrepared() {
    const registry = this.#read()
    if (!registry || registry.prepared === null) return null
    return this.#mutate((current) => {
      if (current.prepared === null) return null
      current.prepared = null
      return null
    })
  }

  removeProfile(id) {
    return this.#withLock(() => {
      const registry = this.#read()
      if (!registry) fail('registry_corrupt', 'profile registry is missing')
      if (registry.active === id) fail('profile_conflict', 'the active profile cannot be deleted')
      const record = registry.profiles.find((profile) => profile.id === id)
      if (!record) fail('profile_not_found', 'profile does not exist')
      if (record.legacy) fail('profile_conflict', 'the legacy Standalone profile state cannot be deleted here')
      if (registry.profiles.length <= 1) fail('profile_conflict', 'the last profile cannot be deleted')
      validateRegistry(registry, this.#stateRoot)
      unsafeDelete(this.#stateRoot, record.stateDir)
      registry.profiles = registry.profiles.filter((profile) => profile.id !== id)
      registry.prepared = null
      this.#write(registry)
      return { profile: cloneRegistry(record), remoteRevocation: 'not_attempted' }
    })
  }

  #read() {
    let bytes
    try {
      bytes = readFileSync(this.filePath)
    } catch (error) {
      if (error?.code === 'ENOENT') return null
      fail('registry_corrupt', 'profile registry could not be read')
    }
    let registry
    try {
      registry = JSON.parse(bytes.toString('utf8'))
    } catch {
      fail('registry_corrupt', 'profile registry is not valid JSON')
    }
    validateRegistry(registry, this.#stateRoot)
    return registry
  }

  #write(registry) {
    validateRegistry(registry, this.#stateRoot)
    const bytes = Buffer.from(`${JSON.stringify(registry, null, 2)}\n`, 'utf8')
    try {
      this.#io(this.filePath, bytes, { fs: this.#fs })
    } catch (error) {
      if (error instanceof RegistryError) throw error
      fail('registry_write_failed', 'profile registry could not be written')
    }
    return cloneRegistry(registry)
  }

  #mutate(fn) {
    return this.#withLock(() => {
      const registry = this.#read()
      if (!registry) fail('registry_corrupt', 'profile registry is missing')
      const outcome = fn(registry)
      registry.revision += 1
      this.#write(registry)
      return outcome === undefined ? cloneRegistry(registry) : cloneRegistry(outcome)
    })
  }

  #withLock(fn) {
    ensurePrivateDirectory(this.#stateRoot)
    let database
    try {
      database = new DatabaseSync(join(this.#stateRoot, 'profiles.lock.sqlite'))
      database.exec(`PRAGMA busy_timeout=${this.#lockTimeoutMs}; CREATE TABLE IF NOT EXISTS registry_guard (id INTEGER PRIMARY KEY); BEGIN IMMEDIATE;`)
    } catch {
      try {
        database?.close()
      } catch {
        // contained
      }
      fail('registry_lock_failed', 'profile registry is busy; retry')
    }
    try {
      const outcome = fn()
      database.exec('COMMIT')
      return outcome
    } catch (error) {
      try {
        database.exec('ROLLBACK')
      } catch {
        // the original error is more useful
      }
      throw error
    } finally {
      try {
        database.close()
      } catch {
        // contained
      }
    }
  }
}

export function atomicWriteJson(path, value, options = {}) {
  atomicWriteFile(path, Buffer.from(`${JSON.stringify(value, null, 2)}\n`, 'utf8'), options)
}

export { cloneRegistry, hasLegacyMarkers, profileStateDir }
