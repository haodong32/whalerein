// Installation-scoped activation ownership for profile switching.
//
// The lock is the same OS-released SQLite instance guard used by the Host
// runtime: `BEGIN EXCLUSIVE` on a dedicated database file. It is released when
// the file handle closes or the process dies, so crash recovery never trusts a
// stale PID file. A second process attempting to own the installation gets a
// conflict instead of running a conflicting active selection.

import { join } from 'node:path'
import { lockInstance } from './state/instance-lock.js'
import { PluginError } from './errors.js'

export class ActivationOwner {
  #stateRoot
  #lockFn
  #lock = null

  constructor({ stateRoot, lock = lockInstance } = {}) {
    if (typeof stateRoot !== 'string' || stateRoot === '') throw new PluginError('invalid_argument', 'activation state root is required')
    this.#stateRoot = stateRoot
    this.#lockFn = lock
  }

  get owned() {
    return this.#lock !== null
  }

  /** Acquire once for this process; repeating while owned is a no-op. */
  acquire() {
    if (this.#lock) return this
    try {
      this.#lock = this.#lockFn(join(this.#stateRoot, 'activation'))
    } catch (error) {
      if (error instanceof PluginError) throw error
      throw new PluginError('conflict', 'another Host process owns the active profile')
    }
    return this
  }

  release() {
    const lock = this.#lock
    this.#lock = null
    try {
      lock?.close()
    } catch {
      // the OS releases the lock even if close reports an error
    }
  }
}
