import { mapAdapterError } from './adapter/errors.js'
// DSH credential binding. Source adapters never load Host keys.
import { AdapterError } from 'whalerein-host/errors.js'
import { sha256Text } from 'whalerein-host/security/digest.js'
const HOST_KEY_SCOPE = 'whalerein-dsh'
const HOST_KEY_REF = 'WHALEREIN_HOST_PRIVATE_KEY'
// The shared manager scopes purpose keys under this namespace. Legacy and
// standalone HostRuntime callers use one fixed record instead.
const MANAGED_CREDENTIAL_PREFIX = 'whalerein-host/v1/'
// The record key is intentionally independent of host_id. Renaming a host must
// not move its credential namespace: otherwise the next start finds a public
// key reference in SQLite with no credential and refuses to start (PLUG-05).
const HOST_KEY_SUFFIX = 'host-key'

export class DshHostCredentials {
  constructor({ctx, hostId = '', profileId = ''} = {}) {
    this.ctx = ctx
    this.hostId = hostId
    this.profileId = typeof profileId === 'string' ? profileId : ''
  }
  #service(name) { try { return this.ctx?.[name] } catch { return undefined } }
  // --- credentials --------------------------------------------------------

  #credentialKey() {
    // DSH credential record keys are slash-separated segments matching
    // /^[a-z][a-z0-9-]*$/. Managed profiles use an opaque, stable scope so two
    // profiles never share signing/poll/transport secrets. Dashes and hex keep
    // a hostile display/profile id out of the record key itself.
    const scope = this.profileId
      ? `profile-${Buffer.from(sha256Text(this.profileId).slice(0, 12)).toString('hex')}`
      : ''
    return scope
      ? `${HOST_KEY_SCOPE}/${scope}/${HOST_KEY_SUFFIX}`
      : `${HOST_KEY_SCOPE}/${HOST_KEY_SUFFIX}`
  }

  // A manager purpose key already includes installation and opaque profile id.
  // Hash it into one valid DSH record segment so a hostile scope cannot create
  // path-like credential keys. Non-manager names keep the legacy fixed record.
  #namedCredentialKey(name) {
    return `${HOST_KEY_SCOPE}/cred-${Buffer.from(sha256Text(name).slice(0, 40)).toString('hex')}`
  }

  #recordKeyFor(name) {
    return typeof name === 'string' && name.startsWith(MANAGED_CREDENTIAL_PREFIX)
      ? this.#namedCredentialKey(name)
      : this.#credentialKey()
  }

  // Earlier releases derived the record key from host_id. Read that location
  // once and migrate it so an upgrade preserves the existing host identity.
  #legacyCredentialKey() {
    // Never migrate the standalone single-host key into a managed profile:
    // that would silently give a cloud profile a legacy Host identity.
    if (this.profileId || !this.hostId) return ''
    const suffix = Buffer.from(sha256Text(this.hostId).slice(0, 8)).toString('hex')
    return `${HOST_KEY_SCOPE}/host-key-${suffix}`
  }

  async #readRecord(credentials, key) {
    if (typeof credentials.readRecord !== 'function') return undefined
    const record = await credentials.readRecord(key)
    if (!record || record.kind !== 'grant') return null
    const value = record.payload && typeof record.payload === 'object'
      ? record.payload.privateKeyBase64
      : undefined
    return typeof value === 'string' ? value : null
  }

  async #writeRecord(credentials, key, value) {
    await credentials.modifyRecord(key, async () => ({
      kind: 'grant',
      payload: { privateKeyBase64: String(value) },
    }))
  }

  async getCredential(name) {
    const credentials = this.#service('credentials')
    if (!credentials) return null
    const managed = typeof name === 'string' && name.startsWith(MANAGED_CREDENTIAL_PREFIX)
    const recordKey = this.#recordKeyFor(name)
    try {
      if (typeof credentials.readRecord === 'function') {
        const current = await this.#readRecord(credentials, recordKey)
        if (managed) return current
        if (current !== null && current !== undefined) return current
        const legacyKey = this.#legacyCredentialKey()
        if (!legacyKey || legacyKey === recordKey) return current ?? null
        const legacy = await this.#readRecord(credentials, legacyKey)
        if (legacy === null) return null
        try {
          // Best-effort migration: a failed copy still returns the legacy key,
          // and the next start retries the same read.
          await this.#writeRecord(credentials, recordKey, legacy)
        } catch {
          // The legacy record remains authoritative for this run.
        }
        return legacy
      }
      if (typeof credentials.resolve === 'function') {
        // Reference-style stores have no named records; manager scopes are
        // unavailable there rather than collapsing every secret into one ref.
        if (managed) return null
        const resolved = await credentials.resolve(HOST_KEY_REF)
        return resolved?.value ?? null
      }
      return null
    } catch (error) {
      throw new AdapterError(mapAdapterError(error), 'credential read failed')
    }
  }

  async setCredential(name, value) {
    const credentials = this.#service('credentials')
    if (!credentials) throw new AdapterError('unavailable', 'credential store unavailable')
    const managed = typeof name === 'string' && name.startsWith(MANAGED_CREDENTIAL_PREFIX)
    const recordKey = this.#recordKeyFor(name)
    try {
      if (typeof credentials.modifyRecord === 'function') {
        await this.#writeRecord(credentials, recordKey, value)
        return
      }
      if (typeof credentials.set === 'function') {
        if (managed) throw new AdapterError('unsupported_capability', 'named credential records unavailable')
        await credentials.set(HOST_KEY_REF, String(value))
        return
      }
      throw new AdapterError('unsupported_capability', 'credential record store unavailable')
    } catch (error) {
      if (error instanceof AdapterError) throw error
      throw new AdapterError(mapAdapterError(error), 'credential write failed')
    }
  }

}
