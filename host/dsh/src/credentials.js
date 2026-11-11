import { mapAdapterError } from './adapter/errors.js'
// DSH credential binding. Source adapters never load Host keys.
import { AdapterError } from 'whalerein-host/errors.js'
import { sha256Text } from 'whalerein-host/security/digest.js'
const HOST_KEY_SCOPE = 'whalerein-dsh'
const HOST_KEY_REF = 'WHALEREIN_HOST_PRIVATE_KEY'

export class DshHostCredentials {
  constructor({ctx, hostId = ''} = {}) { this.ctx=ctx; this.hostId=hostId }
  #service(name) { try { return this.ctx?.[name] } catch { return undefined } }
  // --- credentials --------------------------------------------------------

  #credentialKey() {
    // DSH credential record keys are slash-separated segments matching
    // /^[a-z][a-z0-9-]*$/. Hash to lowercase hex, never interpolate the
    // Uint8Array digest (which would produce comma-separated bytes).
    const suffix = Buffer.from(sha256Text(this.hostId).slice(0, 8)).toString('hex')
    return `${HOST_KEY_SCOPE}/host-key-${suffix}`
  }

  async getCredential() {
    const credentials = this.#service('credentials')
    if (!credentials) return null
    try {
      if (typeof credentials.readRecord === 'function') {
        const record = await credentials.readRecord(this.#credentialKey())
        if (!record || record.kind !== 'grant') return null
        const value = record.payload && typeof record.payload === 'object'
          ? record.payload.privateKeyBase64
          : undefined
        return typeof value === 'string' ? value : null
      }
      if (typeof credentials.resolve === 'function') {
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
    try {
      if (typeof credentials.modifyRecord === 'function') {
        await credentials.modifyRecord(this.#credentialKey(), async () => ({
          kind: 'grant',
          payload: { privateKeyBase64: String(value) },
        }))
        return
      }
      if (typeof credentials.set === 'function') {
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
