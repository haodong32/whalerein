// Test/local-run credentials, separate from conversation sources.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { dirname } from 'node:path'
import { AdapterError } from '../errors.js'
export class MemoryHostCredentials {
  #credentials = new Map()
  #credentialFile
  constructor({ credentials = new Map(), credentialFile = '' } = {}) {
    this.#credentialFile = credentialFile
    for (const [key, value] of credentials) this.#credentials.set(key, value)
    if (credentialFile) {
      mkdirSync(dirname(credentialFile), { recursive: true })
      try {
        const parsed = JSON.parse(readFileSync(credentialFile, 'utf8'))
        for (const [key, value] of Object.entries(parsed)) this.#credentials.set(key, value)
      } catch (error) {
        if (error.code !== 'ENOENT') throw new AdapterError('internal', 'fake credential file unreadable')
      }
    }
  }
  // --- credentials --------------------------------------------------------

  async getCredential(name) {
    return this.#credentials.get(name) ?? null
  }

  async setCredential(name, value) {
    this.#credentials.set(name, value)
    if (this.#credentialFile) {
      const document = Object.fromEntries(this.#credentials)
      writeFileSync(this.#credentialFile, JSON.stringify(document), { mode: 0o600 })
    }
  }
}
