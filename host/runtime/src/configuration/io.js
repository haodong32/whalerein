// Durable replacement and the cross-runtime SQLite writer lock.
//
// The lock sidecar is `<canonical-config>.lock.sqlite` with table
// `whalerein_config_lock`. SQLite `BEGIN IMMEDIATE` serializes cooperating Rust
// Core and Node Host writers and the OS releases the lock when a process dies.
// The sidecar is coordination state only and is never a configuration source.
import {
  closeSync,
  fsyncSync,
  mkdirSync,
  openSync,
  renameSync,
  unlinkSync,
  writeSync,
} from 'node:fs'
import { randomBytes } from 'node:crypto'
import { DatabaseSync } from 'node:sqlite'
import { dirname } from 'node:path'
import { ConfigurationError, configError } from './errors.js'

const defaultIo = Object.freeze({ closeSync, fsyncSync, mkdirSync, openSync, renameSync, unlinkSync, writeSync })

export function lockPathFor(configPath) {
  return `${configPath}.lock.sqlite`
}

/**
 * Write bytes to a temp file in the target directory, fsync, rename, then fsync
 * the directory. A refused write/rename leaves the previous file untouched and
 * reports a typed error; a crash can leave a complete old or new document.
 */
export function atomicReplace(path, bytes, options = {}) {
  const io = options.io ?? defaultIo
  const temporary = `${path}.${process.pid}.${randomBytes(6).toString('hex')}.tmp`
  let fd
  try {
    io.mkdirSync(dirname(path), { recursive: true, mode: 0o700 })
    fd = io.openSync(temporary, 'wx', 0o600)
    let offset = 0
    while (offset < bytes.length) {
      const written = io.writeSync(fd, bytes, offset, bytes.length - offset)
      if (!Number.isSafeInteger(written) || written <= 0) throw new Error('short write')
      offset += written
    }
    io.fsyncSync(fd)
    io.closeSync(fd)
    fd = undefined
    if (options.hooks?.beforeRename) options.hooks.beforeRename(path)
    io.renameSync(temporary, path)
    if (options.hooks?.afterRename) options.hooks.afterRename(path)
    try {
      const directory = io.openSync(dirname(path), 'r')
      try {
        io.fsyncSync(directory)
      } finally {
        io.closeSync(directory)
      }
    } catch {
      // Directory fsync is unavailable on some platforms (Windows).
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
    if (error instanceof ConfigurationError) throw error
    throw configError('unreadable_config', 'configuration could not be written durably', { operation: 'write' })
  }
}

/**
 * Run `fn` while holding the cross-runtime writer lock. Lock acquisition
 * failures surface as a revision conflict so callers retry after a reread.
 */
export function withConfigLock(lockPath, fn, details = {}) {
  let database
  try {
    mkdirSync(dirname(lockPath), { recursive: true, mode: 0o700 })
    database = new DatabaseSync(lockPath)
    database.exec('PRAGMA busy_timeout = 30000')
    database.exec('CREATE TABLE IF NOT EXISTS whalerein_config_lock (id INTEGER PRIMARY KEY)')
    database.exec('BEGIN IMMEDIATE')
  } catch {
    try {
      database?.close()
    } catch {
      // contained
    }
    throw configError('unreadable_config', 'configuration writer lock is unavailable', {
      operation: 'lock',
      currentRevision: details.currentRevision ?? null,
    })
  }
  try {
    return fn()
  } finally {
    try {
      database.exec('ROLLBACK')
    } catch {
      // contained
    }
    try {
      database.close()
    } catch {
      // contained
    }
  }
}
